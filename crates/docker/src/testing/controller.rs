//! A fake Saltbox Docker controller for tests (docker spec, "Start, stop
//! and restart", Testing): sb-docker's own tests and sb's end-to-end tests
//! (through the `testing` feature; never in sb's release or production
//! builds). axum on a loopback `TcpListener`, on a runtime of its own
//! thread, as the fake Engine API beside it
//! (<https://docs.rs/axum/0.8.9/axum/fn.serve.html>). Each test scripts
//! the answers per path (the last one repeats, so a poll can be answered
//! `running` until the script says otherwise) and reads back what it was
//! sent. Why not sb-config's fake service, which works the same way:
//! sb-docker may not depend on sb-config (deny.toml's architecture rules),
//! and a shared test crate for two small fakes isn't earning its keep yet.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

/// One scripted answer.
#[derive(Clone, Debug)]
pub struct Answer {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// How long to wait before answering (a timeout's or a signal's test).
    pub delay: Duration,
    /// Run just before the answer goes out (a signal landing as it
    /// arrives).
    pub then: Option<Hook>,
}

/// A test's action run as an answer goes out.
#[derive(Clone)]
pub struct Hook(Arc<dyn Fn() + Send + Sync>);

impl std::fmt::Debug for Hook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Hook")
    }
}

impl Answer {
    /// A 200 with `body`.
    #[must_use]
    pub fn ok(body: impl Into<Vec<u8>>) -> Self {
        Self::with_status(200, body)
    }

    /// `status` with `body`.
    #[must_use]
    pub fn with_status(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
            delay: Duration::ZERO,
            then: None,
        }
    }

    /// The same answer, with `hook` run just before it goes out.
    #[must_use]
    pub fn then(mut self, hook: impl Fn() + Send + Sync + 'static) -> Self {
        self.then = Some(Hook(Arc::new(hook)));
        self
    }

    /// The same answer after `delay`.
    #[must_use]
    pub fn after(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// The same answer with a header added.
    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
}

/// One request the fake was sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    pub method: String,
    /// The path as sent, still percent-encoded.
    pub path: String,
    /// The query as sent, if the request had one (`None`: no `?`).
    pub raw_query: Option<String>,
    /// The query's pairs, decoded, in order.
    pub query: Vec<(String, String)>,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

#[derive(Default)]
struct Script {
    answers: HashMap<String, VecDeque<Answer>>,
    seen: Vec<Seen>,
}

struct Shared {
    script: Mutex<Script>,
    /// Set when the fake is dropped: delayed answers end at once, so the
    /// graceful shutdown doesn't wait them out.
    stopping: tokio::sync::watch::Sender<bool>,
    /// The handler's panics, caught.
    panics: super::Panics,
}

/// A running fake controller; it stops when dropped, and then fails the
/// test with any handler panic it caught.
pub struct FakeController {
    url: String,
    shared: Arc<Shared>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for FakeController {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeController")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl FakeController {
    /// Start a fake on a free loopback port.
    ///
    /// # Panics
    ///
    /// Not here: a panic on the fake's thread (axum's serve, which
    /// documents that it never errors, breaking that contract) is resumed
    /// by the fake's drop, in the test.
    ///
    /// # Errors
    ///
    /// The I/O error when the loopback port can't be bound or the fake's
    /// runtime can't start.
    pub fn start() -> std::io::Result<Self> {
        // Bound here, so the port exists before `start` returns.
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let url = format!("http://{}", listener.local_addr()?);
        let shared = Arc::new(Shared {
            script: Mutex::new(Script::default()),
            stopping: tokio::sync::watch::channel(false).0,
            panics: super::Panics::default(),
        });
        let app = Router::new()
            .fallback(answer)
            .layer(shared.panics.layer("fake controller"))
            .with_state(Arc::clone(&shared));
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let (ready, started) = std::sync::mpsc::channel::<std::io::Result<()>>();
        let thread = std::thread::Builder::new()
            .name("fake-controller".into())
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
                    // axum documents that this never errors
                    // (<https://docs.rs/axum/0.8.9/axum/serve/struct.Serve.html#method.with_graceful_shutdown>);
                    // the expect only guards that contract, as a panic the
                    // drop resumes.
                    axum::serve(listener, app)
                        .with_graceful_shutdown(async move {
                            let _ = stopped.await;
                        })
                        .await
                        .expect("the fake controller serves");
                });
            })?;
        started
            .recv()
            .unwrap_or_else(|_| Err(std::io::Error::other("the fake's thread ended")))?;
        Ok(Self {
            url,
            shared,
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    /// The fake's base URL (`http://127.0.0.1:<port>`, no path).
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Answer requests to `path` (as sent, without the query) with
    /// `answer`, after any answers already scripted there.
    pub fn script(&self, path: &str, answer: Answer) {
        lock(&self.shared.script)
            .answers
            .entry(path.to_string())
            .or_default()
            .push_back(answer);
    }

    /// What the fake was sent, in order.
    #[must_use]
    pub fn seen(&self) -> Vec<Seen> {
        lock(&self.shared.script).seen.clone()
    }
}

impl Drop for FakeController {
    fn drop(&mut self) {
        self.shared.stopping.send_replace(true);
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.shared.panics.surface(self.thread.take());
    }
}

/// Records the request and serves the next scripted answer for its path
/// (the last one repeats); an unscripted path is chi's plain-text 404, as
/// the controller answers one.
async fn answer(
    State(shared): State<Arc<Shared>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let scripted = {
        let mut script = lock(&shared.script);
        // `Url::query_pairs`'s own decoder
        // (<https://docs.rs/form_urlencoded/1.2.2/form_urlencoded/fn.parse.html>).
        let query = uri
            .query()
            .map_or_default(|q| form_urlencoded::parse(q.as_bytes()).into_owned().collect());
        script.seen.push(Seen {
            method: method.to_string(),
            path: uri.path().to_string(),
            raw_query: uri.query().map(ToString::to_string),
            query,
            content_type: headers
                .get(axum::http::header::CONTENT_TYPE)
                .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned()),
            body: body.to_vec(),
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
        return (StatusCode::NOT_FOUND, "404 page not found\n").into_response();
    };
    if !answer.delay.is_zero() {
        // The delay, or less if the fake is dropped meanwhile.
        let mut stopping = shared.stopping.subscribe();
        if tokio::time::timeout(answer.delay, stopping.wait_for(|stop| *stop))
            .await
            .is_ok()
        {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    }
    if let Some(hook) = &answer.then {
        (hook.0)();
    }
    let status = StatusCode::from_u16(answer.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut response = (status, answer.body).into_response();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `Debug` shows the fake's URL only.
    #[test]
    fn debug_shows_the_url() {
        let fake = FakeController::start().expect("fake");
        assert_eq!(
            format!("{fake:?}"),
            format!("FakeController {{ url: {:?}, .. }}", fake.url())
        );
    }
}
