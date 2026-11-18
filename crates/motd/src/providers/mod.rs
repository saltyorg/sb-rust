//! The HTTP providers (motd spec, Sections, "HTTP instances"; decisions
//! 10, 11 and 12): thin reqwest clients for the media servers and
//! downloaders motd.yml configures, as sb-go's `motd/*.go` providers ask
//! them.
//!
//! - Every request goes through the run's one `sb_config::http::Http`
//!   (reqwest with rustls, no redirects followed, bodies read through
//!   sb-config's 1 MiB cap; the torrent clients' data whole up to their
//!   own caps), built once on the blocking pool, begun as the sections
//!   start when one of them will ask (`Shared::start`), else at the
//!   first provider's request.
//! - Each instance is its own tokio task, so a panic in one is that
//!   instance's error line
//!   (<https://docs.rs/tokio/1.54.1/tokio/task/struct.JoinError.html#method.try_into_panic>).
//! - Each request's limit is the instance's `timeout`, or sb-go's 1 s,
//!   cut to what is left of the section's 30 s (sb-go's
//!   `timeoutFromContext`), set with reqwest's per-request timeout
//!   (<https://docs.rs/reqwest/0.13.5/reqwest/struct.RequestBuilder.html#method.timeout>).
//! - Every error text is built from reqwest's error without its URL
//!   (<https://docs.rs/reqwest/0.13.5/reqwest/struct.Error.html#method.without_url>),
//!   then every configured credential is hidden, then it is cut to one
//!   line safe to draw (term-ui's `DisplayText`).
//! - The run log names each instance and its base URL without user
//!   name, password or query (`sb_config::http::loggable_url`), with its
//!   outcome and time, never the request URL.
//!
//! The JSON bodies are read with serde; each field Go would leave at its
//! zero value (absent or `null`) is an `Option` read as its default.

pub mod arr;
pub mod emby;
pub mod jellyfin;
pub mod nzbget;
pub mod plex;
pub mod qbittorrent;
pub mod rtorrent;
pub mod sabnzbd;
pub mod xmlrpc;

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::HeaderValue;
use sb_config::http::{Http, Proxy};
use tokio::sync::OnceCell;
use tokio::task::JoinSet;
use tokio::time::Instant;
use tracing::Instrument as _;

use crate::config::{AppSection, Instance};
use crate::redact::Secrets;
use crate::text::{capped, first_line, one_line};
use crate::{Out, Paint};

/// How a run's client is built: [`build`], or the tests' stand-ins.
type Build = fn(&Proxy) -> Result<Http, String>;

/// The run's client with `proxy`, built now: reqwest's builder loads the
/// platform's root certificates (sb-config's `Http::client`), several
/// milliseconds of work that [`Shared::start`] moves off the runtime.
fn build(proxy: &Proxy) -> Result<Http, String> {
    let http = Http::new(proxy, DEFAULT_TIMEOUT)?;
    http.client()?;
    Ok(http)
}

/// sb-go's limit for one request when the instance sets none.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(1);

/// What every provider of a run shares: the client, made once on the
/// blocking pool, the section's deadline and the credentials to hide.
/// Its debug output names neither the proxy (its URL may carry a login)
/// nor a credential.
pub struct Shared {
    proxy: Proxy,
    http: OnceCell<Result<Http, String>>,
    build: Build,
    /// The end of the sections' 30 s.
    pub deadline: Instant,
    pub secrets: Secrets,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared")
            .field("deadline", &self.deadline)
            .field("secrets", &self.secrets)
            .finish_non_exhaustive()
    }
}

impl Shared {
    /// A client to be built with `proxy` when [`Shared::start`] begins it
    /// or a provider first asks for it.
    #[must_use]
    pub fn new(proxy: Proxy, deadline: Instant, secrets: Secrets) -> Arc<Self> {
        Self::with_build(proxy, deadline, secrets, build)
    }

    /// [`Shared::new`] with its client built by `build`.
    fn with_build(proxy: Proxy, deadline: Instant, secrets: Secrets, build: Build) -> Arc<Self> {
        Arc::new(Self {
            proxy,
            http: OnceCell::new(),
            build,
            deadline,
            secrets,
        })
    }

    /// Begins building the client, for a run whose providers will ask for
    /// it, so the build runs while the other sections are collected.
    /// Must be called inside the tokio runtime. The build is a task of its
    /// own (`tokio::spawn`), filling the cell the providers wait on
    /// (<https://docs.rs/tokio/1.54.1/tokio/sync/struct.OnceCell.html#method.get_or_init>:
    /// a caller arriving during the build waits for it), so an instance
    /// cancelled at its section's cap doesn't cancel the build for the
    /// others.
    pub fn start(self: &Arc<Self>) {
        let shared = Arc::clone(self);
        tokio::spawn(
            async move {
                shared.http().await;
            }
            .in_current_span(),
        );
    }

