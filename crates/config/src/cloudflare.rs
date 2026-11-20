//! The Cloudflare check (config spec, "Cloudflare"; sb-go's
//! `validateCloudflareCredentialsWithOptions` on cloudflare-go v7.13.1):
//! a global key's `GET /user`, with it the zone of `user.domain` (its
//! registrable domain under the public suffix list), then the zone's
//! SSL/TLS mode, each request retried as cloudflare-go retries. sb-go's guidance follows
//! any zone or SSL failure; the credentials never appear in what it
//! reports.

use std::time::Duration;

use reqwest::header::HeaderMap;

use crate::http::{Http, body, error_chain, remote_text};
use crate::issue::escape_controls;
use crate::secret::hide;

/// Cloudflare's API base (cloudflare-go's default).
pub const BASE_URL: &str = "https://api.cloudflare.com/client/v4/";

/// The credentials a check uses (the structure checks have passed).
#[derive(Clone, PartialEq, Eq)]
pub enum Credentials {
    /// A global API key and its user's email.
    Global { key: String, email: String },
    /// A scoped API token.
    Token(String),
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Credentials::Global { .. } => "Credentials::Global",
            Credentials::Token(_) => "Credentials::Token",
        })
    }
}

/// How long to wait before retry `count` (0-based) when the answer names
/// no delay: cloudflare-go's backoff, or a test's.
pub type Backoff<'a> = dyn Fn(u32) -> Duration + Sync + 'a;

/// Where and how the check runs.
pub struct Check<'a> {
    pub http: &'a Http,
    /// The API base (`CLOUDFLARE_BASE_URL` or `BASE_URL`); `base_url`'s
    /// slash rule is applied to it.
    pub base_url: &'a str,
    pub backoff: &'a Backoff<'a>,
}

/// The backoff, a closure, is left out.
impl std::fmt::Debug for Check<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Check")
            .field("http", self.http)
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

/// A base URL as cloudflare-go's `WithBaseURL` takes it: a path without a
/// trailing slash gets one, so its last segment survives resolution.
#[must_use]
pub fn base_url(text: &str) -> String {
    match reqwest::Url::parse(text) {
        Ok(url) if !url.path().is_empty() && !url.path().ends_with('/') => {
            let mut url = url;
            let path = format!("{}/", url.path());
            url.set_path(&path);
            url.to_string()
        }
        _ => text.to_string(),
    }
}

/// cloudflare-go's backoff (`retryDelay`): 0.5 s doubling per retry, at
/// most 8 s, less a random quarter at most, drawn with fastrand
/// (<https://docs.rs/fastrand/2.5.0/fastrand/fn.u64.html>), which the
/// facts half's temp names brought into sb-config.
#[must_use]
pub fn backoff(count: u32) -> Duration {
    let full = Duration::from_millis(500)
        .saturating_mul(2u32.saturating_pow(count))
        .min(Duration::from_secs(8));
    let quarter = u64::try_from((full / 4).as_nanos())
        .unwrap_or(u64::MAX)
        .max(1);
    // Below a quarter of `full`, so nothing saturates.
    full.saturating_sub(Duration::from_nanos(fastrand::u64(0..quarter)))
}

/// The longest a delay the server names is waited (maintainer,
/// 2026-11-05; a listed difference: cloudflare-go waits as long as it
/// says, so one answer could hold the step for an hour).
const MAX_NAMED_DELAY: Duration = Duration::from_secs(60);

/// cloudflare-go's `retryDelay`: the answer's `Retry-After-Ms`
/// (milliseconds), else its `Retry-After` (seconds, or an HTTP date read
/// with jiff's RFC 2822 parser, a little looser than Go's RFC 1123:
/// <https://docs.rs/jiff/0.2.38/jiff/fmt/rfc2822/struct.DateTimeParser.html#method.parse_timestamp>),
/// never negative and at most [`MAX_NAMED_DELAY`]; else
/// `backoff(count)`. As in Go, a value that doesn't parse as a float
/// (`1e400` overflows) is skipped, and one past Go's Duration (`inf`,
/// `1e300`) converts to a negative duration on amd64, hence 0.
pub(crate) fn retry_delay(
    headers: Option<&HeaderMap>,
    count: u32,
    backoff: &Backoff<'_>,
) -> Duration {
    let named = headers.and_then(|headers| {
        let value = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .filter(|v| !v.is_empty())
        };
        if let Some(ms) = value("retry-after-ms") {
            return go_float(ms).map(|ms| ms / 1000.0);
        }
        let after = value("retry-after")?;
        go_float(after).or_else(|| {
            let at = jiff::fmt::rfc2822::DateTimeParser::new()
                .parse_timestamp(after)
                .ok()?;
            Some(jiff::Timestamp::now().duration_until(at).as_secs_f64())
        })
    });
    match named {
        // Go's Duration holds about 292 years of nanoseconds; past it (or
        // NaN) the conversion goes negative on amd64. (Go's `time.Until`
        // saturates a far-future date instead, waiting ~292 years, which
        // the cap would make a minute: here a date ~292 years out doesn't
        // wait. A listed difference that no real server sends.)
        Some(seconds) if seconds * 1e9 < 9.223_372_036_854_775e18 => {
            Duration::from_secs_f64(seconds.max(0.0)).min(MAX_NAMED_DELAY)
        }
        Some(_) => Duration::ZERO,
        None => backoff(count),
    }
}

/// Go's `strconv.ParseFloat(_, 64)`: what it reads (`rules`' check, its
/// infinities included), as a number; `None` for an overflow like
/// `1e400`, which Go refuses.
fn go_float(text: &str) -> Option<f64> {
    crate::rules::go_parses_float(text)
        .then(|| text.replace('_', "").parse().ok())
        .flatten()
}

/// cloudflare-go's `shouldRetry`: a connection error (or a timeout);
/// `x-should-retry` when it says `true` or `false`; else 408, 409, 429
/// and 5xx.
fn should_retry(response: Option<&reqwest::Response>) -> bool {
    let Some(response) = response else {
        return true;
    };
    match response
        .headers()
        .get("x-should-retry")
        .and_then(|v| v.to_str().ok())
    {
        Some("true") => true,
        Some("false") => false,
        _ => {
            let status = response.status().as_u16();
            matches!(status, 408 | 409 | 429) || status >= 500
        }
    }
}

