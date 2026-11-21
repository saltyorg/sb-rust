//! The credential checks' HTTP client (config spec, "HTTP"): one reqwest
//! client, rustls with the platform's roots, response bodies read up to
//! 1 MiB, remote text escaped and cut before it is shown. The proxy comes
//! as a parameter (`Proxy`), so tests never meet a developer's proxy.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Response bodies are read up to this many bytes.
pub const MAX_BODY: usize = 1 << 20;

/// Remote text shown in an issue is cut to this many characters.
pub const MAX_TEXT: usize = 2000;

/// Each request's limit (sb-go's 10 s, for Docker Hub and Cloudflare).
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// Which proxy the client uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Proxy {
    /// reqwest's own handling of the proxy variables (`HTTP_PROXY`,
    /// `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`, lowercase names too, and
    /// none under CGI's `REQUEST_METHOD`;
    /// <https://docs.rs/reqwest/0.13.5/reqwest/#proxies>: production.
    /// Unlike Go, it proxies loopback addresses too (a listed difference;
    /// sb's end-to-end tests remove the variables).
    System,
    /// None: unit tests.
    Off,
    /// Every request through this proxy URL (the proxy's own test).
    Via(String),
}

/// The client, built at its first request: building one loads the
/// platform's roots (rustls-platform-verifier reads every certificate in
/// the system's store), which a run whose checks make no request (motd's
/// and `sb edit`'s file checks, validate-config without credentials)
/// doesn't need. Clones share the one built, through std's `OnceLock`
/// (<https://doc.rust-lang.org/std/sync/struct.OnceLock.html#method.get_or_init>).
#[derive(Clone)]
pub struct Http {
    lazy: Arc<Lazy>,
}

/// What the client is built from, and the client once built (or why it
/// couldn't be).
struct Lazy {
    proxy: Mode,
    /// Whether the client loads the platform's root certificates.
    roots: bool,
    timeout: Duration,
    client: OnceLock<Result<reqwest::Client, String>>,
}

/// The proxy as the builder takes it; `Via`'s URL already checked.
enum Mode {
    System,
    Off,
    // Boxed: reqwest's `Proxy` is far larger than the other variants.
    Via(Box<reqwest::Proxy>),
}

/// No field is shown: a proxy URL can carry credentials (and reqwest's
/// client lists its proxies), so only whether the client is built.
impl std::fmt::Debug for Http {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Http")
            .field("built", &self.lazy.client.get().is_some())
            .finish_non_exhaustive()
    }
}

impl Http {
    /// A client with `proxy`, each request limited to `timeout`, following
    /// no redirects (neither service redirects, and following one would
    /// send the credentials on): reqwest's own proxy handling for
    /// `System`, `no_proxy` for `Off`, and `Proxy::all` for `Via`
    /// (<https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html#method.no_proxy>,
    /// <https://docs.rs/reqwest/0.13.5/reqwest/struct.Proxy.html#method.all>,
    /// <https://docs.rs/reqwest/0.13.5/reqwest/redirect/struct.Policy.html#method.none>).
    /// The proxy URL is checked here; the client is built at its first
    /// request ([`Http::client`]).
    ///
    /// # Errors
    ///
    /// The text shown when the proxy URL is invalid.
    pub fn new(proxy: &Proxy, timeout: Duration) -> Result<Self, String> {
        Self::with_roots(proxy, timeout, true)
    }

    /// [`Http::new`] whose client loads none of the platform's root
    /// certificates (reqwest's `tls_certs_only` with none,
    /// <https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html#method.tls_certs_only>),
    /// for a caller whose every request is plain http with no proxy: it
    /// verifies no certificate, and loading them costs about 15 M
    /// instructions. An https request through it fails its handshake.
    ///
    /// # Errors
    ///
    /// As [`Http::new`].
    pub fn without_roots(proxy: &Proxy, timeout: Duration) -> Result<Self, String> {
        Self::with_roots(proxy, timeout, false)
    }

    fn with_roots(proxy: &Proxy, timeout: Duration, roots: bool) -> Result<Self, String> {
        let proxy = match proxy {
            Proxy::System => Mode::System,
            Proxy::Off => Mode::Off,
            Proxy::Via(url) => Mode::Via(Box::new(
                reqwest::Proxy::all(url).map_err(|err| transport_error(&err))?,
            )),
        };
        Ok(Http {
            lazy: Arc::new(Lazy {
                proxy,
                roots,
                timeout,
                client: OnceLock::new(),
            }),
        })
    }

    /// The reqwest client, built at the first call and kept.
    ///
    /// # Errors
    ///
    /// The text shown when the client can't be built (the same text, at
    /// every call).
    pub fn client(&self) -> Result<&reqwest::Client, String> {
        self.lazy
            .client
            .get_or_init(|| self.lazy.build())
            .as_ref()
            .map_err(Clone::clone)
    }

