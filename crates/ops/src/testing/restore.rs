//! A fake restore service for tests (ops spec, Testing Strategy, "restore,
//! end to end"): sb's end-to-end tests point `SB_RESTORE_URL` at it, through
//! sb-ops' `testing` feature (never in a release or production build).
//! Each test scripts the answer per path; an unscripted path is a 404, as
//! the service answers for a file never uploaded. It records each
//! request's path and User-Agent. axum on a loopback `TcpListener`, on a
//! runtime of its own thread, as sb-config's fake
//! (<https://docs.rs/axum/0.8.9/axum/fn.serve.html>). Why not that fake
//! itself: its bodies are text, and restore's are ciphertext; and it has
//! no cut connection.

use std::collections::HashMap;
use std::net::{Ipv4Addr, TcpListener};
use std::sync::{Arc, Mutex, PoisonError};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};

/// One scripted answer.
#[derive(Clone, Debug)]
pub enum Reply {
    /// 200 with this body.
    Body(Vec<u8>),
    /// This status, no body.
    Status(u16),
    /// A 302 to this location (sb follows none).
    Redirect(String),
    /// 200 announcing `len` bytes, sending these, then closing: a dropped
    /// connection.
    Cut { sent: Vec<u8>, len: usize },
    /// No answer until the fake is dropped: a request still under way
    /// when sb is interrupted.
    Stall,
}

/// A request the fake was sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    pub path: String,
    pub user_agent: Option<String>,
}

#[derive(Default)]
struct Script {
    replies: HashMap<String, Reply>,
    seen: Vec<Seen>,
}

/// A running fake; it stops when dropped.
pub struct RestoreService {
    url: String,
    script: Arc<Mutex<Script>>,
    /// Lets stalled answers go (at drop, before the server stops).
    release: Arc<tokio::sync::Notify>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for RestoreService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestoreService")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

fn lock(script: &Mutex<Script>) -> std::sync::MutexGuard<'_, Script> {
    script.lock().unwrap_or_else(PoisonError::into_inner)
}

impl RestoreService {
    /// Start a fake on a free loopback port.
    ///
    /// # Errors
    ///
    /// The I/O error when the port can't be bound, or the fake's thread
    /// or runtime can't start.
    pub fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let url = format!("http://{}", listener.local_addr()?);
        let script = Arc::new(Mutex::new(Script::default()));
        let release = Arc::new(tokio::sync::Notify::new());
        let app = Router::new()
            .fallback(answer)
            .with_state((Arc::clone(&script), Arc::clone(&release)));
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let (ready, started) = std::sync::mpsc::channel::<std::io::Result<()>>();
        let thread = std::thread::Builder::new()
            .name("fake-restore".into())
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
        Ok(RestoreService {
            url,
            script,
            release,
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    /// The fake's base URL (`http://<loopback>:<port>`).
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Answer `path` (e.g. `/load/<key>/settings.yml`) with `reply`.
    pub fn reply(&self, path: &str, reply: Reply) {
        lock(&self.script).replies.insert(path.to_owned(), reply);
    }

    /// The requests so far, in order.
    #[must_use]
    pub fn seen(&self) -> Vec<Seen> {
        lock(&self.script).seen.clone()
    }
}

impl Drop for RestoreService {
    fn drop(&mut self) {
        // `notify_waiters` lets every stalled answer go
        // (<https://docs.rs/tokio/1.54.1/tokio/sync/struct.Notify.html#method.notify_waiters>).
        self.release.notify_waiters();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

async fn answer(
    State((script, release)): State<(Arc<Mutex<Script>>, Arc<tokio::sync::Notify>)>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let path = uri.path().to_owned();
    let reply = {
        let mut script = lock(&script);
        script.seen.push(Seen {
            path: path.clone(),
            user_agent: headers
                .get(header::USER_AGENT)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
        });
        script.replies.get(&path).cloned()
    };
    match reply {
        None => StatusCode::NOT_FOUND.into_response(),
        Some(Reply::Stall) => {
            release.notified().await;
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
        Some(Reply::Status(status)) => StatusCode::from_u16(status)
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
            .into_response(),
        Some(Reply::Body(body)) => (StatusCode::OK, body).into_response(),
        Some(Reply::Redirect(location)) => {
            (StatusCode::FOUND, [(header::LOCATION, location)]).into_response()
        }
        Some(Reply::Cut { sent, len }) => {
            // The body announces `len` bytes, sends `sent`, then fails:
            // hyper closes the connection mid-body.
            let chunks = futures_util::stream::iter([
                Ok::<_, std::io::Error>(Bytes::from(sent)),
                Err(std::io::Error::other("cut")),
            ]);
            (
                StatusCode::OK,
                [(header::CONTENT_LENGTH, len.to_string())],
                Body::from_stream(chunks),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fake shows its URL in `Debug`.
    #[test]
    fn debug_shows_the_url() {
        let service = RestoreService::start().expect("fake");
        let shown = format!("{service:?}");
        assert!(
            shown.starts_with("RestoreService { url: \"http://"),
            "{shown}"
        );
        assert!(shown.contains(service.url()), "{shown}");
    }
}