/// cloudflare-go's attempts: the first and two retries. The loop is ours:
/// reqwest's own retries have no backoff (its policy's future is
/// `Ready<()>`) and their classifier sees no headers, so neither
/// `x-should-retry` nor `Retry-After` could be honoured
/// (<https://docs.rs/reqwest/0.13.5/reqwest/retry/index.html>); no retry
/// middleware crate is in the tree (docs/platform-notes/cloudflare-go-retries.md).
const ATTEMPTS: u32 = 3;

/// cloudflare-go's auth headers, each marked sensitive (kept out of any
/// debug output): `authorization: Bearer` for a token, `X-Auth-Key` and
/// `X-Auth-Email` for a global key. A value no header can hold is refused
/// before any request (Go would send it to fail and retry).
fn auth_headers(credentials: &Credentials) -> Result<HeaderMap, String> {
    let value = |text: &str, key: &str| {
        let mut value = reqwest::header::HeaderValue::from_str(text)
            .map_err(|_| format!("cloudflare.{key} isn't a valid header value"))?;
        value.set_sensitive(true);
        Ok::<_, String>(value)
    };
    let mut headers = HeaderMap::new();
    match credentials {
        Credentials::Global { key, email } => {
            headers.insert("x-auth-key", value(key, "api")?);
            headers.insert("x-auth-email", value(email, "email")?);
        }
        Credentials::Token(token) => {
            headers.insert(
                reqwest::header::AUTHORIZATION,
                value(&format!("Bearer {token}"), "scoped_token")?,
            );
        }
    }
    Ok(headers)
}

impl Check<'_> {
    /// `GET` the path `segments` (relative to the base, as Go's URL
    /// resolution, <https://docs.rs/url/2.5.8/url/struct.Url.html#method.join>)
    /// with `credentials`, retried as cloudflare-go retries: the JSON
    /// answer, or the cause (the status and Cloudflare's messages, or the
    /// transport error), with the credentials taken out, escaped and cut.
    async fn get(
        &self,
        credentials: &Credentials,
        segments: &[&str],
        query: Option<(&str, &str)>,
    ) -> Result<serde_json::Value, String> {
        let (first, rest) = segments.split_first().unwrap_or((&"", &[]));
        let mut url = reqwest::Url::parse(&base_url(self.base_url))
            .and_then(|base| base.join(first))
            .map_err(|err| format!("invalid Cloudflare base URL: {err}"))?;
        if let Ok(mut path) = url.path_segments_mut() {
            path.extend(rest);
        }
        if let Some((name, value)) = query {
            url.query_pairs_mut().append_pair(name, value);
        }
        let secrets = |text: &str| match credentials {
            Credentials::Global { key, .. } => hide(text, key),
            Credentials::Token(token) => hide(text, token),
        };
        let headers = auth_headers(credentials)?;
        // Built at the first request (`Http::client`).
        let client = self.http.client()?;
        let mut retry = 0;
        let outcome = loop {
            let sent = client
                .get(url.clone())
                .headers(headers.clone())
                .send()
                .await;
            if retry + 1 >= ATTEMPTS || !should_retry(sent.as_ref().ok()) {
                break sent;
            }
            let delay = retry_delay(
                sent.as_ref().ok().map(reqwest::Response::headers),
                retry,
                self.backoff,
            );
            // The answer is dropped before the wait, as cloudflare-go closes it.
            drop(sent);
            // reqwest runs on tokio; its timer waits between attempts.
            tokio::time::sleep(delay).await;
            retry += 1;
        };
        let response = outcome.map_err(|err| remote_text(&secrets(&error_chain(&err))))?;
        let status = response.status();
        let bytes = body(response).await.map_err(|err| secrets(&err))?;
        let answer = serde_json::from_slice::<serde_json::Value>(&bytes);
        if status.is_success() {
            return answer.map_err(|_| "the answer isn't JSON".to_string());
        }
        let mut cause = format!(
            "HTTP {} {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or("")
        )
        .trim_end()
        .to_string();
        let messages: Vec<String> = answer
            .ok()
            .as_ref()
            .and_then(|answer| answer.get("errors"))
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|error| {
                let message = error.get("message")?.as_str()?;
                Some(
                    match error.get("code").and_then(serde_json::Value::as_i64) {
                        Some(code) => format!("{message} ({code})"),
                        None => message.to_string(),
                    },
                )
            })
            .collect();
        if !messages.is_empty() {
            cause.push_str(": ");
            cause.push_str(&messages.join("; "));
        }
        Err(remote_text(&secrets(&cause)))
    }
}

