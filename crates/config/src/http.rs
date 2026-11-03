//! The credential checks' HTTP client (config spec, "HTTP"): one reqwest
//! client, rustls with the platform's roots, response bodies read up to
//! 1 MiB, remote text escaped and cut before it is shown. The proxy comes
//! as a parameter (`Proxy`), so tests never meet a developer's proxy.

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
    /// https://docs.rs/reqwest/0.13.5/reqwest/#proxies): production.
    /// Unlike Go, it proxies loopback addresses too (a listed difference;
    /// sb's end-to-end tests remove the variables).
    System,
    /// None: unit tests.
    Off,
    /// Every request through this proxy URL (the proxy's own test).
    Via(String),
}

/// The client.
#[derive(Clone, Debug)]
pub struct Http {
    client: reqwest::Client,
}

impl Http {
    /// A client with `proxy`, each request limited to `timeout`, following
    /// no redirects (neither service redirects, and following one would
    /// send the credentials on): reqwest's own proxy handling for
    /// `System`, `no_proxy` for `Off`, and `Proxy::all` for `Via`
    /// (https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html#method.no_proxy,
    /// https://docs.rs/reqwest/0.13.5/reqwest/struct.Proxy.html#method.all,
    /// https://docs.rs/reqwest/0.13.5/reqwest/redirect/struct.Policy.html#method.none).
    pub fn new(proxy: &Proxy, timeout: Duration) -> Result<Self, String> {
        let builder = reqwest::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none());
        let builder = match proxy {
            Proxy::System => builder,
            Proxy::Off => builder.no_proxy(),
            Proxy::Via(url) => {
                builder.proxy(reqwest::Proxy::all(url).map_err(|err| transport_error(&err))?)
            }
        };
        builder
            .build()
            .map(|client| Http { client })
            .map_err(|err| transport_error(&err))
    }

    /// The reqwest client.
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }
}

/// `response`'s body, up to `MAX_BODY` bytes (read chunk by chunk, so a
/// larger body isn't read whole:
/// https://docs.rs/reqwest/0.13.5/reqwest/struct.Response.html#method.chunk).
/// Why not `http_body_util::Limited`: it fails past the limit, where the
/// spec keeps the first MiB.
pub async fn body(mut response: reqwest::Response) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    while body.len() < MAX_BODY {
        match response.chunk().await {
            Ok(Some(chunk)) => body.extend_from_slice(&chunk),
            Ok(None) => break,
            Err(err) => return Err(transport_error(&err)),
        }
    }
    body.truncate(MAX_BODY);
    Ok(body)
}

/// Text from a remote service as an issue may show it: control
/// characters escaped, cut at `MAX_TEXT` characters with `…`.
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
pub fn transport_error(err: &reqwest::Error) -> String {
    remote_text(&error_chain(err))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Answer, Fake};

    fn http() -> Http {
        Http::new(&Proxy::Off, Duration::from_secs(5)).expect("client")
    }

    #[tokio::test]
    async fn bodies_are_read_up_to_a_mebibyte() {
        let fake = Fake::start().expect("fake");
        fake.script("/big", Answer::json(200, "x".repeat(MAX_BODY + 4096)));
        fake.script("/small", Answer::json(200, "hello"));
        let http = http();
        let get = |path: &str| http.client().get(format!("{}{path}", fake.url())).send();
        let big = body(get("/big").await.expect("send")).await.expect("body");
        assert_eq!(big.len(), MAX_BODY);
        let small = body(get("/small").await.expect("send"))
            .await
            .expect("body");
        assert_eq!(small, b"hello");
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
            .get(format!("{}/d", fake.url()))
            .send()
            .await
            .expect("send");
        assert_eq!(response.status().as_u16(), 200);
    }

    #[test]
    fn a_client_builds_for_each_proxy_setting() {
        assert!(Http::new(&Proxy::System, TIMEOUT).is_ok());
        assert!(Http::new(&Proxy::Via("not a url".into()), TIMEOUT).is_err());
    }
}
