//! A fake SVM proxy, GitHub API and download host for tests (release
//! spec, "Testing strategy"): sb-release's own tests and sb's end-to-end
//! tests (through the `testing` feature; never in sb's release or
//! production builds). One loopback TLS port serves all three: the proxy
//! at `/version`, the API under `/api`, downloads at the root. Each test
//! scripts the answers per path and reads back what it was sent.
//!
//! axum on a TLS listener, as axum's low-level-rustls example
//! (<https://github.com/tokio-rs/axum/tree/main/examples/low-level-rustls>),
//! with the handshake in an `axum::serve::Listener`
//! (<https://docs.rs/axum/0.8.9/axum/serve/trait.Listener.html>) so
//! `axum::serve` drives the connections, and tokio-rustls's acceptor
//! (<https://docs.rs/tokio-rustls/0.26.6/tokio_rustls/struct.TlsAcceptor.html>).
//! The certificate is rcgen's, made for `127.0.0.1` when the fake starts
//! (<https://docs.rs/rcgen/0.14.10/rcgen/fn.generate_simple_self_signed.html>):
//! no key in the repository, and only the client from
//! [`Fake::releases`] trusts it (reqwest's `tls_certs_only`,
//! <https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html#method.tls_certs_only>).
//! Why not wiremock or httpmock: neither is in the tree, neither serves
//! TLS with a certificate made at test time.

use std::collections::{HashMap, VecDeque};
use std::future::IntoFuture;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::server::TlsStream;

use crate::{Error, Releases, builder};

/// The variable naming the bases file a `testing` build of sb uses.
pub const BASES_VARIABLE: &str = "SB_RELEASE_TEST_BASES";

/// The client [`Releases::for_sb`] makes in a `testing` build: from the
/// bases file [`BASES_VARIABLE`] names (JSON, [`Fake::write_bases`]).
pub(crate) fn from_env(user_agent: &str) -> Result<Releases, Error> {
    let path = std::env::var_os(BASES_VARIABLE)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| {
            Error::Client(format!(
                "a testing build needs {BASES_VARIABLE}: it never reaches GitHub"
            ))
        })?;
    let text =
        std::fs::read(&path).map_err(|err| Error::Client(format!("{BASES_VARIABLE}: {err}")))?;
    let bases: TestBases = serde_json::from_slice(&text)
        .map_err(|err| Error::Client(format!("{BASES_VARIABLE}: {err}")))?;
    Releases::testing(bases, user_agent)
}

/// Where [`Releases::testing`] sends its requests, and the certificate it
/// trusts (DER). `download` is a root URL ending in `/` (the asset checks
/// compare only its host and port, and name it in their message).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TestBases {
    pub proxy: String,
    pub api: String,
    pub download: String,
    pub certificate: Vec<u8>,
    /// Each metadata request's total limit (60 s in production), short
    /// for a test of a server that trickles.
    pub metadata_timeout: std::time::Duration,
}

impl Releases {
    /// A client for a fake server: `bases` instead of the production
    /// ones, trusting only `bases.certificate`, and no system proxy (a
    /// developer's would see loopback requests). Otherwise as
    /// [`Releases::production`].
    ///
    /// # Errors
    ///
    /// [`Error::Client`] when the certificate or a base is invalid.
    pub fn testing(bases: TestBases, user_agent: &str) -> Result<Self, Error> {
        let TestBases {
            proxy,
            api,
            download,
            certificate,
            metadata_timeout,
        } = bases;
        let certificate = reqwest::Certificate::from_der(&certificate)
            .map_err(|err| Error::Client(err.to_string()))?;
        let builder = builder(user_agent).no_proxy().tls_certs_only([certificate]);
        let mut releases = Self::with(builder, [&proxy, &api, &download])?;
        releases.metadata_timeout = metadata_timeout;
        Ok(releases)
    }
}

/// One scripted answer.
#[derive(Clone, Debug)]
pub struct Answer {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Send the body in parts of this many bytes, pausing this long before
    /// each (a slow or trickling server); all at once when `None`.
    pub trickle: Option<(usize, std::time::Duration)>,
    /// End the body with an error after what it sent (a connection that
    /// breaks mid-body).
    pub cut_off: bool,
}

