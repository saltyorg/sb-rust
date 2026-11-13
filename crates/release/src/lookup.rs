//! Release lookup (release spec, "Release lookup"): GitHub's release
//! metadata through the SVM proxy, then from GitHub directly, once, with
//! sb-go's notices (selfupdate/update.go, `Source.List`); an answer is
//! usable when its release has the asset asked for and the asset passes
//! its checks.

use std::fmt;
use std::sync::atomic::Ordering;
use std::time::Duration;

use reqwest::header::HeaderMap;
use reqwest::{StatusCode, Url};
use sb_term_ui::{DisplayText, Step};
use semver::Version;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::asset::RawAsset;
use crate::{Asset, Error, GithubRepo, Releases, retry};

/// A metadata body is read up to this many bytes; more is invalid JSON
/// (sb-go's `io.LimitReader(resp.Body, 4<<20)`).
const MAX_BODY: usize = 4 << 20;
/// An error body is read up to this many bytes (sb-go's 64 KiB).
const MAX_ERROR_BODY: usize = 64 << 10;
/// Remote text in a message is cut at this many characters (sb-go's
/// `cleanDetail`).
const MAX_DETAIL: usize = 240;

/// Which release of a repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Query {
    /// GitHub's latest release (`/releases/latest`).
    Latest,
    /// The release with this tag (`/releases/tags/<tag>`).
    Tag(String),
}

/// A release with the asset asked for, checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    /// The tag as a semantic version, `v` optional; `None` when it isn't
    /// one.
    pub version: Option<Version>,
    pub asset: Asset,
}

/// A release as GitHub's API describes it; unknown fields ignored. The
/// tag is required, so an answer of another shape (a list for one
/// release, an error object) is invalid rather than an empty release.
#[derive(Debug, Deserialize)]
struct RawRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

/// Why one source (the proxy, or GitHub) gave no usable answer.
#[derive(Debug)]
pub struct Failure(Kind);

#[derive(Debug)]
enum Kind {
    /// Not 200; the error body, made safe and cut.
    Status { code: u16, body: String },
    /// An answer sb can't use (sb-go's `InvalidResponse`).
    Invalid {
        detail: &'static str,
        cause: Option<String>,
    },
    /// The request itself failed: its causes on one line.
    Transport { timed_out: bool, causes: String },
    /// A retryable failure whose next wait was over the limit.
    GaveUp {
        failure: Box<Failure>,
        wait: Duration,
    },
}

impl Failure {
    /// The short reason in the proxy's warning (sb-go's `Describe`).
    fn reason(&self) -> String {
        match &self.0 {
            Kind::Status { code, .. } => format!("returned HTTP {code}"),
            Kind::Invalid { detail, .. } => (*detail).to_owned(),
            Kind::Transport {
                timed_out: true, ..
            } => "timed out".to_owned(),
            Kind::Transport { causes, .. } => format!("request failed ({causes})"),
            Kind::GaveUp { failure, wait } => {
                format!("{}; {}", failure.reason(), retry::too_long(*wait))
            }
        }
    }

    /// Worth another attempt: a transport failure or a retryable status;
    /// an answer sb can't use is not.
    fn retryable(&self) -> bool {
        match &self.0 {
            Kind::Status { code, .. } => StatusCode::from_u16(*code).is_ok_and(retry::retryable),
            Kind::Transport { .. } => true,
            Kind::Invalid { .. } | Kind::GaveUp { .. } => false,
        }
    }

    fn invalid(detail: &'static str, cause: impl fmt::Display) -> Self {
        Failure(Kind::Invalid {
            detail,
            cause: Some(cut(&cause.to_string())),
        })
    }

    /// The cause is what the warning needs; the URL is in the log.
    fn transport(err: reqwest::Error) -> Self {
        Failure(Kind::Transport {
            timed_out: err.is_timeout(),
            causes: causes(err),
        })
    }
}

/// sb-go's `Failure.Error()` texts, `proxy` left out of the status (it
/// names GitHub's failures too).
impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Kind::Status { code, body } if body.is_empty() => write!(f, "returned HTTP {code}"),
            Kind::Status { code, body } => write!(f, "returned HTTP {code}: {body}"),
            Kind::Invalid {
                detail,
                cause: Some(cause),
            } => write!(f, "{detail}: {cause}"),
            Kind::Invalid {
                detail,
                cause: None,
            } => f.write_str(detail),
            Kind::Transport { causes, .. } => write!(f, "fetch releases: {causes}"),
            Kind::GaveUp { failure, wait } => {
                write!(f, "{failure}; {}", retry::too_long(*wait))
            }
        }
    }
}

