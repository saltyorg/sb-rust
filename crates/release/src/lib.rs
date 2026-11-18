//! GitHub releases for sb (docs/specs/release.md): the lookup through the
//! SVM proxy, then GitHub, the checks every asset passes, the verified
//! download and the atomic activation. `sb self-update` builds on these.

mod activate;
mod asset;
mod download;
mod lookup;
mod retry;
mod self_update;
#[cfg(feature = "testing")]
pub mod testing;

// A release build (`SB_VERSION` set) can never carry the fake GitHub: in a
// `testing` build `Releases::for_sb` trusts the server and certificate a
// file names. scripts/check.sh checks sb's normal tree has no `testing`
// feature; this stops a build that turned it on anyway (tests built in a
// release job's target directory). (M26's security review.)
#[cfg(feature = "testing")]
const _: () = assert!(
    option_env!("SB_VERSION").is_none(),
    "a release build (SB_VERSION set) must not enable sb-release's `testing` feature"
);

#[cfg(feature = "testing")]
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use reqwest::Url;

pub use activate::{ElfError, activate, check_elf};
pub use asset::{Asset, sha256_file};
pub use download::{Stage, Staged};
pub use lookup::{Failure, Query, Release, Track};
pub use self_update::{Outcome, SelfUpdate, asset_name, self_update};

/// A GitHub repository: the owner is a parameter (sb's and the fact's
/// `saltyorg`, uv's `astral-sh`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GithubRepo {
    pub owner: &'static str,
    pub name: &'static str,
}

/// A CPU architecture sb has releases for (release spec, R4): sb's and
/// the fact's assets name it as Go does, uv's as Rust does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    /// 64-bit x86 (`x86_64`).
    Amd64,
    /// 64-bit Arm (`aarch64`).
    Arm64,
}

impl Arch {
    /// This build's architecture, from `std::env::consts::ARCH`
    /// (<https://doc.rust-lang.org/std/env/consts/constant.ARCH.html>).
    ///
    /// # Errors
    ///
    /// [`Error::Arch`] for any other: `unsupported architecture <a>`.
    pub fn current() -> Result<Self, Error> {
        Self::named(std::env::consts::ARCH)
    }

    /// The name Go gives it, in sb's and the fact's asset names:
    /// `amd64`, `arm64`.
    #[must_use]
    pub fn go_name(self) -> &'static str {
        match self {
            Self::Amd64 => "amd64",
            Self::Arm64 => "arm64",
        }
    }

    /// The name Rust gives it, in uv's asset names: `x86_64`, `aarch64`.
    #[must_use]
    pub fn rust_name(self) -> &'static str {
        match self {
            Self::Amd64 => "x86_64",
            Self::Arm64 => "aarch64",
        }
    }

    /// The architecture `std::env::consts::ARCH` calls `arch`.
    fn named(arch: &str) -> Result<Self, Error> {
        match arch {
            "x86_64" => Ok(Self::Amd64),
            "aarch64" => Ok(Self::Arm64),
            other => Err(Error::Arch(other.to_owned())),
        }
    }
}

/// Why a release couldn't be looked up or used.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// sb has no releases for this build's architecture.
    #[error("unsupported architecture {0}")]
    Arch(String),
    /// The HTTP client couldn't be built (its TLS setup).
    #[error("build the HTTP client: {0}")]
    Client(String),
    /// An asset failed its checks (sb-go's texts).
    #[error("{0}")]
    Asset(String),
    /// The release has no asset of that name (sb-go's text).
    #[error("release {tag} is missing {asset}")]
    Missing { tag: String, asset: String },
    /// No release in the list is stable with a semantic version.
    #[error("no stable releases with valid semantic versions")]
    NoStable,
    /// No release in the list, prereleases included, has a semantic
    /// version (the beta channel's lookup).
    #[error("no releases with valid semantic versions")]
    NoRelease,
    /// A tag query was answered with another release (the tag made safe).
    #[error("release {asked} answered as {got}")]
    OtherTag { asked: String, got: String },
    /// A download failed or didn't verify; nothing is left staged.
    #[error("{0}")]
    Download(String),
    /// A staged file couldn't be put in place.
    #[error("{0}")]
    Activate(String),
    /// The step's token was cancelled (Ctrl-C, a failing sibling).
    #[error("interrupted")]
    Interrupted,
    /// Both the proxy and GitHub failed (sb-go's joined text).
    #[error("{proxy}; fallback GitHub API request failed: {github}")]
    Lookup { proxy: Failure, github: Failure },
}

/// The production bases (release spec, "Release lookup").
const PROXY: &str = "https://svm.saltbox.dev/version";
const API: &str = "https://api.github.com";
const DOWNLOAD: &str = "https://github.com/";

/// Connect within this, else fail (release spec, "Download").
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Each read within this, reset after every successful read: no total, so
/// a slow link finishes (release spec, decision 5).
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Each metadata request in total, body included: a proxy trickling bytes
/// under the read timeout can't stall a lookup without end. Downloads have
/// no total (decision 5; the module plan's follow-ups).
pub(crate) const METADATA_TIMEOUT: Duration = Duration::from_secs(60);

