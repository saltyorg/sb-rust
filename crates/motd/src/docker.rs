//! Docker and Traefik (motd spec, Sections; decisions 13 and 15;
//! Differences 10 and 11), where sb-go ran `systemctl is-active docker`,
//! `docker ps` and `curl` (`motd/info.go:916-1047`, `:1187-1307`).
//!
//! - Both sections read one listing of sb-docker's (`connect::list`,
//!   bollard), made once per run however many of the two are shown
//!   (tokio's `OnceCell`,
//!   <https://docs.rs/tokio/1.54.1/tokio/sync/struct.OnceCell.html>).
//!   Each container's state and role are `ps::status`'s, the module's one
//!   classification (decision 13).
//! - The listing is limited to sb-go's 5 s for `docker ps`, a child
//!   under its `ExecCommand`.
//! - Traefik's routers come from reqwest, `GET <base>/api/http/routers`
//!   with sb-go's `curl --connect-timeout 3` inside the same 5 s, the
//!   body read through sb-config's 1 MiB cap. Why not
//!   `sb_config::http::Http`: it sets one limit for the whole request and
//!   none for the connection, where this call has curl's connect limit
//!   as well
//!   (<https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html#method.connect_timeout>).
//!   Its proxy is chosen as sb-config's is, a parameter, so the tests
//!   never meet a developer's proxy.
//! - Names, statuses and router errors are text from outside sb: term-ui's
//!   `DisplayText` (decision 24).

use std::sync::Arc;
use std::time::Duration;

use sb_config::http::Proxy;
use sb_docker::connect::Listing;
use serde_json::{Value, json};
use tokio::sync::OnceCell;

use crate::text::one_line;
use crate::{Out, Paint};

/// Traefik's API where Saltbox serves it, the base sb-go's `curl` asked.
const TRAEFIK_BASE: &str = "http://traefik:8080";

/// sb-go's line for a Traefik container that isn't up.
const TRAEFIK_DOWN: &str = "Traefik container is not running";

/// sb-go's `curl --connect-timeout 3` (decision 11).
const TRAEFIK_CONNECT: Duration = Duration::from_secs(3);

/// sb-go's limit for each child (`motd/utils.go`, `ExecCommand`; decision
/// 11), which bounded its `docker ps` and its `curl`: here the listing's
/// and Traefik's call's.
const LIMIT: Duration = crate::packages::CHILD_TIMEOUT;

/// Where the two sections read: Docker's address settings, Traefik's
/// base URL and the proxy, from the environment (the tests point them at
/// fakes, with no proxy).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// `DOCKER_HOST`, `DOCKER_TLS_VERIFY` and `DOCKER_CERT_PATH`, read as
    /// `sb docker ps` reads them (sb-docker's `connect::address`).
    pub docker_host: Option<String>,
    pub tls_verify: Option<String>,
    pub cert_path: Option<String>,
    /// Traefik's base URL: `SB_MOTD_TRAEFIK_URL`, or Saltbox's
    /// `http://traefik:8080`.
    pub traefik_base: String,
    /// The proxy for Traefik's API: reqwest's own reading of the proxy
    /// variables in a run, as sb-go's `curl` read them (sb-config's
    /// `Proxy::System`, with its listed differences), none in the tests.
    pub proxy: Proxy,
}

impl Default for Settings {
    /// No Docker settings, Saltbox's Traefik, no proxy.
    fn default() -> Self {
        Self {
            docker_host: None,
            tls_verify: None,
            cert_path: None,
            traefik_base: TRAEFIK_BASE.to_string(),
            proxy: Proxy::Off,
        }
    }
}

impl Settings {
    /// The settings this run's environment gives.
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    /// The settings the variables `var` reads give: an unset or empty
    /// `SB_MOTD_TRAEFIK_URL` is Saltbox's base.
    fn from_vars(var: impl Fn(&str) -> Option<String>) -> Self {
        Self {
            docker_host: var("DOCKER_HOST"),
            tls_verify: var("DOCKER_TLS_VERIFY"),
            cert_path: var("DOCKER_CERT_PATH"),
            traefik_base: var("SB_MOTD_TRAEFIK_URL")
                .filter(|url| !url.is_empty())
                .unwrap_or_else(|| TRAEFIK_BASE.to_string()),
            proxy: Proxy::System,
        }
    }
}

/// Why there is no listing.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Down {
    /// The daemon couldn't be reached (or its address is one sb refuses).
    Unreachable(String),
    /// Its socket answered with a permission error: the socket, named
    /// (Difference 33). Never read as the daemon being down.
    Denied(String),
    /// It answered, but its container list failed.
    NoList,
}

/// The run's one listing, shared by the Docker and Traefik sections, and
/// the one client Traefik's three requests share (building one loads the
/// platform's roots, so one is built, at the first request).
#[derive(Debug)]
pub struct Containers {
    settings: Settings,
    listing: OnceCell<Result<Listing, Down>>,
    client: OnceCell<Result<reqwest::Client, String>>,
    /// The listing's limit: [`LIMIT`], shorter in the tests.
    limit: Duration,
}

impl Containers {
    /// A listing to be made from `settings` when a section first asks.
    #[must_use]
    pub fn new(settings: Settings) -> Arc<Self> {
        Self::within(settings, LIMIT)
    }

    /// The same with the listing limited to `limit`.
    fn within(settings: Settings, limit: Duration) -> Arc<Self> {
        Arc::new(Self {
            settings,
            listing: OnceCell::new(),
            client: OnceCell::new(),
            limit,
        })
    }

    /// Traefik's client, built by the first request: curl's connect limit
    /// inside the 5 s, no redirect followed, the settings' proxy.
    async fn client(&self) -> &Result<reqwest::Client, String> {
        self.client
            .get_or_init(|| async { traefik_client(&self.settings.proxy) })
            .await
    }

    /// Whether the `traefik` row is up, or the line Traefik's section
    /// shows instead.
    async fn traefik_up(&self, paint: &Paint) -> Result<(), String> {
        let listing = match self.listing().await {
            Ok(listing) => listing,
            Err(Down::Unreachable(_)) => return Err("Docker service is not running".into()),
            Err(Down::Denied(resource)) => return Err(denied(resource, paint)),
            Err(Down::NoList) => return Err(TRAEFIK_DOWN.into()),
        };
        // `docker ps` without `-a` lists paused and restarting containers too.
        let up = listing.rows.iter().any(|row| {
            row.name == "traefik"
                && matches!(row.state.as_str(), "running" | "paused" | "restarting")
        });
        if up { Ok(()) } else { Err(TRAEFIK_DOWN.into()) }
    }

    /// `path` under Traefik's base URL.
    fn traefik_url(&self, path: &str) -> String {
        format!("{}{path}", self.settings.traefik_base.trim_end_matches('/'))
    }

    /// The listing, made by the first caller.
    async fn listing(&self) -> &Result<Listing, Down> {
        self.listing
            .get_or_init(|| async {
                // A daemon that takes the connection and doesn't answer
                // (docker.socket while dockerd starts) would hold the
                // login for the section's 30 s.
                tokio::time::timeout(self.limit, list(&self.settings))
                    .await
                    .unwrap_or_else(|_| {
                        tracing::warn!(limit_ms = %self.limit.as_millis(), "docker's listing timed out");
                        Err(Down::NoList)
                    })
            })
            .await
    }
}

