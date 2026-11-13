//! GitHub releases for sb (docs/specs/release.md): the lookup through the
//! SVM proxy, then GitHub, the checks every asset passes, the verified
//! download and the atomic activation. `sb self-update` builds on these.

mod activate;
mod asset;
mod download;
mod lookup;
#[cfg(feature = "testing")]
pub mod testing;

use std::sync::atomic::AtomicBool;
use std::time::Duration;

use reqwest::Url;

pub use activate::activate;
pub use asset::Asset;
pub use download::{Stage, Staged};
pub use lookup::{Failure, Query, Release};

/// A GitHub repository: the owner is a parameter (sb's and the fact's
/// `saltyorg`, uv's `astral-sh`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GithubRepo {
    pub owner: &'static str,
    pub name: &'static str,
}

/// Why a release couldn't be looked up or used.
#[derive(Debug, thiserror::Error)]
pub enum Error {
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

    /// The client from `builder` on the three bases (the constants or a
    /// fake's).
    fn with(
        builder: reqwest::ClientBuilder,
        [proxy, api, download]: [&str; 3],
    ) -> Result<Self, Error> {
        let client = builder
            .build()
            .map_err(|err| Error::Client(err.to_string()))?;
        let parse = |base: &str| {
            Url::parse(base).map_err(|err| Error::Client(format!("base {base}: {err}")))
        };
        Ok(Releases {
            client,
            proxy: parse(proxy)?,
            api: parse(api)?,
            download: parse(download)?,
            metadata_timeout: METADATA_TIMEOUT,
            warned: AtomicBool::new(false),
            fell_back: AtomicBool::new(false),
        })
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
