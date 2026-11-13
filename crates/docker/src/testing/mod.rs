//! A fake Docker Engine API on a Unix socket, for tests (docker spec,
//! "Testing strategy"): sb-docker's own tests and sb's end-to-end tests
//! (through the `testing` feature; not in sb's release or production
//! builds). It serves the synthetic fixtures (documentation addresses only)
//! on the paths bollard sends, behaves like dockerd about API versions and
//! `all`, and can make chosen containers vanish or fail, hold inspects
//! open, and record what it was sent. axum on a `UnixListener`
//! (<https://docs.rs/axum/0.8.9/axum/serve/trait.Listener.html>), on a
//! runtime of its own thread, so a test can drive it from anywhere.

pub mod controller;
mod logs;

pub use logs::{FakeEntry, FakeLog};

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bollard::models::{ContainerInspectResponse, ContainerSummary};
use serde_json::{Value, json};

use crate::ps::{Row, sorted};

/// The fixtures: Docker's inspect answers for a few made-up containers.
pub const FIXTURES: &str = include_str!("../fixtures/containers.json");

/// The fixtures as raw JSON, one value per container.
#[must_use]
pub fn inspects() -> Vec<Value> {
    serde_json::from_str(FIXTURES).unwrap_or_default()
}

/// A container's list entry as Docker would give it for its inspect: the
/// ID, the name (left out when the inspect has none) and the state word.
#[must_use]
pub fn summary(inspect: &Value) -> Value {
    let mut entry = json!({ "Id": inspect["Id"] });
    if let Some(name) = inspect.get("Name") {
        entry["Names"] = json!([name]);
    }
    if let Some(state) = inspect.pointer("/State/Status") {
        entry["State"] = state.clone();
    }
    entry
}

/// The fixtures through bollard's models: (list entry, inspect) pairs.
#[must_use]
pub fn fixture_models() -> Vec<(ContainerSummary, ContainerInspectResponse)> {
    inspects()
        .iter()
        .filter_map(|inspect| {
            let summary = serde_json::from_value(summary(inspect)).ok()?;
            let inspect = serde_json::from_value(inspect.clone()).ok()?;
            Some((summary, inspect))
        })
        .collect()
}

/// The fixtures' rows, sorted by name, as `ps` builds them.
#[must_use]
pub fn fixture_rows() -> Vec<Row> {
    sorted(
        fixture_models()
            .iter()
            .map(|(summary, inspect)| Row::new(summary, inspect))
            .collect(),
    )
}

/// How the fake behaves.
#[derive(Clone, Debug)]
pub struct Options {
    /// The daemon's API versions (minimum, maximum): versioned paths
    /// outside them are refused with 400, as dockerd refuses them (Docker
    /// 29: 1.40 to 1.55; platform note).
    pub api_versions: ((u32, u32), (u32, u32)),
    /// Refuse unversioned paths (what Docker's planned removal would do).
    pub refuse_unversioned: bool,
    /// The containers' inspect answers (default: the fixtures).
    pub inspects: Vec<Value>,
    /// The list answer (default: built from `inspects`; without `all=true`
    /// only the running ones, as dockerd).
    pub list: Option<Value>,
    /// IDs whose inspect answers 404 (removed since the list).
    pub vanished: HashSet<String>,
    /// IDs whose inspect answers 500.
    pub failing: HashSet<String>,
    /// Hold every inspect open until [`FakeEngine::release`].
    pub hold: bool,
    /// The containers' logs, by ID (`/containers/{id}/logs`; an ID
    /// without one answers 404).
    pub logs: HashMap<String, FakeLog>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            api_versions: ((1, 40), (1, 53)),
            refuse_unversioned: false,
            inspects: inspects(),
            list: None,
            vanished: HashSet::new(),
            failing: HashSet::new(),
            hold: false,
            logs: HashMap::new(),
        }
    }
}

struct Shared {
    options: Options,
    paths: Mutex<Vec<String>>,
    open: AtomicUsize,
    peak: AtomicUsize,
    release: tokio::sync::watch::Sender<bool>,
    /// The logs, appended to by [`FakeEngine::append`].
    logs: Mutex<HashMap<String, FakeLog>>,
    /// IDs whose follows end once they have sent everything.
    ended: Mutex<HashSet<String>>,
    /// Sent on every append and end, to wake the follows waiting
    /// (`send_replace` notifies whether or not anyone waits,
    /// <https://docs.rs/tokio/1.54.1/tokio/sync/watch/struct.Sender.html#method.send_replace>).
    changes: tokio::sync::watch::Sender<()>,
}