/// Why connecting to `address` failed with `err`: a permission error
/// (EACCES or EPERM) names the socket rather than reading as a daemon
/// that is down (Difference 33). bollard keeps the OS error in the chain
/// for a refused connect; its "socket not found" check (`Path::exists`)
/// doesn't, so for a Unix socket `probe` (a `stat` of the path, which
/// can't block) tells a directory on its path that refuses search apart.
/// Anything else stays unreachable, with its cause.
fn connect_failure(
    address: &str,
    cause: String,
    err: &(dyn std::error::Error + 'static),
    probe: impl FnOnce(&std::path::Path) -> std::io::Result<()>,
) -> Down {
    let Some(path) = address.strip_prefix("unix://") else {
        return Down::Unreachable(cause);
    };
    let typed = std::iter::successors(Some(err), |e| e.source())
        .filter_map(|e| e.downcast_ref::<std::io::Error>())
        .any(crate::text::is_denial);
    if typed || probe(std::path::Path::new(path)).is_err_and(|e| crate::text::is_denial(&e)) {
        Down::Denied(format!("Docker's socket ({path})"))
    } else {
        Down::Unreachable(cause)
    }
}

/// The Docker and Traefik line for a socket that answered with a
/// permission error (Difference 33).
fn denied(resource: &str, paint: &Paint) -> String {
    paint.error(&format!(
        "Error: permission denied on {}",
        one_line(resource.as_bytes())
    ))
}

/// sb-docker's listing of every container, or why there is none (the
/// cause in the run log).
async fn list(settings: &Settings) -> Result<Listing, Down> {
    let address = sb_docker::connect::address(
        settings.docker_host.as_deref(),
        settings.tls_verify.as_deref(),
        settings.cert_path.as_deref(),
    )
    .map_err(|err| {
        tracing::warn!(error = %format!("{err:#}"), "docker's address refused");
        Down::Unreachable(format!("{err:#}"))
    })?;
    let daemon = sb_docker::connect::connect(&address).await.map_err(|err| {
        tracing::warn!(error = %format!("{err:#}"), "docker not reached");
        connect_failure(&address, format!("{err:#}"), err.as_ref(), |path| {
            std::fs::metadata(path).map(drop)
        })
    })?;
    sb_docker::connect::list(&daemon).await.map_err(|err| {
        tracing::warn!(error = %format!("{err:#}"), "docker containers not listed");
        Down::NoList
    })
}

/// Docker: `N containers (R running)`, or with containers needing
/// attention `N containers (R running, A need attention)` and one
/// `name: status` line for each, sorted by name (sb-go's text; which
/// ones need attention is Difference 10's: the Problem and Warn roles).
/// A container whose inspect failed needs attention too. An unreachable
/// daemon reads `Error: <cause>` in the error colour.
///
/// Its `--json` value (motd spec, `--json`) is `{total, running,
/// need_attention: [{name, status}]}`; no daemon, or no list, is an error.
pub async fn docker(containers: &Containers, paint: &Paint) -> Out {
    let listing = match containers.listing().await {
        Ok(listing) => listing,
        Err(Down::Unreachable(cause)) => {
            return Out::error(paint.error(&format!("Error: {}", one_line(cause.as_bytes()))));
        }
        Err(Down::Denied(resource)) => return Out::error(denied(resource, paint)),
        Err(Down::NoList) => {
            return Out::error("Docker is running but container list is unavailable".into());
        }
    };
    let total = listing.rows.len() + listing.failures.len();
    if total == 0 {
        return Out::value(
            "Docker is running but no containers found".into(),
            json!({ "total": 0, "running": 0, "need_attention": [] }),
        );
    }
    let running = listing
        .rows
        .iter()
        .filter(|row| row.state == "running")
        .count();
    let mut attention: Vec<(String, String)> = listing
        .rows
        .iter()
        .filter(|row| {
            matches!(
                row.role,
                sb_cli_core::theme::Role::Problem | sb_cli_core::theme::Role::Warn
            )
        })
        .map(|row| {
            (
                one_line(row.name.as_bytes()),
                one_line(row.status.as_bytes()),
            )
        })
        .collect();
    attention.extend(listing.failures.iter().map(|failure| {
        (
            one_line(failure.short_id.as_bytes()),
            "inspect failed".to_string(),
        )
    }));
    attention.sort();
    let value = json!({
        "total": total,
        "running": running,
        "need_attention": attention
            .iter()
            .map(|(name, status)| json!({ "name": name, "status": status }))
            .collect::<Vec<_>>(),
    });
    let summary = summary(
        total,
        running,
        attention.len(),
        "containers",
        "running",
        paint,
    );
    Out::value(lines(summary, &attention, paint), value)
}

/// sb-go's summary line: the total and the good count in the value
/// colour, or the total and the attention count in the warning colour
/// when any need attention.
fn summary(
    total: usize,
    good: usize,
    attention: usize,
    things: &str,
    state: &str,
    paint: &Paint,
) -> String {
    if attention == 0 {
        format!(
            "{} {things} ({} {state})",
            paint.value(&total.to_string()),
            paint.value(&good.to_string())
        )
    } else {
        format!(
            "{} {things} ({} {state}, {} need attention)",
            paint.warning(&total.to_string()),
            paint.value(&good.to_string()),
            paint.warning(&attention.to_string())
        )
    }
}

/// The summary, then one `name: problem` line per entry, the problem in
/// the error colour.
fn lines(summary: String, entries: &[(String, String)], paint: &Paint) -> String {
    let mut out = summary;
    for (name, problem) in entries {
        out.push('\n');
        out.push_str(name);
        out.push_str(": ");
        out.push_str(&paint.error(problem));
    }
    out
}

/// One router of Traefik's answer: Go's `json.Unmarshal` leaves a field
/// it doesn't find, or finds `null`, at its zero value.
#[derive(Debug, serde::Deserialize)]
struct Router {
    name: Option<String>,
    status: Option<String>,
    error: Option<Value>,
}

/// Traefik: with its container running, its routers counted, sb-go's
/// texts for each way there is no count (Difference 11 for an answer
/// that isn't JSON).
///
/// Its `--json` value (motd spec, `--json`) is `{running, total, active,
/// need_attention: [{name, problem}], text}`: `running` false, and the
/// counts `null`, when Traefik's container isn't up; no daemon, no API
/// and an answer that isn't the routers are errors.
pub async fn traefik(containers: &Containers, paint: &Paint) -> Out {
    if let Err(line) = containers.traefik_up(paint).await {
        if line == TRAEFIK_DOWN {
            let value = json!({
                "running": false,
                "total": null,
                "active": null,
                "need_attention": [],
                "text": line,
            });
            return Out::value(line, value);
        }
        return Out::error(line);
    }
    let url = containers.traefik_url("/api/http/routers");
    let (status, body) = match fetch(containers, &url).await {
        Ok(answer) => answer,
        Err(err) => {
            tracing::warn!(error = %err, "traefik's api not reached");
            return Out::error("Traefik container is running but API is not accessible".into());
        }
    };
    routers(status, &body, paint)
}

/// The answer's status and body (at most sb-config's 1 MiB), or why
/// there is none, on the run's Traefik client.
async fn fetch(containers: &Containers, url: &str) -> Result<(u16, Vec<u8>), String> {
    let client = containers.client().await.as_ref().map_err(Clone::clone)?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|err| sb_config::http::error_chain(&err.without_url()))?;
    let status = response.status().as_u16();
    Ok((status, sb_config::http::body(response).await?))
}