    /// Whether the client has been built (the laziness's tests).
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn built(&self) -> bool {
        self.lazy.client.get().is_some()
    }
}

impl Lazy {
    fn build(&self) -> Result<reqwest::Client, String> {
        let builder = reqwest::Client::builder()
            .timeout(self.timeout)
            .redirect(reqwest::redirect::Policy::none());
        let builder = if self.roots {
            builder
        } else {
            builder.tls_certs_only(std::iter::empty::<reqwest::Certificate>())
        };
        let builder = match &self.proxy {
            Mode::System => builder,
            Mode::Off => builder.no_proxy(),
            Mode::Via(proxy) => builder.proxy((**proxy).clone()),
        };
        // A missing CA bundle is said with its fix (cli-core's `tls`).
        builder.build().map_err(|err| {
            if sb_cli_core::tls::is_missing_ca(&err) {
                sb_cli_core::tls::MISSING_CA.to_owned()
            } else {
                transport_error(&err)
            }
        })
    }
}

/// `response`'s body, up to `MAX_BODY` bytes (read chunk by chunk, so a
/// larger body isn't read whole:
/// <https://docs.rs/reqwest/0.13.5/reqwest/struct.Response.html#method.chunk>).
/// Why not `http_body_util::Limited`: it fails past the limit, where the
/// spec keeps the first MiB.
///
/// # Errors
///
/// The text shown when reading the body fails part way.
pub async fn body(mut response: reqwest::Response) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    while body.len() < MAX_BODY {
        match response.chunk().await {
            Ok(Some(chunk)) => body.extend_from_slice(&chunk),
            Ok(None) => break,
            // Without its URL, as every motd provider error is built.
            Err(err) => return Err(transport_error(&err.without_url())),
        }
    }
    body.truncate(MAX_BODY);
    Ok(body)
}

/// `response`'s whole body, for an answer that can be larger than
/// [`MAX_BODY`] by design (motd's torrent clients list every torrent),
/// or an error once it passes `cap`: such an answer would be cut, and a
/// cut one can't be read.
///
/// # Errors
///
/// The text shown when reading the body fails part way, or when it is
/// larger than `cap`.
pub async fn body_within(mut response: reqwest::Response, cap: usize) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if body.len() + chunk.len() > cap {
                    return Err(format!("the answer is larger than {} MiB", cap >> 20));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => return Ok(body),
            Err(err) => return Err(transport_error(&err.without_url())),
        }
    }
}

/// Text from a remote service as an issue may show it: control
/// characters escaped, cut at `MAX_TEXT` characters with `…`.
#[must_use]
pub fn remote_text(text: &str) -> String {
    let escaped = crate::issue::escape_controls(text);
    if escaped.chars().count() > MAX_TEXT {
        let mut cut: String = escaped.chars().take(MAX_TEXT).collect();
        cut.push('…');
        cut
    } else {
        escaped
    }
}

