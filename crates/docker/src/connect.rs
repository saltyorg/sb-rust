//! Talking to the daemon (docker spec, "Daemon", "API version", "Rows",
//! "Errors"): which address `DOCKER_HOST` names, the connection and its
//! one `version()` call, and the list with an inspect per container.

use anyhow::Context;
use bollard::Docker;
use bollard::errors::Error;
use bollard::query_parameters::{InspectContainerOptions, ListContainersOptionsBuilder};
use futures_util::stream::{self, StreamExt};

use crate::ps::{Row, short_id, sorted};

/// A setting that is set: unset or empty count as unset, as in sb-go.
fn set(value: Option<&str>) -> Option<&str> {
    value.filter(|v| !v.is_empty())
}

/// Docker's default socket: bollard's `DEFAULT_SOCKET` (bollard 0.21.1
/// `src/docker.rs`), which it doesn't export; sb-go's client uses the same.
const DEFAULT_SOCKET: &str = "unix:///var/run/docker.sock";

/// The address to connect to, from `DOCKER_HOST`, `DOCKER_TLS_VERIFY` and
/// `DOCKER_CERT_PATH` (unset or empty: not set, as sb-go).
///
/// bollard is built without TLS (docker spec, "Daemon"): with a TLS
/// setting it would connect `tcp://` in plain text where sb-go uses TLS,
/// so any TLS or ssh setting stops with an error naming it. Why not
/// `Docker::connect_with_defaults()`: it reads `DOCKER_HOST` itself and
/// fails on an empty one, which sb-go treats as unset.
pub fn address(
    docker_host: Option<&str>,
    tls_verify: Option<&str>,
    cert_path: Option<&str>,
) -> anyhow::Result<String> {
    if set(tls_verify).is_some() {
        anyhow::bail!("DOCKER_TLS_VERIFY is set, but sb talks to Docker without TLS");
    }
    if set(cert_path).is_some() {
        anyhow::bail!("DOCKER_CERT_PATH is set, but sb talks to Docker without TLS");
    }
    match set(docker_host) {
        None => Ok(DEFAULT_SOCKET.to_string()),
        // The schemes bollard's `connect_with_host` serves with its
        // default features, checked here on purpose: a clearer message,
        // before any connection, and still refusing TLS and ssh if those
        // features are ever turned on.
        Some(host)
            if ["unix://", "tcp://", "http://"]
                .iter()
                .any(|scheme| host.starts_with(scheme)) =>
        {
            Ok(host.to_string())
        }
        Some(host) => anyhow::bail!(
            "DOCKER_HOST={host} isn't supported: sb talks to Docker over unix://, tcp:// or http:// (no TLS or ssh)"
        ),
    }
}

/// A connected daemon: the client, its address and what its one
/// `version()` call said.
pub struct Daemon {
    pub docker: Docker,
    pub address: String,
    pub version: Option<String>,
    pub api_version: Option<String>,
}

/// Connect to `address` and ask the daemon its version once (docker spec,
/// "API version": bollard 0.21.1 sends unversioned paths, so no version is
/// negotiated; the answer goes to the run log). A failed call ends the run
/// as an unreachable daemon does.
pub async fn connect(address: &str) -> anyhow::Result<Daemon> {
    let docker = Docker::connect_with_host(address)
        .with_context(|| format!("couldn't reach Docker at {address}"))?;
    let answer = docker
        .version()
        .await
        .with_context(|| format!("couldn't get Docker's version at {address}"))?;
    tracing::info!(
        address,
        version = answer.version.as_deref().unwrap_or("unknown"),
        api_version = answer.api_version.as_deref().unwrap_or("unknown"),
        "docker daemon"
    );
    Ok(Daemon {
        docker,
        address: address.to_string(),
        version: answer.version,
        api_version: answer.api_version,
    })
}