impl Answer {
    /// `status` with `body`, no headers.
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Answer {
            status,
            headers: Vec::new(),
            body: body.into(),
            trickle: None,
            cut_off: false,
        }
    }

    /// This answer's body broken off after it is sent: the connection
    /// fails before the body's end.
    #[must_use]
    pub fn cut_off(mut self) -> Self {
        self.cut_off = true;
        self.trickle
            .get_or_insert((self.body.len().max(1), std::time::Duration::ZERO));
        self
    }

    /// This answer with its body sent `part` bytes at a time, `pause`
    /// before each part.
    #[must_use]
    pub fn trickled(mut self, part: usize, pause: std::time::Duration) -> Self {
        self.trickle = Some((part.max(1), pause));
        self
    }

    /// 200 with a JSON `body`.
    pub fn json(body: impl Into<Vec<u8>>) -> Self {
        Answer::new(200, body).header("content-type", "application/json")
    }

    /// This answer with another header.
    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }
}

/// One request the fake was sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    pub method: String,
    /// The path with its query string, as sent.
    pub target: String,
    /// Header names in lowercase.
    pub headers: Vec<(String, String)>,
}

impl Seen {
    /// The first value of header `name` (lowercase).
    #[must_use]
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

/// A running fake; it stops when dropped, closing its connections.
pub struct Fake {
    url: String,
    certificate: Vec<u8>,
    script: Arc<Mutex<Script>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for Fake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fake")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The fake's listener: each accepted connection's TLS handshake done
/// before axum sees it; a failed or stalled handshake is dropped and the
/// next connection taken. Accept errors are axum's own `TcpListener` impl's
/// to handle (it waits out EMFILE). Why not the example's handshake in a
/// task per connection, or axum-server: the fake's one client connects a
/// few times per test, so handshakes one at a time are enough, bounded by
/// [`HANDSHAKE`] so a connection that never finishes one can't hold the
/// rest; axum-server isn't in the tree, and axum 0.8.9 has no TLS adapter.
/// The longest a connection's TLS handshake may take.
const HANDSHAKE: std::time::Duration = std::time::Duration::from_secs(5);

struct Tls {
    tcp: TcpListener,
    acceptor: TlsAcceptor,
}

impl axum::serve::Listener for Tls {
    type Io = TlsStream<TcpStream>;
    type Addr = std::net::SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let (stream, addr) = axum::serve::Listener::accept(&mut self.tcp).await;
            if let Ok(Ok(tls)) = tokio::time::timeout(HANDSHAKE, self.acceptor.accept(stream)).await
            {
                return (tls, addr);
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.tcp.local_addr()
    }
}

impl Fake {
    /// Start a fake on a free loopback port with a new certificate.
    ///
    /// # Errors
    ///
    /// The I/O error when the port can't be bound, the certificate made
    /// or the runtime started.
    pub fn start() -> std::io::Result<Self> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let url = format!("https://{}", listener.local_addr()?);
        let made = rcgen::generate_simple_self_signed(["127.0.0.1".to_owned()])
            .map_err(std::io::Error::other)?;
        let certificate = made.cert.der().to_vec();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(made.signing_key.serialize_der()));
        let provider = Arc::new(tokio_rustls::rustls::crypto::aws_lc_rs::default_provider());
        let config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .and_then(|config| {
                config
                    .with_no_client_auth()
                    .with_single_cert(vec![CertificateDer::from(certificate.clone())], key)
            })
            .map_err(std::io::Error::other)?;
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let script = Arc::new(Mutex::new(Script::default()));
        let app = Router::new()
            .fallback(answer)
            .with_state(Arc::clone(&script));
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let (ready, started) = std::sync::mpsc::channel::<std::io::Result<()>>();
        let thread = std::thread::Builder::new()
            .name("fake-github".into())
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
                    let tcp = match TcpListener::from_std(listener) {
                        Ok(tcp) => tcp,
                        Err(err) => {
                            let _ = ready.send(Err(err));
                            return;
                        }
                    };
                    let _ = ready.send(Ok(()));
                    // Stopped at once, not gracefully: a client that
                    // stopped reading mid-body (a refused download) would
                    // hold a graceful shutdown for ever, and `Drop` waits
                    // on this thread from the test's own runtime. Leaving
                    // `block_on` drops the server, and the runtime's end
                    // with the thread cancels its connection tasks
                    // (<https://docs.rs/tokio/1.54.1/tokio/runtime/struct.Runtime.html#shutdown>).
                    tokio::select! {
                        _ = axum::serve(Tls { tcp, acceptor }, app).into_future() => {}
                        _ = stopped => {}
                    }
                });
            })?;
        started
            .recv()
            .unwrap_or_else(|_| Err(std::io::Error::other("the fake's thread ended")))?;
        Ok(Fake {
            url,
            certificate,
            script,
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    /// The fake's root URL (`https://127.0.0.1:<port>`).
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The bases a client uses to reach this fake.
    #[must_use]
    pub fn bases(&self) -> TestBases {
        TestBases {
            proxy: format!("{}/version", self.url),
            api: format!("{}/api", self.url),
            download: format!("{}/", self.url),
            certificate: self.certificate.clone(),
            metadata_timeout: crate::METADATA_TIMEOUT,
        }
    }

    /// Write this fake's bases to `path`, for a `testing` build of sb
    /// ([`BASES_VARIABLE`]).
    ///
    /// # Errors
    ///
    /// The I/O error writing the file.
    pub fn write_bases(&self, path: &std::path::Path) -> std::io::Result<()> {
        let json = serde_json::to_vec(&self.bases()).map_err(std::io::Error::other)?;
        std::fs::write(path, json)
    }

    /// A client for this fake with `User-Agent: <user_agent>`.
    ///
    /// # Errors
    ///
    /// As [`Releases::testing`].
    pub fn releases(&self, user_agent: &str) -> Result<Releases, Error> {
        Releases::testing(self.bases(), user_agent)
    }

    /// Answer requests to `path` (without its query) with `answer`, after
    /// any answers already scripted there. Unscripted paths are 404.
    pub fn script(&self, path: &str, answer: Answer) {
        lock(&self.script)
            .answers
            .entry(path.to_owned())
            .or_default()
            .push_back(answer);
    }

    /// The requests sent so far, in order.
    #[must_use]
    pub fn seen(&self) -> Vec<Seen> {
        lock(&self.script).seen.clone()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Record the request, then serve the next answer scripted for its path.
async fn answer(
    State(script): State<Arc<Mutex<Script>>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let scripted = {
        let mut script = lock(&script);
        script.seen.push(Seen {
            method: method.to_string(),
            target: uri
                .path_and_query()
                .map_or_else(|| uri.path().to_owned(), ToString::to_string),
            headers: headers
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_owned(),
                        String::from_utf8_lossy(value.as_bytes()).into_owned(),
                    )
                })
                .collect(),
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
        return StatusCode::NOT_FOUND.into_response();
    };
    let status = StatusCode::from_u16(answer.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut response = match answer.trickle {
        None => (status, answer.body).into_response(),
        // A body from a stream of parts, each after its pause (axum's
        // `Body::from_stream`,
        // <https://docs.rs/axum/0.8.9/axum/body/struct.Body.html#method.from_stream>).
        Some((part, pause)) => {
            let mut parts: Vec<Result<Vec<u8>, std::io::Error>> = answer
                .body
                .chunks(part)
                .map(|part| Ok(part.to_vec()))
                .collect();
            if answer.cut_off {
                parts.push(Err(std::io::Error::other("cut off")));
            }
            let stream = futures_util::stream::iter(parts).then(move |part| async move {
                tokio::time::sleep(pause).await;
                part
            });
            (status, axum::body::Body::from_stream(stream)).into_response()
        }
    };
    for (name, value) in &answer.headers {
        if let (Ok(name), Ok(value)) = (
            axum::http::HeaderName::try_from(name.as_str()),
            axum::http::HeaderValue::try_from(value.as_str()),
        ) {
            response.headers_mut().insert(name, value);
        }
    }
    response
}
