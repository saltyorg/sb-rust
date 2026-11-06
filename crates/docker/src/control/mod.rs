//! `sb docker start`, `stop` and `restart` (docker spec, "Start, stop and
//! restart"): the Saltbox Docker controller, sb-go's values and texts
//! (`cmd/docker.go`), its API as docs/platform-notes/docker-controller.md
//! records it. `client` asks for jobs; the service check and the commands
//! as term-ui steps join it.

mod client;

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

/// sb-go's `dockerJobMaxPolls`.
pub const MAX_POLLS: u32 = 60;

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

/// Why a request to the controller failed: sb-go's texts, a cause from
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
}

/// A job the controller accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    id: String,
}

impl Job {
    /// The job's ID, as the controller sent it (sb-go polls it untrimmed).
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
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
    max_polls: u32,
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
            max_polls: MAX_POLLS,
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

    /// The wait's interval and its number of polls instead.
    #[must_use]
    pub fn with_polls(mut self, interval: Duration, max: u32) -> Self {
        self.poll_interval = interval;
        self.max_polls = max;
        self
    }

    /// At most `limit` bytes of an answer read instead.
    #[must_use]
    pub fn with_body_limit(mut self, limit: usize) -> Self {
        self.body_limit = limit;
        self
    }
}