/// An inspect that failed for a reason other than the container being
/// gone: the container's short ID and the cause.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub short_id: String,
    pub cause: String,
}

/// The error for failed inspects (docker spec, "Errors"): sb-go's
/// headline, then one line per container, `error inspecting container
/// <short ID>: <cause>`, each a cause in cli-core's error block. `None`
/// when nothing failed. Why a chain of contexts for a list: anyhow and
/// cli-core's error block (and the run log's `error.sources`) model one
/// linear chain; a multi-error type would need a cli-core change.
pub fn failures_error(failures: &[Failure]) -> Option<anyhow::Error> {
    let mut lines = failures
        .iter()
        .rev()
        .map(|f| format!("error inspecting container {}: {}", f.short_id, f.cause));
    let last = anyhow::Error::msg(lines.next()?);
    let chain = lines.fold(last, anyhow::Error::context);
    Some(chain.context(format!("failed to inspect {} container(s)", failures.len())))
}

/// What `ps` found: the rows, sorted by name, and the failed inspects.
#[derive(Debug)]
pub struct Listing {
    pub rows: Vec<Row>,
    pub failures: Vec<Failure>,
}

/// How many inspects run at once (docker spec, "Rows").
const INSPECTS_AT_ONCE: usize = 16;

/// Every container (`all=true`), each inspected by ID, at most
/// [`INSPECTS_AT_ONCE`] at a time
/// (https://docs.rs/futures-util/0.3.34/futures_util/stream/trait.StreamExt.html#method.buffer_unordered);
/// one removed since the list (404) is left out, other failures are kept
/// with the short ID. The rows come back sorted by name, as sb-go.
pub async fn list(daemon: &Daemon) -> anyhow::Result<Listing> {
    let started = std::time::Instant::now();
    let docker = &daemon.docker;
    let options = ListContainersOptionsBuilder::default().all(true).build();
    let summaries = docker
        .list_containers(Some(options))
        .await
        .with_context(|| {
            format!(
                "couldn't list the containers of Docker at {}",
                daemon.address
            )
        })?;
    let listed = summaries.len();
    let inspected: Vec<_> = stream::iter(summaries)
        .map(|summary| async move {
            let id = summary.id.clone().unwrap_or_default();
            let result = docker
                .inspect_container(&id, None::<InspectContainerOptions>)
                .await;
            (summary, id, result)
        })
        .buffer_unordered(INSPECTS_AT_ONCE)
        .collect()
        .await;
    let mut rows = Vec::new();
    let mut failures = Vec::new();
    for (summary, id, result) in inspected {
        match result {
            Ok(inspect) => rows.push(Row::new(&summary, &inspect)),
            Err(Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {
                tracing::debug!(
                    container = short_id(&id),
                    "removed since the list; left out"
                );
            }
            Err(err) => {
                // With its causes: bollard's own message for a transport
                // error names only the client, the reason is in its source.
                let cause = format!("{:#}", anyhow::Error::from(err));
                tracing::warn!(container = short_id(&id), error = %cause, "inspect failed");
                failures.push(Failure {
                    short_id: short_id(&id),
                    cause,
                });
            }
        }
    }
    failures.sort_by(|a, b| a.short_id.cmp(&b.short_id));
    tracing::info!(
        listed,
        inspected = rows.len(),
        failed = failures.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "containers listed"
    );
    Ok(Listing {
        rows: sorted(rows),
        failures,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(r: anyhow::Result<String>) -> String {
        r.expect_err("an error").to_string()
    }

    /// Unset or empty: bollard's default socket, as sb-go.
    #[test]
    fn no_docker_host_means_the_default_socket() {
        assert_eq!(
            address(None, None, None).unwrap(),
            "unix:///var/run/docker.sock"
        );
        assert_eq!(
            address(Some(""), Some(""), Some("")).unwrap(),
            "unix:///var/run/docker.sock"
        );
    }

    #[test]
    fn unix_tcp_and_http_hosts_are_followed() {
        for host in [
            "unix:///tmp/d.sock",
            "tcp://198.51.100.7:2375",
            "http://198.51.100.7:2375",
        ] {
            assert_eq!(address(Some(host), None, None).unwrap(), host);
        }
    }

    /// No TLS compiled in: a TLS or ssh setting is an error naming it,
    /// never a silent plain-text connection.
    #[test]
    fn tls_and_ssh_settings_are_errors() {
        assert!(err(address(None, Some("1"), None)).contains("DOCKER_TLS_VERIFY"));
        assert!(err(address(None, None, Some("/certs"))).contains("DOCKER_CERT_PATH"));
        for host in [
            "https://198.51.100.7:2376",
            "ssh://user@198.51.100.7",
            "npipe:////./pipe/x",
            "nonsense",
        ] {
            let e = err(address(Some(host), None, None));
            assert!(e.contains("DOCKER_HOST") && e.contains(host), "{e}");
        }
        // TLS wins over an otherwise good host.
        assert!(
            err(address(Some("tcp://198.51.100.7:2376"), Some("1"), None))
                .contains("DOCKER_TLS_VERIFY")
        );
    }

    use crate::testing::{FakeEngine, Options, inspects, summary};
    use serde_json::json;

    fn fake(options: Options) -> FakeEngine {
        FakeEngine::start(options).expect("fake Engine API")
    }

    async fn ps(engine: &FakeEngine) -> anyhow::Result<Listing> {
        let daemon = connect(&engine.host()).await?;
        list(&daemon).await
    }

    fn names(listing: &Listing) -> Vec<&str> {
        listing.rows.iter().map(|r| r.name.as_str()).collect()
    }

    /// Every fixture container, sorted by name, from one version call, a
    /// list and an inspect each, all unversioned (docker spec, "Guarding
    /// that behaviour": if this fails, a bollard upgrade changed the paths;
    /// re-check the spec's API version bullet before moving the pin).
    #[tokio::test]
    async fn every_container_is_listed_and_inspected_unversioned() {
        let engine = fake(Options::default());
        let listing = ps(&engine).await.expect("ps");
        assert_eq!(
            names(&listing),
            [
                "0123456789ab",
                "authelia",
                "lidarr",
                "plex",
                "radarr",
                "sonarr"
            ]
        );
        assert!(listing.failures.is_empty());
        let paths = engine.paths();
        assert_eq!(paths[0], "/version");
        // Every container, running or not.
        assert_eq!(paths[1], "/containers/json?all=true&size=false");
        assert_eq!(paths.len(), 2 + 6);
        assert!(
            paths.iter().all(|p| !p.starts_with("/v1.")),
            "versioned paths (see the docker spec's API version bullet): {paths:?}"
        );
    }

    /// The connection keeps bollard's 120 s timeout and records the
    /// daemon's own version, not a capped one.
    #[tokio::test]
    async fn the_daemon_version_is_its_own() {
        let engine = fake(Options {
            api_versions: ((1, 40), (1, 55)),
            ..Options::default()
        });
        let daemon = connect(&engine.host()).await.expect("connect");
        assert_eq!(daemon.api_version.as_deref(), Some("1.55"));
        assert_eq!(daemon.version.as_deref(), Some("0.0.0-fake"));
        assert_eq!(daemon.docker.timeout(), std::time::Duration::from_secs(120));
    }

    /// An older daemon (refusing versions above its own) still answers.
    #[tokio::test]
    async fn an_old_daemon_answers() {
        let engine = fake(Options {
            api_versions: ((1, 40), (1, 41)),
            ..Options::default()
        });
        assert_eq!(ps(&engine).await.expect("ps").rows.len(), 6);
    }

    /// A daemon refusing unversioned paths (Docker's planned removal) is
    /// an error, not a panic or an empty table.
    #[tokio::test]
    async fn a_daemon_refusing_unversioned_paths_is_an_error() {
        let engine = fake(Options {
            refuse_unversioned: true,
            ..Options::default()
        });
        let err = format!("{:#}", ps(&engine).await.expect_err("an error"));
        assert!(
            err.contains("couldn't get Docker's version at unix://"),
            "{err}"
        );
        assert!(
            err.contains("API without a version prefix is not supported"),
            "{err}"
        );
        // Nothing is sent after the failed version call.
        assert_eq!(engine.paths(), ["/version"]);
    }

    #[tokio::test]
    async fn an_unreachable_daemon_is_an_error_naming_the_address() {
        let dir = tempfile::tempdir().expect("temp dir");
        let address = format!("unix://{}", dir.path().join("none.sock").display());
        let err = connect(&address).await.err().expect("an error").to_string();
        assert!(err.contains(&address), "{err}");
    }

    /// A socket nobody answers on any more: the version call fails.
    #[tokio::test]
    async fn a_stale_socket_fails_the_version_call() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("stale.sock");
        drop(std::os::unix::net::UnixListener::bind(&path).expect("bind"));
        let address = format!("unix://{}", path.display());
        let err = format!("{:#}", connect(&address).await.err().expect("an error"));
        assert!(
            err.contains(&format!("couldn't get Docker's version at {address}")),
            "{err}"
        );
    }

    /// The list's failure names the daemon's address.
    #[tokio::test]
    async fn a_failed_list_names_the_address() {
        let engine = fake(Options {
            list: Some(json!({ "not": "a list" })),
            ..Options::default()
        });
        let err = format!("{:#}", ps(&engine).await.expect_err("an error"));
        assert!(
            err.contains(&format!(
                "couldn't list the containers of Docker at {}",
                engine.host()
            )),
            "{err}"
        );
    }

    fn fixture_id(name: &str) -> String {
        inspects()
            .into_iter()
            .find(|i| i["Name"] == format!("/{name}").as_str())
            .and_then(|i| i["Id"].as_str().map(str::to_string))
            .expect("fixture")
    }

    /// Removed since the list: left out, no failure.
    #[tokio::test]
    async fn a_vanished_container_is_left_out() {
        let engine = fake(Options {
            vanished: [fixture_id("plex")].into(),
            ..Options::default()
        });
        let listing = ps(&engine).await.expect("ps");
        assert!(!names(&listing).contains(&"plex"));
        assert_eq!(listing.rows.len(), 5);
        assert!(listing.failures.is_empty());
    }

    /// Any other failure: kept, with the short ID and the cause.
    #[tokio::test]
    async fn a_failing_inspect_is_kept_with_its_short_id() {
        let id = fixture_id("sonarr");
        let engine = fake(Options {
            failing: [id.clone()].into(),
            ..Options::default()
        });
        let listing = ps(&engine).await.expect("ps");
        assert_eq!(listing.rows.len(), 5);
        assert_eq!(listing.failures.len(), 1);
        assert_eq!(listing.failures[0].short_id, id[..12]);
        assert!(listing.failures[0].cause.contains("fake inspect failure"));
    }

    /// Several failures come back in short-ID order, whatever order the
    /// list and the inspects finished in.
    #[tokio::test]
    async fn failures_are_ordered_by_short_id() {
        // The list's order is plex (a1…), sonarr (b2…), then the unnamed
        // one (01…): sorted, the unnamed one comes first.
        let ids: Vec<String> = ["plex", "sonarr"].iter().map(|n| fixture_id(n)).collect();
        let unnamed = inspects()[3]["Id"].as_str().unwrap_or_default().to_string();
        let engine = fake(Options {
            failing: [ids[0].clone(), ids[1].clone(), unnamed.clone()].into(),
            ..Options::default()
        });
        let listing = ps(&engine).await.expect("ps");
        let order: Vec<_> = listing
            .failures
            .iter()
            .map(|f| f.short_id.clone())
            .collect();
        assert_eq!(
            order,
            [
                unnamed[..12].to_string(),
                ids[0][..12].to_string(),
                ids[1][..12].to_string()
            ]
        );
    }

    /// A newer daemon's shape: a field bollard expects left out, one it
    /// doesn't know added.
    #[tokio::test]
    async fn a_newer_daemons_shape_parses() {
        let mut newer = inspects();
        for inspect in &mut newer {
            inspect["SomethingNew"] = json!({ "x": 1 });
            if let Some(object) = inspect.as_object_mut() {
                object.remove("Config");
            }
        }
        let engine = fake(Options {
            inspects: newer,
            ..Options::default()
        });
        let listing = ps(&engine).await.expect("ps");
        assert_eq!(listing.rows.len(), 6);
        assert!(listing.rows.iter().all(|r| r.traefik.is_empty()));
    }

    /// bollard's enums are closed: an unknown state word in the list fails
    /// the list call; met only in an inspect, that inspect.
    #[tokio::test]
    async fn an_unknown_state_word_fails_the_list_or_that_inspect() {
        let mut list: Vec<_> = inspects().iter().map(summary).collect();
        list[0]["State"] = json!("hibernating");
        let engine = fake(Options {
            list: Some(json!(list)),
            ..Options::default()
        });
        let err = format!("{:#}", ps(&engine).await.expect_err("list failure"));
        assert!(err.contains("couldn't list the containers"), "{err}");

        // The list as Docker gave it; the word appears only in one inspect.
        let list: Vec<_> = inspects().iter().map(summary).collect();
        let mut odd = inspects();
        odd[1]["State"]["Status"] = json!("hibernating");
        let engine = fake(Options {
            inspects: odd,
            list: Some(json!(list)),
            ..Options::default()
        });
        let listing = ps(&engine).await.expect("ps");
        assert_eq!((listing.rows.len(), listing.failures.len()), (5, 1));
    }

    /// At most 16 inspects at once, and more than one: with 20 containers
    /// held open, the fake sees exactly 16.
    #[tokio::test]
    async fn at_most_sixteen_inspects_run_at_once() {
        let many: Vec<_> = (0..20)
            .map(|n| json!({ "Id": format!("{n:064x}"), "Name": format!("/c{n:02}") }))
            .collect();
        let engine = fake(Options {
            inspects: many,
            hold: true,
            ..Options::default()
        });
        let daemon = connect(&engine.host()).await.expect("connect");
        let task = tokio::spawn(async move { list(&daemon).await });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while engine.open_inspects() < 16 && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        // Give a 17th the chance to arrive, then let them all answer.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(engine.open_inspects(), 16);
        assert_eq!(engine.peak_inspects(), 16);
        engine.release();
        let listing = tokio::time::timeout(std::time::Duration::from_secs(10), task)
            .await
            .expect("answered after the release")
            .expect("join")
            .expect("ps");
        assert_eq!(listing.rows.len(), 20);
        assert_eq!(engine.open_inspects(), 0);
    }

    /// sb-go's headline, then each container's line, in order, as the
    /// error block's causes.
    #[test]
    fn failures_become_one_error_with_a_line_each() {
        assert!(failures_error(&[]).is_none());
        let failures = [
            Failure {
                short_id: "aaaaaaaaaaaa".into(),
                cause: "boom".into(),
            },
            Failure {
                short_id: "bbbbbbbbbbbb".into(),
                cause: "bang".into(),
            },
        ];
        let err = failures_error(&failures).expect("an error");
        let lines: Vec<_> = err.chain().map(ToString::to_string).collect();
        assert_eq!(
            lines,
            [
                "failed to inspect 2 container(s)",
                "error inspecting container aaaaaaaaaaaa: boom",
                "error inspecting container bbbbbbbbbbbb: bang",
            ]
        );
    }
}