/// A transport error with its causes on one line (`a: b: c`; cli-core's
/// `chain`), raw: hide secrets in it, then show it with `remote_text`.
pub fn error_chain(err: &reqwest::Error) -> String {
    sb_cli_core::exit::chain(err)
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

/// A transport error with its causes, one line, shown as remote text.
#[must_use]
pub fn transport_error(err: &reqwest::Error) -> String {
    remote_text(&error_chain(err))
}

/// `text`, a service URL, as a log record may show it: its user name
/// and password taken out
/// (<https://docs.rs/url/2.5.8/url/struct.Url.html#method.set_password>);
/// text that isn't a URL is withheld whole, since it could be anything.
/// A query parameter whose name is a secret key (the shared rule,
/// `facts::secret`; ops decision 32) has its value replaced by
/// `[REDACTED]`; the others stay.
#[must_use]
pub fn loggable_url(text: &str) -> String {
    match reqwest::Url::parse(text) {
        Ok(mut url) => {
            // Both fail only for URLs that can't have credentials.
            let _ = url.set_username("");
            let _ = url.set_password(None);
            if url
                .query_pairs()
                .any(|(name, _)| crate::facts::secret::is_secret_key(&name))
            {
                let pairs: Vec<(String, String)> = url
                    .query_pairs()
                    .map(|(name, value)| {
                        let value = if crate::facts::secret::is_secret_key(&name) {
                            crate::secret::REDACTED.to_owned()
                        } else {
                            value.into_owned()
                        };
                        (name.into_owned(), value)
                    })
                    .collect();
                // url's serializer
                // (<https://docs.rs/url/2.5.8/url/struct.Url.html#method.query_pairs_mut>).
                url.query_pairs_mut().clear().extend_pairs(pairs);
            }
            url.to_string()
        }
        Err(_) => "(not a URL)".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Answer, Fake};

    /// Set in this test binary's child that runs without CA certificates:
    /// the file it writes when its check ran.
    const NO_CA_CHILD: &str = "SB_CONFIG_TEST_NO_CA_CHILD";

    /// Without the system's CA certificates the client's error is
    /// cli-core's sentence with the fix (ops spec, decision 44), which
    /// each credential check shows, not reqwest's "builder error". In a
    /// child of this test binary with `SSL_CERT_FILE` an empty file, which
    /// rustls-native-certs reads instead of the system's store
    /// (<https://docs.rs/rustls-native-certs/0.8.5/rustls_native_certs/fn.load_native_certs.html>).
    #[test]
    fn without_ca_certificates_the_client_says_so() {
        if let Some(ran) = std::env::var_os(NO_CA_CHILD) {
            let http = Http::new(&Proxy::Off, Duration::from_secs(1)).expect("http");
            assert_eq!(
                http.client().err().as_deref(),
                Some(sb_cli_core::tls::MISSING_CA)
            );
            std::fs::write(ran, "ran").expect("mark");
            return;
        }
        let dir = tempfile::tempdir().expect("dir");
        let (empty, ran) = (dir.path().join("empty.pem"), dir.path().join("ran"));
        std::fs::write(&empty, "").expect("empty bundle");
        let child = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "http::tests::without_ca_certificates_the_client_says_so",
            ])
            .env(NO_CA_CHILD, &ran)
            .env("SSL_CERT_FILE", &empty)
            .env_remove("SSL_CERT_DIR")
            .output()
            .expect("child");
        assert!(
            child.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        assert!(ran.exists(), "the child's check didn't run");
    }

    /// A URL as a log record shows it: no user name or password; text
    /// that isn't a URL is withheld whole.
    #[test]
    fn a_logged_url_has_no_credentials() {
        assert_eq!(
            loggable_url("http://u:hunter2@127.0.0.1:9/client/v4/"),
            "http://127.0.0.1:9/client/v4/"
        );
        assert_eq!(
            loggable_url("https://hunter2@example.com/"),
            "https://example.com/"
        );
        assert_eq!(loggable_url("http://127.0.0.1:9/x"), "http://127.0.0.1:9/x");
        assert_eq!(loggable_url("hunter2 not a url"), "(not a URL)");
    }

    /// A secret-named query parameter's value is masked; the others and
    /// a query with none stay as they were.
    #[test]
    fn a_logged_url_masks_secret_query_values() {
        assert_eq!(
            loggable_url("https://example.test/x?token=hunter2&page=2"),
            "https://example.test/x?token=%5BREDACTED%5D&page=2"
        );
        assert_eq!(
            loggable_url("https://example.test/x?page=2&q=a%20b"),
            "https://example.test/x?page=2&q=a%20b"
        );
    }

    fn http() -> Http {
        Http::new(&Proxy::Off, Duration::from_secs(5)).expect("client")
    }

    #[tokio::test]
    async fn bodies_are_read_up_to_a_mebibyte() {
        let fake = Fake::start().expect("fake");
        fake.script("/big", Answer::json(200, "x".repeat(MAX_BODY + 4096)));
        fake.script("/small", Answer::json(200, "hello"));
        let http = http();
        let get = |path: &str| {
            http.client()
                .expect("client")
                .get(format!("{}{path}", fake.url()))
                .send()
        };
        let big = body(get("/big").await.expect("send")).await.expect("body");
        assert_eq!(big.len(), MAX_BODY);
        let small = body(get("/small").await.expect("send"))
            .await
            .expect("body");
        assert_eq!(small, b"hello");
    }

    /// A body that can be larger is read whole up to its cap, and one past
    /// it is an error, not a cut body.
    #[tokio::test]
    async fn a_larger_body_is_read_whole_or_refused() {
        let fake = Fake::start().expect("fake");
        fake.script("/two", Answer::json(200, "x".repeat(2 * MAX_BODY)));
        let http = http();
        let get = || {
            http.client()
                .expect("client")
                .get(format!("{}/two", fake.url()))
                .send()
        };
        let whole = body_within(get().await.expect("send"), 3 * MAX_BODY)
            .await
            .expect("body");
        assert_eq!(whole.len(), 2 * MAX_BODY);
        let exact = body_within(get().await.expect("send"), 2 * MAX_BODY)
            .await
            .expect("body");
        assert_eq!(exact.len(), 2 * MAX_BODY);
        let err = body_within(get().await.expect("send"), MAX_BODY)
            .await
            .expect_err("too large");
        assert_eq!(err, "the answer is larger than 1 MiB");
    }

    #[test]
    fn remote_text_is_escaped_and_cut() {
        assert_eq!(remote_text("bad \u{1b}[31mred\n"), "bad \\u{1b}[31mred\\n");
        let long = "é".repeat(MAX_TEXT + 10);
        let cut = remote_text(&long);
        assert_eq!(cut.chars().count(), MAX_TEXT + 1);
        assert!(cut.ends_with('…'));
        assert_eq!(remote_text(&"a".repeat(MAX_TEXT)), "a".repeat(MAX_TEXT));
    }

    /// Each request is limited to the client's timeout.
    #[tokio::test]
    async fn a_held_answer_times_out() {
        let fake = Fake::start().expect("fake");
        fake.script("/slow", Answer::json(200, "late").held());
        let http = Http::new(&Proxy::Off, Duration::from_millis(200)).expect("client");
        let err = http
            .client()
            .expect("client")
            .get(format!("{}/slow", fake.url()))
            .send()
            .await
            .expect_err("times out");
        let shown = transport_error(&err);
        assert!(shown.contains("timed out"), "{shown}");
        assert!(!shown.contains('\n'), "{shown}");
    }

    /// Given a proxy, an `https://` request goes through it as a `CONNECT`
    /// (the proxy here refuses it, so no TLS is needed).
    #[tokio::test]
    async fn a_proxy_gets_a_connect() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let proxy = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let url = format!("http://{}", proxy.local_addr().expect("addr"));
        let seen = tokio::spawn(async move {
            let (mut socket, _) = proxy.accept().await.expect("accept");
            let mut buffer = vec![0; 1024];
            let read = socket.read(&mut buffer).await.expect("read");
            let _ = socket
                .write_all(b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\n\r\n")
                .await;
            String::from_utf8_lossy(&buffer[..read]).into_owned()
        });
        let http = Http::new(&Proxy::Via(url), Duration::from_secs(5)).expect("client");
        let result = http
            .client()
            .expect("client")
            .get("https://dockerhub.invalid/v2/")
            .send()
            .await;
        assert!(result.is_err());
        let request = tokio::time::timeout(Duration::from_secs(5), seen)
            .await
            .expect("the proxy was used")
            .expect("join");
        assert!(
            request.starts_with("CONNECT dockerhub.invalid:443 "),
            "{request}"
        );
    }

    /// No proxy: a request goes straight to the server.
    #[tokio::test]
    async fn off_means_direct() {
        let fake = Fake::start().expect("fake");
        fake.script("/d", Answer::json(200, "direct"));
        let response = http()
            .client()
            .expect("client")
            .get(format!("{}/d", fake.url()))
            .send()
            .await
            .expect("send");
        assert_eq!(response.status().as_u16(), 200);
    }

    /// The client, with the platform's roots it loads, is built at the
    /// first request, not with `Http`: a run whose checks make no request
    /// builds none. Clones share the one built.
    #[test]
    fn the_client_is_built_at_first_use_and_shared() {
        let http = http();
        let copy = http.clone();
        assert!(!http.built());
        assert!(copy.client().is_ok());
        assert!(http.built());
        assert!(http.client().is_ok());
    }

    /// The client without the platform's roots still makes plain http
    /// requests, built at its first one as the other.
    #[tokio::test]
    async fn a_client_without_roots_asks_over_http() {
        let fake = Fake::start().expect("fake");
        fake.script("/x", Answer::json(200, "hello"));
        let http = Http::without_roots(&Proxy::Off, TIMEOUT).expect("http");
        assert!(!http.built());
        let response = http
            .client()
            .expect("client")
            .get(format!("{}/x", fake.url()))
            .send()
            .await
            .expect("send");
        assert_eq!(body(response).await.expect("body"), b"hello");
        assert!(Http::without_roots(&Proxy::Via("not a url".into()), TIMEOUT).is_err());
    }

    /// The debug form shows no proxy URL, which can carry credentials.
    #[test]
    fn the_debug_form_shows_no_proxy() {
        let via = Proxy::Via("http://user:hunter2@127.0.0.1:9".into());
        let shown = format!("{:?}", Http::new(&via, TIMEOUT).expect("client"));
        assert!(shown.starts_with("Http"), "{shown}");
        assert!(!shown.contains("hunter2"), "{shown}");
    }

    #[test]
    fn a_client_builds_for_each_proxy_setting() {
        assert!(Http::new(&Proxy::System, TIMEOUT).is_ok());
        // And the client builds, at its first use.
        let system = Http::new(&Proxy::System, TIMEOUT).expect("http");
        assert!(system.client().is_ok());
        assert!(Http::new(&Proxy::Via("not a url".into()), TIMEOUT).is_err());
    }
}