/// The check: nothing, or why not (sb-go's text, guidance included; the
/// domain and zone echoed escaped).
///
/// # Errors
///
/// The text shown when the check fails: the credentials or the zone
/// refused, the service unreachable or answering unexpectedly.
pub async fn check(
    check: &Check<'_>,
    credentials: &Credentials,
    domain: &str,
) -> Result<(), String> {
    let zone = root_domain(domain);
    // A global key's own check, and beside it the zone lookup (a listed
    // difference, the maintainer's of 2026-11-20: sb-go asks one after
    // the other), each answer taken in sb-go's order: the key's failure
    // first, the zone's after a key that passed. `try_join!` ends at the
    // key's failure, dropping the lookup, and never at the lookup's,
    // which it holds until the key's answer
    // (<https://docs.rs/tokio/1.54.1/tokio/macro.try_join.html>).
    let user = async {
        check
            .get(credentials, &["user"], None)
            .await
            .map_err(|cause| format!("cloudflare Global API key verification failed: {cause}"))
    };
    let looked_up = match (credentials, &zone) {
        (Credentials::Global { .. }, Ok(zone)) => {
            let zones = async {
                Ok::<_, String>(
                    check
                        .get(credentials, &["zones"], Some(("name", zone)))
                        .await,
                )
            };
            let (_user, zones) = tokio::try_join!(user, zones)?;
            Some(zones)
        }
        // A domain with no zone: the key's answer first, as sb-go.
        (Credentials::Global { .. }, Err(_)) => {
            user.await?;
            None
        }
        (Credentials::Token(_), _) => None,
    };
    let zone = zone?;
    let shown = escape_controls(&zone);
    let guidance = |operation: &str, setting: &str, cause: String| {
        guidance(credentials, operation, setting, &shown, &cause)
    };
    let zones = match looked_up {
        Some(zones) => zones,
        None => {
            check
                .get(credentials, &["zones"], Some(("name", &zone)))
                .await
        }
    }
    .map_err(|cause| guidance("Cloudflare zone lookup failed", "Zone", cause))?;
    let Some(id) = zones
        .get("result")
        .and_then(serde_json::Value::as_array)
        .and_then(|zones| zones.first())
        .and_then(|first| first.get("id"))
        .and_then(serde_json::Value::as_str)
    else {
        return Err(guidance(
            "Cloudflare zone lookup failed",
            "Zone",
            format!("zone {shown} was not returned for these credentials"),
        ));
    };
    let ssl = check
        .get(credentials, &["zones", id, "settings", "ssl"], None)
        .await
        .map_err(|cause| {
            guidance(
                "Cloudflare SSL/TLS mode lookup failed",
                "Zone Settings",
                cause,
            )
        })?;
    let mode = ssl
        .pointer("/result/value")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if matches!(mode, "flexible" | "off") {
        // sb-go's text, its lines run together as it writes them.
        return Err(format!(
            "incompatible SSL/TLS mode detected: '{mode}'\n  This SSL/TLS mode is not compatible with Saltbox.  Please update your Cloudflare settings:  1. Log in to your Cloudflare dashboard  2. Go to the SSL/TLS section for domain '{shown}'  3. Change the encryption mode to 'Full' or 'Full (strict)'  4. Save your changes"
        ));
    }
    Ok(())
}

/// The registrable domain of `name`, from psl's compiled-in list
/// (<https://docs.rs/psl/2.1.241/psl/fn.domain_str.html>), or `None`
/// when it has none.
///
/// sb's one call into psl: its lookup is generic, so each crate that
/// called `psl::domain_str` compiled its own copy of the list into the
/// binary (159 KB, this crate's and sb-motd's; ADR-026). Not inlined, so
/// link-time optimisation can't copy it into a caller again.
#[inline(never)]
#[must_use]
pub fn registrable_domain(name: &str) -> Option<&str> {
    psl::domain_str(name)
}

/// sb-go's `getRootDomain` with x/net's `EffectiveTLDPlusOne`: empty
/// labels and addresses refused in x/net's words, then the registrable
/// domain from psl, both sections
/// (<https://docs.rs/psl/2.1.241/psl/fn.domain_str.html>; why psl over
/// `publicsuffix`: the list is compiled in, with nothing to load).
/// Errors echo the domain escaped.
fn root_domain(domain: &str) -> Result<String, String> {
    if domain.is_empty() {
        return Err("empty domain name".to_string());
    }
    let refused = |reason: &str| {
        let shown = escape_controls(domain);
        format!("invalid domain format: {shown}: publicsuffix: {reason} \"{shown}\"")
    };
    if domain.starts_with('.') || domain.ends_with('.') || domain.contains("..") {
        return Err(refused("empty label in domain"));
    }
    if domain.parse::<std::net::IpAddr>().is_ok() {
        return Err(refused("cannot derive eTLD+1 for domain"));
    }
    registrable_domain(domain)
        .map(str::to_string)
        .ok_or_else(|| refused("cannot derive eTLD+1 for domain"))
}

