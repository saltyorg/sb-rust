//! A fake HTTP service for tests (config spec, "Testing strategy"):
//! sb-config's own tests and sb's end-to-end tests (through the `testing`
//! feature; never in sb's release or production builds). It stands in for
//! Docker Hub and Cloudflare on a loopback port: each test scripts the
//! answers per path (status, headers, body, or held until released) and
//! reads back what it was sent. axum on a `TcpListener`, on a runtime of
//! its own thread, as sb-docker's fake Engine API
//! (https://docs.rs/axum/0.8.9/axum/fn.serve.html). Why not wiremock or
//! httpmock: neither is in the tree, and neither holds an answer until
//! released. It also lays out the fixture sets as a Saltbox root in a
//! temporary directory.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

/// One scripted answer.
#[derive(Clone, Debug)]
pub struct Answer {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
    /// Wait for `Fake::release` before answering (a timeout's test).
    pub hold: bool,
}

impl Answer {
    /// `status` with `body`, no headers.
    pub fn new(status: u16, body: impl Into<String>) -> Self {
        Answer {
            status,
            headers: Vec::new(),
            body: body.into(),
            hold: false,
        }
    }

    /// `status` with a JSON `body`.
    pub fn json(status: u16, body: impl Into<String>) -> Self {
        Answer {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            body: body.into(),
            hold: false,
        }
    }

    /// This answer with another header.
    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// This answer, held until released.
    #[must_use]
    pub fn held(mut self) -> Self {
        self.hold = true;
        self
    }
}

/// One request the fake was sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    pub method: String,
    /// The path with its query string.
    pub target: String,
    /// Header names in lowercase.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Seen {
    /// The first value of header `name` (lowercase).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Default)]
struct Script {
    /// Answers by path, served in order; the last one repeats.
    answers: HashMap<String, VecDeque<Answer>>,
    seen: Vec<Seen>,
}

struct Shared {
    script: Mutex<Script>,
    release: tokio::sync::watch::Sender<bool>,
}