impl Shared {
    fn new(options: Options) -> Arc<Self> {
        Arc::new(Shared {
            paths: Mutex::new(Vec::new()),
            open: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            release: tokio::sync::watch::channel(false).0,
            logs: Mutex::new(options.logs.clone()),
            ended: Mutex::new(HashSet::new()),
            changes: tokio::sync::watch::channel(()).0,
            options,
        })
    }

    fn max_version(&self) -> String {
        version_text(self.options.api_versions.1)
    }
}

/// A running fake; it stops when dropped, letting held inspects answer.
pub struct FakeEngine {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    shared: Arc<Shared>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for FakeEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeEngine")
            .field("socket", &self.socket)
            .finish_non_exhaustive()
    }
}

impl FakeEngine {
    /// Start a fake with `options` on a socket in a new temporary directory.
    ///
    /// # Errors
    ///
    /// The I/O error when the temporary directory or the socket can't be
    /// made.
    pub fn start(options: Options) -> std::io::Result<Self> {
        let dir = tempfile::tempdir()?;
        let socket = dir.path().join("docker.sock");
        // Bound here, so the socket exists before `start` returns and a
        // failure comes back as an error; the runtime lives on the thread.
        let listener = std::os::unix::net::UnixListener::bind(&socket)?;
        listener.set_nonblocking(true)?;
        let shared = Shared::new(options);
        let app = router(Arc::clone(&shared));
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        // The thread reports whether it is serving, so a failure to start
        // comes back from `start` instead of as a refused connection later.
        let (ready, started) = std::sync::mpsc::channel::<std::io::Result<()>>();
        let thread = std::thread::Builder::new()
            .name("fake-engine".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        let _ = ready.send(Err(err));
                        return;
                    }
                };
                runtime.block_on(async move {
                    let listener = match tokio::net::UnixListener::from_std(listener) {
                        Ok(listener) => listener,
                        Err(err) => {
                            let _ = ready.send(Err(err));
                            return;
                        }
                    };
                    let _ = ready.send(Ok(()));
                    let _ = axum::serve(listener, app)
                        .with_graceful_shutdown(async move {
                            let _ = stopped.await;
                        })
                        .await;
                });
            })?;
        started
            .recv()
            .unwrap_or_else(|_| Err(std::io::Error::other("the fake's thread ended")))?;
        Ok(FakeEngine {
            _dir: dir,
            socket,
            shared,
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    /// The `DOCKER_HOST` value that reaches this fake.
    #[must_use]
    pub fn host(&self) -> String {
        format!("unix://{}", self.socket.display())
    }

    /// The paths it was sent, in order, with their query strings.
    #[must_use]
    pub fn paths(&self) -> Vec<String> {
        lock(&self.shared.paths).clone()
    }

    /// How many inspects are open right now (held ones included).
    #[must_use]
    pub fn open_inspects(&self) -> usize {
        self.shared.open.load(Ordering::SeqCst)
    }

    /// The most inspects that were open at once.
    #[must_use]
    pub fn peak_inspects(&self) -> usize {
        self.shared.peak.load(Ordering::SeqCst)
    }

    /// Let held inspects answer, now and later ones too. `send_replace`
    /// stores the value even while no inspect is waiting (`send` would
    /// drop it, <https://docs.rs/tokio/1.54.1/tokio/sync/watch/struct.Sender.html#method.send>).
    pub fn release(&self) {
        self.shared.release.send_replace(true);
    }

    /// Add `entry` to `id`'s log (a new log if it had none); a follow of it
    /// sends it.
    pub fn append(&self, id: &str, entry: FakeEntry) {
        lock(&self.shared.logs)
            .entry(id.to_string())
            .or_default()
            .entries
            .push(entry);
        self.shared.changes.send_replace(());
    }

    /// End `id`'s follows once they have sent what its log holds, as
    /// dockerd ends them when the container stops; later follows end at
    /// once.
    pub fn end_follows(&self, id: &str) {
        lock(&self.shared.ended).insert(id.to_string());
        self.shared.changes.send_replace(());
    }
}

impl Drop for FakeEngine {
    fn drop(&mut self) {
        self.release();
        // Open follows end, so the graceful shutdown doesn't wait on them.
        let ids: Vec<String> = lock(&self.shared.logs).keys().cloned().collect();
        lock(&self.shared.ended).extend(ids);
        self.shared.changes.send_replace(());
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn version_text((major, minor): (u32, u32)) -> String {
    format!("{major}.{minor}")
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

type Fake = State<Arc<Shared>>;

fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/_ping", get(ping))
        .route("/{version}/_ping", get(ping_at))
        .route("/version", get(version))
        .route("/{version}/version", get(version_at))
        .route("/containers/json", get(list))
        .route("/{version}/containers/json", get(list_at))
        .route("/containers/{id}/json", get(inspect))
        .route("/{version}/containers/{id}/json", get(inspect_at))
        .route("/containers/{id}/logs", get(logs::logs))
        .route("/{version}/containers/{id}/logs", get(logs::logs_at))
        .fallback(not_found)
        .method_not_allowed_fallback(not_found)
        .with_state(shared)
}

/// A JSON answer with dockerd's `Api-Version` header, which it sets on
/// every answer, errors included.
fn answer(shared: &Shared, status: StatusCode, body: Value) -> Response {
    (
        status,
        [("Api-Version", shared.max_version())],
        axum::Json(body),
    )
        .into_response()
}

/// Every request is recorded, its query string included.
fn record(shared: &Shared, uri: &Uri) {
    let path = uri
        .path_and_query()
        .map_or_else(|| uri.path().to_string(), |p| p.as_str().to_string());
    lock(&shared.paths).push(path);
}

/// dockerd's version check (its `version.go`): `None` when the path's
/// version (if any) is served, else its 400.
fn refused(shared: &Shared, version: Option<&str>) -> Option<Response> {
    let (min, max) = shared.options.api_versions;
    let refuse = |message: String| {
        Some(answer(
            shared,
            StatusCode::BAD_REQUEST,
            json!({ "message": message }),
        ))
    };
    let Some(version) = version else {
        return if shared.options.refuse_unversioned {
            refuse("API without a version prefix is not supported".into())
        } else {
            None
        };
    };
    let text = version.trim_start_matches('v');
    let asked = version
        .strip_prefix('v')
        .and_then(|v| v.split_once('.'))
        .and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?)));
    match asked {
        Some(asked) if asked < min => refuse(format!(
            "client version {text} is too old. Minimum supported API version is {}",
            version_text(min)
        )),
        Some(asked) if asked <= max => None,
        _ => refuse(format!(
            "client version {text} is too new. Maximum supported API version is {}",
            version_text(max)
        )),
    }
}

