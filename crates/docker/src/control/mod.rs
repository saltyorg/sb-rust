//! `sb docker start`, `stop` and `restart` (docker spec, "Start, stop and
//! restart"): the Saltbox Docker controller, sb-go's values and texts
//! (`cmd/docker.go`), its API as docs/platform-notes/docker-controller.md
//! records it. `client` asks for jobs and waits for them; the service
//! check and the commands as term-ui steps join it.

mod client;
mod commands;
mod service;

pub use commands::{restart, start, stop};
/// exec's forced-exit hook, for the binary to register once with its
/// `Shutdown` before running these commands (deny.toml lets only sb-docker
/// wrap sb-exec): a second signal SIGKILLs a `systemctl` still running.
pub use sb_exec::on_forced_exit;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

/// The controller's address, sb-go's `layout.DockerControllerAPIURL`.
pub const ADDRESS: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3377);

/// The controller's unit file, sb-go's `layout.DockerControllerServiceFile`.
pub const UNIT_FILE: &str = "/etc/systemd/system/saltbox_managed_docker_controller.service";

/// The controller's unit, as `systemctl is-active` names it.
pub const UNIT: &str = "saltbox_managed_docker_controller.service";

/// sb-go's `systemctl is-active` limit.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// sb-go's limit for each request (its `http.Client` timeout).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// sb-go's `dockerJobPollInterval`.
pub const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// How long sb waits for a start job: the controller's own limit for one
/// (its default job timeout, docs/platform-notes/docker-controller.md),
/// so sb doesn't give up while the controller is still at it (docker
/// spec decision 12; sb-go gives up after about 5 minutes).
pub const START_LIMIT: Duration = Duration::from_secs(600);

/// How long sb waits for a stop job: the controller's own limit, as
/// [`START_LIMIT`].
pub const STOP_LIMIT: Duration = Duration::from_secs(300);

/// How long sb goes on asking past a job's limit, for the controller's
/// own ending: at its deadline it cancels the job's Docker calls and
/// lists the containers again (up to 5 s; v1.0.8's `canceledSnapshot`)
/// before the job reads `failed`, and sb's last poll should see that.
pub const ENDING_GRACE: Duration = Duration::from_secs(15);

/// sb-go's `dockerAPIResponseLimit`: at most this much of an answer is read.
pub const BODY_LIMIT: usize = 1 << 20;

/// What a job asks the controller to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
}

impl Action {
    /// The request path's segment (`/start`, `/stop`).
    #[must_use]
    pub fn segment(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
        }
    }
}

/// Why the service check, a request or a wait failed: sb-go's texts, a cause from
/// reqwest or `serde_json` in place of Go's. The cause is part of the text
/// only, not a `source` as well: term-ui's failure block shows the whole
/// chain, which would print it twice.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ControlError {
    #[error("parse Docker controller URL: {0}")]
    ParseUrl(String),
    #[error("build the HTTP client: {0}")]
    Client(String),
    #[error("send Docker controller request: {0}")]
    Send(String),
    #[error("docker controller request failed with status code: {0}")]
    RequestStatus(u16),
    #[error("decode Docker controller response: {0}")]
    DecodeJob(String),
    #[error("docker controller response is missing job ID")]
    MissingJobId,
    /// A poll's transport failure or timeout: reqwest's cause alone, as
    /// sb-go returns its client's error unwrapped.
    #[error("{0}")]
    Poll(String),
    #[error("job status check failed with status code: {0}")]
    StatusCheck(u16),
    #[error("decode job status response: {0}")]
    DecodeStatus(String),
    #[error("job status response is missing status")]
    MissingStatus,
    /// The job read `failed`: sb-go's "job failed", then what the
    /// controller said of it ([`Failure`]).
    #[error("{0}")]
    JobFailed(Failure),
    #[error("unknown job status: {0}")]
    UnknownStatus(String),
    #[error("timeout waiting for job completion")]
    Timeout,
    #[error("the Docker controller service does not exist")]
    NoService,
    /// Another stat error: "stat <path>: <cause>" (Go's `PathError` names
    /// the path; Rust's `io::Error` doesn't, so it is added).
    #[error("error checking service status: {0}")]
    Stat(String),
    #[error("the Docker controller service is not running")]
    NotRunning,
    /// The step's token was cancelled (the service check, or a request or
    /// wait the step's body ended). When sb's shutdown cancelled it, the
    /// runner shows the step's interrupted (or stopped) ending instead of
    /// this text; under a caller's own token it is the step's failure.
    #[error("interrupted")]
    Interrupted,
}