/// Traefik's client: sb-go's `curl --connect-timeout 3` inside the 5 s,
/// no redirect followed (curl follows none without -L), `proxy` as
/// sb-config's `Http::new` applies it.
fn traefik_client(proxy: &Proxy) -> Result<reqwest::Client, String> {
    let builder = reqwest::Client::builder()
        .connect_timeout(TRAEFIK_CONNECT)
        .timeout(LIMIT)
        // curl follows no redirect without -L.
        .redirect(reqwest::redirect::Policy::none());
    // As sb-config's `Http::new` applies it
    // (<https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html#method.no_proxy>,
    // <https://docs.rs/reqwest/0.13.5/reqwest/struct.Proxy.html#method.all>).
    let builder = match proxy {
        Proxy::System => builder,
        Proxy::Off => builder.no_proxy(),
        Proxy::Via(via) => builder
            .proxy(reqwest::Proxy::all(via).map_err(|err| sb_config::http::error_chain(&err))?),
    };
    builder
        .build()
        .map_err(|err| sb_config::http::error_chain(&err))
}

/// A day's seconds, for `--json`'s `days`.
const DAY_SECONDS: i64 = 86_400;

/// Traefik's warning: a certificate with under 30 days left
/// (`getCertificateStatus`, `pkg/api/certificate.go` at v3.7.16).
const SOON: jiff::SignedDuration = jiff::SignedDuration::from_hours(30 * 24);

/// The overview's `certificates` (`pkg/api/handler_overview.go`'s
/// `section`): every certificate served, those under 30 days from
/// expiring, and those expired.
#[derive(serde::Deserialize)]
struct Counted {
    total: u64,
    warnings: u64,
    errors: u64,
}

/// The part of Traefik's overview the line reads; `certificates` is left
/// out before v3.7.0, and when Traefik has no TLS manager.
#[derive(serde::Deserialize)]
struct Overview {
    certificates: Option<Counted>,
}

/// Certificates (motd spec, Certificates; decision 27): the certificates
/// Traefik serves, from its API, never from `acme.json`.
///
/// Its `--json` value (motd spec, `--json`) is `{total, valid,
/// expiring_soon, expired, first_not_after, days}`: the first
/// certificate's `notAfter` and the whole days to it (below 0 once
/// passed), `null` when not known; hidden is `empty`.
pub async fn certificates(containers: &Containers, paint: &Paint) -> Out {
    certificates_at(containers, paint, jiff::Timestamp::now()).await
}

/// [`certificates`] against `now`: hidden when Traefik isn't up or its API
/// isn't reached (the Traefik line says which), else the line from the
/// overview's counts and the first certificate to expire, both asked at
/// once.
async fn certificates_at(containers: &Containers, paint: &Paint, now: jiff::Timestamp) -> Out {
    let hidden = || Out::value(String::new(), json!({}));
    if containers.traefik_up(paint).await.is_err() {
        return hidden();
    }
    let overview = containers.traefik_url("/api/overview");
    let first = containers.traefik_url("/api/certificates?sortBy=validUntil&per_page=1");
    let (overview, first) = tokio::join!(fetch(containers, &overview), fetch(containers, &first));
    let (status, body) = match overview {
        Ok(answer) => answer,
        Err(err) => {
            tracing::warn!(error = %err, "traefik's overview not reached");
            return hidden();
        }
    };
    let first = match first {
        Ok((200, body)) => first_expiry(&body),
        Ok((status, _)) => Err(format!("traefik's certificates not listed (HTTP {status})")),
        Err(err) => Err(format!("traefik's certificates not reached: {err}")),
    };
    let (line, counted) =
        certificates_line(status, &body, first.as_ref().ok().copied(), now, paint);
    // Only where the line counts certificates: a Traefik before v3.7.0
    // answers the list with a 404 at every login, and shows no line.
    if counted && let Err(why) = &first {
        tracing::warn!(why, "the first certificate's date not shown");
    }
    line
}

/// The `notAfter` of the one certificate listed, or why there is none.
fn first_expiry(body: &[u8]) -> Result<jiff::Timestamp, String> {
    let listed: Value = serde_json::from_slice(body)
        .map_err(|_| "traefik's certificates answer isn't JSON".to_string())?;
    let not_after = listed
        .get(0)
        .and_then(|first| first.get("notAfter"))
        .and_then(Value::as_str)
        .ok_or_else(|| "traefik's certificates answer has no date".to_string())?;
    not_after
        .parse::<jiff::Timestamp>()
        .map_err(|err| format!("traefik's certificate date not read: {err}"))
}

/// The line for the overview's answer and the first `notAfter`, and
/// whether it counts certificates (a run-log warning about the date is
/// only worth it then).
fn certificates_line(
    status: u16,
    body: &[u8],
    first: Option<jiff::Timestamp>,
    now: jiff::Timestamp,
    paint: &Paint,
) -> (Out, bool) {
    let Ok(overview) = serde_json::from_slice::<Overview>(body) else {
        let line = paint.error(&format!(
            "Error: unexpected answer from Traefik (HTTP {status})"
        ));
        return (Out::error(line), false);
    };
    let Some(counted) = overview.certificates else {
        return (Out::value(String::new(), json!({})), false);
    };
    if counted.total == 0 {
        let value = json!({
            "total": 0,
            "valid": 0,
            "expiring_soon": 0,
            "expired": 0,
            "first_not_after": null,
            "days": null,
        });
        return (
            Out::value("Traefik is serving no certificates".into(), value),
            false,
        );
    }
    let valid = counted
        .total
        .saturating_sub(counted.warnings)
        .saturating_sub(counted.errors);
    let total = counted.total.to_string();
    let total = if counted.errors > 0 {
        paint.error(&total)
    } else if counted.warnings > 0 {
        paint.warning(&total)
    } else {
        paint.value(&total)
    };
    let noun = if counted.total == 1 {
        "certificate"
    } else {
        "certificates"
    };
    // Each count only when it is above 0 (the maintainer, 2026-11-17);
    // with a total above 0, one of them is.
    let mut parts = Vec::new();
    if valid > 0 {
        parts.push(format!("{} valid", paint.value(&valid.to_string())));
    }
    if counted.warnings > 0 {
        parts.push(format!(
            "{} expiring soon",
            paint.warning(&counted.warnings.to_string())
        ));
    }
    if counted.errors > 0 {
        parts.push(format!(
            "{} expired",
            paint.error(&counted.errors.to_string())
        ));
    }
    let mut line = format!("{total} {noun} ({})", parts.join(", "));
    if let Some(at) = first {
        line.push_str(", ");
        line.push_str(&expiry(now.duration_until(at), paint));
    }
    let value = json!({
        "total": counted.total,
        "valid": valid,
        "expiring_soon": counted.warnings,
        "expired": counted.errors,
        "first_not_after": first.map(|at| at.to_string()),
        // Whole days, rounded down: below 0 from the moment it passes.
        "days": first.map(|at| now.duration_until(at).as_secs().div_euclid(DAY_SECONDS)),
    });
    (Out::value(line, value), true)
}