/// A running fake; it stops when dropped, letting held answers go.
pub struct Fake {
    url: String,
    shared: Arc<Shared>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Fake {
    /// Start a fake on a free loopback port.
    pub fn start() -> std::io::Result<Self> {
        // Bound here, so the port exists before `start` returns.
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let url = format!("http://{}", listener.local_addr()?);
        let shared = Arc::new(Shared {
            script: Mutex::new(Script::default()),
            release: tokio::sync::watch::channel(false).0,
        });
        let app = Router::new()
            .fallback(answer)
            .with_state(Arc::clone(&shared));
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let (ready, started) = std::sync::mpsc::channel::<std::io::Result<()>>();
        let thread = std::thread::Builder::new()
            .name("fake-http".into())
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
                    let listener = match tokio::net::TcpListener::from_std(listener) {
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
        Ok(Fake {
            url,
            shared,
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    /// The fake's base URL (`http://127.0.0.1:<port>`).
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Answer requests to `path` (without its query) with `answer`, after
    /// any answers already scripted there.
    pub fn script(&self, path: &str, answer: Answer) {
        lock(&self.shared.script)
            .answers
            .entry(path.to_string())
            .or_default()
            .push_back(answer);
    }

    /// What the fake was sent, in order.
    pub fn seen(&self) -> Vec<Seen> {
        lock(&self.shared.script).seen.clone()
    }

    /// Let held answers go, now and later ones too (`send_replace` keeps
    /// the value while nothing waits,
    /// https://docs.rs/tokio/1.54.1/tokio/sync/watch/struct.Sender.html#method.send_replace).
    pub fn release(&self) {
        self.shared.release.send_replace(true);
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.release();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Records the request and serves the next scripted answer for its path
/// (the last one repeats); an unscripted path is a 404.
async fn answer(
    State(shared): State<Arc<Shared>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let scripted = {
        let mut script = lock(&shared.script);
        script.seen.push(Seen {
            method: method.to_string(),
            target: uri
                .path_and_query()
                .map_or_else(|| uri.path().to_string(), ToString::to_string),
            headers: headers
                .iter()
                .map(|(n, v)| {
                    (
                        n.as_str().to_string(),
                        String::from_utf8_lossy(v.as_bytes()).into_owned(),
                    )
                })
                .collect(),
            body: String::from_utf8_lossy(&body).into_owned(),
        });
        script.answers.get_mut(uri.path()).and_then(|queue| {
            if queue.len() > 1 {
                queue.pop_front()
            } else {
                queue.front().cloned()
            }
        })
    };
    let Some(answer) = scripted else {
        return (StatusCode::NOT_FOUND, "not scripted").into_response();
    };
    if answer.hold {
        let mut release = shared.release.subscribe();
        let _ = release.wait_for(|released| *released).await;
    }
    let status = StatusCode::from_u16(answer.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut response = (status, answer.body).into_response();
    // One value per header name (a repeated name replaces the last).
    for (name, value) in answer.headers {
        if let (Ok(name), Ok(value)) = (
            axum::http::HeaderName::try_from(name),
            axum::http::HeaderValue::try_from(value),
        ) {
            response.headers_mut().insert(name, value);
        }
    }
    response
}

/// A Saltbox root in a new temporary directory: the fixture set `set`
/// (`valid`, say) as its config files, and the pinned schemas under
/// `schema/`, as Saltbox lays them out. Copied per test, so a test can
/// change its files freely.
pub fn saltbox_root(set: &str) -> std::io::Result<tempfile::TempDir> {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let root = tempfile::tempdir()?;
    for entry in std::fs::read_dir(fixtures.join(set))? {
        let path = entry?.path();
        if let Some(name) = path.file_name() {
            std::fs::copy(&path, root.path().join(name))?;
        }
    }
    let schema = root.path().join("schema");
    std::fs::create_dir(&schema)?;
    for entry in std::fs::read_dir(fixtures.join("saltbox-schemas"))? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "yml")
            && let Some(name) = path.file_name()
        {
            std::fs::copy(&path, schema.join(name))?;
        }
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn get(url: &str) -> (u16, String) {
        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client")
            .get(url)
            .send()
            .await
            .expect("send");
        (
            response.status().as_u16(),
            response.text().await.expect("text"),
        )
    }

    #[tokio::test]
    async fn scripted_answers_are_served_in_order_and_the_last_repeats() {
        let fake = Fake::start().expect("fake");
        fake.script("/a", Answer::json(200, "first"));
        fake.script("/a", Answer::json(503, "second"));
        let url = format!("{}/a?x=1", fake.url());
        assert_eq!(get(&url).await, (200, "first".into()));
        assert_eq!(get(&url).await, (503, "second".into()));
        assert_eq!(get(&url).await, (503, "second".into()));
        assert_eq!(get(&format!("{}/nope", fake.url())).await.0, 404);
        let seen = fake.seen();
        assert_eq!(seen.len(), 4);
        assert_eq!(
            (seen[0].method.as_str(), seen[0].target.as_str()),
            ("GET", "/a?x=1")
        );
    }

    #[tokio::test]
    async fn headers_and_bodies_are_recorded_and_sent() {
        let fake = Fake::start().expect("fake");
        fake.script("/p", Answer::json(429, "{}").header("retry-after", "2"));
        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client")
            .post(format!("{}/p", fake.url()))
            .header("x-auth", "k")
            .body("payload")
            .send()
            .await
            .expect("send");
        assert_eq!(response.headers()["retry-after"], "2");
        assert_eq!(response.headers()["content-type"], "application/json");
        let seen = &fake.seen()[0];
        assert_eq!(seen.header("x-auth"), Some("k"));
        assert_eq!(seen.body, "payload");
        assert_eq!(seen.header("missing"), None);
    }

    #[tokio::test]
    async fn a_held_answer_waits_for_release() {
        let fake = Fake::start().expect("fake");
        fake.script("/h", Answer::new(200, "late").held());
        let url = format!("{}/h", fake.url());
        let waiting = tokio::spawn(async move { get(&url).await });
        // Released only once the request is held there.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while fake.seen().is_empty() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(fake.seen().len(), 1);
        assert!(!waiting.is_finished());
        fake.release();
        assert_eq!(waiting.await.expect("join"), (200, "late".into()));
    }

    #[test]
    fn dropping_the_fake_stops_it_with_answers_held() {
        let fake = Fake::start().expect("fake");
        fake.script("/h", Answer::json(200, "x").held());
        let url = format!("{}/h", fake.url());
        let client = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(async move {
                    let client = reqwest::Client::builder()
                        .no_proxy()
                        .build()
                        .expect("client");
                    client.get(url).send().await.is_ok()
                })
        });
        // Dropped only once the request is held there.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while fake.seen().is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(fake.seen().len(), 1);
        drop(fake);
        let _ = client.join();
    }

    #[test]
    fn a_saltbox_root_has_the_set_and_the_schemas() {
        let root = saltbox_root("valid").expect("root");
        assert!(root.path().join("accounts.yml").is_file());
        assert!(root.path().join("schema/accounts.schema.yml").is_file());
        assert!(!root.path().join("schema/PROVENANCE.md").exists());
    }
}