/// sb-go's `cloudflarePermissionError`: the failure, then how to fix the
/// credential kind in use.
fn guidance(
    credentials: &Credentials,
    operation: &str,
    setting: &str,
    zone: &str,
    cause: &str,
) -> String {
    match credentials {
        Credentials::Global { .. } => format!(
            "{operation}: {cause}\n  Fix the Global API Key configuration:\n  1. Open User Profile → API Tokens in the Cloudflare dashboard.\n  2. In API Keys, find Global API Key and select View.\n  3. Confirm cloudflare.api in accounts.yml contains that key.\n  4. Confirm cloudflare.email contains the email for the same Cloudflare user.\n  5. Confirm that user has access to the {zone} zone.\n  6. Save accounts.yml and rerun the command.\n  Full instructions: https://docs.saltbox.dev/reference/domain/#preferred-global-api-key"
        ),
        Credentials::Token(_) => format!(
            "{operation}: {cause}\n  Fix the scoped token in the Cloudflare dashboard:\n  1. For an account-owned token, select the account and open Manage Account → Account API Tokens.\n     For a user-owned token, open My Profile → API Tokens.\n  2. Find the token used by Saltbox and select Edit.\n  3. Add or correct DNS and Zones → {setting} → Read.\n  4. Under Zone Resources, select Include → Specific zone → {zone}.\n  5. Save the token and rerun the command.\n  Full instructions: https://docs.saltbox.dev/reference/domain/#alternative-scoped-api-token"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::Proxy;
    use crate::testing::{Answer, Fake};

    /// `Debug` shows the client and the base URL; the backoff, a closure,
    /// is left out.
    #[test]
    fn a_checks_debug_leaves_the_backoff_out() {
        let http = Http::new(&Proxy::Off, Duration::from_secs(5)).expect("client");
        let backoff = |_: u32| Duration::ZERO;
        let check = Check {
            http: &http,
            base_url: "https://api.example.com/client/v4/",
            backoff: &backoff,
        };
        let shown = format!("{check:?}");
        assert!(shown.starts_with("Check { http: Http"), "{shown}");
        assert!(
            shown.ends_with(", base_url: \"https://api.example.com/client/v4/\", .. }"),
            "{shown}"
        );
    }

    const ZONES: &str = r#"{"success":true,"errors":[],"result":[{"id":"z1","name":"example.com","status":"active"}]}"#;
    const NO_ZONES: &str = r#"{"success":true,"errors":[],"result":[]}"#;
    const SSL_FULL: &str = r#"{"success":true,"errors":[],"result":{"id":"ssl","value":"full"}}"#;
    const DENIED: &str = r#"{"success":false,"errors":[{"code":9109,"message":"Unauthorized to access requested resource"}]}"#;

    fn token() -> Credentials {
        Credentials::Token("not-a-real-token".into())
    }

    fn global() -> Credentials {
        Credentials::Global {
            key: "not-a-real-key".into(),
            email: "seed@example.com".into(),
        }
    }

    fn no_wait(_: u32) -> Duration {
        Duration::ZERO
    }

    async fn run(fake: &Fake, credentials: &Credentials, domain: &str) -> Result<(), String> {
        let http = Http::new(&Proxy::Off, Duration::from_secs(5)).expect("client");
        let base = format!("{}/client/v4/", fake.url());
        check(
            &Check {
                http: &http,
                base_url: &base,
                backoff: &no_wait,
            },
            credentials,
            domain,
        )
        .await
    }

    fn healthy(fake: &Fake) {
        fake.script("/client/v4/zones", Answer::json(200, ZONES));
        fake.script(
            "/client/v4/zones/z1/settings/ssl",
            Answer::json(200, SSL_FULL),
        );
    }

    #[tokio::test]
    async fn a_scoped_token_with_a_good_zone_passes() {
        let fake = Fake::start().expect("fake");
        healthy(&fake);
        assert_eq!(run(&fake, &token(), "app.example.com").await, Ok(()));
        let seen = fake.seen();
        let targets: Vec<&str> = seen.iter().map(|s| s.target.as_str()).collect();
        assert_eq!(
            targets,
            [
                "/client/v4/zones?name=example.com",
                "/client/v4/zones/z1/settings/ssl"
            ]
        );
        assert_eq!(
            seen[0].header("authorization"),
            Some("Bearer not-a-real-token")
        );
        assert_eq!(seen[0].header("x-auth-key"), None);
    }

    /// A global key's `GET /user` goes with the zone lookup, the SSL
    /// lookup after both (the two at once arrive in either order).
    #[tokio::test]
    async fn a_global_key_checks_the_user_with_the_zone() {
        let fake = Fake::start().expect("fake");
        fake.script(
            "/client/v4/user",
            Answer::json(200, r#"{"success":true,"result":{"id":"u"}}"#),
        );
        healthy(&fake);
        assert_eq!(run(&fake, &global(), "example.com").await, Ok(()));
        let seen = fake.seen();
        let mut first: Vec<&str> = seen[..2].iter().map(|s| s.target.as_str()).collect();
        first.sort_unstable();
        assert_eq!(
            first,
            ["/client/v4/user", "/client/v4/zones?name=example.com"]
        );
        let user = seen
            .iter()
            .find(|s| s.target == "/client/v4/user")
            .expect("user");
        assert_eq!(user.header("x-auth-key"), Some("not-a-real-key"));
        assert_eq!(user.header("x-auth-email"), Some("seed@example.com"));
        assert_eq!(user.header("authorization"), None);
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[2].target, "/client/v4/zones/z1/settings/ssl");
    }

    /// A refused key is the failure, whatever the zone lookup sent beside
    /// it found; the SSL lookup isn't made.
    #[tokio::test]
    async fn a_refused_global_key_says_so() {
        let fake = Fake::start().expect("fake");
        fake.script("/client/v4/user", Answer::json(403, r#"{"success":false,"errors":[{"code":9103,"message":"Unknown X-Auth-Key or X-Auth-Email"}]}"#));
        healthy(&fake);
        assert_eq!(
            run(&fake, &global(), "example.com").await,
            Err("cloudflare Global API key verification failed: HTTP 403 Forbidden: Unknown X-Auth-Key or X-Auth-Email (9103)".into())
        );
        let seen = fake.seen();
        assert!(
            seen.iter().all(|s| s.target == "/client/v4/user"
                || s.target == "/client/v4/zones?name=example.com"),
            "{seen:?}"
        );
        assert_eq!(
            seen[..]
                .iter()
                .filter(|s| s.target == "/client/v4/user")
                .count(),
            1
        );
    }

    /// A global key with a domain that has no zone: the key's answer
    /// first, then the domain's refusal, and no zone lookup (sb-go's
    /// order).
    #[tokio::test]
    async fn a_global_key_with_no_zone_is_checked_first() {
        let fake = Fake::start().expect("fake");
        fake.script("/client/v4/user", Answer::json(403, DENIED));
        assert_eq!(
            run(&fake, &global(), "").await,
            Err("cloudflare Global API key verification failed: HTTP 403 Forbidden: Unauthorized to access requested resource (9109)".into())
        );
        let fake = Fake::start().expect("fake");
        fake.script("/client/v4/user", Answer::json(200, "{}"));
        healthy(&fake);
        assert_eq!(
            run(&fake, &global(), "").await,
            Err("empty domain name".into())
        );
        let seen = fake.seen();
        let targets: Vec<&str> = seen.iter().map(|s| s.target.as_str()).collect();
        assert_eq!(targets, ["/client/v4/user"]);
    }

    /// `check` with a global key while `/user`'s answer is held until the
    /// zone lookup has been sent too, then let go: the zone's answer comes
    /// first. Whether both were in flight at once, and the outcome.
    async fn user_answering_last(fake: &Fake) -> (bool, Result<(), String>) {
        let both_sent = async {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            let sent = loop {
                let seen = fake.seen();
                if seen.iter().any(|s| s.target == "/client/v4/user")
                    && seen
                        .iter()
                        .any(|s| s.target.starts_with("/client/v4/zones?"))
                {
                    break true;
                }
                if tokio::time::Instant::now() > deadline {
                    break false;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            };
            fake.release();
            sent
        };
        let credentials = global();
        tokio::join!(both_sent, run(fake, &credentials, "example.com"))
    }

    /// The key's failure is the one reported, though the zone lookup's
    /// failure came back first (sb-go's order: the key, then the zone).
    #[tokio::test]
    async fn a_refused_key_answering_last_is_still_the_failure() {
        let fake = Fake::start().expect("fake");
        fake.script(
            "/client/v4/user",
            Answer::json(403, r#"{"success":false,"errors":[{"code":9103,"message":"Unknown X-Auth-Key or X-Auth-Email"}]}"#).held(),
        );
        fake.script("/client/v4/zones", Answer::json(403, DENIED));
        let (both_sent, result) = user_answering_last(&fake).await;
        assert!(both_sent, "{:?}", fake.seen());
        assert_eq!(
            result,
            Err("cloudflare Global API key verification failed: HTTP 403 Forbidden: Unknown X-Auth-Key or X-Auth-Email (9103)".into())
        );
    }

    /// A key that passes, answering last: the zone lookup's failure is
    /// then the one reported, and with a zone that passes the SSL lookup
    /// follows only after the key's answer.
    #[tokio::test]
    async fn a_key_answering_last_lets_the_zone_answer_count() {
        let fake = Fake::start().expect("fake");
        fake.script("/client/v4/user", Answer::json(200, "{}").held());
        fake.script("/client/v4/zones", Answer::json(200, NO_ZONES));
        let (both_sent, result) = user_answering_last(&fake).await;
        assert!(both_sent, "{:?}", fake.seen());
        let err = result.expect_err("no zone");
        assert!(
            err.starts_with("Cloudflare zone lookup failed: zone example.com was not returned for these credentials\n  Fix the Global API Key configuration:"),
            "{err}"
        );
        let fake = Fake::start().expect("fake");
        fake.script("/client/v4/user", Answer::json(200, "{}").held());
        healthy(&fake);
        let (both_sent, result) = user_answering_last(&fake).await;
        assert!(both_sent, "{:?}", fake.seen());
        assert_eq!(result, Ok(()));
        let seen = fake.seen();
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[2].target, "/client/v4/zones/z1/settings/ssl");
    }

    #[tokio::test]
    async fn a_zone_lookup_failure_gives_the_tokens_guidance() {
        let fake = Fake::start().expect("fake");
        fake.script("/client/v4/zones", Answer::json(403, DENIED));
        assert_eq!(
            run(&fake, &token(), "example.com").await,
            Err("Cloudflare zone lookup failed: HTTP 403 Forbidden: Unauthorized to access requested resource (9109)\n  Fix the scoped token in the Cloudflare dashboard:\n  1. For an account-owned token, select the account and open Manage Account → Account API Tokens.\n     For a user-owned token, open My Profile → API Tokens.\n  2. Find the token used by Saltbox and select Edit.\n  3. Add or correct DNS and Zones → Zone → Read.\n  4. Under Zone Resources, select Include → Specific zone → example.com.\n  5. Save the token and rerun the command.\n  Full instructions: https://docs.saltbox.dev/reference/domain/#alternative-scoped-api-token".into())
        );
    }

    #[tokio::test]
    async fn a_missing_zone_gives_the_global_keys_guidance() {
        let fake = Fake::start().expect("fake");
        fake.script("/client/v4/user", Answer::json(200, "{}"));
        fake.script("/client/v4/zones", Answer::json(200, NO_ZONES));
        assert_eq!(
            run(&fake, &global(), "example.com").await,
            Err("Cloudflare zone lookup failed: zone example.com was not returned for these credentials\n  Fix the Global API Key configuration:\n  1. Open User Profile → API Tokens in the Cloudflare dashboard.\n  2. In API Keys, find Global API Key and select View.\n  3. Confirm cloudflare.api in accounts.yml contains that key.\n  4. Confirm cloudflare.email contains the email for the same Cloudflare user.\n  5. Confirm that user has access to the example.com zone.\n  6. Save accounts.yml and rerun the command.\n  Full instructions: https://docs.saltbox.dev/reference/domain/#preferred-global-api-key".into())
        );
    }

    #[tokio::test]
    async fn an_ssl_lookup_failure_names_the_zone_settings() {
        let fake = Fake::start().expect("fake");
        fake.script("/client/v4/zones", Answer::json(200, ZONES));
        fake.script(
            "/client/v4/zones/z1/settings/ssl",
            Answer::json(403, DENIED),
        );
        let err = run(&fake, &token(), "example.com")
            .await
            .expect_err("fails");
        assert!(
            err.starts_with(
                "Cloudflare SSL/TLS mode lookup failed: HTTP 403 Forbidden: Unauthorized"
            ),
            "{err}"
        );
        assert!(
            err.contains("  3. Add or correct DNS and Zones → Zone Settings → Read.\n"),
            "{err}"
        );
    }

    /// Flexible or off isn't compatible with Saltbox (sb-go's text, run
    /// together as sb-go writes it).
    #[tokio::test]
    async fn flexible_or_off_ssl_is_refused() {
        for mode in ["flexible", "off"] {
            let fake = Fake::start().expect("fake");
            fake.script("/client/v4/zones", Answer::json(200, ZONES));
            fake.script(
                "/client/v4/zones/z1/settings/ssl",
                Answer::json(
                    200,
                    format!(r#"{{"success":true,"result":{{"id":"ssl","value":"{mode}"}}}}"#),
                ),
            );
            assert_eq!(
                run(&fake, &token(), "example.com").await,
                Err(format!(
                    "incompatible SSL/TLS mode detected: '{mode}'\n  This SSL/TLS mode is not compatible with Saltbox.  Please update your Cloudflare settings:  1. Log in to your Cloudflare dashboard  2. Go to the SSL/TLS section for domain 'example.com'  3. Change the encryption mode to 'Full' or 'Full (strict)'  4. Save your changes"
                )),
                "{mode}"
            );
        }
    }

    /// The zone is the registrable domain, the private section included.
    #[tokio::test]
    async fn the_zone_is_the_registrable_domain() {
        for (domain, zone) in [
            ("a.b.example.co.uk", "example.co.uk"),
            ("me.github.io", "me.github.io"),
        ] {
            let fake = Fake::start().expect("fake");
            healthy(&fake);
            let _ = run(&fake, &token(), domain).await;
            assert_eq!(
                fake.seen()[0].target,
                format!("/client/v4/zones?name={zone}"),
                "{domain}"
            );
        }
    }

    /// A 429's named delay reaches the retry: `Retry-After-Ms` is waited,
    /// and the backoff isn't asked.
    #[tokio::test]
    async fn a_named_delay_is_taken_from_the_answer() {
        let fake = Fake::start().expect("fake");
        fake.script(
            "/client/v4/zones",
            Answer::json(429, "{}").header("retry-after-ms", "1"),
        );
        healthy(&fake);
        let asked = std::sync::atomic::AtomicUsize::new(0);
        let backoff = |_: u32| {
            asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Duration::ZERO
        };
        let http = Http::new(&Proxy::Off, Duration::from_secs(5)).expect("client");
        let base = format!("{}/client/v4/", fake.url());
        let check = Check {
            http: &http,
            base_url: &base,
            backoff: &backoff,
        };
        assert_eq!(super::check(&check, &token(), "example.com").await, Ok(()));
        assert_eq!(fake.seen().len(), 3, "the retry, then the SSL setting");
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    /// cloudflare-go's retries: two more attempts on 408, 409, 429 and 5xx.
    #[tokio::test]
    async fn retries_take_two_more_attempts() {
        let fake = Fake::start().expect("fake");
        fake.script("/client/v4/zones", Answer::json(503, "{}"));
        fake.script("/client/v4/zones", Answer::json(429, "{}"));
        fake.script("/client/v4/zones", Answer::json(200, ZONES));
        fake.script(
            "/client/v4/zones/z1/settings/ssl",
            Answer::json(200, SSL_FULL),
        );
        assert_eq!(run(&fake, &token(), "example.com").await, Ok(()));
        assert_eq!(fake.seen().len(), 4);
        let fake = Fake::start().expect("fake");
        fake.script("/client/v4/zones", Answer::json(500, "{}"));
        let err = run(&fake, &token(), "example.com")
            .await
            .expect_err("fails");
        assert!(
            err.starts_with("Cloudflare zone lookup failed: HTTP 500 Internal Server Error\n"),
            "{err}"
        );
        assert_eq!(fake.seen().len(), 3);
        for status in [408, 409] {
            let fake = Fake::start().expect("fake");
            fake.script("/client/v4/zones", Answer::json(status, "{}"));
            let _ = run(&fake, &token(), "example.com").await;
            assert_eq!(fake.seen().len(), 3, "{status}");
        }
    }

    #[tokio::test]
    async fn other_failures_are_not_retried() {
        let fake = Fake::start().expect("fake");
        fake.script("/client/v4/zones", Answer::json(404, "{}"));
        let _ = run(&fake, &token(), "example.com").await;
        assert_eq!(fake.seen().len(), 1);
    }

    /// `x-should-retry` wins over the status, either way.
    #[tokio::test]
    async fn x_should_retry_decides() {
        let fake = Fake::start().expect("fake");
        fake.script(
            "/client/v4/zones",
            Answer::json(503, "{}").header("x-should-retry", "false"),
        );
        let _ = run(&fake, &token(), "example.com").await;
        assert_eq!(fake.seen().len(), 1);
        let fake = Fake::start().expect("fake");
        fake.script(
            "/client/v4/zones",
            Answer::json(400, "{}").header("x-should-retry", "true"),
        );
        let _ = run(&fake, &token(), "example.com").await;
        assert_eq!(fake.seen().len(), 3);
    }

    /// A timed-out attempt is a connection error: retried, then the
    /// guidance follows the transport error. The retries are counted on
    /// the client's side, by the backoff it asks before each one: the
    /// fake counts an attempt only once its request reaches the handler,
    /// which a loaded host can delay past the attempt's timeout.
    #[tokio::test]
    async fn a_hung_lookup_times_out_after_its_retries() {
        let counts = std::sync::Mutex::new(Vec::new());
        let record = |count: u32| {
            counts.lock().expect("lock").push(count);
            Duration::ZERO
        };
        let fake = Fake::start().expect("fake");
        fake.script("/client/v4/zones", Answer::json(200, ZONES).held());
        let http = Http::new(&Proxy::Off, Duration::from_millis(200)).expect("client");
        let base = format!("{}/client/v4/", fake.url());
        let err = check(
            &Check {
                http: &http,
                base_url: &base,
                backoff: &record,
            },
            &token(),
            "example.com",
        )
        .await
        .expect_err("fails");
        assert!(err.starts_with("Cloudflare zone lookup failed: "), "{err}");
        assert!(err.contains("timed out"), "{err}");
        assert!(err.contains("\n  Fix the scoped token"), "{err}");
        assert_eq!(*counts.lock().expect("lock"), [0, 1]);
    }

    /// The key and token never appear in a result, even when echoed.
    #[tokio::test]
    async fn credentials_are_never_shown() {
        let fake = Fake::start().expect("fake");
        fake.script(
            "/client/v4/user",
            Answer::json(
                403,
                r#"{"errors":[{"code":1,"message":"bad key not-a-real-key"}]}"#,
            ),
        );
        let err = run(&fake, &global(), "example.com")
            .await
            .expect_err("fails");
        assert!(!err.contains("not-a-real-key"), "{err}");
        assert!(err.contains("[REDACTED]"), "{err}");
        let fake = Fake::start().expect("fake");
        fake.script(
            "/client/v4/zones",
            Answer::json(
                403,
                r#"{"errors":[{"code":1,"message":"token not-a-real-token"}]}"#,
            ),
        );
        let err = run(&fake, &token(), "example.com")
            .await
            .expect_err("fails");
        assert!(!err.contains("not-a-real-token"), "{err}");
        assert_eq!(format!("{:?}", token()), "Credentials::Token");
    }

    #[test]
    fn the_retry_delay_follows_cloudflare_go() {
        let headers = |pairs: &[(&str, &str)]| {
            let mut map = reqwest::header::HeaderMap::new();
            for (name, value) in pairs {
                map.insert(
                    reqwest::header::HeaderName::from_bytes(name.as_bytes()).expect("name"),
                    reqwest::header::HeaderValue::from_str(value).expect("value"),
                );
            }
            map
        };
        let fixed = |_: u32| Duration::from_secs(99);
        assert_eq!(
            retry_delay(Some(&headers(&[("retry-after-ms", "250")])), 0, &fixed),
            Duration::from_millis(250)
        );
        assert_eq!(
            retry_delay(Some(&headers(&[("retry-after", "1.5")])), 0, &fixed),
            Duration::from_millis(1500)
        );
        assert_eq!(
            retry_delay(
                Some(&headers(&[("retry-after-ms", "100"), ("retry-after", "9")])),
                0,
                &fixed
            ),
            Duration::from_millis(100)
        );
        assert_eq!(
            retry_delay(Some(&headers(&[("retry-after", "-3")])), 0, &fixed),
            Duration::ZERO
        );
        assert_eq!(
            retry_delay(
                Some(&headers(&[(
                    "retry-after",
                    "Mon, 01 Jan 2001 00:00:00 GMT"
                )])),
                0,
                &fixed
            ),
            Duration::ZERO
        );
        assert_eq!(
            retry_delay(Some(&headers(&[("retry-after", "soon")])), 1, &fixed),
            Duration::from_secs(99)
        );
        assert_eq!(retry_delay(None, 1, &fixed), Duration::from_secs(99));
    }

    #[test]
    fn the_backoff_doubles_to_eight_seconds_less_jitter() {
        for (count, full) in [(0, 500), (1, 1000), (2, 2000), (4, 8000), (10, 8000)] {
            let delay = backoff(count);
            let full = Duration::from_millis(full);
            assert!(delay <= full && delay > full * 3 / 4, "{count}: {delay:?}");
        }
    }

    /// cloudflare-go's `WithBaseURL` adds a trailing slash to a path that
    /// lacks one, so its last segment stays.
    #[test]
    fn the_base_url_gets_a_trailing_slash() {
        assert_eq!(
            base_url("http://127.0.0.1:9/client/v4"),
            "http://127.0.0.1:9/client/v4/"
        );
        assert_eq!(
            base_url("http://127.0.0.1:9/client/v4/"),
            "http://127.0.0.1:9/client/v4/"
        );
        assert_eq!(base_url("http://127.0.0.1:9"), "http://127.0.0.1:9");
        assert_eq!(base_url("not a url"), "not a url");
    }

    #[tokio::test]
    async fn a_base_url_without_its_slash_still_reaches_v4() {
        let fake = Fake::start().expect("fake");
        healthy(&fake);
        let http = Http::new(&Proxy::Off, Duration::from_secs(5)).expect("client");
        let base = base_url(&format!("{}/client/v4", fake.url()));
        let result = check(
            &Check {
                http: &http,
                base_url: &base,
                backoff: &no_wait,
            },
            &token(),
            "example.com",
        )
        .await;
        assert_eq!(result, Ok(()));
    }

    /// x/net's `EffectiveTLDPlusOne`: empty labels and addresses refused
    /// before any request, in its words.
    #[tokio::test]
    async fn a_domain_without_a_zone_is_refused() {
        let fake = Fake::start().expect("fake");
        assert_eq!(
            run(&fake, &token(), "").await,
            Err("empty domain name".into())
        );
        for (domain, reason) in [
            ("co.uk", "cannot derive eTLD+1 for domain \"co.uk\""),
            ("192.0.2.4", "cannot derive eTLD+1 for domain \"192.0.2.4\""),
            ("::1", "cannot derive eTLD+1 for domain \"::1\""),
            ("example.com.", "empty label in domain \"example.com.\""),
            (".example.com", "empty label in domain \".example.com\""),
            ("a..example.com", "empty label in domain \"a..example.com\""),
        ] {
            assert_eq!(
                run(&fake, &token(), domain).await,
                Err(format!(
                    "invalid domain format: {domain}: publicsuffix: {reason}"
                )),
                "{domain}"
            );
        }
        assert_eq!(fake.seen(), [] as [crate::testing::Seen; 0]);
    }

    /// The domain and zone are echoed escaped (the file is group-writable).
    #[tokio::test]
    async fn the_domain_is_echoed_escaped() {
        let fake = Fake::start().expect("fake");
        fake.script("/client/v4/zones", Answer::json(200, NO_ZONES));
        let err = run(&fake, &token(), "a.exa\u{1b}[31mmple.com")
            .await
            .expect_err("fails");
        assert!(!err.contains('\u{1b}'), "{err}");
        assert!(
            err.contains("zone exa\\u{1b}[31mmple.com was not returned"),
            "{err}"
        );
        let err = run(&fake, &token(), "\u{7}").await.expect_err("fails");
        assert!(!err.contains('\u{7}'), "{err}");
    }

    /// A success that isn't JSON doesn't pass: cloudflare-go fails to
    /// decode it.
    #[tokio::test]
    async fn a_success_that_is_not_json_fails() {
        let fake = Fake::start().expect("fake");
        fake.script("/client/v4/zones", Answer::json(200, ZONES));
        fake.script(
            "/client/v4/zones/z1/settings/ssl",
            Answer::new(200, "<html>ok</html>"),
        );
        let err = run(&fake, &token(), "example.com")
            .await
            .expect_err("fails");
        assert!(
            err.starts_with("Cloudflare SSL/TLS mode lookup failed: the answer isn't JSON\n"),
            "{err}"
        );
    }

    /// The backoff hears each retry's count (0, then 1); a named delay
    /// replaces it.
    #[tokio::test]
    async fn the_backoff_is_asked_per_retry() {
        let counts = std::sync::Mutex::new(Vec::new());
        let record = |count: u32| {
            counts.lock().expect("lock").push(count);
            Duration::ZERO
        };
        let fake = Fake::start().expect("fake");
        fake.script("/client/v4/zones", Answer::json(503, "{}"));
        let http = Http::new(&Proxy::Off, Duration::from_secs(5)).expect("client");
        let base = format!("{}/client/v4/", fake.url());
        let _ = check(
            &Check {
                http: &http,
                base_url: &base,
                backoff: &record,
            },
            &token(),
            "example.com",
        )
        .await;
        assert_eq!(*counts.lock().expect("lock"), [0, 1]);
        // A minute's backoff isn't waited for when the answer names 1 ms.
        let fake = Fake::start().expect("fake");
        fake.script(
            "/client/v4/zones",
            Answer::json(503, "{}").header("retry-after-ms", "1"),
        );
        let base = format!("{}/client/v4/", fake.url());
        let minute = |_: u32| Duration::from_secs(60);
        let started = std::time::Instant::now();
        let _ = check(
            &Check {
                http: &http,
                base_url: &base,
                backoff: &minute,
            },
            &token(),
            "example.com",
        )
        .await;
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(fake.seen().len(), 3);
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

    /// A refused connection is a transport error, followed by the guidance.
    #[tokio::test]
    async fn a_refused_connection_gives_the_guidance() {
        let (_held, addr) = refused();
        let base = format!("http://{addr}/client/v4/");
        let http = Http::new(&Proxy::Off, Duration::from_secs(5)).expect("client");
        let err = check(
            &Check {
                http: &http,
                base_url: &base,
                backoff: &no_wait,
            },
            &token(),
            "example.com",
        )
        .await
        .expect_err("fails");
        assert!(err.starts_with("Cloudflare zone lookup failed: "), "{err}");
        assert!(err.contains("\n  Fix the scoped token"), "{err}");
    }

    /// Go's float handling of a named delay: `inf` (and a value past
    /// Go's Duration) is 0 on amd64; `1e400` doesn't parse, so the backoff
    /// decides; a future date waits until then.
    #[test]
    fn extreme_named_delays_follow_go() {
        let headers = |value: &str| {
            let mut map = HeaderMap::new();
            map.insert(
                "retry-after",
                reqwest::header::HeaderValue::from_str(value).expect("value"),
            );
            map
        };
        let fixed = |_: u32| Duration::from_secs(7);
        assert_eq!(
            retry_delay(Some(&headers("inf")), 0, &fixed),
            Duration::ZERO
        );
        assert_eq!(
            retry_delay(Some(&headers("1e300")), 0, &fixed),
            Duration::ZERO
        );
        assert_eq!(
            retry_delay(Some(&headers("1e400")), 0, &fixed),
            Duration::from_secs(7)
        );
        let later = jiff::Timestamp::now() + jiff::SignedDuration::from_secs(30);
        let date = jiff::fmt::rfc2822::DateTimePrinter::new()
            .timestamp_to_rfc9110_string(&later)
            .expect("date");
        let delay = retry_delay(Some(&headers(&date)), 0, &fixed);
        assert!(
            delay > Duration::from_secs(25) && delay <= Duration::from_secs(30),
            "{delay:?}"
        );
    }

    /// A delay the server names waits at most a minute (maintainer,
    /// 2026-11-05; config spec, differences): cloudflare-go waits as long
    /// as it says, so one answer could hold the step for an hour.
    #[test]
    fn a_named_delay_waits_at_most_a_minute() {
        let headers = |name: &str, value: &str| {
            let mut map = HeaderMap::new();
            map.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).expect("name"),
                reqwest::header::HeaderValue::from_str(value).expect("value"),
            );
            map
        };
        let fixed = |_: u32| Duration::from_secs(7);
        let minute = Duration::from_secs(60);
        for (name, value) in [("retry-after", "3600"), ("retry-after-ms", "3600000")] {
            assert_eq!(
                retry_delay(Some(&headers(name, value)), 0, &fixed),
                minute,
                "{name}"
            );
        }
        let later = jiff::Timestamp::now() + jiff::SignedDuration::from_hours(2);
        let date = jiff::fmt::rfc2822::DateTimePrinter::new()
            .timestamp_to_rfc9110_string(&later)
            .expect("date");
        assert_eq!(
            retry_delay(Some(&headers("retry-after", &date)), 0, &fixed),
            minute
        );
        // Under the cap, the named delay itself; at and just past it, the
        // cap.
        for (value, waited) in [("59", 59.0), ("60", 60.0), ("60.5", 60.0)] {
            assert_eq!(
                retry_delay(Some(&headers("retry-after", value)), 0, &fixed),
                Duration::from_secs_f64(waited),
                "{value}"
            );
        }
    }

    /// A status with no reason phrase still reads cleanly.
    #[tokio::test]
    async fn an_unknown_status_reads_cleanly() {
        let fake = Fake::start().expect("fake");
        fake.script("/client/v4/zones", Answer::json(499, DENIED));
        let err = run(&fake, &token(), "example.com")
            .await
            .expect_err("fails");
        assert!(
            err.starts_with("Cloudflare zone lookup failed: HTTP 499: Unauthorized"),
            "{err}"
        );
    }

    /// A credential no header can hold is refused before any request.
    #[tokio::test]
    async fn a_credential_no_header_can_hold_is_refused() {
        let fake = Fake::start().expect("fake");
        let bad = Credentials::Global {
            key: "k\nx".into(),
            email: "e@example.com".into(),
        };
        assert_eq!(
            run(&fake, &bad, "example.com").await,
            Err("cloudflare Global API key verification failed: cloudflare.api isn't a valid header value".into())
        );
        let bad = Credentials::Token("t\u{7f}".into());
        let err = run(&fake, &bad, "example.com").await.expect_err("fails");
        assert!(
            err.starts_with(
                "Cloudflare zone lookup failed: cloudflare.scoped_token isn't a valid header value"
            ),
            "{err}"
        );
        assert_eq!(fake.seen(), [] as [crate::testing::Seen; 0]);
        assert_eq!(format!("{:?}", global()), "Credentials::Global");
    }

    /// A value past Go's Duration (about 292 years) doesn't wait; one just
    /// under it waits the cap (a minute).
    #[test]
    fn a_delay_past_go_s_duration_does_not_wait() {
        let mut map = HeaderMap::new();
        map.insert(
            "retry-after",
            reqwest::header::HeaderValue::from_static("1e10"),
        );
        assert_eq!(
            retry_delay(Some(&map), 0, &|_: u32| Duration::from_secs(7)),
            Duration::ZERO
        );
        map.insert(
            "retry-after",
            reqwest::header::HeaderValue::from_static("9e9"),
        );
        assert_eq!(
            retry_delay(Some(&map), 0, &|_: u32| Duration::from_secs(7)),
            MAX_NAMED_DELAY
        );
    }

    /// An error without a code is shown as its message.
    #[tokio::test]
    async fn an_error_without_a_code_shows_its_message() {
        let fake = Fake::start().expect("fake");
        fake.script(
            "/client/v4/zones",
            Answer::json(400, r#"{"errors":[{"message":"bad request"}]}"#),
        );
        let err = run(&fake, &token(), "example.com")
            .await
            .expect_err("fails");
        assert!(
            err.starts_with("Cloudflare zone lookup failed: HTTP 400 Bad Request: bad request\n"),
            "{err}"
        );
    }
}