/// dockerd's answer to a path it doesn't serve.
async fn not_found(State(shared): Fake, uri: Uri) -> Response {
    record(&shared, &uri);
    answer(
        &shared,
        StatusCode::NOT_FOUND,
        json!({ "message": "page not found" }),
    )
}

/// dockerd's ping: `OK` and its `Api-Version` header, which clients such as
/// sb-go's negotiate their version from (dockerd's other ping headers,
/// such as `Swarm` and `Builder-Version`, are left out; clients tolerate
/// that).
async fn ping(State(shared): Fake, uri: Uri) -> Response {
    ping_answer(&shared, &uri, None)
}

async fn ping_at(State(shared): Fake, uri: Uri, Path(v): Path<String>) -> Response {
    ping_answer(&shared, &uri, Some(&v))
}

fn ping_answer(shared: &Shared, uri: &Uri, version: Option<&str>) -> Response {
    record(shared, uri);
    // An unversioned ping stays served even with `refuse_unversioned`:
    // clients negotiate through it before they know a version (an
    // assumption about Docker's planned removal, not checked against it).
    if let Some(refusal) = version.and_then(|v| refused(shared, Some(v))) {
        return refusal;
    }
    (
        StatusCode::OK,
        [("Api-Version", shared.max_version())],
        "OK",
    )
        .into_response()
}

async fn version(State(shared): Fake, uri: Uri) -> Response {
    version_answer(&shared, &uri, None)
}

