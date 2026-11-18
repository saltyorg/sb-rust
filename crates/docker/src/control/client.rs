//! Asking the controller for a job and waiting for it (sb-go's
//! `requestDockerJob`, `waitForJobCompletion` and `getJobStatus`).

use std::time::Duration;

use super::{Action, ControlError, Controller, FailedContainer, Failure, Job};

/// How many failed polls in a row the wait rides out before it gives up
/// (docker spec decision 10; docker-M2): a poll that can't reach the
/// controller, times out, gets a non-200 or no status. One alone can be
/// a blip: a pooled connection the controller was just closing
/// (docker-L4), a moment of load. Three that fail at once take 10 s (two
/// 5 s intervals between them); three that time out take 40 s (three
/// 10 s request limits and the two intervals): long enough to mean the
/// controller is gone or stuck, short beside the wait's 300 s or 600 s.
/// They don't lengthen the wait: its limit is one of time.
pub(super) const FAILED_POLLS_IN_A_ROW: u32 = 3;

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
                Ok(Job { id, action })
            }
            _ => Err(ControlError::MissingJobId),
        }
    }

    /// Wait for `job` to end (sb-go's `waitForJobCompletion`, for as long
    /// as the controller gives the job and its ending:
    /// [`Controller::window`]): its status asked at once, then every
    /// `poll_interval` ([`wait_until`]). Cancelled
    /// by dropping the future (the step's token, `run_until_cancelled`:
    /// <https://docs.rs/tokio-util/0.7.20/tokio_util/sync/struct.CancellationToken.html#method.run_until_cancelled>).
    /// Each status goes to the run log.
    ///
    /// # Errors
    ///
    /// The [`ControlError`] that ended the wait: the job failed (with what
    /// the controller said of it), an unknown status, the last of
    /// [`FAILED_POLLS_IN_A_ROW`] failed polls (or a failed last poll), or
    /// the job still pending or running at the limit.
    pub async fn wait(&self, job: &Job) -> Result<(), ControlError> {
        let url = self.url(&["job_status", job.id()])?;
        let window = self.window(job.action());
        wait_until(window, self.poll_interval, || self.status(url.clone(), job)).await
    }

    /// One poll (sb-go's `getJobStatus`): the job's status, escaped for
    /// the log and the screen.
    async fn status(&self, url: reqwest::Url, job: &Job) -> Result<Polled, ControlError> {
        let response = self
            .http
            .get(url)
            .timeout(self.request_timeout)
            .send()
            .await
            .map_err(|err| ControlError::Poll(cause(&err)))?;
        let code = response.status();
        if code != reqwest::StatusCode::OK {
            tracing::info!(status = code.as_u16(), "job status answer");
            return Err(ControlError::StatusCheck(code.as_u16()));
        }
        let body = self
            .body(response)
            .await
            .map_err(ControlError::DecodeStatus)?;
        let answer: Option<StatusAnswer> =
            first_value(&body).map_err(ControlError::DecodeStatus)?;
        let Some(answer) = answer else {
            return Err(ControlError::MissingStatus);
        };
        match answer.status.as_deref() {
            Some(status) if !status.is_empty() => {
                let status = sb_cli_core::config::escape_controls(status);
                tracing::info!(
                    job = %sb_cli_core::config::escape_controls(job.id()),
                    status = %status,
                    "job status"
                );
                Ok(match status.as_str() {
                    "completed" => Polled::Completed,
                    "failed" => Polled::Failed(answer.failure()),
                    "pending" | "running" => Polled::Waiting,
                    _ => Polled::Unknown(status),
                })
            }
            _ => Err(ControlError::MissingStatus),
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

/// What one poll said of the job.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Polled {
    Completed,
    Failed(Failure),
    /// `pending` or `running`.
    Waiting,
    /// Another status, escaped.
    Unknown(String),
}

/// The wait's schedule, over `poll` (one status each call): the first at
/// once, then one `interval` after each answer, the last one when `limit`
/// has passed since the wait began (the sleep before it cut short), so
/// a job is asked about up to its limit and no longer (docker spec
/// decision 12); no sleep after the last. A few failed polls in a row are
/// ridden out (decision 10). The time is tokio's, so a test's paused
/// clock runs it (<https://docs.rs/tokio/1.54.1/tokio/time/fn.pause.html>).
/// Why not `tokio::time::interval`: its period runs from the tick, where
/// sb-go's timer starts after each answer. Why not `tokio::time::timeout_at`
/// around the loop: it would drop a poll in flight at the limit, where the
/// last poll's own answer ends the wait.
pub(super) async fn wait_until<F, P>(
    limit: Duration,
    interval: Duration,
    mut poll: F,
) -> Result<(), ControlError>
where
    F: FnMut() -> P,
    P: Future<Output = Result<Polled, ControlError>>,
{
    let deadline = tokio::time::Instant::now() + limit;
    let mut failed = 0;
    loop {
        let polled = poll().await;
        let now = tokio::time::Instant::now();
        let last = now >= deadline;
        match polled {
            Ok(Polled::Completed) => return Ok(()),
            Ok(Polled::Failed(failure)) => return Err(ControlError::JobFailed(failure)),
            Ok(Polled::Unknown(status)) => return Err(ControlError::UnknownStatus(status)),
            Ok(Polled::Waiting) if last => return Err(ControlError::Timeout),
            Ok(Polled::Waiting) => failed = 0,
            // A few failed polls in a row are ridden out (docker spec
            // decision 10).
            Err(err) => {
                failed += 1;
                if failed >= FAILED_POLLS_IN_A_ROW || last {
                    return Err(err);
                }
                let error: &(dyn std::error::Error + 'static) = &err;
                tracing::warn!(error, failed, "job status poll failed; asking again");
            }
        }
        tokio::time::sleep(interval.min(deadline.saturating_duration_since(now))).await;
    }
}

/// The status answer's fields sb reads: the status, and for a failed job
/// what the controller said of it (docs/platform-notes/docker-controller.md;
/// the controller's `jobStatusResponse`). Each one absent or `null` is
/// none.
#[derive(serde::Deserialize)]
struct StatusAnswer {
    status: Option<String>,
    error: Option<String>,
    failed: Option<Vec<String>>,
    outcomes: Option<Vec<OutcomeAnswer>>,
}

/// One container's entry in a job's `outcomes`.
#[derive(serde::Deserialize)]
struct OutcomeAnswer {
    container: Option<String>,
    outcome: Option<String>,
    reason: Option<String>,
    error: Option<String>,
}

/// The outcomes that keep a container from the requested state, the
/// controller's `blocked`, `failed` and `canceled`; `changed`,
/// `unchanged`, `ignored` and `protected` don't fail a job.
const FAILING: [&str; 3] = ["failed", "blocked", "canceled"];

impl StatusAnswer {
    /// The failed job's [`Failure`]: the containers its `failed` names, in
    /// its order, then any other whose outcome failed it, each with its
    /// `outcomes` entry where it has one. Every text made safe for one
    /// line (cli-core's `exit::escaped`, which keeps quotes as they are:
    /// Docker's errors quote paths); blank names and fields left out.
    fn failure(&self) -> Failure {
        let escape = |text: Option<&str>| {
            text.filter(|t| !t.trim().is_empty())
                .map(sb_cli_core::exit::escaped)
        };
        let outcomes = self.outcomes.as_deref().unwrap_or_default();
        let mut names: Vec<&str> = self
            .failed
            .iter()
            .flatten()
            .map(String::as_str)
            .filter(|name| !name.trim().is_empty())
            .collect();
        for o in outcomes {
            if let (Some(name), Some(outcome)) = (o.container.as_deref(), o.outcome.as_deref())
                && FAILING.contains(&outcome)
                && !name.trim().is_empty()
                && !names.contains(&name)
            {
                names.push(name);
            }
        }
        let containers = names
            .into_iter()
            .map(|name| {
                let entry = outcomes
                    .iter()
                    .find(|o| o.container.as_deref() == Some(name));
                let field = |pick: fn(&OutcomeAnswer) -> Option<&str>| escape(entry.and_then(pick));
                let outcome = field(|o| o.outcome.as_deref());
                FailedContainer {
                    name: sb_cli_core::exit::escaped(name),
                    // A reason or an error shows only with its outcome.
                    reason: outcome
                        .as_ref()
                        .and_then(|_| field(|o| o.reason.as_deref())),
                    error: outcome.as_ref().and_then(|_| field(|o| o.error.as_deref())),
                    outcome,
                }
            })
            .collect();
        Failure {
            error: escape(self.error.as_deref()),
            containers,
        }
    }
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
    use crate::control::{POLL_INTERVAL, UNIT, UNIT_FILE};
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
        // sb-go's address, compared in parts, so the tree holds no
        // address-and-port literal.
        assert_eq!(c.base.scheme(), "http");
        assert_eq!(c.base.host_str(), Some("127.0.0.1"));
        assert_eq!(c.base.port(), Some(3377));
        assert_eq!(c.base.path(), "/");
        assert_eq!((c.base.query(), c.base.fragment()), (None, None));
        assert_eq!((c.base.username(), c.base.password()), ("", None));
        assert_eq!(c.unit_file, PathBuf::from(UNIT_FILE));
        assert_eq!(c.systemctl, PathBuf::from("systemctl"));
        assert_eq!(c.check_timeout, Duration::from_secs(5));
        assert_eq!(c.request_timeout, Duration::from_secs(10));
        assert_eq!(c.poll_interval, Duration::from_secs(5));
        assert_eq!(c.body_limit, 1 << 20);
        // The controller's own job limits (decision 12), not sb-go's 60
        // polls.
        assert_eq!(c.limit(Action::Start), Duration::from_secs(600));
        assert_eq!(c.limit(Action::Stop), Duration::from_secs(300));
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

    /// A loopback address nothing listens on, with the socket that holds its
    /// port: bound and never listening, so a connect to it is refused
    /// (connect(2), `ECONNREFUSED`: "no one listening on the remote
    /// address", <https://man7.org/linux/man-pages/man2/connect.2.html>) and,
    /// while the caller keeps the socket, no other test's server can be given
    /// the port, as a port bound and freed again could be. tokio's `TcpSocket`
    /// binds without listening
    /// (<https://docs.rs/tokio/1.54.1/tokio/net/struct.TcpSocket.html#method.bind>);
    /// std's `TcpListener::bind` always listens. The port stays held because
    /// the socket has no `SO_REUSEADDR` (tokio's `TcpSocket` leaves it off),
    /// and Linux lets a second socket share the address only when both set it
    /// (socket(7), <https://man7.org/linux/man-pages/man7/socket.7.html>);
    /// the assertion below checks it.
    fn refused() -> (tokio::net::TcpSocket, std::net::SocketAddr) {
        let socket = tokio::net::TcpSocket::new_v4().expect("socket");
        socket
            .bind(std::net::SocketAddr::from((
                std::net::Ipv4Addr::LOCALHOST,
                0,
            )))
            .expect("bind");
        let addr = socket.local_addr().expect("addr");
        // The port is held: nothing else can bind it while the socket lives.
        let second = std::net::TcpListener::bind(addr).map(|_| ());
        assert_eq!(
            second.map_err(|e| e.kind()),
            Err(std::io::ErrorKind::AddrInUse),
            "{addr} not held"
        );
        (socket, addr)
    }

    /// Nothing listening (a held port), and an answer slower than the
    /// request's limit: sb-go's send error, reqwest's cause after it.
    #[tokio::test]
    async fn transport_failures_are_send_errors() {
        let (_held, addr) = refused();
        let fake = FakeController::start().expect("fake");
        let refused = controller(&fake)
            .with_base(&format!("http://{addr}"))
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

    fn job(id: &str) -> Job {
        Job {
            id: id.to_string(),
            action: Action::Start,
        }
    }

    fn status(text: &str) -> Answer {
        Answer::ok(format!(r#"{{"id":"job-1","status":"{text}"}}"#))
    }

    /// `pending` and `running` wait, `completed` ends it; the status is
    /// asked at `<base>/job_status/<id>`, each time a GET.
    #[tokio::test]
    async fn the_wait_ends_when_the_job_completes() {
        let fake = FakeController::start().expect("fake");
        for answer in ["pending", "running", "running", "completed"] {
            fake.script("/job_status/job-1", status(answer));
        }
        let c = controller(&fake).with_wait(Duration::from_millis(10), Duration::from_secs(60));
        assert_eq!(c.wait(&job("job-1")).await, Ok(()));
        let seen = fake.seen();
        assert_eq!(seen.len(), 4);
        assert!(
            seen.iter()
                .all(|s| s.method == "GET" && s.path == "/job_status/job-1")
        );
    }

    /// Each answer that ends the wait, with sb-go's text: a failed job or
    /// a status sb doesn't know at once; a failed poll (a non-200, an
    /// answer that can't be read or has no status) once it has failed
    /// [`FAILED_POLLS_IN_A_ROW`] times in a row (docker spec decision 10,
    /// where sb-go ends at the first). Nothing is asked after it.
    #[tokio::test]
    async fn these_end_the_wait() {
        let at_once: [(Answer, ControlError); 2] = [
            (
                status("failed"),
                ControlError::JobFailed(Failure::default()),
            ),
            (
                status("queued"),
                ControlError::UnknownStatus("queued".to_string()),
            ),
        ];
        for (answer, expected) in at_once {
            let fake = FakeController::start().expect("fake");
            fake.script("/job_status/job-1", status("running"));
            fake.script("/job_status/job-1", answer);
            let c = controller(&fake).with_wait(Duration::from_millis(10), Duration::from_secs(60));
            let shown = expected.to_string();
            assert_eq!(c.wait(&job("job-1")).await, Err(expected), "{shown}");
            assert_eq!(fake.seen().len(), 2, "{shown}");
        }
        let failed_polls: [(Answer, ControlError); 4] = [
            (
                Answer::with_status(404, r#"{"status":"not_found"}"#),
                ControlError::StatusCheck(404),
            ),
            (
                Answer::ok("{"),
                ControlError::DecodeStatus(
                    "EOF while parsing an object at line 1 column 1".to_string(),
                ),
            ),
            (Answer::ok("{}"), ControlError::MissingStatus),
            (Answer::ok(r#"{"status":""}"#), ControlError::MissingStatus),
        ];
        for (answer, expected) in failed_polls {
            let fake = FakeController::start().expect("fake");
            fake.script("/job_status/job-1", status("running"));
            // The last answer scripted repeats.
            fake.script("/job_status/job-1", answer);
            let c = controller(&fake).with_wait(Duration::from_millis(10), Duration::from_secs(60));
            let shown = expected.to_string();
            assert_eq!(c.wait(&job("job-1")).await, Err(expected), "{shown}");
            assert_eq!(
                fake.seen().len(),
                1 + FAILED_POLLS_IN_A_ROW as usize,
                "{shown}"
            );
        }
        assert_eq!(
            ControlError::JobFailed(Failure::default()).to_string(),
            "job failed"
        );
        assert_eq!(
            ControlError::UnknownStatus("queued".into()).to_string(),
            "unknown job status: queued"
        );
        assert_eq!(
            ControlError::StatusCheck(404).to_string(),
            "job status check failed with status code: 404"
        );
        assert_eq!(
            ControlError::MissingStatus.to_string(),
            "job status response is missing status"
        );
        assert!(
            ControlError::DecodeStatus("x".into())
                .to_string()
                .starts_with("decode job status response: ")
        );
    }

    /// Polls that can't reach the controller, or time out, as many in a
    /// row as are tolerated: the wait ends with reqwest's cause alone, as
    /// sb-go returns its client's error.
    #[tokio::test]
    async fn failed_polls_end_with_reqwests_cause() {
        let fake = FakeController::start().expect("fake");
        fake.script(
            "/job_status/job-1",
            status("running").after(Duration::from_secs(5)),
        );
        let err = controller(&fake)
            .with_request_timeout(Duration::from_millis(100))
            .with_wait(Duration::from_millis(20), Duration::from_secs(60))
            .wait(&job("job-1"))
            .await
            .expect_err("timeout");
        assert!(matches!(err, ControlError::Poll(_)), "{err:?}");
        assert!(err.to_string().contains("timed out"), "{err}");
        assert_eq!(fake.seen().len(), FAILED_POLLS_IN_A_ROW as usize);
        // Nothing listening (a held port): the same, quickly.
        let (_held, addr) = refused();
        let started = std::time::Instant::now();
        let err = controller(&fake)
            .with_base(&format!("http://{addr}"))
            .expect("base")
            .with_wait(Duration::from_millis(20), Duration::from_secs(60))
            .wait(&job("job-1"))
            .await
            .expect_err("refused");
        assert!(matches!(err, ControlError::Poll(_)), "{err:?}");
        assert!(
            err.to_string().starts_with("error sending request"),
            "{err}"
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// Decision 10: fewer failed polls in a row than tolerated, of each
    /// kind (a timeout, a non-200, an answer without a status), don't end
    /// the wait; a good answer between them starts the count again.
    ///
    /// The request timeout is bounded by a poll's own time here and now (a
    /// loaded host slows the fake's answers too): 20 of them, at least 1 s,
    /// so only the held answer times out. The held answer waits five
    /// timeouts, and ends at once when the fake is dropped.
    #[tokio::test]
    async fn a_few_failed_polls_in_a_row_are_tolerated() {
        assert_eq!(FAILED_POLLS_IN_A_ROW, 3);
        let path = "/job_status/job-1";
        let baseline = {
            let fake = FakeController::start().expect("fake");
            fake.script(path, status("completed"));
            let started = std::time::Instant::now();
            assert_eq!(controller(&fake).wait(&job("job-1")).await, Ok(()));
            started.elapsed()
        };
        let timeout = (baseline * 20).max(Duration::from_secs(1));
        let fake = FakeController::start().expect("fake");
        fake.script(path, status("running").after(timeout * 5));
        fake.script(path, Answer::with_status(502, ""));
        fake.script(path, status("running"));
        fake.script(path, Answer::ok("{}"));
        fake.script(path, Answer::with_status(503, ""));
        fake.script(path, status("completed"));
        let c = controller(&fake)
            .with_request_timeout(timeout)
            .with_wait(Duration::from_millis(20), Duration::from_secs(60));
        assert_eq!(c.wait(&job("job-1")).await, Ok(()));
        assert_eq!(fake.seen().len(), 6);
    }

    /// A status with control characters is shown escaped.
    #[tokio::test]
    async fn an_unknown_status_is_shown_escaped() {
        let fake = FakeController::start().expect("fake");
        fake.script("/job_status/job-1", status("queued\\u001b[31m"));
        let err = controller(&fake)
            .wait(&job("job-1"))
            .await
            .expect_err("unknown");
        let shown = err.to_string();
        assert!(shown.starts_with("unknown job status: queued"), "{shown}");
        assert!(!shown.contains('\u{1b}'), "{shown:?}");
    }

    /// At its limit: sb-go's timeout, the first poll at once, the last at
    /// the limit and no sleep after it, so a limit of two intervals takes
    /// three polls and ends at the limit, not an interval past it (real
    /// time: tokio's paused clock runs ahead while a real socket waits;
    /// [`wait_until`]'s own tests run the schedule on it).
    #[tokio::test]
    async fn the_wait_gives_up_at_its_limit() {
        let fake = FakeController::start().expect("fake");
        fake.script("/job_status/job-1", status("running"));
        let interval = Duration::from_millis(300);
        let limit = interval * 2;
        let c = controller(&fake).with_wait(interval, limit);
        let started = std::time::Instant::now();
        assert_eq!(c.wait(&job("job-1")).await, Err(ControlError::Timeout));
        let elapsed = started.elapsed();
        assert_eq!(fake.seen().len(), 3);
        assert!(elapsed >= limit, "{elapsed:?}");
        assert!(elapsed < limit + interval, "{elapsed:?}");
        assert_eq!(
            ControlError::Timeout.to_string(),
            "timeout waiting for job completion"
        );
    }

    /// sb-go's `TestWaitForJobCompletionHonorsCancellation`: cancelled
    /// mid-wait, it ends at once and asks nothing more.
    #[tokio::test]
    async fn a_cancelled_wait_ends_at_once() {
        let fake = FakeController::start().expect("fake");
        fake.script("/job_status/job-1", status("running"));
        let c = controller(&fake).with_wait(Duration::from_secs(60), Duration::from_secs(120));
        let token = tokio_util::sync::CancellationToken::new();
        let cancel = token.clone();
        let seen = std::sync::Arc::new(fake);
        let watched = std::sync::Arc::clone(&seen);
        // Cancelled once the first poll has arrived: the wait is asleep.
        tokio::spawn(async move {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while watched.seen().is_empty() && std::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel.cancel();
        });
        let started = std::time::Instant::now();
        assert_eq!(token.run_until_cancelled(c.wait(&job("job-1"))).await, None);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(seen.seen().len(), 1);
    }

    /// The ID is one path segment, escaped, under a base with a path.
    #[tokio::test]
    async fn the_job_id_is_one_path_segment() {
        let fake = FakeController::start().expect("fake");
        fake.script("/api/job_status/a%2Fb%3Fc", status("completed"));
        let c = controller(&fake)
            .with_base(&format!("{}/api", fake.url()))
            .expect("base");
        assert_eq!(c.wait(&job("a/b?c")).await, Ok(()));
        assert_eq!(fake.seen()[0].raw_query, None);
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
        let _spawns = crate::control::SPAWNS.blocking_lock();
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

    /// A job the controller still runs is waited out to the controller's
    /// own limit for it and the grace for its ending (decision 12): 600 s
    /// for a start and 300 s for a stop, then 15 s, polled every 5 s from
    /// the first at once, so 124 and 64 polls, the last at the end, then
    /// sb-go's timeout. On tokio's paused clock, so the minutes pass at
    /// once.
    #[tokio::test(start_paused = true)]
    async fn a_long_job_is_waited_out_to_the_controllers_limit() {
        let c = Controller::saltbox().expect("client");
        for (action, polls) in [(Action::Start, 124), (Action::Stop, 64)] {
            let window = c.window(action);
            assert_eq!(window, c.limit(action) + Duration::from_secs(15));
            let started = tokio::time::Instant::now();
            let mut asked = 0;
            let ended = wait_until(window, POLL_INTERVAL, || {
                asked += 1;
                async { Ok(Polled::Waiting) }
            })
            .await;
            assert_eq!(ended, Err(ControlError::Timeout), "{action:?}");
            assert_eq!(asked, polls, "{action:?}");
            assert_eq!(started.elapsed(), window, "{action:?}");
        }
        // A start that takes nearly its 600 s still ends done, where
        // sb-go's 60 polls (about 5 minutes) gave up.
        let started = tokio::time::Instant::now();
        let ended = wait_until(c.window(Action::Start), POLL_INTERVAL, || {
            let at = started.elapsed();
            async move {
                Ok(if at >= Duration::from_secs(595) {
                    Polled::Completed
                } else {
                    Polled::Waiting
                })
            }
        })
        .await;
        assert_eq!(ended, Ok(()));
        assert_eq!(started.elapsed(), Duration::from_secs(595));
    }

    /// The controller's own ending is seen: polls that take a second each
    /// still reach past the limit, so a job it fails at its deadline (its
    /// containers cancelled, read a few seconds after the 600 s) is the
    /// failure, not sb's timeout.
    #[tokio::test(start_paused = true)]
    async fn the_controllers_deadline_failure_is_seen() {
        let c = Controller::saltbox().expect("client");
        let started = tokio::time::Instant::now();
        let cancelled = || Failure {
            error: Some("context deadline exceeded".to_string()),
            containers: vec![FailedContainer {
                name: "plex".to_string(),
                outcome: Some("canceled".to_string()),
                reason: Some("deadline_exceeded".to_string()),
                error: Some("context deadline exceeded".to_string()),
            }],
        };
        let ended = wait_until(c.window(Action::Start), POLL_INTERVAL, || async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            Ok(if started.elapsed() >= Duration::from_secs(606) {
                Polled::Failed(cancelled())
            } else {
                Polled::Waiting
            })
        })
        .await;
        assert_eq!(ended, Err(ControlError::JobFailed(cancelled())));
    }

    /// The limit counts the polls' own time too, and the last poll comes
    /// at the limit, its sleep cut short: polls of 4 s each, every 9 s,
    /// then one at 60 s.
    #[tokio::test(start_paused = true)]
    async fn slow_polls_count_toward_the_limit() {
        let limit = Duration::from_secs(60);
        let started = tokio::time::Instant::now();
        let mut asked = Vec::new();
        let ended = wait_until(limit, POLL_INTERVAL, || {
            asked.push(started.elapsed().as_secs());
            async {
                tokio::time::sleep(Duration::from_secs(4)).await;
                Ok(Polled::Waiting)
            }
        })
        .await;
        assert_eq!(ended, Err(ControlError::Timeout));
        assert_eq!(asked, [0, 9, 18, 27, 36, 45, 54, 60]);
        assert_eq!(started.elapsed(), Duration::from_secs(64));
    }

    /// A failed poll at the limit ends the wait with its error, whatever
    /// the count in a row; a zero limit polls once.
    #[tokio::test(start_paused = true)]
    async fn the_last_poll_at_the_limit_ends_the_wait_with_its_own_error() {
        let mut asked = 0;
        let ended = wait_until(Duration::from_secs(10), POLL_INTERVAL, || {
            asked += 1;
            let answer = if asked == 3 {
                Err(ControlError::StatusCheck(502))
            } else {
                Ok(Polled::Waiting)
            };
            async move { answer }
        })
        .await;
        assert_eq!(ended, Err(ControlError::StatusCheck(502)));
        assert_eq!(asked, 3);
        let mut asked = 0;
        let ended = wait_until(Duration::ZERO, POLL_INTERVAL, || {
            asked += 1;
            async { Ok(Polled::Waiting) }
        })
        .await;
        assert_eq!(ended, Err(ControlError::Timeout));
        assert_eq!(asked, 1);
    }

    /// A failed job names its containers (decision 11), from v1.0.8's
    /// answer as it sends it (`jobStatusResponse`;
    /// docs/platform-notes/docker-controller.md): its `error`, then each
    /// container `failed` names (sorted, exactly those whose outcome is
    /// `failed`, `blocked` or `canceled`) with its outcome, reason and
    /// error; a changed or skipped container isn't named. Docker's
    /// quotes stay as they are.
    #[tokio::test]
    async fn a_failed_job_names_its_containers_and_their_outcomes() {
        let fake = FakeController::start().expect("fake");
        fake.script("/job_status/job-1", status("running"));
        fake.script(
            "/job_status/job-1",
            Answer::ok(
                r#"{"id":"job-1","type":"start","status":"failed","created_at":"2026-11-18T10:00:00Z","started_at":"2026-11-18T10:00:00Z","ended_at":"2026-11-18T10:01:00Z","timeout":600,"ignore":[],"started":["sonarr"],"skipped":["traefik"],"failed":["plex","radarr"],"outcomes":[{"container":"plex","outcome":"failed","reason":"operation_failed","error":"Error response from daemon: error while creating mount source path '/mnt/unionfs/Media': mkdir /mnt/unionfs: file exists"},{"container":"radarr","outcome":"blocked","reason":"dependency_failed","error":"plex"},{"container":"sonarr","outcome":"changed"},{"container":"traefik","outcome":"unchanged","reason":"already_running"}],"error":"one or more containers did not reach the requested state"}"#,
            ),
        );
        let c = controller(&fake).with_wait(Duration::from_millis(10), Duration::from_secs(60));
        let err = c.wait(&job("job-1")).await.expect_err("failed");
        assert_eq!(
            err.to_string(),
            concat!(
                "job failed: one or more containers did not reach the requested state\n",
                "  plex: failed (operation_failed): Error response from daemon: error while creating mount source path '/mnt/unionfs/Media': mkdir /mnt/unionfs: file exists\n",
                "  radarr: blocked (dependency_failed): plex",
            )
        );
        // A failed job is final: nothing is asked after it.
        assert_eq!(fake.seen().len(), 2);
    }

    /// At its deadline the controller cancels every container it manages,
    /// each with the same outcome: they share one line. Defensively, a
    /// name `failed` gives without an entry stands alone, an entry that
    /// fails the job but isn't in `failed` comes after them, blank names
    /// are left out, and control characters are escaped.
    #[tokio::test]
    async fn alike_outcomes_share_a_line_and_odd_answers_are_read_safely() {
        let fake = FakeController::start().expect("fake");
        fake.script(
            "/job_status/job-1",
            Answer::ok(
                r#"{"status":"failed","failed":["bazarr","lidarr","overseerr"," "],"outcomes":[{"container":"bazarr","outcome":"canceled","reason":"deadline_exceeded","error":"context deadline exceeded"},{"container":"lidarr","outcome":"canceled","reason":"deadline_exceeded","error":"context deadline exceeded"},{"container":"tautulli\u001b[31m","outcome":"failed","reason":"operation_failed","error":"x"}],"error":"context deadline exceeded"}"#,
            ),
        );
        let err = controller(&fake)
            .wait(&job("job-1"))
            .await
            .expect_err("failed");
        assert_eq!(
            err.to_string(),
            concat!(
                "job failed: context deadline exceeded\n",
                "  bazarr, lidarr: canceled (deadline_exceeded): context deadline exceeded\n",
                "  overseerr\n",
                "  tautulli\\u{1b}[31m: failed (operation_failed): x",
            )
        );
        // Without the fields (an older controller, or none named): sb-go's
        // text alone.
        let fake = FakeController::start().expect("fake");
        fake.script(
            "/job_status/job-1",
            Answer::ok(r#"{"status":"failed","failed":null,"outcomes":null,"error":""}"#),
        );
        let err = controller(&fake)
            .wait(&job("job-1"))
            .await
            .expect_err("failed");
        assert_eq!(err.to_string(), "job failed");
    }
}