    /// The run's client, or why it couldn't be built: built once, on the
    /// blocking pool (`sb_cli_core::spawn::blocking`), so the runtime's
    /// one thread runs the other sections meanwhile. A build that panics
    /// is kept as the error `panic: <message>`
    /// (`JoinError::try_into_panic`), the line each instance showed when
    /// the build ran in its own task, now from one build. The other join
    /// error, a cancelled build, needs the runtime shut down while a
    /// section still waits, which `sb motd`'s run doesn't do.
    async fn http(&self) -> &Result<Http, String> {
        self.http
            .get_or_init(|| async {
                let proxy = self.proxy.clone();
                let build = self.build;
                match sb_cli_core::spawn::blocking(move || build(&proxy)).await {
                    Ok(http) => http,
                    Err(err) => match err.try_into_panic() {
                        Ok(payload) => Err(format!(
                            "panic: {}",
                            crate::run::panic_message(Some(payload.as_ref()))
                        )),
                        Err(err) => Err(format!("failed to build client: {err}")),
                    },
                }
            })
            .await
    }

    /// The run's client, built at the first request, or why it couldn't
    /// be.
    async fn client(&self) -> Result<&reqwest::Client, String> {
        self.http().await.as_ref().map_err(Clone::clone)?.client()
    }

    /// One request's limit for `instance`: its `timeout` or sb-go's 1 s,
    /// cut to what is left before the deadline (1 ms once it has passed,
    /// as sb-go's `timeoutFromContext`).
    #[must_use]
    pub fn timeout(&self, instance: &Instance, fallback: Duration) -> Duration {
        self.within(instance.timeout.unwrap_or(fallback))
    }

    /// `wanted`, cut to what is left before the deadline (1 ms once it has
    /// passed): the torrent clients' fixed 1 s check, which no `timeout`
    /// changes.
    #[must_use]
    pub fn within(&self, wanted: Duration) -> Duration {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Duration::from_millis(1);
        }
        wanted.min(left)
    }

    /// `text` as an error line shows it: credentials hidden, its first
    /// line made safe to draw, hidden again (dropping a control character
    /// could join a credential's parts), cut to sb-config's limit for
    /// remote text; or sb-go's `instance error` for nothing left.
    #[must_use]
    pub fn error_text(&self, text: &str) -> String {
        let line = self.remote(&one_line(self.secrets.hide(text).as_bytes()));
        if line.is_empty() {
            "Error: instance error".to_string()
        } else {
            format!("Error: {line}")
        }
    }
}

impl Shared {
    /// `text` with `instance`'s Basic login hidden too: its user and
    /// password as reqwest's `basic_auth` encodes them for the
    /// `Authorization` header
    /// (<https://docs.rs/reqwest/0.13.5/reqwest/struct.RequestBuilder.html#method.basic_auth>),
    /// which a server echoing the request's headers would show; taken from
    /// a request built and not sent. Why not the base64 crate: not one of
    /// sb's dependencies, for one encoding reqwest already makes.
    #[must_use]
    pub fn hide_login(&self, instance: &Instance, text: &str) -> String {
        let (Some(user), Some(password)) = (instance.user.as_deref(), instance.password.as_deref())
        else {
            return text.to_string();
        };
        let Some(Ok(http)) = self.http.get() else {
            return text.to_string();
        };
        // Built already: the cell is filled only by `Shared::http`, whose
        // build makes the client first.
        let Ok(client) = http.client() else {
            return text.to_string();
        };
        let token = client
            .get("http://login.invalid/")
            .basic_auth(user, Some(password))
            .build()
            .ok()
            .and_then(|request| {
                request
                    .headers()
                    .get(reqwest::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.strip_prefix("Basic "))
                    .map(ToString::to_string)
            });
        match token {
            Some(token) => sb_config::secret::hide(text, &token),
            None => text.to_string(),
        }
    }

    /// Remote text, already one line safe to draw, as motd shows it:
    /// credentials hidden, then cut to sb-config's `MAX_TEXT` characters
    /// (a long serde message or `SABnzbd` field stays one short line).
    #[must_use]
    pub fn remote(&self, line: &str) -> String {
        capped(&self.secrets.hide(line))
    }
}

