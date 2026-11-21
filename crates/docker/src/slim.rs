//! The container list read into only the fields a row uses (docker spec,
//! "Rows"; performance round 5, motd finding 3): `sb docker ps`, motd's
//! Docker and Traefik lines and the log picker list through it alike.
//!
//! Why not bollard's model: `Docker::list_containers` reads every field
//! of every entry into a `ContainerSummary`, each of a Saltbox
//! container's labels (44 on a typical app), its mounts and its networks
//! into strings and hashed maps: about 0.16 M instructions for an entry
//! of 5 KB, 8.5% of a login at 60 containers and a third of `sb docker
//! ps` at 200. sb reads an entry's ID, names, state, health and ports,
//! and of its labels only the Traefik service ports. bollard has no
//! request that hands back the body (its `process_into_value` is
//! crate-private), so this one request goes beside it, on the client and
//! connector bollard itself uses for a Unix socket (bollard 0.21.1's
//! `Docker::connect_with_unix`: hyper-util's legacy `Client` on
//! hyperlocal's `UnixConnector`,
//! <https://docs.rs/hyperlocal/0.9.1/hyperlocal/struct.UnixConnector.html>),
//! at the daemon's request version, with bollard's request timeout and
//! bollard's own error for each failure (its `Docker::process_request`
//! and `decode_response`). The entry is mapped into bollard's
//! `ContainerSummary`, so the rows are built as before. A daemon on
//! `tcp://` or `http://` is listed through bollard's model: Saltbox's
//! daemon is on its socket.

use std::collections::HashMap;
use std::time::Duration;

use bollard::ClientVersion;
use bollard::errors::Error;
use bollard::models::{
    ContainerSummary, ContainerSummaryHealth, ContainerSummaryStateEnum, PortSummary,
};
use http_body_util::BodyExt as _;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use serde::Deserialize;
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, Visitor};

/// A list entry: the fields of bollard's `ContainerSummary` a row reads,
/// under the same names and types.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Entry {
    id: Option<String>,
    names: Option<Vec<String>>,
    state: Option<ContainerSummaryStateEnum>,
    health: Option<ContainerSummaryHealth>,
    #[serde(default, deserialize_with = "service_labels")]
    labels: Option<HashMap<String, String>>,
    ports: Option<Vec<PortSummary>>,
}

impl From<Entry> for ContainerSummary {
    fn from(entry: Entry) -> Self {
        ContainerSummary {
            id: entry.id,
            names: entry.names,
            state: entry.state,
            health: entry.health,
            labels: entry.labels,
            ports: entry.ports,
            ..ContainerSummary::default()
        }
    }
}

/// Whether a label's key can name a Traefik service's port: the first of
/// `ps::traefik_ports`'s conditions (it calls this), which reads no other
/// label.
pub(crate) fn service_port(key: &str) -> bool {
    key.contains("traefik.http.services")
}

/// A label's key, kept when [`service_port`] may read it.
struct Key;

impl<'de> DeserializeSeed<'de> for Key {
    type Value = Option<String>;

    fn deserialize<D: serde::Deserializer<'de>>(self, de: D) -> Result<Self::Value, D::Error> {
        de.deserialize_str(self)
    }
}

impl Visitor<'_> for Key {
    type Value = Option<String>;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a label's name")
    }

    fn visit_str<E: serde::de::Error>(self, key: &str) -> Result<Self::Value, E> {
        Ok(service_port(key).then(|| key.to_string()))
    }
}

/// `Labels`: a mapping, of which only the Traefik service ports are
/// kept, or null.
struct Labels;