/// When the first certificate expires, `left` from now: whole 24-hour
/// days, rounded down (jiff's `SignedDuration`,
/// <https://docs.rs/jiff/0.2.38/jiff/struct.SignedDuration.html#method.as_hours>);
/// the number, or `today`, in its state's colour (value, warning under
/// Traefik's 30 days, error once passed).
fn expiry(left: jiff::SignedDuration, paint: &Paint) -> String {
    let days = left.as_hours().unsigned_abs() / 24;
    let passed = left.is_negative();
    let paint_state = |text: &str| {
        if passed {
            paint.error(text)
        } else if left < SOON {
            paint.warning(text)
        } else {
            paint.value(text)
        }
    };
    let unit = if days == 1 { "day" } else { "days" };
    match (passed, days) {
        (true, 0) => format!("earliest expired {}", paint_state("today")),
        (true, _) => format!(
            "earliest expired {} {unit} ago",
            paint_state(&days.to_string())
        ),
        (false, 0) => format!("next expires {}", paint_state("today")),
        (false, _) => format!("next expires in {} {unit}", paint_state(&days.to_string())),
    }
}

/// The routers line for Traefik's answer: each router with an error, or
/// disabled, needs attention (sb-go's `GetTraefikInfo`).
fn routers(status: u16, body: &[u8], paint: &Paint) -> Out {
    let none = || {
        let line = "Traefik is running with no routers configured".to_string();
        let value = json!({
            "running": true,
            "total": 0,
            "active": 0,
            "need_attention": [],
            "text": line,
        });
        Out::value(line, value)
    };
    let text = String::from_utf8_lossy(body);
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed == "[]" {
        return none();
    }
    let Ok(routers) = serde_json::from_str::<Vec<Router>>(trimmed) else {
        return Out::error(paint.error(&format!(
            "Error: unexpected answer from Traefik (HTTP {status})"
        )));
    };
    if routers.is_empty() {
        return none();
    }
    let mut attention = Vec::new();
    for router in &routers {
        let name = one_line(router.name.as_deref().unwrap_or_default().as_bytes());
        let error = router.error.as_ref().map_or_default(router_error);
        if !error.is_empty() {
            attention.push((name, one_line(error.as_bytes())));
        } else if router.status.as_deref() == Some("disabled") {
            attention.push((name, "router is disabled".to_string()));
        }
    }
    let healthy = routers.len() - attention.len();
    let summary = summary(
        routers.len(),
        healthy,
        attention.len(),
        "routers",
        "active",
        paint,
    );
    let line = lines(summary, &attention, paint);
    let value = json!({
        "running": true,
        "total": routers.len(),
        "active": healthy,
        "need_attention": attention
            .iter()
            .map(|(name, problem)| json!({ "name": name, "problem": problem }))
            .collect::<Vec<_>>(),
        "text": line,
    });
    Out::value(line, value)
}