async fn version_at(State(shared): Fake, uri: Uri, Path(v): Path<String>) -> Response {
    version_answer(&shared, &uri, Some(&v))
}

fn version_answer(shared: &Shared, uri: &Uri, version: Option<&str>) -> Response {
    record(shared, uri);
    if let Some(refusal) = refused(shared, version) {
        return refusal;
    }
    let min = version_text(shared.options.api_versions.0);
    answer(
        shared,
        StatusCode::OK,
        json!({ "Version": "0.0.0-fake", "ApiVersion": shared.max_version(), "MinAPIVersion": min }),
    )
}

async fn list(State(shared): Fake, uri: Uri) -> Response {
    list_answer(&shared, &uri, None)
}

async fn list_at(State(shared): Fake, uri: Uri, Path(v): Path<String>) -> Response {
    list_answer(&shared, &uri, Some(&v))
}

fn list_answer(shared: &Shared, uri: &Uri, version: Option<&str>) -> Response {
    record(shared, uri);
    if let Some(refusal) = refused(shared, version) {
        return refusal;
    }
    let all = uri
        .query()
        .is_some_and(|q| q.split('&').any(|p| p == "all=true" || p == "all=1"));
    let body = shared.options.list.clone().unwrap_or_else(|| {
        Value::Array(
            shared
                .options
                .inspects
                .iter()
                .filter(|i| all || i.pointer("/State/Status") == Some(&json!("running")))
                .map(summary)
                .collect(),
        )
    });
    answer(shared, StatusCode::OK, body)
}

async fn inspect(State(shared): Fake, uri: Uri, Path(id): Path<String>) -> Response {
    inspect_answer(shared, uri, None, id).await
}

async fn inspect_at(
    State(shared): Fake,
    uri: Uri,
    Path((v, id)): Path<(String, String)>,
) -> Response {
    inspect_answer(shared, uri, Some(v), id).await
}

