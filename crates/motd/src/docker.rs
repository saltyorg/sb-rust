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
use serde_json::Value;
use tokio::sync::OnceCell;

use crate::Paint;
use crate::text::one_line;

/// Traefik's API where Saltbox serves it, the base sb-go's `curl` asked.
const TRAEFIK_BASE: &str = "http://traefik:8080";

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
    /// It answered, but its container list failed.
    NoList,
}

/// The run's one listing, shared by the Docker and Traefik sections.
#[derive(Debug)]
pub struct Containers {
    settings: Settings,
    listing: OnceCell<Result<Listing, Down>>,
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
            limit,
        })
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
        Down::Unreachable(format!("{err:#}"))
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
pub async fn docker(containers: &Containers, paint: &Paint) -> String {
    let listing = match containers.listing().await {
        Ok(listing) => listing,
        Err(Down::Unreachable(cause)) => {
            return paint.error(&format!("Error: {}", one_line(cause.as_bytes())));
        }
        Err(Down::NoList) => return "Docker is running but container list is unavailable".into(),
    };
    let total = listing.rows.len() + listing.failures.len();
    if total == 0 {
        return "Docker is running but no containers found".into();
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
    let summary = summary(
        total,
        running,
        attention.len(),
        "containers",
        "running",
        paint,
    );
    lines(summary, &attention, paint)
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
pub async fn traefik(containers: &Containers, paint: &Paint) -> String {
    let listing = match containers.listing().await {
        Ok(listing) => listing,
        Err(Down::Unreachable(_)) => return "Docker service is not running".into(),
        Err(Down::NoList) => return "Traefik container is not running".into(),
    };
    // `docker ps` without `-a` lists paused and restarting containers too.
    let up = listing.rows.iter().any(|row| {
        row.name == "traefik" && matches!(row.state.as_str(), "running" | "paused" | "restarting")
    });
    if !up {
        return "Traefik container is not running".into();
    }
    let url = format!(
        "{}/api/http/routers",
        containers.settings.traefik_base.trim_end_matches('/')
    );
    let (status, body) = match fetch(&url, &containers.settings.proxy).await {
        Ok(answer) => answer,
        Err(err) => {
            tracing::warn!(error = %err, "traefik's api not reached");
            return "Traefik container is running but API is not accessible".into();
        }
    };
    routers(status, &body, paint)
}

/// The answer's status and body (at most sb-config's 1 MiB), or why
/// there is none.
async fn fetch(url: &str, proxy: &Proxy) -> Result<(u16, Vec<u8>), String> {
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
    let client = builder
        .build()
        .map_err(|err| sb_config::http::error_chain(&err))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|err| sb_config::http::error_chain(&err.without_url()))?;
    let status = response.status().as_u16();
    Ok((status, sb_config::http::body(response).await?))
}

/// The routers line for Traefik's answer: each router with an error, or
/// disabled, needs attention (sb-go's `GetTraefikInfo`).
fn routers(status: u16, body: &[u8], paint: &Paint) -> String {
    let text = String::from_utf8_lossy(body);
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed == "[]" {
        return "Traefik is running with no routers configured".into();
    }
    let Ok(routers) = serde_json::from_str::<Vec<Router>>(trimmed) else {
        return paint.error(&format!(
            "Error: unexpected answer from Traefik (HTTP {status})"
        ));
    };
    if routers.is_empty() {
        return "Traefik is running with no routers configured".into();
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
    lines(summary, &attention, paint)
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

    /// A loopback URL nothing listens on: a port bound, then let go.
    fn refused() -> String {
        let closed = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = closed.local_addr().unwrap();
        drop(closed);
        format!("http://{address}")
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
            traefik(&Containers::new(proxied), &Paint::plain()).await,
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
            docker(&containers, &Paint::plain()).await,
            "9 containers (3 running, 5 need attention)\n\
             authelia: running (unhealthy)\n\
             bazarr: restarting\n\
             lidarr: created (failed to start)\n\
             overseerr: running (starting)\n\
             sonarr: exited (code 1)"
        );
        let paint = painted();
        let text = docker(&containers, &paint).await;
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
            docker(&containers, &paint).await,
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
            docker(&containers, &Paint::plain()).await,
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
            docker(&containers, &Paint::plain()).await,
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
            docker(&containers, &Paint::plain()).await,
            "Docker is running but container list is unavailable"
        );
        assert_eq!(
            traefik(&containers, &Paint::plain()).await,
            "Traefik container is not running"
        );
    }

    /// No daemon, or settings sb refuses: `Error: <cause>`, and Traefik
    /// says Docker isn't running.
    #[tokio::test]
    async fn an_unreachable_daemon_shows_its_cause() {
        let dir = tempfile::tempdir().unwrap();
        let containers = Containers::new(nowhere(&dir));
        let paint = painted();
        let text = docker(&containers, &Paint::plain()).await;
        assert!(
            text.starts_with("Error: couldn't reach Docker at unix://"),
            "{text}"
        );
        assert!(!text.contains('\n'));
        assert!(docker(&containers, &paint).await.starts_with("\x1b["));
        assert_eq!(
            traefik(&containers, &Paint::plain()).await,
            "Docker service is not running"
        );
        let refused = Containers::new(Settings {
            tls_verify: Some("1".into()),
            ..Settings::default()
        });
        assert_eq!(
            docker(&refused, &Paint::plain()).await,
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
        assert_eq!(shown, "1 containers (1 running)");
        assert_eq!(routers, "Traefik container is not running");
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
            docker(&containers, &Paint::plain()).await,
            "Docker is running but container list is unavailable"
        );
        assert_eq!(
            traefik(&containers, &Paint::plain()).await,
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
            let containers = Containers::new(settings(&engine, &refused()));
            assert_eq!(
                traefik(&containers, &Paint::plain()).await,
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
                traefik(&containers, &Paint::plain()).await,
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
            traefik(&containers, &Paint::plain()).await,
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
            traefik(&containers, &paint).await,
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
                traefik(&containers, &Paint::plain()).await,
                "Traefik is running with no routers configured",
                "{body:?}"
            );
        }
        assert_eq!(
            traefik(&containers, &Paint::plain()).await,
            "Error: unexpected answer from Traefik (HTTP 404)"
        );
        let paint = painted();
        assert_eq!(
            traefik(&containers, &paint).await,
            paint.error("Error: unexpected answer from Traefik (HTTP 200)")
        );
        let refused = Containers::new(settings(&engine, &refused()));
        assert_eq!(
            traefik(&refused, &Paint::plain()).await,
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
}