/// A job the controller accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    id: String,
    action: Action,
}

impl Job {
    /// The job's ID, as the controller sent it (sb-go polls it untrimmed).
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// What the job was asked to do.
    #[must_use]
    pub fn action(&self) -> Action {
        self.action
    }
}

/// What the controller said of a failed job (docker spec decision 11):
/// its `error`, and each container that didn't reach the requested
/// state, with its outcome where the job's `outcomes` has one
/// (docs/platform-notes/docker-controller.md). Every text is the
/// controller's, made safe for one line (cli-core's `exit::escaped`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Failure {
    /// The job's `error`, when it has one.
    pub error: Option<String>,
    /// Each container named, in the controller's order.
    pub containers: Vec<FailedContainer>,
}

/// One container of a failed job, and its `outcomes` entry's fields
/// where it has one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FailedContainer {
    pub name: String,
    /// `failed`, `blocked` or `canceled`.
    pub outcome: Option<String>,
    /// The controller's reason (`operation_failed`, `dependency_failed`).
    pub reason: Option<String>,
    /// The outcome's error: Docker's for a failed operation, the failed
    /// dependency's name for a blocked one, the context's for a cancelled
    /// one.
    pub error: Option<String>,
}

impl FailedContainer {
    /// The outcome as shown, `failed (operation_failed): <error>`; `None`
    /// without one.
    fn shown(&self) -> Option<String> {
        let outcome = self.outcome.as_deref()?;
        let reason = self
            .reason
            .as_deref()
            .map_or_default(|reason| format!(" ({reason})"));
        let error = self
            .error
            .as_deref()
            .map_or_default(|error| format!(": {error}"));
        Some(format!("{outcome}{reason}{error}"))
    }
}

/// "job failed", the job's error after it, then a line per outcome:
/// term-ui's failure block shows every line. Containers with the same
/// outcome share its line (`bazarr, lidarr: canceled (deadline_exceeded):
/// context deadline exceeded`): at its deadline the controller cancels
/// every container it manages, which would be dozens of lines alike.
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("job failed")?;
        if let Some(error) = &self.error {
            write!(f, ": {error}")?;
        }
        let mut lines: Vec<(Option<String>, Vec<&str>)> = Vec::new();
        for container in &self.containers {
            let shown = container.shown();
            match lines.iter_mut().find(|(outcome, _)| *outcome == shown) {
                Some((_, names)) => names.push(&container.name),
                None => lines.push((shown, vec![&container.name])),
            }
        }
        for (outcome, names) in lines {
            let names = names.join(", ");
            match outcome {
                Some(outcome) => write!(f, "\n  {names}: {outcome}")?,
                None => write!(f, "\n  {names}")?,
            }
        }
        Ok(())
    }
}

/// Where the controller is and how sb talks to it; `saltbox()` gives
/// sb-go's values, the setters a test's or an override's.
#[derive(Debug)]
pub struct Controller {
    base: reqwest::Url,
    unit_file: PathBuf,
    systemctl: PathBuf,
    check_timeout: Duration,
    request_timeout: Duration,
    poll_interval: Duration,
    body_limit: usize,
    start_limit: Duration,
    stop_limit: Duration,
    ending_grace: Duration,
    http: reqwest::Client,
}