impl std::error::Error for Failure {}

/// `err` with its causes on one line (cli-core's `chain`), without the
/// request's URL (reqwest's `without_url`,
/// <https://docs.rs/reqwest/0.13.5/reqwest/struct.Error.html#method.without_url>),
/// made safe and cut.
pub(crate) fn causes(err: reqwest::Error) -> String {
    let err = err.without_url();
    let causes = sb_cli_core::exit::chain(&err)
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ");
    cut(&causes)
}

/// Remote text as a message shows it: escape sequences and controls
/// dropped by term-ui's `DisplayText`, whitespace runs one space, cut at
/// [`MAX_DETAIL`] characters with `...` (sb-go's `cleanDetail`).
fn cut(text: &str) -> String {
    let safe = DisplayText::from_bytes(text.as_bytes(), false);
    let joined = safe
        .as_str()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    match joined.char_indices().nth(MAX_DETAIL) {
        Some((end, _)) => format!("{}...", &joined[..end]),
        None => joined,
    }
}

impl Releases {
    /// The release `query` names in `repo`, with its asset `asset`
    /// checked: through the proxy, then GitHub (sb-go's notices on
    /// `step`). A release without the asset: `release <tag> is missing
    /// <asset>`. A tag query takes only that tag's release; for `Latest`,
    /// which release is latest is the source's word (the spec's known
    /// limit: the proxy and GitHub are trusted for the metadata).
    ///
    /// # Errors
    ///
    /// The asset's check or [`Error::Missing`] from GitHub's answer,
    /// [`Error::Lookup`] when both sources fail, [`Error::Interrupted`]
    /// when the step's token is cancelled.
    pub async fn release(
        &self,
        repo: GithubRepo,
        query: Query,
        asset: &str,
        step: &mut Step,
    ) -> Result<Release, Error> {
        let path = match &query {
            Query::Latest => vec!["releases", "latest"],
            Query::Tag(tag) => vec!["releases", "tags", tag.as_str()],
        };
        self.fetch(repo, &path, step, |raw: RawRelease| {
            if let Query::Tag(tag) = &query
                && raw.tag_name != *tag
            {
                return Err(Error::OtherTag {
                    asked: tag.clone(),
                    got: cut(&raw.tag_name),
                });
            }
            self.pick(raw, repo, asset)
        })
        .await
    }

    /// The newest stable release of `repo` (drafts and prereleases
    /// skipped, tags with an optional `v` read as semantic versions, others
    /// skipped), with `asset` checked; `None` when it isn't newer than
    /// `current`. The list is GitHub's first page, newest first.
    ///
    /// # Errors
    ///
    /// As [`Releases::release`], and [`Error::NoStable`] when no release
    /// is stable with a semantic version.
    pub async fn newest(
        &self,
        repo: GithubRepo,
        current: &Version,
        asset: &str,
        step: &mut Step,
    ) -> Result<Option<Release>, Error> {
        self.fetch(repo, &["releases"], step, |raws: Vec<RawRelease>| {
            let (raw, version) = raws
                .into_iter()
                .filter(|raw| !raw.draft && !raw.prerelease)
                .filter_map(|raw| tag_version(&raw.tag_name).map(|version| (raw, version)))
                .max_by(|(_, a), (_, b)| a.cmp_precedence(b))
                .ok_or(Error::NoStable)?;
            let release = self.pick(raw, repo, asset)?;
            // Precedence, as sb-go's `GreaterThan`: build metadata doesn't
            // make a release newer
            // (<https://docs.rs/semver/1.0.28/semver/struct.Version.html#method.cmp_precedence>).
            Ok((version.cmp_precedence(current) == std::cmp::Ordering::Greater).then_some(release))
        })
        .await
    }

    /// `raw`'s asset `asset`, checked (sb-go's `releaseListUsabilityError`
    /// and `Select`).
    /// The remote tag reaches errors made safe ([`cut`]).
    fn pick(&self, raw: RawRelease, repo: GithubRepo, asset: &str) -> Result<Release, Error> {
        let tag = raw.tag_name;
        let found = raw
            .assets
            .iter()
            .find(|candidate| candidate.name == asset)
            .ok_or_else(|| Error::Missing {
                tag: cut(&tag),
                asset: asset.to_owned(),
            })?;
        let checked = Asset::check(found, &self.download, repo, &tag).map_err(|err| {
            Error::Asset(format!(
                "release {} has unusable {asset} metadata: {err}",
                cut(&tag)
            ))
        })?;
        Ok(Release {
            version: tag_version(&tag),
            tag,
            asset: checked,
        })
    }

