//! Asking the controller for a job (sb-go's `requestDockerJob`).

use super::{Action, ControlError, Controller, Job};

impl Controller {
    /// Ask the controller for a job: `POST <base>/start` or `/stop`, each
    /// non-empty name in `ignore` as an `ignore` query value. The
    /// request's limit covers reading the answer too, so a timeout part way
    /// through the body is a decode error, as Go's client gives it through
    /// its `Decoder`. Each request and its answer go to the run log.
    ///
    /// # Errors
    ///
    /// The [`ControlError`] that names what failed: the request, its
    /// status, its answer's decoding or a missing job ID.
    pub async fn request(&self, action: Action, ignore: &[String]) -> Result<Job, ControlError> {
        let mut url = self.url(&[action.segment()])?;
        let names: Vec<&String> = ignore.iter().filter(|name| !name.is_empty()).collect();
        // Only with names: an empty `query_pairs_mut` would leave a bare
        // `?`, where sb-go's `Encode` of nothing gives no query.
        if !names.is_empty() {
            let mut query = url.query_pairs_mut();
            for name in &names {
                query.append_pair("ignore", name);
            }
        }
        tracing::info!(method = "POST", path = url.path(), ignored = ?names, "controller request");
        let response = self
            .http
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .timeout(self.request_timeout)
            .send()
            .await
            .map_err(|err| ControlError::Send(cause(&err)))?;
        let status = response.status();
        tracing::info!(status = status.as_u16(), "controller answer");
        if status != reqwest::StatusCode::OK {
            return Err(ControlError::RequestStatus(status.as_u16()));
        }
        let body = self.body(response).await.map_err(ControlError::DecodeJob)?;
        let answer: Option<JobAnswer> = first_value(&body).map_err(ControlError::DecodeJob)?;
        match answer.and_then(|a| a.job_id) {
            Some(id) if !id.trim().is_empty() => {
                tracing::info!(job = %sb_cli_core::config::escape_controls(&id), "controller job");
                Ok(Job { id })
            }
            _ => Err(ControlError::MissingJobId),
        }
    }

    /// `base` with `segments` added to its path, one segment each (escaped
    /// as path segments; `pop_if_empty` so a bare base gives `/start`, not
    /// `//start`:
    /// <https://docs.rs/url/2.5.8/url/struct.PathSegmentsMut.html#method.pop_if_empty>).
    pub(super) fn url(&self, segments: &[&str]) -> Result<reqwest::Url, ControlError> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|()| ControlError::ParseUrl(format!("{}: can't take a path", self.base)))?
            .pop_if_empty()
            .extend(segments);
        Ok(url)
    }

    /// At most `body_limit` bytes of `response`'s body, read chunk by chunk
    /// (<https://docs.rs/reqwest/0.13.5/reqwest/struct.Response.html#method.chunk>),
    /// as sb-go's `LimitReader` reads it; the cause when reading fails.
    /// Why not `http_body_util::Limited`: it fails past the limit, where
    /// sb-go keeps the first part. Why not sb-config's `http::body`, which
    /// does the same with its own error text: sb-docker may not depend on
    /// sb-config (deny.toml).
    pub(super) async fn body(&self, mut response: reqwest::Response) -> Result<Vec<u8>, String> {
        let mut body = Vec::new();
        while body.len() < self.body_limit {
            match response.chunk().await {
                Ok(Some(chunk)) => body.extend_from_slice(&chunk),
                Ok(None) => break,
                Err(err) => return Err(cause(&err)),
            }
        }
        body.truncate(self.body_limit);
        Ok(body)
    }
}

/// The job-creation answer's one field sb reads.
#[derive(serde::Deserialize)]
struct JobAnswer {
    job_id: Option<String>,
}

/// The first JSON value in `body`, as Go's `Decoder` reads one: trailing
/// data is ignored (`serde_json`'s `StreamDeserializer`; `from_slice` would
/// refuse it:
/// <https://docs.rs/serde_json/1.0.154/serde_json/struct.StreamDeserializer.html>).
/// No value at all is Go's `EOF`.
pub(super) fn first_value<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, String> {
    match serde_json::Deserializer::from_slice(body)
        .into_iter::<T>()
        .next()
    {
        Some(Ok(value)) => Ok(value),
        Some(Err(err)) => Err(err.to_string()),
        None => Err("EOF".to_string()),
    }
}