impl<'de> Visitor<'de> for Labels {
    type Value = Option<HashMap<String, String>>;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // serde's own for a `HashMap`, as bollard's model words it.
        f.write_str("a map")
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut kept = HashMap::new();
        while let Some(key) = map.next_key_seed(Key)? {
            match key {
                Some(key) => {
                    kept.insert(key, map.next_value::<String>()?);
                }
                None => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(Some(kept))
    }
}

fn service_labels<'de, D: serde::Deserializer<'de>>(
    de: D,
) -> Result<Option<HashMap<String, String>>, D::Error> {
    de.deserialize_any(Labels)
}

/// A daemon's error body (bollard's `DockerServerErrorMessage`).
#[derive(Deserialize)]
struct Message {
    message: String,
}

/// Every container (`all=true`, `size=false`: bollard's query), from the
/// daemon on the Unix socket at `socket`, asked at API `version`, its
/// answer's head awaited for at most `timeout`: as bollard's
/// `list_containers` gives them, but for the
/// fields no row reads.
///
/// # Errors
///
/// bollard's error for each failure, as its own list call returns it: the
/// connection's or the request's, a timeout, the daemon's answer other
/// than a success (its status and message), or a body that isn't the list.
pub(crate) async fn entries(
    socket: &str,
    version: &ClientVersion,
    timeout: Duration,
) -> Result<Vec<ContainerSummary>, Error> {
    let mut builder = Client::builder(TokioExecutor::new());
    builder.pool_max_idle_per_host(0);
    let client = builder.build(hyperlocal::UnixConnector);
    let uri = hyperlocal::Uri::new(
        socket,
        &format!("/v{version}/containers/json?all=true&size=false"),
    );
    let request =
        http::Request::get(http::Uri::from(uri)).body(bollard::body_full(Vec::new().into()))?;
    let response = match tokio::time::timeout(timeout, client.request(request)).await {
        Ok(answer) => answer?,
        Err(_) => return Err(Error::RequestTimeoutError),
    };
    let status = response.status();
    let body = response.into_body().collect().await?.to_bytes();
    if !(status.is_success()
        || status == http::StatusCode::NOT_MODIFIED
        || status == http::StatusCode::SWITCHING_PROTOCOLS)
    {
        let contents = String::from_utf8_lossy(&body).into_owned();
        let message = if contents.is_empty() {
            contents
        } else {
            match serde_json::from_str::<Message>(&contents) {
                Ok(found) => found.message,
                Err(err) if err.is_data() || err.is_syntax() => contents,
                Err(err) => return Err(err.into()),
            }
        };
        return Err(Error::DockerResponseServerError {
            status_code: status.as_u16(),
            message,
        });
    }
    let entries: Vec<Entry> = serde_json::from_slice(&body).map_err(|err| {
        if err.is_data() || err.is_syntax() {
            Error::JsonDataError {
                message: err.to_string(),
                column: err.column(),
            }
        } else {
            err.into()
        }
    })?;
    Ok(entries.into_iter().map(ContainerSummary::from).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeEngine, Options};
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use bollard::query_parameters::ListContainersOptionsBuilder;
    use serde_json::json;

    const TIMEOUT: Duration = Duration::from_secs(10);

    fn bollard_on(socket: &str, timeout: u64) -> bollard::Docker {
        bollard::Docker::connect_with_unix(socket, timeout, bollard::API_DEFAULT_VERSION)
            .expect("client")
    }

    async fn bollards(docker: &bollard::Docker) -> Result<Vec<ContainerSummary>, Error> {
        let options = ListContainersOptionsBuilder::default().all(true).build();
        docker.list_containers(Some(options)).await
    }

    /// The fields a row reads, as bollard's model has them; of the
    /// labels, those `service_port` keeps.
    fn read(summary: &ContainerSummary) -> ContainerSummary {
        ContainerSummary {
            id: summary.id.clone(),
            names: summary.names.clone(),
            state: summary.state,
            health: summary.health.clone(),
            labels: summary.labels.as_ref().map(|labels| {
                labels
                    .iter()
                    .filter(|(key, _)| service_port(key))
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect()
            }),
            ports: summary.ports.clone(),
            ..ContainerSummary::default()
        }
    }

    /// The fake's containers (every state, health in the list, Traefik
    /// labels and others), and a list with nulls and fields left out:
    /// what bollard's model reads, field by field, at the request's
    /// version.
    #[tokio::test]
    async fn the_entries_are_what_bollards_model_reads() {
        let odd = json!([
            {"Id": "a", "Names": null, "State": "exited", "Labels": null, "Ports": null},
            {"Id": "b", "State": "", "Labels": {"x": "1", "traefik.http.services.b.loadbalancer.server.port": "80"}},
            {"Image": "only"},
        ]);
        for options in [
            Options::default(),
            Options {
                list: Some(odd),
                ..Options::default()
            },
        ] {
            let fake = FakeEngine::start(options).expect("fake");
            let host = fake.host();
            let socket = host.strip_prefix("unix://").expect("a socket");
            let version = ClientVersion {
                major_version: 1,
                minor_version: 52,
            };
            let slim = entries(socket, &version, TIMEOUT).await.expect("entries");
            let full = bollards(&bollard_on(&host, 10)).await.expect("bollard");
            assert_ne!(slim.len(), 0);
            assert_eq!(slim, full.iter().map(read).collect::<Vec<_>>());
            // The Traefik column from either, not through `service_port`.
            let ports = |summaries: &[ContainerSummary]| -> Vec<Vec<String>> {
                summaries
                    .iter()
                    .map(|s| s.labels.as_ref().map_or_default(crate::ps::traefik_ports))
                    .collect()
            };
            assert_eq!(ports(&slim), ports(&full));
            assert!(ports(&full).iter().any(|found| !found.is_empty()));
            assert_eq!(
                fake.paths().first().map(String::as_str),
                Some("/v1.52/containers/json?all=true&size=false")
            );
        }
    }

    /// One answer for every request, on a socket of its own.
    struct Answering {
        _dir: tempfile::TempDir,
        socket: String,
    }

    /// Called inside the test's runtime, which serves it.
    fn answering(status: StatusCode, body: &'static str) -> Answering {
        let dir = tempfile::tempdir().expect("dir");
        let socket = dir.path().join("d.sock");
        let listener = tokio::net::UnixListener::bind(&socket).expect("bind");
        let app =
            axum::Router::new().fallback(move || async move { (status, body).into_response() });
        tokio::spawn(async move { axum::serve(listener, app).await });
        Answering {
            _dir: dir,
            socket: socket.display().to_string(),
        }
    }

    /// Each failure is bollard's own error, worded as its list call
    /// words it; a label sb doesn't read may hold anything.
    #[tokio::test]
    async fn each_answer_reads_as_bollards_list_reads_it() {
        let version = ClientVersion {
            major_version: 1,
            minor_version: 53,
        };
        for (status, body) in [
            (StatusCode::INTERNAL_SERVER_ERROR, r#"{"message": "boom"}"#),
            (StatusCode::BAD_REQUEST, "plain words"),
            (StatusCode::NOT_FOUND, ""),
            (StatusCode::INTERNAL_SERVER_ERROR, "{"),
            (StatusCode::INTERNAL_SERVER_ERROR, r#"{"other": 1}"#),
            (StatusCode::OK, "[{"),
            (StatusCode::OK, "{}"),
            (
                StatusCode::OK,
                r#"[{"Labels": {"traefik.http.services.a.loadbalancer.server.port": 80}}]"#,
            ),
            (StatusCode::OK, r#"[{"Labels": 7}]"#),
        ] {
            let server = answering(status, body);
            let slim = entries(&server.socket, &version, TIMEOUT).await;
            let full = bollards(&bollard_on(&format!("unix://{}", server.socket), 10)).await;
            let slim = slim.expect_err("an error").to_string();
            assert_eq!(
                slim,
                full.expect_err("an error").to_string(),
                "{status} {body}"
            );
        }
        let server = answering(
            StatusCode::OK,
            r#"[{"Id": "x", "Labels": {"other": [1, {"a": null}]}}]"#,
        );
        let slim = entries(&server.socket, &version, TIMEOUT)
            .await
            .expect("entries");
        assert_eq!(slim[0].id.as_deref(), Some("x"));
        assert_eq!(slim[0].labels, Some(HashMap::new()));
    }

    /// An answer that doesn't come within the timeout is bollard's
    /// timeout; a socket nobody listens on, its connection error.
    #[tokio::test]
    async fn a_late_answer_and_no_daemon_are_bollards_errors() {
        let dir = tempfile::tempdir().expect("dir");
        let socket = dir.path().join("silent.sock");
        // Bound, never accepted: the request's head never comes back.
        let _listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        let version = bollard::API_DEFAULT_VERSION;
        let late = entries(
            &socket.display().to_string(),
            version,
            Duration::from_millis(50),
        )
        .await;
        assert!(matches!(late, Err(Error::RequestTimeoutError)), "{late:?}");
        let gone = dir.path().join("gone.sock");
        let none = entries(&gone.display().to_string(), version, TIMEOUT).await;
        let none = none.expect_err("no daemon").to_string();
        assert!(
            none.starts_with("Error in the hyper legacy client: "),
            "{none}"
        );
    }
}