    /// The metadata at `path` under `repo` in GitHub's API, read as `T`
    /// and made a result by `usable`: through the proxy first; on a
    /// failure there, or an answer `usable` refuses, from GitHub once.
    /// GitHub's own answer is final: `usable`'s error is the result. Each
    /// notice at most once per run.
    async fn fetch<T: DeserializeOwned, R>(
        &self,
        repo: GithubRepo,
        path: &[&str],
        step: &mut Step,
        usable: impl Fn(T) -> Result<R, Error>,
    ) -> Result<R, Error> {
        let api = self.api_url(repo, path);
        let mut proxied = self.proxy.clone();
        proxied.query_pairs_mut().append_pair("url", api.as_str());
        let token = step.token();
        let proxy = match token.run_until_cancelled(self.get::<T>(proxied)).await {
            None => return Err(Error::Interrupted),
            Some(Ok(answer)) => match usable(answer) {
                Ok(result) => {
                    tracing::info!(url = %api, "release metadata from the SVM proxy");
                    return Ok(result);
                }
                Err(err) => Failure::invalid("returned unusable release metadata", err),
            },
            Some(Err(failure)) => failure,
        };
        tracing::warn!(url = %api, %proxy, "SVM proxy failed; trying GitHub directly");
        if !self.warned.swap(true, Ordering::Relaxed) {
            step.warn(format!(
                "SVM proxy {}; trying GitHub directly",
                proxy.reason()
            ));
        }
        match token.run_until_cancelled(self.get::<T>(api)).await {
            None => Err(Error::Interrupted),
            Some(Ok(answer)) => {
                let result = usable(answer)?;
                if !self.fell_back.swap(true, Ordering::Relaxed) {
                    step.info("GitHub fallback succeeded");
                }
                Ok(result)
            }
            Some(Err(github)) => Err(Error::Lookup { proxy, github }),
        }
    }

    /// `<api>/repos/<owner>/<repo>/<path...>`, each part encoded as one
    /// path segment by url
    /// (<https://docs.rs/url/2.5.8/url/struct.PathSegmentsMut.html#method.extend>).
    fn api_url(&self, repo: GithubRepo, path: &[&str]) -> Url {
        let mut url = self.api.clone();
        if let Ok(mut segments) = url.path_segments_mut() {
            segments
                .pop_if_empty()
                .extend(["repos", repo.owner, repo.name])
                .extend(path);
        }
        url
    }

    /// [`Releases::get_once`] with the retry policy (decision 7): a
    /// transport failure or a retryable status is tried again, up to
    /// [`retry::ATTEMPTS`] in all; a wait over the limit gives up with
    /// the reason.
    async fn get<T: DeserializeOwned>(&self, url: Url) -> Result<T, Failure> {
        let mut attempt = 1;
        loop {
            let (failure, headers) = match self.get_once::<T>(url.clone()).await {
                Ok(answer) => return Ok(answer),
                Err(failed) => failed,
            };
            if !failure.retryable() || attempt >= retry::ATTEMPTS {
                return Err(failure);
            }
            match retry::wait(attempt, headers.as_deref(), jiff::Timestamp::now()) {
                Ok(wait) => self.pause(wait, url.path(), attempt, &failure).await,
                Err(wait) => {
                    return Err(Failure(Kind::GaveUp {
                        failure: Box::new(failure),
                        wait,
                    }));
                }
            }
            attempt += 1;
        }
    }

    /// GET `url` once, 200 only, its JSON body read chunk by chunk up to
    /// [`MAX_BODY`]
    /// (<https://docs.rs/reqwest/0.13.5/reqwest/struct.Response.html#method.chunk>).
    /// A failure comes with the response's headers when there was one
    /// (the retry waits read them).
    async fn get_once<T: DeserializeOwned>(
        &self,
        url: Url,
    ) -> Result<T, (Failure, Option<Box<HeaderMap>>)> {
        let mut response = self
            .client
            .get(url)
            // A total for the request, body included
            // (<https://docs.rs/reqwest/0.13.5/reqwest/struct.RequestBuilder.html#method.timeout>).
            .timeout(self.metadata_timeout)
            .send()
            .await
            .map_err(|err| (Failure::transport(err), None))?;
        let status = response.status();
        let headers = Box::new(response.headers().clone());
        if status != StatusCode::OK {
            let body = read(&mut response, MAX_ERROR_BODY)
                .await
                .unwrap_or_default();
            let failure = Failure(Kind::Status {
                code: status.as_u16(),
                body: cut(&String::from_utf8_lossy(&body)),
            });
            return Err((failure, Some(headers)));
        }
        let body = read(&mut response, MAX_BODY + 1)
            .await
            .map_err(|err| (Failure::transport(err), Some(headers)))?;
        let invalid =
            |cause: &dyn fmt::Display| (Failure::invalid("returned invalid JSON", cause), None);
        if body.len() > MAX_BODY {
            return Err(invalid(&"larger than 4 MiB"));
        }
        serde_json::from_slice(&body).map_err(|err| invalid(&err))
    }
}