/// `value`, a credential, as a header value that never shows in debug
/// output (`HeaderValue::set_sensitive`,
/// <https://docs.rs/http/1.5.0/http/header/struct.HeaderValue.html#method.set_sensitive>,
/// as reqwest's `basic_auth` marks its own).
///
/// # Errors
///
/// sb-go's request error when the value can't be a header.
pub fn secret_header(value: &str) -> Result<HeaderValue, String> {
    let mut header =
        HeaderValue::from_str(value).map_err(|err| format!("failed to create request: {err}"))?;
    header.set_sensitive(true);
    Ok(header)
}

/// One instance's answer: its summary, or the error text to show in the
/// error colour.
pub type Answer = Result<Summary, String>;

/// An instance's summary: its line (already painted) and its `--json`
/// value (motd spec, `--json`).
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub text: String,
    pub value: serde_json::Value,
}

/// Each enabled instance of `section` that `usable` accepts, asked by
/// `ask` as its own task at once, each answer with the instance's name
/// and logged when it ends. The name is sb-go's: the instance's own, or
/// `fallback` when it is empty, and on an error line trimmed first
/// (`providerInstanceName`). None when motd.yml has no such section or
/// it is off. The tasks are a tokio `JoinSet`, so a section ended at its
/// cap aborts them all on drop
/// (<https://docs.rs/tokio/1.54.1/tokio/task/struct.JoinSet.html>).
pub async fn ask_each<F, Fut>(
    app: &'static str,
    section: Option<&AppSection>,
    fallback: &'static str,
    shared: &Arc<Shared>,
    usable: fn(&Instance) -> bool,
    ask: F,
) -> Vec<(String, Answer)>
where
    F: Fn(Arc<Shared>, Instance) -> Fut,
    Fut: Future<Output = Answer> + Send + 'static,
{
    let Some(section) = section.filter(|s| s.enabled) else {
        return Vec::new();
    };
    let mut tasks = JoinSet::new();
    let mut started = Vec::new();
    let mut index_of = HashMap::new();
    for instance in section
        .instances
        .iter()
        .filter(|instance| instance.enabled && usable(instance))
    {
        let names = (
            shown_name(&instance.name, fallback),
            instance_name(&instance.name, fallback),
        );
        let url = loggable(&instance.url, &shared.secrets);
        let handle = tasks.spawn(ask(Arc::clone(shared), instance.clone()));
        index_of.insert(handle.id(), started.len());
        started.push((names, url, Instant::now()));
    }
    let mut answers: Vec<Option<Answer>> = vec![None; started.len()];
    while let Some(joined) = tasks.join_next_with_id().await {
        let (id, answer) = match joined {
            Ok((id, answer)) => (id, answer),
            Err(err) => {
                let id = err.id();
                let payload = err.try_into_panic().ok();
                let message = crate::run::panic_message(payload.as_deref());
                (id, Err(shared.error_text(&format!("panic: {message}"))))
            }
        };
        if let Some(&index) = index_of.get(&id) {
            let ((shown, trimmed), url, start) = &started[index];
            let name = if answer.is_ok() { shown } else { trimmed };
            log(app, name, url, &answer, *start);
            answers[index] = Some(answer);
        }
    }
    started
        .into_iter()
        .zip(answers)
        .filter_map(|(((shown, trimmed), _, _), answer)| {
            let answer = answer?;
            let name = if answer.is_ok() { shown } else { trimmed };
            Some((name, answer))
        })
        .collect()
}

/// An instance with a URL and a `token` (Plex, Emby, Jellyfin; sb-go
/// skips the others).
fn has_token(instance: &Instance) -> bool {
    !instance.url.is_empty() && is_set(instance.token.as_ref())
}

/// An instance with a URL and an `apikey` (`SABnzbd`, the Arr apps).
fn has_apikey(instance: &Instance) -> bool {
    !instance.url.is_empty() && is_set(instance.apikey.as_ref())
}