/// One HTTP client and the three bases: the SVM proxy, GitHub's API and
/// GitHub's download host. A run makes one, so its notices come at most
/// once.
#[derive(Debug)]
pub struct Releases {
    client: reqwest::Client,
    proxy: Url,
    api: Url,
    download: Url,
    metadata_timeout: Duration,
    /// Each retry wait is slept divided by this: 1 in production; more
    /// for a fake, so its tests check the waits without taking them. Why
    /// not tokio's paused clock (`tokio::time::pause`): it advances
    /// whenever the runtime waits, on I/O too
    /// (<https://docs.rs/tokio/1.54.1/tokio/time/fn.pause.html>), so the
    /// client's timeouts would fire against the fake's real loopback
    /// socket; and sb's end-to-end tests run sb as its own process.
    wait_divisor: u32,
    /// The retry waits taken, in order, for the tests.
    #[cfg(feature = "testing")]
    waited: Mutex<Vec<Duration>>,
    /// The proxy's warning and the fallback's info, once per run each.
    warned: AtomicBool,
    fell_back: AtomicBool,
}

impl Releases {
    /// The production client and bases: reqwest on rustls with the
    /// platform's roots and its system proxy variables, `User-Agent:
    /// <user_agent>` (GitHub refuses requests without one), connect and
    /// per-read timeouts and no total, and `https_only`, so a redirect to
    /// plain http fails
    /// (<https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html#method.read_timeout>,
    /// <https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html#method.https_only>,
    /// <https://docs.github.com/en/rest/using-the-rest-api/getting-started-with-the-rest-api#user-agent>).
    ///
    /// # Errors
    ///
    /// [`Error::Client`] when the client can't be built.
    pub fn production(user_agent: &str) -> Result<Self, Error> {
        Self::with(builder(user_agent), [PROXY, API, DOWNLOAD])
    }

    /// The client sb uses: [`Releases::production`]; in a build with the
    /// `testing` feature (sb's own tests, never a release or production
    /// build), the fake that `SB_RELEASE_TEST_BASES` names instead, and an
    /// error without it, so a test build never reaches GitHub.
    ///
    /// # Errors
    ///
    /// [`Error::Client`] when the client can't be built, or (`testing`)
    /// the variable is unset or its file unreadable.
    pub fn for_sb(user_agent: &str) -> Result<Self, Error> {
        #[cfg(feature = "testing")]
        return testing::from_env(user_agent);
        #[cfg(not(feature = "testing"))]
        Self::production(user_agent)
    }

    /// The client from `builder` on the three bases (the constants or a
    /// fake's).
    fn with(
        builder: reqwest::ClientBuilder,
        [proxy, api, download]: [&str; 3],
    ) -> Result<Self, Error> {
        // With reqwest's causes (cli-core's chain, as every transport
        // error here): "builder error" alone names none.
        let client = builder
            .build()
            .map_err(|err| Error::Client(lookup::causes(err)))?;
        let parse = |base: &str| {
            Url::parse(base).map_err(|err| Error::Client(format!("base {base}: {err}")))
        };
        Ok(Releases {
            client,
            proxy: parse(proxy)?,
            api: parse(api)?,
            download: parse(download)?,
            metadata_timeout: METADATA_TIMEOUT,
            wait_divisor: 1,
            #[cfg(feature = "testing")]
            waited: Mutex::new(Vec::new()),
            warned: AtomicBool::new(false),
            fell_back: AtomicBool::new(false),
        })
    }

    /// Wait `wait` before attempt `attempt + 1` at `what`, logged with
    /// the failure `reason`, on tokio's timer, so the step's token cancels
    /// it with the request
    /// (<https://docs.rs/tokio/1.54.1/tokio/time/fn.sleep.html>).
    async fn pause(
        &self,
        wait: Duration,
        what: &str,
        attempt: u32,
        reason: &dyn std::fmt::Display,
    ) {
        tracing::info!(
            ?wait,
            what,
            attempt,
            %reason,
            "retrying after a transient failure"
        );
        #[cfg(feature = "testing")]
        self.waited
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(wait);
        tokio::time::sleep(wait / self.wait_divisor.max(1)).await;
    }
}

/// The client's settings, shared by production and the fake.
fn builder(user_agent: &str) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .user_agent(user_agent)
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .https_only(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client that can't be built names reqwest's cause, not only its
    /// "builder error" (in a container without CA certificates, bench's
    /// Speedtest row said nothing more): a User-Agent that isn't a header
    /// value makes reqwest's builder fail with a cause of its own.
    #[test]
    fn a_client_that_cannot_be_built_names_the_cause() {
        let Err(Error::Client(text)) = Releases::production("sb/\n1") else {
            panic!("the client was built");
        };
        assert_eq!(text, "builder error: failed to parse header value");
    }

    /// A base that isn't a URL is the client's error, naming the base.
    #[test]
    fn a_base_that_isnt_a_url_is_named() {
        let Err(Error::Client(text)) =
            Releases::with(builder("sb/1"), ["not a url", API, DOWNLOAD])
        else {
            panic!("the client was made");
        };
        assert_eq!(text, "base not a url: relative URL without a base");
    }

    #[test]
    fn the_architecture_is_std_s_name() {
        assert_eq!(Arch::named("x86_64").expect("amd64"), Arch::Amd64);
        assert_eq!(Arch::named("aarch64").expect("arm64"), Arch::Arm64);
        assert_eq!(
            Arch::named("riscv64").expect_err("none").to_string(),
            "unsupported architecture riscv64"
        );
    }

    #[test]
    fn each_architecture_has_go_s_and_rust_s_names() {
        assert_eq!(Arch::Amd64.go_name(), "amd64");
        assert_eq!(Arch::Arm64.go_name(), "arm64");
        assert_eq!(Arch::Amd64.rust_name(), "x86_64");
        assert_eq!(Arch::Arm64.rust_name(), "aarch64");
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn an_x86_64_build_is_amd64() {
        assert_eq!(Arch::current().expect("this build's"), Arch::Amd64);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn an_aarch64_build_is_arm64() {
        assert_eq!(Arch::current().expect("this build's"), Arch::Arm64);
    }
}