/// `response`'s body, up to `limit` bytes, read chunk by chunk. reqwest's
/// `Response` has no size limit of its own; why not `http_body_util::Limited`:
/// it fails past the limit, where an error body is kept cut (and the
/// metadata path reads one byte past the limit to tell "too large").
async fn read(response: &mut reqwest::Response, limit: usize) -> Result<Vec<u8>, reqwest::Error> {
    let mut body = Vec::new();
    while body.len() < limit {
        match response.chunk().await? {
            Some(chunk) => body.extend_from_slice(&chunk),
            None => break,
        }
    }
    body.truncate(limit);
    Ok(body)
}

/// `tag` as a semantic version, an optional `v` first (sb-go trims
/// whitespace too).
fn tag_version(tag: &str) -> Option<Version> {
    let tag = tag.trim();
    Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_read_as_versions_with_an_optional_v() {
        assert_eq!(tag_version("v1.2.3"), Some(Version::new(1, 2, 3)));
        assert_eq!(tag_version(" 1.2.3\n"), Some(Version::new(1, 2, 3)));
        assert_eq!(tag_version("vv1.2.3"), None);
        assert_eq!(tag_version("nightly"), None);
        assert_eq!(tag_version("1.2"), None);
    }

    #[test]
    fn remote_text_is_made_safe_joined_and_cut() {
        assert_eq!(
            cut("  bad \u{1b}]0;title\u{7}\u{1b}[31mred\n  line "),
            "bad red line"
        );
        let long = "é".repeat(MAX_DETAIL + 5);
        let cut_long = cut(&long);
        assert_eq!(cut_long.chars().count(), MAX_DETAIL + 3);
        assert!(cut_long.ends_with("..."));
        assert_eq!(cut(&"a".repeat(MAX_DETAIL)), "a".repeat(MAX_DETAIL));
    }

    #[test]
    fn failures_read_as_sb_gos() {
        let status = Failure(Kind::Status {
            code: 502,
            body: "bad gateway".into(),
        });
        assert_eq!(status.to_string(), "returned HTTP 502: bad gateway");
        assert_eq!(status.reason(), "returned HTTP 502");
        let bare = Failure(Kind::Status {
            code: 404,
            body: String::new(),
        });
        assert_eq!(bare.to_string(), "returned HTTP 404");
        let invalid = Failure::invalid("returned invalid JSON", "eof");
        assert_eq!(invalid.to_string(), "returned invalid JSON: eof");
        assert_eq!(invalid.reason(), "returned invalid JSON");
        let detail_only = Failure(Kind::Invalid {
            detail: "returned no releases",
            cause: None,
        });
        assert_eq!(detail_only.to_string(), "returned no releases");
        let timeout = Failure(Kind::Transport {
            timed_out: true,
            causes: "operation timed out".into(),
        });
        assert_eq!(timeout.reason(), "timed out");
        assert_eq!(timeout.to_string(), "fetch releases: operation timed out");
        let refused = Failure(Kind::Transport {
            timed_out: false,
            causes: "error sending request: connection refused".into(),
        });
        assert_eq!(
            refused.reason(),
            "request failed (error sending request: connection refused)"
        );
        let gave_up = Failure(Kind::GaveUp {
            failure: Box::new(Failure(Kind::Status {
                code: 403,
                body: "API rate limit exceeded".into(),
            })),
            wait: Duration::from_secs(7200),
        });
        assert_eq!(
            gave_up.to_string(),
            "returned HTTP 403: API rate limit exceeded; retry wait 7200s exceeds the 60s limit"
        );
        assert_eq!(
            gave_up.reason(),
            "returned HTTP 403; retry wait 7200s exceeds the 60s limit"
        );
        assert!(!gave_up.retryable());
        assert!(refused.retryable() && timeout.retryable() && status.retryable());
        assert!(!bare.retryable() && !invalid.retryable());
    }
}