/// A credential that is there and not empty.
fn is_set(value: Option<&String>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

/// The run log's record for one instance (criterion 12): its name, its
/// base URL without credentials or query, the outcome, and the error
/// (already hidden).
fn log(app: &str, name: &str, url: &str, answer: &Answer, start: Instant) {
    let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    match answer {
        Ok(_) => tracing::info!(
            app,
            instance = name,
            url,
            outcome = "shown",
            elapsed_ms,
            "motd instance"
        ),
        Err(error) => tracing::warn!(
            app,
            instance = name,
            url,
            outcome = "error",
            elapsed_ms,
            error = error.as_str(),
            "motd instance"
        ),
    }
}

/// `url` as the run log shows it: no user name, password or query, any
/// credential hidden.
fn loggable(url: &str, secrets: &Secrets) -> String {
    let base = url.split(['?', '#']).next().unwrap_or_default();
    secrets.hide(&sb_config::http::loggable_url(base))
}

/// The name of an instance that answered, as sb-go's providers set it:
/// its own, or `fallback` when it is empty; its first line made safe to
/// draw, not trimmed.
fn shown_name(name: &str, fallback: &str) -> String {
    if name.is_empty() {
        fallback.to_string()
    } else {
        first_line(name.as_bytes())
    }
}

/// sb-go's `providerInstanceName` (an error's name): trimmed, or
/// `fallback`; made safe to draw.
fn instance_name(name: &str, fallback: &str) -> String {
    let name = one_line(name.as_bytes());
    if name.is_empty() {
        fallback.to_string()
    } else {
        name
    }
}

/// The section's value (sb-go's `format*Output`): sorted by name; one
/// instance's summary alone; several, each on its own line after its
/// `Name:`, padded to the longest name (by bytes, as Go's `len`), in the
/// app name colour. Errors in the error colour. Empty for none.
///
/// Its `--json` form is each instance's value or error, in the same
/// order; none is `empty`.
#[must_use]
pub fn listing(answers: Vec<(String, Answer)>, paint: &Paint) -> Out {
    lines(answers, paint, false)
}

/// [`listing`] with every line named, one instance too (the Arr queues:
/// sb-go's `formatDetailedQueueOutput`).
#[must_use]
pub fn named_listing(answers: Vec<(String, Answer)>, paint: &Paint) -> Out {
    lines(answers, paint, true)
}

fn lines(mut answers: Vec<(String, Answer)>, paint: &Paint, always_named: bool) -> Out {
    answers.sort_by(|a, b| a.0.cmp(&b.0));
    let shown = |answer: &Answer| match answer {
        Ok(summary) => summary.text.clone(),
        Err(error) => paint.error(error),
    };
    let text = if !always_named && let [(_, answer)] = answers.as_slice() {
        shown(answer)
    } else {
        let width = answers
            .iter()
            .map(|(name, _)| name.len())
            .max()
            .unwrap_or(0);
        answers
            .iter()
            .map(|(name, answer)| {
                let pad = " ".repeat(width - name.len() + 1);
                format!(
                    "{}{}",
                    paint.app_name(&format!("{name}:{pad}")),
                    shown(answer)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    if answers.is_empty() {
        return Out::from(text);
    }
    let instances = answers
        .into_iter()
        .map(|(name, answer)| (name, answer.map(|summary| summary.value)))
        .collect();
    Out {
        text,
        json: crate::Json::Instances(instances),
    }
}

/// How a provider names its API in sb-go's errors.
#[derive(Clone, Copy, Debug)]
pub struct Texts {
    /// `failed to connect to <app>: …`.
    pub app: &'static str,
    /// `<api> API returned status code N`.
    pub api: &'static str,
    /// `failed to read <read>: …`.
    pub read: &'static str,
}

/// `request` sent with `timeout`, its body when the status is 200, or
/// sb-go's text for what went wrong.
///
/// # Errors
///
/// sb-go's texts: the connection, a status other than 200, or the body.
pub async fn send(
    shared: &Shared,
    request: impl FnOnce(&reqwest::Client) -> reqwest::RequestBuilder,
    timeout: Duration,
    texts: Texts,
) -> Result<Vec<u8>, String> {
    let client = shared.client().await?;
    let response = request(client)
        .timeout(timeout)
        .send()
        .await
        .map_err(|err| {
            format!(
                "failed to connect to {}: {}",
                texts.app,
                sb_config::http::error_chain(&err.without_url())
            )
        })?;
    let status = response.status();
    if status != reqwest::StatusCode::OK {
        return Err(format!(
            "{} API returned status code {}",
            texts.api,
            status.as_u16()
        ));
    }
    sb_config::http::body(response)
        .await
        .map_err(|err| format!("failed to read {}: {err}", texts.read))
}

/// `count` in the value colour.
fn count(paint: &Paint, count: impl std::fmt::Display) -> String {
    paint.value(&count.to_string())
}

/// sb-go's `N active stream(s) (a, b)` for the media servers: each kind
/// with a count above zero, in `kinds`' order.
fn streams(paint: &Paint, active: usize, kinds: &[(usize, &str)]) -> String {
    let noun = if active == 1 { "stream" } else { "streams" };
    let mut summary = format!("{} active {noun}", count(paint, active));
    let parts: Vec<String> = kinds
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, kind)| format!("{} {kind}", count(paint, n)))
        .collect();
    if !parts.is_empty() {
        summary = format!("{summary} ({})", parts.join(", "));
    }
    summary
}

/// The media servers' `--json` value: `active`, then each kind's count
/// under its name in snake case (`direct_play`), as [`streams`] reads
/// them.
fn streams_value(active: usize, kinds: &[(usize, &str)]) -> serde_json::Value {
    let mut value = serde_json::Map::new();
    value.insert("active".to_owned(), active.into());
    for (n, kind) in kinds {
        value.insert(kind.replace(' ', "_"), (*n).into());
    }
    serde_json::Value::Object(value)
}

/// The downloaders' `--json` value: `{items, left, size, speed, paused}`,
/// the sizes and speed as the line shows them, `speed` `null` when
/// paused.
fn queue_value(items: i64, left: &str, size: &str, speed: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "items": items,
        "left": left,
        "size": size,
        "speed": speed,
        "paused": speed.is_none(),
    })
}

/// sb-go's downloader summary: `Queue is empty`, or `Paused, …` or
/// `Downloading at <speed>, …` before `N item(s) in queue (<left>
/// remaining / <size> total)`.
fn queue(paint: &Paint, items: i64, left: &str, size: &str, speed: Option<&str>) -> String {
    if items == 0 {
        return "Queue is empty".to_string();
    }
    let noun = if items == 1 { "item" } else { "items" };
    let summary = format!(
        "{} {noun} in queue ({} remaining / {} total)",
        count(paint, items),
        paint.value(left),
        paint.value(size)
    );
    match speed {
        None => format!("Paused, {summary}"),
        Some(speed) => format!("Downloading at {}, {summary}", paint.value(speed)),
    }
}

/// The largest value sb-go's `int64` holds, as an `f64` (2^63).
const MAX_INT64: f64 = 9_223_372_036_854_775_808.0;

/// sb-go's `formatBytes(int64(count))`: whole bytes (towards zero, at
/// most `int64`'s range), `B` below 1024, else powers of 1024 with one
/// decimal. The unit is found on whole numbers, as Go's integer
/// division does.
pub(crate) fn bytes(count: f64) -> String {
    const UNIT: f64 = 1024.0;
    // `+ 0.0` turns a negative zero into zero, as Go's int has no sign.
    let whole = count.trunc().min(MAX_INT64) + 0.0;
    if whole < UNIT {
        return format!("{whole} B");
    }
    let (mut div, mut exp) = (UNIT, 0);
    let mut n = (whole / UNIT).floor();
    while n >= UNIT {
        div *= UNIT;
        exp += 1;
        n = (n / UNIT).floor();
    }
    let unit = ["K", "M", "G", "T", "P", "E"].get(exp).unwrap_or(&"E");
    format!("{:.1} {unit}B", whole / div)
}

/// A rate in bytes a second as `bytes` takes it: none below zero, at
/// most 4 GiB/s (a real rate is far below; std has no lossless `i64` to
/// `f64` conversion, and `as` would need an exception).
pub(crate) fn rate_bytes(rate: i64) -> f64 {
    f64::from(u32::try_from(rate.max(0)).unwrap_or(u32::MAX))
}

/// The torrent clients' summary (sb-go's `formatQbittorrentSummary` and
/// `formatRtorrentSummary`): `No torrents present`, or `↓<down> ↑<up> |
/// N Downloading | N Seeding`, then ` | N Stopped` and ` | N Error` in the
/// error colour when there are any; counts and speeds in the value
/// colour.
pub(crate) fn torrents_summary(paint: &Paint, counts: [usize; 4], down: &str, up: &str) -> String {
    let [downloading, seeding, stopped, errored] = counts;
    if counts.iter().all(|n| *n == 0) {
        return "No torrents present".to_string();
    }
    let mut parts = vec![
        format!("↓{} ↑{}", paint.value(down), paint.value(up)),
        format!("{} Downloading", count(paint, downloading)),
        format!("{} Seeding", count(paint, seeding)),
    ];
    if stopped > 0 {
        parts.push(paint.error(&format!("{stopped} Stopped")));
    }
    if errored > 0 {
        parts.push(paint.error(&format!("{errored} Error")));
    }
    parts.join(" | ")
}

/// The torrent clients' `--json` value: `{down, up, downloading,
/// seeding, stopped, errored}`, the speeds as the line shows them.
pub(crate) fn torrents_value(counts: [usize; 4], down: &str, up: &str) -> serde_json::Value {
    let [downloading, seeding, stopped, errored] = counts;
    serde_json::json!({
        "down": down,
        "up": up,
        "downloading": downloading,
        "seeding": seeding,
        "stopped": stopped,
        "errored": errored,
    })
}

#[cfg(test)]
pub(crate) mod tests;