impl Controller {
    /// sb-go's controller: its address, unit file and limits.
    ///
    /// # Errors
    ///
    /// [`ControlError::Client`] when the HTTP client can't be built.
    pub fn saltbox() -> Result<Self, ControlError> {
        // No proxy: Go's `ProxyFromEnvironment` never proxies a loopback
        // address, reqwest's system proxy would; no redirects: the
        // controller never answers with one (decision 8). The timeout is
        // set per request (`RequestBuilder::timeout`), so it can be a field
        // (<https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html#method.no_proxy>,
        // <https://docs.rs/reqwest/0.13.5/reqwest/redirect/struct.Policy.html#method.none>,
        // <https://docs.rs/reqwest/0.13.5/reqwest/struct.RequestBuilder.html#method.timeout>).
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|err| ControlError::Client(client::cause(&err)))?;
        let base = reqwest::Url::parse(&format!("http://{ADDRESS}"))
            .map_err(|err| ControlError::ParseUrl(err.to_string()))?;
        Ok(Self {
            base,
            unit_file: PathBuf::from(UNIT_FILE),
            systemctl: PathBuf::from("systemctl"),
            check_timeout: CHECK_TIMEOUT,
            request_timeout: REQUEST_TIMEOUT,
            poll_interval: POLL_INTERVAL,
            body_limit: BODY_LIMIT,
            start_limit: START_LIMIT,
            stop_limit: STOP_LIMIT,
            ending_grace: ENDING_GRACE,
            http,
        })
    }

    /// The controller at `url` instead (`SB_DOCKER_CONTROLLER_URL`; tests).
    ///
    /// # Errors
    ///
    /// sb-go's [`ControlError::ParseUrl`] when `url` isn't a URL that can
    /// take a path.
    pub fn with_base(mut self, url: &str) -> Result<Self, ControlError> {
        let base =
            reqwest::Url::parse(url).map_err(|err| ControlError::ParseUrl(err.to_string()))?;
        if base.cannot_be_a_base() {
            return Err(ControlError::ParseUrl(format!(
                "{}: can't take a path",
                sb_cli_core::config::escape_controls(url)
            )));
        }
        self.base = base;
        Ok(self)
    }

    /// The unit file at `path` instead (`SB_DOCKER_CONTROLLER_UNIT`; tests).
    #[must_use]
    pub fn with_unit_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.unit_file = path.into();
        self
    }

    /// Run `program` as `systemctl` (tests; `saltbox()` finds it on `PATH`).
    #[must_use]
    pub fn with_systemctl(mut self, program: impl Into<PathBuf>) -> Self {
        self.systemctl = program.into();
        self
    }

    /// `systemctl is-active`'s limit instead.
    #[must_use]
    pub fn with_check_timeout(mut self, limit: Duration) -> Self {
        self.check_timeout = limit;
        self
    }

    /// Each request's limit instead.
    #[must_use]
    pub fn with_request_timeout(mut self, limit: Duration) -> Self {
        self.request_timeout = limit;
        self
    }

    /// The wait's interval, and how long it waits for a job of either
    /// action, with no grace after it, instead.
    #[must_use]
    pub fn with_wait(mut self, interval: Duration, limit: Duration) -> Self {
        self.poll_interval = interval;
        self.start_limit = limit;
        self.stop_limit = limit;
        self.ending_grace = Duration::ZERO;
        self
    }

    /// The controller's limit for a job of `action`.
    #[must_use]
    pub fn limit(&self, action: Action) -> Duration {
        match action {
            Action::Start => self.start_limit,
            Action::Stop => self.stop_limit,
        }
    }

    /// How long the wait asks about a job of `action`: its limit, and
    /// the grace for the controller's own ending ([`ENDING_GRACE`]).
    #[must_use]
    pub fn window(&self, action: Action) -> Duration {
        self.limit(action) + self.ending_grace
    }

    /// At most `limit` bytes of an answer read instead.
    #[must_use]
    pub fn with_body_limit(mut self, limit: usize) -> Self {
        self.body_limit = limit;
        self
    }
}

/// Tests that write a stand-in program and run it hold this, so no other
/// test's fork holds the program's write descriptor when it runs (exec
/// fails with ETXTBSY otherwise).
#[cfg(test)]
pub(crate) static SPAWNS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A stand-in `systemctl` in `dir` running `script` (sh): it checks its
/// arguments are sb-go's, then leaves `ran` behind. Callers hold
/// `SPAWNS` from writing it until its run is over, so no other test's
/// fork holds its write descriptor when it runs (ETXTBSY).
#[cfg(test)]
pub(crate) fn systemctl(dir: &std::path::Path, script: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join("systemctl");
    let ran = dir.join("ran");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\n[ \"$1 $2\" = 'is-active {}' ] || exit 9\n: > '{}'\n{script}\n",
            UNIT,
            ran.display(),
        ),
    )
    .expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}