async fn inspect_answer(
    shared: Arc<Shared>,
    uri: Uri,
    version: Option<String>,
    id: String,
) -> Response {
    record(&shared, &uri);
    if let Some(refusal) = refused(&shared, version.as_deref()) {
        return refusal;
    }
    let open = shared.open.fetch_add(1, Ordering::SeqCst) + 1;
    shared.peak.fetch_max(open, Ordering::SeqCst);
    if shared.options.hold {
        let mut released = shared.release.subscribe();
        let _ = released.wait_for(|r| *r).await;
    }
    shared.open.fetch_sub(1, Ordering::SeqCst);
    let not_found = || {
        answer(
            &shared,
            StatusCode::NOT_FOUND,
            json!({ "message": format!("No such container: {id}") }),
        )
    };
    if shared.options.vanished.contains(&id) {
        return not_found();
    }
    if shared.options.failing.contains(&id) {
        return answer(
            &shared,
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "message": "fake inspect failure" }),
        );
    }
    match shared
        .options
        .inspects
        .iter()
        .find(|inspect| inspect["Id"] == id.as_str())
    {
        Some(found) => answer(&shared, StatusCode::OK, found.clone()),
        None => not_found(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Debug` shows the fake's socket only.
    #[test]
    fn debug_shows_the_socket() {
        let engine = FakeEngine::start(Options::default()).expect("engine");
        let shown = format!("{engine:?}");
        assert!(shown.starts_with("FakeEngine { socket: \""), "{shown}");
        assert!(shown.ends_with("\", .. }"), "{shown}");
    }
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    /// One request through the router, as axum's testing example does
    /// (<https://github.com/tokio-rs/axum/blob/axum-v0.8.9/examples/testing/src/main.rs>):
    /// the status, the `Api-Version` header and the body. The socket itself
    /// is exercised by connect.rs's tests through bollard.
    async fn get(shared: &Arc<Shared>, path: &str) -> (StatusCode, String, String) {
        let request = Request::get(path).body(Body::empty()).expect("request");
        let response = router(Arc::clone(shared))
            .oneshot(request)
            .await
            .expect("answer");
        let status = response.status();
        let header = response
            .headers()
            .get("api-version")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        (status, header, String::from_utf8_lossy(&body).into_owned())
    }

    /// The log, unversioned and versioned, as dockerd: frames with the
    /// time; a container without a log is a 404 with its message.
    #[tokio::test]
    async fn logs_answer_on_both_paths() {
        let time: jiff::Timestamp = "2026-11-13T12:00:00.5Z".parse().expect("time");
        let shared = Shared::new(Options {
            logs: [(
                "abc".to_string(),
                FakeLog {
                    tty: false,
                    entries: vec![FakeEntry::new(time, "hi\n")],
                },
            )]
            .into(),
            ..Options::default()
        });
        let frame = "\u{1}\0\0\0\0\0\0\u{22}2026-11-13T12:00:00.500000000Z hi\n";
        for path in ["/containers/abc/logs", "/v1.53/containers/abc/logs"] {
            let (status, _, body) = get(&shared, &format!("{path}?stdout=1&timestamps=1")).await;
            assert_eq!((status, body.as_str()), (StatusCode::OK, frame), "{path}");
        }
        let (status, _, body) = get(&shared, "/v1.53/containers/gone/logs").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("No such container: gone"), "{body}");
        let (status, _, _) = get(&shared, "/v1.99/containers/abc/logs").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn versioned_paths_follow_dockerds_rule() {
        let shared = Shared::new(Options {
            api_versions: ((1, 40), (1, 41)),
            ..Options::default()
        });
        let (status, header, body) = get(&shared, "/v1.53/version").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(header, "1.41");
        assert!(
            body.contains("client version 1.53 is too new. Maximum supported API version is 1.41"),
            "{body}"
        );
        let (status, _, body) = get(&shared, "/v1.24/version").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body.contains("client version 1.24 is too old. Minimum supported API version is 1.40"),
            "{body}"
        );
        let (status, _, body) = get(&shared, "/v1.41/containers/json?all=true").await;
        assert!(status == StatusCode::OK && body.contains("plex"), "{body}");
        assert_eq!(get(&shared, "/v1.40/version").await.0, StatusCode::OK);
        assert_eq!(
            get(&shared, "/vnonsense/version").await.0,
            StatusCode::BAD_REQUEST
        );
        let (status, header, body) = get(&shared, "/version").await;
        assert!(
            status == StatusCode::OK && header == "1.41",
            "{status} {header}"
        );
        assert!(body.contains(r#""MinAPIVersion":"1.40""#), "{body}");
        assert_eq!(
            lock(&shared.paths).clone(),
            [
                "/v1.53/version",
                "/v1.24/version",
                "/v1.41/containers/json?all=true",
                "/v1.40/version",
                "/vnonsense/version",
                "/version",
            ]
        );
    }

    /// Without `all=true`, only the running containers, as dockerd.
    #[tokio::test]
    async fn the_list_honours_all() {
        let shared = Shared::new(Options::default());
        let (_, _, every) = get(&shared, "/containers/json?all=true&size=false").await;
        assert!(
            every.contains("sonarr") && every.contains("lidarr"),
            "{every}"
        );
        let (_, _, one) = get(&shared, "/containers/json?size=false&all=1").await;
        assert!(one.contains("sonarr"), "{one}");
        for path in [
            "/containers/json",
            "/containers/json?size=false",
            "/containers/json?all=false",
        ] {
            let (_, _, running) = get(&shared, path).await;
            assert!(
                running.contains("plex") && !running.contains("sonarr"),
                "{path}: {running}"
            );
            assert!(!running.contains("lidarr"), "{path}: {running}");
        }
    }

    /// Unversioned paths can be refused; unknown paths answer dockerd's
    /// 404 and are recorded.
    #[tokio::test]
    async fn unversioned_and_unknown_paths() {
        let shared = Shared::new(Options {
            refuse_unversioned: true,
            ..Options::default()
        });
        let (status, _, body) = get(&shared, "/containers/json").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("API without a version prefix"), "{body}");
        let (status, _, body) = get(&shared, "/no-such-path").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("page not found"), "{body}");
        assert_eq!(
            lock(&shared.paths).last().map(String::as_str),
            Some("/no-such-path")
        );
    }

    /// An unknown ID, a vanished one and a failing one; versioned inspects
    /// answer too.
    #[tokio::test]
    async fn inspects_answer_by_id() {
        let all = inspects();
        let id = |n: usize| all[n]["Id"].as_str().unwrap_or_default().to_string();
        let shared = Shared::new(Options {
            vanished: [id(0)].into(),
            failing: [id(1)].into(),
            ..Options::default()
        });
        let path = |n: usize| format!("/containers/{}/json", id(n));
        assert_eq!(get(&shared, &path(0)).await.0, StatusCode::NOT_FOUND);
        assert_eq!(
            get(&shared, &format!("/v1.53{}", path(1))).await.0,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            get(&shared, "/containers/nope/json").await.0,
            StatusCode::NOT_FOUND
        );
        let (status, _, body) = get(&shared, &path(2)).await;
        assert!(
            status == StatusCode::OK && body.contains("/authelia"),
            "{body}"
        );
    }

    /// One raw inspect over the fake's socket, giving up after 10 s, so a
    /// test whose release is lost fails instead of hanging.
    fn raw_inspect(socket: PathBuf, id: String) -> std::thread::JoinHandle<String> {
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let mut stream = std::os::unix::net::UnixStream::connect(&socket).expect("connect");
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .expect("timeout");
            let request = format!(
                "GET /containers/{id}/json HTTP/1.1\r\nHost: d\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(request.as_bytes()).expect("write");
            let mut answer = String::new();
            let _ = stream.read_to_string(&mut answer);
            answer
        })
    }

    /// `release` before any inspect arrives is kept: a later held one
    /// answers at once.
    #[test]
    fn an_early_release_is_kept() {
        let engine = FakeEngine::start(Options {
            hold: true,
            ..Options::default()
        })
        .expect("fake");
        engine.release();
        let id = inspects()[2]["Id"].as_str().unwrap_or_default().to_string();
        let answer = raw_inspect(engine.socket.clone(), id)
            .join()
            .expect("client");
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    }

    /// A request with a method the fake doesn't serve is recorded too.
    #[tokio::test]
    async fn wrong_methods_are_recorded() {
        let shared = Shared::new(Options::default());
        let request = Request::post("/containers/json")
            .body(Body::empty())
            .expect("request");
        let response = router(Arc::clone(&shared))
            .oneshot(request)
            .await
            .expect("answer");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(lock(&shared.paths).clone(), ["/containers/json"]);
    }

    /// Dropping the fake lets a held inspect answer and stops it: its
    /// thread ends and the socket goes.
    #[test]
    fn dropping_the_fake_releases_held_inspects_and_stops_it() {
        let engine = FakeEngine::start(Options {
            hold: true,
            ..Options::default()
        })
        .expect("fake");
        let id = inspects()[2]["Id"].as_str().unwrap_or_default().to_string();
        let client = raw_inspect(engine.socket.clone(), id);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while engine.open_inspects() == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(engine.open_inspects(), 1);
        let socket = engine.socket.clone();
        drop(engine);
        let answer = client.join().expect("client");
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        assert!(!socket.exists());
    }

    /// The list entry: ID, name (left out without one) and state word.
    #[test]
    fn summaries_mirror_the_inspects() {
        let all = inspects();
        let plex = summary(&all[0]);
        assert_eq!(plex["Names"], json!(["/plex"]));
        assert_eq!(plex["State"], json!("running"));
        let unnamed = summary(&all[3]);
        assert!(unnamed.get("Names").is_none());
        let stateless = summary(&all[5]);
        assert!(stateless.get("State").is_none());
        assert_eq!(fixture_models().len(), all.len());
        assert_eq!(fixture_rows().len(), all.len());
    }

    /// The ping answers `OK` with the `Api-Version` header, versioned too
    /// within the daemon's range.
    #[tokio::test]
    async fn the_ping_answers_with_the_api_version() {
        let shared = Shared::new(Options::default());
        let (status, header, body) = get(&shared, "/_ping").await;
        assert_eq!(
            (status, header.as_str(), body.as_str()),
            (StatusCode::OK, "1.53", "OK")
        );
        let (status, header, body) = get(&shared, "/v1.44/_ping").await;
        assert_eq!(
            (status, header.as_str(), body.as_str()),
            (StatusCode::OK, "1.53", "OK")
        );
        let refusing = Shared::new(Options {
            refuse_unversioned: true,
            ..Options::default()
        });
        assert_eq!(get(&refusing, "/_ping").await.0, StatusCode::OK);
        assert_eq!(
            get(&shared, "/v1.99/_ping").await.0,
            StatusCode::BAD_REQUEST
        );
    }
}