/// A router's `error` as sb-go's `extractTraefikRouterError` reads it: a
/// string, the first non-empty string of a list, an object's `message`
/// or the object itself as JSON, or else the JSON text without quotes.
fn router_error(raw: &Value) -> String {
    match raw {
        Value::Null => String::new(),
        Value::String(text) => text.trim().to_string(),
        Value::Array(items) if items.iter().all(Value::is_string) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .find(|text| !text.is_empty())
            .unwrap_or_default()
            .to_string(),
        Value::Object(fields) => match fields.get("message") {
            Some(Value::String(message)) => message.trim().to_string(),
            _ => raw.to_string(),
        },
        other => other.to_string().trim_matches('"').to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sb_config::testing::{Answer, Fake};
    use sb_docker::testing::{FakeEngine, Options};
    use serde_json::json;

    fn painted() -> Paint {
        let theme = sb_cli_core::theme::Theme::resolve(
            &sb_cli_core::theme::Palette::default(),
            sb_cli_core::theme::Profile::TrueColor,
        );
        Paint::new(&theme, true)
    }

    /// A container's inspect as dockerd answers it, with `state`.
    fn container(n: u8, name: &str, state: &Value) -> Value {
        json!({
            "Id": format!("{n:02x}").repeat(32),
            "Name": format!("/{name}"),
            "State": state,
            "Config": { "Labels": {} },
            "NetworkSettings": { "Ports": {} },
        })
    }

    fn running(n: u8, name: &str) -> Value {
        container(
            n,
            name,
            &json!({ "Status": "running", "Running": true, "ExitCode": 0 }),
        )
    }

    /// Settings that reach `engine`, and Traefik at `traefik`.
    fn settings(engine: &FakeEngine, traefik: &str) -> Settings {
        Settings {
            docker_host: Some(engine.host()),
            traefik_base: traefik.to_string(),
            ..Settings::default()
        }
    }

    fn engine(inspects: Vec<Value>) -> FakeEngine {
        FakeEngine::start(Options {
            inspects,
            ..Options::default()
        })
        .unwrap()
    }

    /// A loopback URL nothing listens on, with the socket that holds its
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
    fn refused() -> (tokio::net::TcpSocket, String) {
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
        (socket, format!("http://{addr}"))
    }

    /// A daemon at a socket that isn't there.
    fn nowhere(dir: &tempfile::TempDir) -> Settings {
        Settings {
            docker_host: Some(format!("unix://{}", dir.path().join("none.sock").display())),
            traefik_base: "http://traefik.invalid".into(),
            ..Settings::default()
        }
    }

    #[test]
    fn the_settings_come_from_the_environment() {
        let vars = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, v)| (*v).to_string())
            }
        };
        let none = Settings::from_vars(vars(&[]));
        assert_eq!(none.traefik_base, "http://traefik:8080");
        assert_eq!(none.proxy, Proxy::System);
        assert_eq!(none.docker_host, None);
        let empty = Settings::from_vars(vars(&[("SB_MOTD_TRAEFIK_URL", "")]));
        assert_eq!(empty.traefik_base, "http://traefik:8080");
        let set = Settings::from_vars(vars(&[
            ("SB_MOTD_TRAEFIK_URL", "http://fake"),
            ("DOCKER_HOST", "unix:///d.sock"),
            ("DOCKER_TLS_VERIFY", "1"),
            ("DOCKER_CERT_PATH", "/c"),
        ]));
        assert_eq!(
            set,
            Settings {
                docker_host: Some("unix:///d.sock".into()),
                tls_verify: Some("1".into()),
                cert_path: Some("/c".into()),
                traefik_base: "http://fake".into(),
                proxy: Proxy::System,
            }
        );
        assert_eq!(Settings::default().proxy, Proxy::Off);
        assert_eq!(TRAEFIK_CONNECT, Duration::from_secs(3));
    }

    /// The proxy setting reaches the client: an unusable proxy fails the
    /// call, so the API reads as not accessible.
    #[tokio::test]
    async fn traefiks_call_takes_the_settings_proxy() {
        let engine = engine(vec![running(1, "traefik")]);
        let api = Fake::start().unwrap();
        api.script("/api/http/routers", Answer::json(200, "[]"));
        let mut proxied = settings(&engine, api.url());
        proxied.proxy = Proxy::Via("http://proxy.invalid:1".into());
        assert_eq!(
            traefik(&Containers::new(proxied), &Paint::plain())
                .await
                .text,
            "Traefik container is running but API is not accessible"
        );
        assert_eq!(api.seen(), Vec::new());
    }

    /// Every state through `ps::status`: the Problem and Warn roles need
    /// attention, the Idle ones (exited 0, created, paused) are counted
    /// only (Difference 10).
    #[tokio::test]
    async fn containers_are_counted_and_the_ones_needing_attention_listed() {
        let engine = engine(vec![
            running(1, "plex"),
            container(2, "sonarr", &json!({ "Status": "exited", "ExitCode": 1 })),
            container(3, "radarr", &json!({ "Status": "exited", "ExitCode": 0 })),
            container(
                4,
                "authelia",
                &json!({ "Status": "running", "Running": true,
                "Health": { "Status": "unhealthy" } }),
            ),
            container(
                5,
                "lidarr",
                &json!({ "Status": "created", "Error": "port is taken" }),
            ),
            container(6, "paused", &json!({ "Status": "paused" })),
            container(7, "bazarr", &json!({ "Status": "restarting" })),
            container(
                8,
                "overseerr",
                &json!({ "Status": "running", "Running": true,
                "Health": { "Status": "starting" } }),
            ),
            container(9, "created", &json!({ "Status": "created" })),
        ]);
        let containers = Containers::new(settings(&engine, "http://traefik.invalid"));
        assert_eq!(
            docker(&containers, &Paint::plain()).await.text,
            "9 containers (3 running, 5 need attention)\n\
             authelia: running (unhealthy)\n\
             bazarr: restarting\n\
             lidarr: created (failed to start)\n\
             overseerr: running (starting)\n\
             sonarr: exited (code 1)"
        );
        let paint = painted();
        let text = docker(&containers, &paint).await.text;
        assert!(text.starts_with(&format!(
            "{} containers ({} running, {} need attention)\n",
            paint.warning("9"),
            paint.value("3"),
            paint.warning("5")
        )));
        assert!(text.ends_with(&format!("sonarr: {}", paint.error("exited (code 1)"))));
    }

    #[tokio::test]
    async fn all_well_is_the_counts_alone() {
        let engine = engine(vec![
            running(1, "plex"),
            container(2, "radarr", &json!({ "Status": "exited", "ExitCode": 0 })),
        ]);
        let containers = Containers::new(settings(&engine, "http://traefik.invalid"));
        let paint = painted();
        assert_eq!(
            docker(&containers, &paint).await.text,
            format!(
                "{} containers ({} running)",
                paint.value("2"),
                paint.value("1")
            )
        );
    }

    #[tokio::test]
    async fn no_containers_says_so() {
        let engine = engine(Vec::new());
        let containers = Containers::new(settings(&engine, "http://traefik.invalid"));
        assert_eq!(
            docker(&containers, &Paint::plain()).await.text,
            "Docker is running but no containers found"
        );
    }

    /// A failed inspect is a container needing attention, by short ID.
    #[tokio::test]
    async fn a_failed_inspect_needs_attention() {
        let failing = running(2, "sonarr");
        let id = failing["Id"].as_str().unwrap().to_string();
        let engine = FakeEngine::start(Options {
            inspects: vec![running(1, "plex"), failing],
            failing: [id].into_iter().collect(),
            ..Options::default()
        })
        .unwrap();
        let containers = Containers::new(settings(&engine, "http://traefik.invalid"));
        assert_eq!(
            docker(&containers, &Paint::plain()).await.text,
            "2 containers (1 running, 1 need attention)\n020202020202: inspect failed"
        );
    }

    #[tokio::test]
    async fn a_list_that_fails_says_the_list_is_unavailable() {
        let engine = FakeEngine::start(Options {
            list: Some(json!({ "not": "a list" })),
            ..Options::default()
        })
        .unwrap();
        let containers = Containers::new(settings(&engine, "http://traefik.invalid"));
        assert_eq!(
            docker(&containers, &Paint::plain()).await.text,
            "Docker is running but container list is unavailable"
        );
        assert_eq!(
            traefik(&containers, &Paint::plain()).await.text,
            "Traefik container is not running"
        );
    }

    /// A socket that answers with a permission error (Difference 33): both
    /// sections name it and the socket, in the error colour; neither says
    /// the daemon is down.
    #[tokio::test]
    async fn a_denied_socket_is_named_as_a_permission_error() {
        let containers = Containers {
            settings: Settings::default(),
            listing: OnceCell::new_with(Some(Err(Down::Denied(
                "Docker's socket (/run/docker.sock)".into(),
            )))),
            client: OnceCell::new(),
            limit: LIMIT,
        };
        let text = "Error: permission denied on Docker's socket (/run/docker.sock)";
        assert_eq!(docker(&containers, &Paint::plain()).await.text, text);
        assert_eq!(traefik(&containers, &Paint::plain()).await.text, text);
        let paint = painted();
        assert_eq!(docker(&containers, &paint).await.text, paint.error(text));
        assert_eq!(traefik(&containers, &paint).await.text, paint.error(text));
    }

    /// A failed connection to a Unix socket is a permission error when
    /// its chain holds EACCES or EPERM, or, for bollard's "socket not
    /// found" (no OS error), when a `stat` of the path is refused; anything
    /// else, and any other address, stays unreachable with its cause.
    #[test]
    fn a_failed_connection_is_denied_only_on_a_permission_error() {
        /// An error around `cause`, as sb-docker's context wraps bollard's.
        #[derive(Debug, thiserror::Error)]
        #[error("couldn't reach Docker")]
        struct Wrapped(#[source] std::io::Error);
        let socket = "unix:///run/docker.sock";
        let named = Down::Denied("Docker's socket (/run/docker.sock)".into());
        let fine = |_: &std::path::Path| Ok(());
        let refused =
            |_: &std::path::Path| Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        let eacces = Wrapped(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        assert_eq!(connect_failure(socket, "x".into(), &eacces, fine), named);
        let eperm = Wrapped(std::io::Error::from_raw_os_error(1));
        assert_eq!(connect_failure(socket, "x".into(), &eperm, fine), named);
        // bollard's "socket not found" holds no OS error: the stat decides.
        let not_found = std::fmt::Error;
        assert_eq!(
            connect_failure(socket, "x".into(), &not_found, refused),
            named
        );
        assert_eq!(
            connect_failure(socket, "gone".into(), &not_found, fine),
            Down::Unreachable("gone".into())
        );
        let missing = Wrapped(std::io::Error::from(std::io::ErrorKind::NotFound));
        assert_eq!(
            connect_failure(socket, "gone".into(), &missing, fine),
            Down::Unreachable("gone".into())
        );
        assert_eq!(
            connect_failure("tcp://docker:2375", "gone".into(), &eacces, refused),
            Down::Unreachable("gone".into())
        );
    }

    /// No daemon, or settings sb refuses: `Error: <cause>`, and Traefik
    /// says Docker isn't running.
    #[tokio::test]
    async fn an_unreachable_daemon_shows_its_cause() {
        let dir = tempfile::tempdir().unwrap();
        let containers = Containers::new(nowhere(&dir));
        let paint = painted();
        let text = docker(&containers, &Paint::plain()).await.text;
        assert!(
            text.starts_with("Error: couldn't reach Docker at unix://"),
            "{text}"
        );
        assert!(!text.contains('\n'));
        assert!(docker(&containers, &paint).await.text.starts_with("\x1b["));
        assert_eq!(
            traefik(&containers, &Paint::plain()).await.text,
            "Docker service is not running"
        );
        let refused = Containers::new(Settings {
            tls_verify: Some("1".into()),
            ..Settings::default()
        });
        assert_eq!(
            docker(&refused, &Paint::plain()).await.text,
            "Error: DOCKER_TLS_VERIFY is set, but sb talks to Docker without TLS"
        );
    }

    /// The two sections share one listing: the daemon is listed once.
    #[tokio::test]
    async fn both_sections_read_one_listing() {
        let engine = engine(vec![running(1, "plex")]);
        let containers = Containers::new(settings(&engine, "http://traefik.invalid"));
        let plain = Paint::plain();
        let (shown, routers) =
            tokio::join!(docker(&containers, &plain), traefik(&containers, &plain));
        assert_eq!(shown.text, "1 containers (1 running)");
        assert_eq!(routers.text, "Traefik container is not running");
        let lists = engine
            .paths()
            .iter()
            .filter(|path| path.contains("/containers/json"))
            .count();
        assert_eq!(lists, 1);
    }

    /// A daemon that takes the connection and never answers: after the
    /// limit the list reads as unavailable, as sb-go's `docker ps` past
    /// its 5 s did.
    #[tokio::test]
    async fn a_daemon_that_never_answers_is_given_up_on() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("docker.sock");
        // Bound, never accepted: the connection waits in the backlog.
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let containers = Containers::within(
            Settings {
                docker_host: Some(format!("unix://{}", socket.display())),
                ..Settings::default()
            },
            Duration::from_millis(200),
        );
        let start = std::time::Instant::now();
        assert_eq!(
            docker(&containers, &Paint::plain()).await.text,
            "Docker is running but container list is unavailable"
        );
        assert_eq!(
            traefik(&containers, &Paint::plain()).await.text,
            "Traefik container is not running"
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
        assert_eq!(LIMIT, Duration::from_secs(5));
    }

    /// A paused or restarting Traefik is listed by `docker ps`, so its API
    /// is asked, as sb-go asked it.
    #[tokio::test]
    async fn a_restarting_traefik_is_still_asked() {
        for state in ["paused", "restarting"] {
            let engine = engine(vec![container(1, "traefik", &json!({ "Status": state }))]);
            let (_held, url) = refused();
            let containers = Containers::new(settings(&engine, &url));
            assert_eq!(
                traefik(&containers, &Paint::plain()).await.text,
                "Traefik container is running but API is not accessible",
                "{state}"
            );
        }
    }

    /// The `traefik` container must be running, by its exact name.
    #[tokio::test]
    async fn traefik_needs_its_container_running() {
        for inspects in [
            vec![running(1, "traefik2"), running(2, "my-traefik")],
            vec![container(
                1,
                "traefik",
                &json!({ "Status": "exited", "ExitCode": 0 }),
            )],
        ] {
            let engine = engine(inspects);
            let containers = Containers::new(settings(&engine, "http://traefik.invalid"));
            assert_eq!(
                traefik(&containers, &Paint::plain()).await.text,
                "Traefik container is not running"
            );
        }
    }

    /// Traefik's routers from a fake API: errors in each of the forms
    /// sb-go reads, a disabled router, and healthy ones.
    #[tokio::test]
    async fn routers_are_counted_and_the_ones_needing_attention_listed() {
        let engine = engine(vec![running(1, "traefik")]);
        let api = Fake::start().unwrap();
        api.script(
            "/api/http/routers",
            Answer::json(
                200,
                r#"[
                  {"name":"plex@docker","status":"enabled"},
                  {"name":"sonarr@docker","status":"enabled","error":["  ","middleware \"x\" does not exist"]},
                  {"name":"radarr@docker","status":"disabled"},
                  {"name":"api@internal","status":"enabled","error":null},
                  {"name":"bad\u001b[2J@docker","status":"enabled","error":"  no service  "}
                ]"#,
            ),
        );
        let containers = Containers::new(settings(&engine, &format!("{}/", api.url())));
        assert_eq!(
            traefik(&containers, &Paint::plain()).await.text,
            "5 routers (2 active, 3 need attention)\n\
             sonarr@docker: middleware \"x\" does not exist\n\
             radarr@docker: router is disabled\n\
             bad@docker: no service"
        );
        let seen = api.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(
            (seen[0].method.as_str(), seen[0].target.as_str()),
            ("GET", "/api/http/routers")
        );
    }

    /// `--share` over Traefik's line (criterion 7): a router's error
    /// naming the user's domain and an address shows them masked, the
    /// router names kept.
    #[tokio::test]
    async fn shared_router_errors_mask_hosts_and_addresses() {
        let engine = engine(vec![running(1, "traefik")]);
        let api = Fake::start().unwrap();
        api.script(
            "/api/http/routers",
            Answer::json(
                200,
                r#"[{"name":"plex-secure@docker","status":"enabled","error":"unable to obtain ACME certificate for domains \"plex.mine.io\" from 203.0.113.5"}]"#,
            ),
        );
        let containers = Containers::new(settings(&engine, &format!("{}/", api.url())));
        let line = traefik(&containers, &Paint::plain()).await.text;
        assert_eq!(
            crate::redact::Share::default().mask(&line, true),
            "1 routers (0 active, 1 need attention)\n\
             plex-secure@docker: unable to obtain ACME certificate for domains \"[HOST]\" from xxx.xxx.xxx.xxx"
        );
    }

    #[tokio::test]
    async fn healthy_routers_are_the_counts_alone() {
        let engine = engine(vec![running(1, "traefik")]);
        let api = Fake::start().unwrap();
        api.script(
            "/api/http/routers",
            Answer::json(
                200,
                r#"[{"name":"a","status":"enabled","extra":1},{"name":null}]"#,
            ),
        );
        let containers = Containers::new(settings(&engine, api.url()));
        let paint = painted();
        assert_eq!(
            traefik(&containers, &paint).await.text,
            format!("{} routers ({} active)", paint.value("2"), paint.value("2"))
        );
    }

    /// No routers, an answer that isn't JSON (Difference 11), and an API
    /// that refuses the connection.
    #[tokio::test]
    async fn traefiks_other_answers() {
        let engine = engine(vec![running(1, "traefik")]);
        let api = Fake::start().unwrap();
        let containers = Containers::new(settings(&engine, api.url()));
        // The fake serves its answers in order.
        for body in ["[]", "  \n", ""] {
            api.script("/api/http/routers", Answer::json(200, body));
        }
        api.script(
            "/api/http/routers",
            Answer::new(404, "404 page not found\n"),
        );
        api.script("/api/http/routers", Answer::json(200, r#"[{"name":5}]"#));
        for body in ["[]", "  \n", ""] {
            assert_eq!(
                traefik(&containers, &Paint::plain()).await.text,
                "Traefik is running with no routers configured",
                "{body:?}"
            );
        }
        assert_eq!(
            traefik(&containers, &Paint::plain()).await.text,
            "Error: unexpected answer from Traefik (HTTP 404)"
        );
        let paint = painted();
        assert_eq!(
            traefik(&containers, &paint).await.text,
            paint.error("Error: unexpected answer from Traefik (HTTP 200)")
        );
        let (_held, url) = refused();
        let refused = Containers::new(settings(&engine, &url));
        assert_eq!(
            traefik(&refused, &Paint::plain()).await.text,
            "Traefik container is running but API is not accessible"
        );
    }

    /// sb-go's `extractTraefikRouterError`, form by form.
    #[test]
    fn router_errors_read_as_sb_go_reads_them() {
        let cases = [
            (json!(null), ""),
            (json!("  text  "), "text"),
            (json!([]), ""),
            (json!(["", " second "]), "second"),
            (json!({ "message": " m " }), "m"),
            (json!({ "b": 1, "a": "x" }), r#"{"a":"x","b":1}"#),
            (json!({ "message": 7 }), r#"{"message":7}"#),
            (json!(42), "42"),
            (json!(true), "true"),
            (json!([1, "x"]), r#"[1,"x"]"#),
        ];
        for (raw, shown) in cases {
            assert_eq!(router_error(&raw), shown, "{raw}");
        }
    }

    // --- certificates (decision 27) ---

    /// The run's clock in the certificates tests.
    fn now() -> jiff::Timestamp {
        "2026-11-17T12:00:00Z".parse().expect("now")
    }

    /// `now` moved by `seconds`, as Traefik writes a `notAfter`.
    fn after(seconds: i64) -> String {
        now()
            .checked_add(jiff::SignedDuration::from_secs(seconds))
            .expect("time")
            .to_string()
    }

    const DAY: i64 = 86_400;

    /// A fake Traefik with these counts and this first `notAfter` (none:
    /// the certificates request answers 404).
    fn certificate_api(overview: &str, first: Option<&str>) -> Fake {
        let api = Fake::start().unwrap();
        api.script("/api/overview", Answer::json(200, overview));
        match first {
            Some(not_after) => api.script(
                "/api/certificates",
                Answer::json(
                    200,
                    format!(r#"[{{"commonName":"x.example","notAfter":"{not_after}"}}]"#),
                ),
            ),
            None => api.script(
                "/api/certificates",
                Answer::json(404, "404 page not found\n"),
            ),
        }
        api
    }

    fn counts(total: u32, warnings: u32, errors: u32) -> String {
        format!(
            r#"{{"http":{{"routers":{{"total":3,"warnings":0,"errors":0}}}},"certificates":{{"total":{total},"warnings":{warnings},"errors":{errors}}}}}"#
        )
    }

    /// The line Traefik's answers make, plain, with `traefik` running.
    async fn certificate_line(api: &Fake, paint: &Paint) -> String {
        let engine = engine(vec![running(1, "traefik")]);
        let containers = Containers::new(settings(&engine, api.url()));
        certificates_at(&containers, paint, now()).await.text
    }

    /// Each form of the line (the motd spec, Certificates): all valid,
    /// some expiring soon, some expired, one certificate, the days in each
    /// direction, and none served.
    #[tokio::test]
    async fn the_certificates_line_reads_each_case() {
        let cases: [(&str, String, Option<String>); 10] = [
            ("all valid", counts(3, 0, 0), Some(after(38 * DAY + 3600))),
            ("expiring", counts(3, 1, 0), Some(after(12 * DAY + 60))),
            ("expired", counts(3, 1, 1), Some(after(-3 * DAY - 3600))),
            ("one", counts(1, 0, 0), Some(after(61 * DAY))),
            ("in one day", counts(1, 1, 0), Some(after(DAY + 300))),
            ("today", counts(1, 1, 0), Some(after(5 * 3600))),
            ("expired today", counts(1, 0, 1), Some(after(-2 * 3600))),
            ("one day ago", counts(2, 0, 1), Some(after(-DAY - 3600))),
            ("none served", counts(0, 0, 0), None),
            ("no first", counts(3, 0, 0), None),
        ];
        let mut shown = Vec::new();
        for (name, overview, first) in cases {
            let api = certificate_api(&overview, first.as_deref());
            shown.push(format!(
                "{name}: {}",
                certificate_line(&api, &Paint::plain()).await
            ));
        }
        insta::assert_snapshot!(shown.join("\n"), @r"
        all valid: 3 certificates (3 valid), next expires in 38 days
        expiring: 3 certificates (2 valid, 1 expiring soon), next expires in 12 days
        expired: 3 certificates (1 valid, 1 expiring soon, 1 expired), earliest expired 3 days ago
        one: 1 certificate (1 valid), next expires in 61 days
        in one day: 1 certificate (1 expiring soon), next expires in 1 day
        today: 1 certificate (1 expiring soon), next expires today
        expired today: 1 certificate (1 expired), earliest expired today
        one day ago: 2 certificates (1 valid, 1 expired), earliest expired 1 day ago
        none served: Traefik is serving no certificates
        no first: 3 certificates (3 valid)
        ");
    }

    /// The colours are the Traefik line's slots: the counts in the value
    /// colour when all are valid; an expiring count, its total and its
    /// days in the warning colour; an expired count, its total and its
    /// days in the error colour.
    #[tokio::test]
    async fn the_certificates_line_takes_the_traefik_lines_colours() {
        let paint = painted();
        let api = certificate_api(&counts(3, 0, 0), Some(&after(38 * DAY)));
        assert_eq!(
            certificate_line(&api, &paint).await,
            format!(
                "{} certificates ({} valid), next expires in {} days",
                paint.value("3"),
                paint.value("3"),
                paint.value("38")
            )
        );
        let api = certificate_api(&counts(3, 1, 0), Some(&after(12 * DAY)));
        assert_eq!(
            certificate_line(&api, &paint).await,
            format!(
                "{} certificates ({} valid, {} expiring soon), next expires in {} days",
                paint.warning("3"),
                paint.value("2"),
                paint.warning("1"),
                paint.warning("12")
            )
        );
        let api = certificate_api(&counts(3, 1, 1), Some(&after(-3 * DAY)));
        assert_eq!(
            certificate_line(&api, &paint).await,
            format!(
                "{} certificates ({} valid, {} expiring soon, {} expired), earliest expired {} days ago",
                paint.error("3"),
                paint.value("1"),
                paint.warning("1"),
                paint.error("1"),
                paint.error("3")
            )
        );
    }

    /// Hidden where the Traefik line already says why, or where Traefik
    /// has no certificates to count (before v3.7.0, or no TLS manager).
    #[tokio::test]
    async fn the_certificates_line_hides_when_there_is_nothing_to_count() {
        let api = certificate_api(r#"{"http":{"routers":{"total":1}}}"#, None);
        assert_eq!(certificate_line(&api, &Paint::plain()).await, "");
        // Traefik isn't running: no request at all.
        let idle = engine(vec![]);
        let api = certificate_api(&counts(3, 0, 0), Some(&after(38 * DAY)));
        let containers = Containers::new(settings(&idle, api.url()));
        assert_eq!(
            certificates_at(&containers, &Paint::plain(), now())
                .await
                .text,
            ""
        );
        assert_eq!(api.seen(), [] as [sb_config::testing::Seen; 0]);
        // Traefik's API isn't there.
        let (_held, url) = refused();
        let up = engine(vec![running(1, "traefik")]);
        let containers = Containers::new(settings(&up, &url));
        assert_eq!(
            certificates_at(&containers, &Paint::plain(), now())
                .await
                .text,
            ""
        );
    }

    /// An overview that isn't Traefik's JSON is Difference 11's error; a
    /// first certificate whose date doesn't parse leaves the counts alone.
    #[tokio::test]
    async fn the_certificates_line_reads_unexpected_answers() {
        let api = Fake::start().unwrap();
        api.script("/api/overview", Answer::json(500, "oops"));
        assert_eq!(
            certificate_line(&api, &Paint::plain()).await,
            "Error: unexpected answer from Traefik (HTTP 500)"
        );
        let api = certificate_api(&counts(3, 0, 0), Some("not a date"));
        assert_eq!(
            certificate_line(&api, &Paint::plain()).await,
            "3 certificates (3 valid)"
        );
        // A list that isn't JSON, or holds no certificate: the counts alone.
        for body in ["oops", "[]", r#"[{"commonName":"x"}]"#] {
            let api = Fake::start().unwrap();
            api.script("/api/overview", Answer::json(200, counts(3, 0, 0)));
            api.script("/api/certificates", Answer::json(200, body));
            assert_eq!(
                certificate_line(&api, &Paint::plain()).await,
                "3 certificates (3 valid)",
                "{body}"
            );
        }
    }

    /// At Traefik's edges: exactly 30 days left is no longer a warning,
    /// and a certificate that expires this second hasn't expired yet.
    #[tokio::test]
    async fn the_certificates_line_at_traefiks_edges() {
        let paint = painted();
        let api = certificate_api(&counts(1, 0, 0), Some(&after(30 * DAY)));
        assert_eq!(
            certificate_line(&api, &paint).await,
            format!(
                "{} certificate ({} valid), next expires in {} days",
                paint.value("1"),
                paint.value("1"),
                paint.value("30")
            )
        );
        let api = certificate_api(&counts(1, 1, 0), Some(&after(0)));
        assert_eq!(
            certificate_line(&api, &paint).await,
            format!(
                "{} certificate ({} expiring soon), next expires {}",
                paint.warning("1"),
                paint.warning("1"),
                paint.warning("today")
            )
        );
    }

    /// The section's job, on the host's clock.
    #[tokio::test]
    async fn the_certificates_section_reads_the_clock() {
        let in_400 = jiff::Timestamp::now()
            .checked_add(jiff::SignedDuration::from_hours(400 * 24 + 1))
            .expect("time")
            .to_string();
        let api = certificate_api(&counts(1, 0, 0), Some(&in_400));
        let engine = engine(vec![running(1, "traefik")]);
        let containers = Containers::new(settings(&engine, api.url()));
        assert_eq!(
            certificates(&containers, &Paint::plain()).await.text,
            "1 certificate (1 valid), next expires in 400 days"
        );
    }

    /// The two requests: the overview, and one certificate, the first to
    /// expire (Traefik sorts by `notAfter`, ascending by default).
    #[tokio::test]
    async fn the_certificates_line_asks_for_the_first_to_expire() {
        let api = certificate_api(&counts(3, 0, 0), Some(&after(38 * DAY)));
        certificate_line(&api, &Paint::plain()).await;
        let mut targets: Vec<String> = api.seen().into_iter().map(|seen| seen.target).collect();
        targets.sort();
        assert_eq!(
            targets,
            [
                "/api/certificates?sortBy=validUntil&per_page=1",
                "/api/overview"
            ]
        );
    }

    /// The routers and the certificates take one client between them, so
    /// one set of the platform's roots is loaded for all three requests.
    #[tokio::test]
    async fn traefiks_requests_share_one_client() {
        let engine = engine(vec![running(1, "traefik")]);
        let api = certificate_api(&counts(3, 0, 0), Some(&after(38 * DAY)));
        api.script("/api/http/routers", Answer::json(200, "[]"));
        let containers = Containers::new(settings(&engine, api.url()));
        traefik(&containers, &Paint::plain()).await;
        // Built by the routers' request, before this test asks for it.
        assert!(containers.client.initialized());
        let first: *const reqwest::Client = containers.client().await.as_ref().unwrap();
        certificates_at(&containers, &Paint::plain(), now()).await;
        let second: *const reqwest::Client = containers.client().await.as_ref().unwrap();
        assert_eq!(first, second);
    }

    // --- `--json` (motd spec, `--json`) ---

    /// Docker's value: the counts and each container needing attention.
    #[tokio::test]
    async fn dockers_value_counts_and_lists_attention() {
        let failing = running(2, "sonarr");
        let id = failing["Id"].as_str().unwrap().to_string();
        let engine = FakeEngine::start(Options {
            inspects: vec![running(1, "plex"), failing],
            failing: [id].into_iter().collect(),
            ..Options::default()
        })
        .unwrap();
        let containers = Containers::new(settings(&engine, "http://traefik.invalid"));
        assert_eq!(
            docker(&containers, &Paint::plain()).await.json,
            crate::Json::Value(json!({
                "total": 2,
                "running": 1,
                "need_attention": [{"name": "020202020202", "status": "inspect failed"}],
            }))
        );
        let plain = Paint::plain();
        let (_, routers) = tokio::join!(docker(&containers, &plain), traefik(&containers, &plain));
        assert_eq!(
            routers.json,
            crate::Json::Value(json!({
                "running": false,
                "total": null,
                "active": null,
                "need_attention": [],
                "text": "Traefik container is not running",
            }))
        );
    }

    /// Traefik's value: the routers counted, each needing attention with
    /// its problem; an answer that isn't the routers is an error.
    #[test]
    fn traefiks_value_counts_the_routers() {
        let body =
            br#"[{"name":"a@docker","status":"enabled"},{"name":"b@docker","status":"disabled"}]"#;
        assert_eq!(
            routers(200, body, &Paint::plain()).json,
            crate::Json::Value(json!({
                "running": true,
                "total": 2,
                "active": 1,
                "need_attention": [{"name": "b@docker", "problem": "router is disabled"}],
                "text": "2 routers (1 active, 1 need attention)\nb@docker: router is disabled",
            }))
        );
        assert_eq!(
            routers(502, b"<html>", &Paint::plain()).json,
            crate::Json::Error("Error: unexpected answer from Traefik (HTTP 502)".to_owned())
        );
        assert!(matches!(
            routers(200, b"[]", &Paint::plain()).json,
            crate::Json::Value(value) if value["total"] == 0
        ));
    }

    /// Certificates' value: the counts, the first `notAfter` and the days
    /// to it, below 0 once passed; none served has no date.
    #[test]
    fn certificates_value_has_the_counts_and_the_first_date() {
        let overview = br#"{"certificates":{"total":3,"warnings":1,"errors":1}}"#;
        let first: jiff::Timestamp = after(-2 * DAY - 60).parse().unwrap();
        let (out, counted) = certificates_line(200, overview, Some(first), now(), &Paint::plain());
        assert!(counted);
        assert_eq!(
            out.json,
            crate::Json::Value(json!({
                "total": 3,
                "valid": 1,
                "expiring_soon": 1,
                "expired": 1,
                "first_not_after": first.to_string(),
                // Rounded down: 2 days and a minute ago is -3.
                "days": -3,
            }))
        );
        let days = |seconds: i64| {
            let at: jiff::Timestamp = after(seconds).parse().unwrap();
            certificates_line(200, overview, Some(at), now(), &Paint::plain())
                .0
                .json
        };
        for (seconds, expected) in [
            (-12 * 3600, -1),
            (-1, -1),
            (0, 0),
            (DAY - 1, 0),
            (38 * DAY, 38),
        ] {
            assert!(
                matches!(days(seconds), crate::Json::Value(ref v) if v["days"] == expected),
                "{seconds}: {:?}",
                days(seconds)
            );
        }
        let none = br#"{"certificates":{"total":0,"warnings":0,"errors":0}}"#;
        assert_eq!(
            certificates_line(200, none, None, now(), &Paint::plain())
                .0
                .json,
            crate::Json::Value(json!({
                "total": 0,
                "valid": 0,
                "expiring_soon": 0,
                "expired": 0,
                "first_not_after": null,
                "days": null,
            }))
        );
        assert_eq!(
            certificates_line(200, b"{}", None, now(), &Paint::plain())
                .0
                .json,
            crate::Json::Empty
        );
    }
}