/// A reqwest error with its causes on one line (`a: b: c`; cli-core's
/// `chain`), control characters escaped.
pub(super) fn cause(err: &reqwest::Error) -> String {
    let text = sb_cli_core::exit::chain(err)
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ");
    sb_cli_core::config::escape_controls(&text)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;
    use crate::control::{UNIT, UNIT_FILE};
    use crate::testing::controller::{Answer, FakeController};

    const JOB: &str = r#"{"job_id":"job-1","status":"pending"}"#;

    /// Set by the proxy test for the re-run child: the controller's URL.
    const PROXY_CHILD: &str = "SB_DOCKER_TEST_PROXY_CHILD";

    fn controller(fake: &FakeController) -> Controller {
        Controller::saltbox()
            .expect("client")
            .with_base(fake.url())
            .expect("base")
            .with_request_timeout(Duration::from_secs(5))
    }

    /// sb-go's values (`cmd/docker.go`, `layout/layout.go`).
    #[test]
    fn saltboxs_values_are_sb_gos() {
        let c = Controller::saltbox().expect("client");
        assert_eq!(c.base.port(), Some(3377));
        assert_eq!(c.unit_file, PathBuf::from(UNIT_FILE));
        assert_eq!(c.systemctl, PathBuf::from("systemctl"));
        assert_eq!(c.check_timeout, Duration::from_secs(5));
        assert_eq!(c.request_timeout, Duration::from_secs(10));
        assert_eq!(c.poll_interval, Duration::from_secs(5));
        assert_eq!(c.body_limit, 1 << 20);
        assert_eq!(c.max_polls, 60);
        assert_eq!(UNIT, "saltbox_managed_docker_controller.service");
    }

    /// sb-go's `TestRequestDockerJobUsesContextAndEncodesIgnoredContainers`:
    /// a POST, the names as `ignore` values (a space, an ampersand), empty
    /// ones dropped, the JSON content type and no body.
    #[tokio::test]
    async fn a_job_is_requested_with_its_ignored_names() {
        let fake = FakeController::start().expect("fake");
        fake.script("/stop", Answer::ok(JOB));
        let ignore = ["with space", "", "ampersand&name"].map(String::from);
        let job = controller(&fake)
            .request(Action::Stop, &ignore)
            .await
            .expect("job");
        assert_eq!(job.id(), "job-1");
        let seen = fake.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].method, "POST");
        assert_eq!(seen[0].path, "/stop");
        assert_eq!(
            seen[0].query,
            [
                ("ignore".to_string(), "with space".to_string()),
                ("ignore".to_string(), "ampersand&name".to_string()),
            ]
        );
        assert_eq!(
            seen[0].raw_query.as_deref(),
            Some("ignore=with+space&ignore=ampersand%26name")
        );
        assert_eq!(seen[0].content_type.as_deref(), Some("application/json"));
        assert_eq!(seen[0].body, b"");
    }

    /// No names, or only empty ones: no query at all, not a bare `?`
    /// (sb-go's `Encode` of nothing).
    #[tokio::test]
    async fn a_request_without_names_has_no_query() {
        let fake = FakeController::start().expect("fake");
        fake.script("/start", Answer::ok(JOB));
        let c = controller(&fake);
        let job = c.request(Action::Start, &[]).await;
        assert_eq!(job.map(|j| j.id().to_string()), Ok("job-1".to_string()));
        c.request(Action::Start, &[String::new()])
            .await
            .expect("job");
        let seen = fake.seen();
        assert_eq!(seen[0].path, "/start");
        assert_eq!(seen[0].raw_query, None);
        assert_eq!(seen[1].raw_query, None);
    }

    /// A bare base, one with a trailing slash and one with a path each
    /// give the right path (never `//start`, which the controller 404s).
    #[tokio::test]
    async fn the_path_follows_the_base() {
        let fake = FakeController::start().expect("fake");
        fake.script("/start", Answer::ok(JOB));
        fake.script("/api/start", Answer::ok(JOB));
        for base in [
            fake.url().to_string(),
            format!("{}/", fake.url()),
            format!("{}/api", fake.url()),
            format!("{}/api/", fake.url()),
        ] {
            let c = controller(&fake).with_base(&base).expect("base");
            c.request(Action::Start, &[]).await.expect(&base);
        }
        let paths: Vec<String> = fake.seen().into_iter().map(|s| s.path).collect();
        assert_eq!(paths, ["/start", "/start", "/api/start", "/api/start"]);
    }

    /// A base that isn't a URL, or can't take a path: sb-go's parse text,
    /// the given text escaped.
    #[test]
    fn a_bad_base_is_a_parse_error() {
        for base in ["not a url", "mailto:x@example.com", "mailto:\u{1b}[31m"] {
            let err = Controller::saltbox()
                .expect("client")
                .with_base(base)
                .expect_err(base);
            assert!(matches!(err, ControlError::ParseUrl(_)), "{base}: {err:?}");
            let shown = err.to_string();
            assert!(
                shown.starts_with("parse Docker controller URL: "),
                "{shown}"
            );
            assert!(!shown.contains('\u{1b}'), "{shown:?}");
        }
    }

    /// The job ID is checked trimmed and kept as sent; a missing, null or
    /// blank one is sb-go's error.
    #[tokio::test]
    async fn the_job_id_must_be_there() {
        let fake = FakeController::start().expect("fake");
        fake.script("/start", Answer::ok(r#"{"job_id":" job-2 "}"#));
        let job = controller(&fake).request(Action::Start, &[]).await;
        assert_eq!(job.map(|j| j.id().to_string()), Ok(" job-2 ".to_string()));
        for body in ["{}", r#"{"job_id":null}"#, r#"{"job_id":"  "}"#, "null"] {
            let fake = FakeController::start().expect("fake");
            fake.script("/start", Answer::ok(body));
            assert_eq!(
                controller(&fake).request(Action::Start, &[]).await,
                Err(ControlError::MissingJobId),
                "{body}"
            );
        }
        assert_eq!(
            ControlError::MissingJobId.to_string(),
            "docker controller response is missing job ID"
        );
    }

    /// Only 200 is a job: a 201 with a job, a 503 and a 307 are refused,
    /// and the 307's target is never asked (sb follows no redirects;
    /// decision 8).
    #[tokio::test]
    async fn another_status_fails_and_redirects_are_not_followed() {
        let fake = FakeController::start().expect("fake");
        fake.script("/start", Answer::with_status(201, JOB));
        fake.script("/start", Answer::with_status(503, r#"{"error":"blocked"}"#));
        fake.script(
            "/stop",
            Answer::with_status(307, "").header("location", "/elsewhere"),
        );
        fake.script("/elsewhere", Answer::ok(JOB));
        let c = controller(&fake);
        let err = c.request(Action::Start, &[]).await.expect_err("201");
        assert_eq!(err, ControlError::RequestStatus(201));
        let err = c.request(Action::Start, &[]).await.expect_err("503");
        assert_eq!(err, ControlError::RequestStatus(503));
        assert_eq!(
            err.to_string(),
            "docker controller request failed with status code: 503"
        );
        let err = c.request(Action::Stop, &[]).await.expect_err("307");
        assert_eq!(err, ControlError::RequestStatus(307));
        assert!(fake.seen().iter().all(|s| s.path != "/elsewhere"));
    }

    /// An answer that doesn't decode, one cut at the body limit included,
    /// is sb-go's decode error with `serde_json`'s cause after it (Go's `EOF`
    /// for no value at all); trailing data after the first JSON value is
    /// ignored, as Go's `Decoder` ignores it.
    #[tokio::test]
    async fn answers_are_decoded_as_go_decodes_them() {
        for (body, cause) in [
            (&b""[..], "EOF"),
            (b"not json", "expected"),
            (br#"{"job_id": 5}"#, "invalid type"),
            (b"[]", "invalid length"),
        ] {
            let fake = FakeController::start().expect("fake");
            fake.script("/start", Answer::ok(body));
            let err = controller(&fake)
                .request(Action::Start, &[])
                .await
                .expect_err("decode");
            assert!(
                matches!(err, ControlError::DecodeJob(_)),
                "{body:?}: {err:?}"
            );
            let shown = err.to_string();
            assert!(
                shown.starts_with("decode Docker controller response: "),
                "{shown}"
            );
            assert!(shown.contains(cause), "{body:?}: {shown}");
        }
        let fake = FakeController::start().expect("fake");
        fake.script("/start", Answer::ok(format!("{JOB} and then {{")));
        let job = controller(&fake).request(Action::Start, &[]).await;
        assert_eq!(job.map(|j| j.id().to_string()), Ok("job-1".to_string()));
        // Past the limit: the first 16 bytes of a longer answer.
        let fake = FakeController::start().expect("fake");
        fake.script("/start", Answer::ok(JOB));
        let err = controller(&fake)
            .with_body_limit(16)
            .request(Action::Start, &[])
            .await
            .expect_err("cut");
        assert!(matches!(err, ControlError::DecodeJob(_)), "{err:?}");
        // At the limit exactly: the whole answer.
        let job = controller(&fake)
            .with_body_limit(JOB.len())
            .request(Action::Start, &[])
            .await;
        assert_eq!(job.map(|j| j.id().to_string()), Ok("job-1".to_string()));
    }

    /// Nothing listening (a port just freed), and an answer slower than
    /// the request's limit: sb-go's send error, reqwest's cause after it.
    #[tokio::test]
    async fn transport_failures_are_send_errors() {
        let port = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .and_then(|l| l.local_addr())
            .expect("free port")
            .port();
        let fake = FakeController::start().expect("fake");
        let refused = controller(&fake)
            .with_base(&format!("http://127.0.0.1:{port}"))
            .expect("base");
        let err = refused
            .request(Action::Start, &[])
            .await
            .expect_err("refused");
        assert!(matches!(err, ControlError::Send(_)), "{err:?}");
        let shown = err.to_string();
        assert!(
            shown.starts_with("send Docker controller request: error sending request"),
            "{shown}"
        );
        fake.script("/start", Answer::ok(JOB).after(Duration::from_secs(5)));
        let started = std::time::Instant::now();
        let err = controller(&fake)
            .with_request_timeout(Duration::from_millis(100))
            .request(Action::Start, &[])
            .await
            .expect_err("timeout");
        assert!(matches!(err, ControlError::Send(_)), "{err:?}");
        assert!(err.to_string().contains("timed out"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// The proxy test's child: one request through sb's client, one
    /// through a client with reqwest's system proxy.
    async fn proxy_child(url: &str) {
        let c = Controller::saltbox()
            .expect("client")
            .with_base(url)
            .expect("base");
        let direct = c.request(Action::Start, &[]).await.expect("job");
        let proxied = reqwest::Client::new()
            .post(format!("{url}/start"))
            .send()
            .await
            .expect("send")
            .text()
            .await
            .expect("body");
        println!("direct={} proxied={proxied}", direct.id());
    }

    /// The proxy variables set (the test binary re-run as a child with
    /// them: `set_var` is unsafe, and unsafe code is forbidden): the
    /// controller is still asked directly, as Go never proxies loopback.
    /// The control case, a client with reqwest's system proxy, goes
    /// through the proxy, so the variables did reach the child.
    #[test]
    fn the_proxy_variables_are_not_used() {
        if let Some(url) = std::env::var_os(PROXY_CHILD) {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            runtime.block_on(proxy_child(&url.to_string_lossy()));
            return;
        }
        let fake = FakeController::start().expect("fake");
        fake.script("/start", Answer::ok(JOB));
        let proxy = FakeController::start().expect("proxy");
        proxy.script("/start", Answer::ok(r#"{"job_id":"via-proxy"}"#));
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "control::client::tests::the_proxy_variables_are_not_used",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(PROXY_CHILD, fake.url())
            .env("HTTP_PROXY", proxy.url())
            .env("http_proxy", proxy.url())
            .env("ALL_PROXY", proxy.url())
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .output()
            .expect("run child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{out:?}");
        assert!(
            stdout.contains(r#"direct=job-1 proxied={"job_id":"via-proxy"}"#),
            "{stdout}"
        );
        assert_eq!(fake.seen().len(), 1);
        assert_eq!(proxy.seen().len(), 1);
    }
}
