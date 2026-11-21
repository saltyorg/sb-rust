//! The daemon's container events, for the log picker and viewer (docker
//! spec, `events.rs`; tui spec, "Live updates" and the docker follow):
//! an event marks the picker's container list stale, and a `start` lets a
//! stopped follow resume. Read through bollard's `Docker::events`
//! (<https://docs.rs/bollard/0.21.1/bollard/struct.Docker.html#method.events>)
//! with the `type=container` filter its example shows, and the actions
//! (`event`) and container (`container`) each reader needs, so the
//! daemon keeps the rest, a health check's three exec events every few
//! seconds above all (perf round 5, item 6); the shapes are in
//! docs/platform-notes/docker-logs.md, "Container events".

use std::collections::HashMap;

use bollard::models::EventMessage;
use bollard::query_parameters::EventsOptionsBuilder;
use futures_util::stream::BoxStream;
use futures_util::{Stream, StreamExt as _};

use crate::connect::Daemon;

/// What happened to a container. The ones the viewer acts on are named;
/// the rest come through as the daemon wrote them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Create,
    Start,
    Die,
    Destroy,
    Rename,
    /// `health_status: <state>`, with the state (`healthy`, `unhealthy`,
    /// `starting`).
    HealthStatus(String),
    /// Any other action (`stop`, `kill`, `exec_start: <command>`, ...).
    Other(String),
}

impl Action {
    fn from_daemon(action: &str) -> Self {
        match action {
            "create" => Action::Create,
            "start" => Action::Start,
            "die" => Action::Die,
            "destroy" => Action::Destroy,
            "rename" => Action::Rename,
            other => match other.strip_prefix("health_status: ") {
                Some(state) => Action::HealthStatus(state.to_string()),
                None => Action::Other(other.to_string()),
            },
        }
    }
}

/// One container event: what happened, to which container. `name` is
/// the container's name without Docker's leading `/` (after a rename, the
/// new one); either is empty when the daemon left it out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContainerEvent {
    pub action: Action,
    pub id: String,
    pub name: String,
}

impl ContainerEvent {
    fn from_message(message: EventMessage) -> Self {
        let (id, mut attributes) = message
            .actor
            .map_or_default(|actor| (actor.id, actor.attributes));
        let name = attributes
            .as_mut()
            .and_then(|attributes| attributes.remove("name"))
            .unwrap_or_default();
        ContainerEvent {
            action: Action::from_daemon(message.action.as_deref().unwrap_or_default()),
            id: id.unwrap_or_default(),
            name: name.strip_prefix('/').unwrap_or(&name).to_string(),
        }
    }
}

/// The actions that can change a picker row: its state, status, health,
/// restart count, times or name. Not a health check's `exec_create`,
/// `exec_start` and `exec_die` (its result comes as `health_status`), nor
/// `attach`, `commit`, `copy`, `detach`, `export`, `resize` or `top`.
/// With `health_status` among them, dockerd matches each as a prefix of
/// the event's action (`matchEvent` in
/// <https://github.com/moby/moby/blob/docker-v29.0.0/daemon/events/filter.go>, and `FuzzyMatch` in
/// <https://github.com/moby/moby/blob/docker-v29.0.0/daemon/internal/filters/parse.go>), so
/// `health_status` takes `health_status: healthy` and its other states.
const ROW_ACTIONS: [&str; 13] = [
    "create",
    "start",
    "die",
    "destroy",
    "rename",
    "health_status",
    "stop",
    "kill",
    "pause",
    "unpause",
    "restart",
    "oom",
    "update",
];

/// The daemon's container messages, as bollard reads them with the
/// `type=container` filter and `filters`, boxed (`StreamExt::boxed`,
/// <https://docs.rs/futures-util/0.3.34/futures_util/stream/trait.StreamExt.html#method.boxed>),
/// so sb-docker alone compiles bollard's and hyper's client under it, not
/// sb-tui, which polls the events (ADR-026's amendment). Not inlined, so
/// no caller's crate gets the wrapper, and with it the stream's code,
/// back.
#[inline(never)]
fn messages<'a>(
    daemon: &Daemon,
    filters: impl IntoIterator<Item = (&'a str, Vec<&'a str>)>,
) -> BoxStream<'static, Result<EventMessage, bollard::errors::Error>> {
    let mut all = HashMap::from([("type", vec!["container"])]);
    all.extend(filters);
    let options = EventsOptionsBuilder::default().filters(&all).build();
    daemon.docker.events(Some(options)).boxed()
}

/// The daemon's container events from now on that can change a picker
/// row ([`ROW_ACTIONS`]). The stream ends when the daemon ends it, or
/// after its first error, which is its last item and carries the
/// daemon's message.
pub fn container_events(
    daemon: &Daemon,
) -> impl Stream<Item = anyhow::Result<ContainerEvent>> + Send + 'static {
    events(messages(daemon, [("event", ROW_ACTIONS.to_vec())]))
}

/// The `start` and `die` events from now on of the container named
/// `name`, for its log viewer, as [`container_events`] ends. dockerd
/// matches `name` as a prefix of a container's name or of its ID
/// (`fuzzyMatchName` in
/// <https://github.com/moby/moby/blob/docker-v29.0.0/daemon/events/filter.go>), so a reader
/// still compares the name.
pub fn starts_and_dies(
    daemon: &Daemon,
    name: &str,
) -> impl Stream<Item = anyhow::Result<ContainerEvent>> + Send + 'static {
    events(messages(
        daemon,
        [("container", vec![name]), ("event", vec!["start", "die"])],
    ))
}

/// The messages as events, `None` after an error.
fn events(
    events: BoxStream<'static, Result<EventMessage, bollard::errors::Error>>,
) -> impl Stream<Item = anyhow::Result<ContainerEvent>> + Send + 'static {
    // futures-util's documented way to turn a state machine into a stream
    // (<https://docs.rs/futures-util/0.3.34/futures_util/stream/fn.unfold.html>):
    // `None` once an error has been sent. Why not a plain `map`: bollard's
    // stream does end after an error today (a refused request is a
    // one-shot future; tokio-util's `FramedRead` ends after a decode or
    // read error), but bollard's docs don't promise it, and `FramedRead`
    // may read again if polled past that end.
    futures_util::stream::unfold(Some(events), |events| async move {
        let mut events = events?;
        match events.next().await? {
            Ok(message) => Some((Ok(ContainerEvent::from_message(message)), Some(events))),
            Err(err) => Some((
                Err(anyhow::Error::new(err).context("reading the container events")),
                None,
            )),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use bollard::models::EventActor;
    use serde_json::json;

    use crate::connect::connect;
    use crate::testing::{FakeEngine, Options, container_event};

    /// The stream's next item, or a failure after 5 s rather than a hung
    /// test run.
    async fn next<S: Stream + Unpin>(events: &mut S) -> Option<S::Item> {
        tokio::time::timeout(std::time::Duration::from_secs(5), events.next())
            .await
            .expect("the next item within 5 s")
    }

    #[test]
    fn the_named_actions_map_and_others_pass_through() {
        let cases = [
            ("create", Action::Create),
            ("start", Action::Start),
            ("die", Action::Die),
            ("destroy", Action::Destroy),
            ("rename", Action::Rename),
            (
                "health_status: healthy",
                Action::HealthStatus("healthy".into()),
            ),
            (
                "health_status: unhealthy",
                Action::HealthStatus("unhealthy".into()),
            ),
            ("stop", Action::Other("stop".into())),
            (
                "exec_start: /bin/sh -c true",
                Action::Other("exec_start: /bin/sh -c true".into()),
            ),
            ("", Action::Other(String::new())),
        ];
        for (daemon, action) in cases {
            assert_eq!(Action::from_daemon(daemon), action, "{daemon:?}");
        }
    }

    #[test]
    fn an_event_carries_the_id_and_the_name() {
        let message = EventMessage {
            action: Some("rename".into()),
            actor: Some(EventActor {
                id: Some("c0ffee".into()),
                attributes: Some(HashMap::from([
                    ("name".into(), "new".into()),
                    ("oldName".into(), "/old".into()),
                ])),
            }),
            ..EventMessage::default()
        };
        assert_eq!(
            ContainerEvent::from_message(message),
            ContainerEvent {
                action: Action::Rename,
                id: "c0ffee".into(),
                name: "new".into(),
            }
        );
    }

    #[test]
    fn a_missing_actor_reads_as_empty() {
        let event = ContainerEvent::from_message(EventMessage::default());
        assert_eq!(
            event,
            ContainerEvent {
                action: Action::Other(String::new()),
                id: String::new(),
                name: String::new(),
            }
        );
        // A leading `/`, as other Docker names have, is dropped.
        let message = EventMessage {
            actor: Some(EventActor {
                id: None,
                attributes: Some(HashMap::from([("name".into(), "/app".into())])),
            }),
            ..EventMessage::default()
        };
        assert_eq!(ContainerEvent::from_message(message).name, "app");
    }

    #[tokio::test]
    async fn container_events_come_until_the_daemon_ends_them() {
        let engine = FakeEngine::start(Options::default()).expect("fake Engine API");
        let daemon = connect(&engine.host()).await.expect("connect");
        engine.event(container_event("start", "c0ffee", "app"));
        // Not a container's: the type filter leaves it out.
        engine.event(json!({ "Type": "network", "Action": "connect", "Actor": { "ID": "n1" } }));
        // A health check's exec events, and an attach: no row changes, so
        // the daemon keeps them.
        for action in [
            "exec_create: true",
            "exec_start: true",
            "exec_die",
            "attach",
        ] {
            engine.event(container_event(action, "c0ffee", "app"));
        }
        engine.event(container_event("health_status: healthy", "c0ffee", "app"));
        let mut events = std::pin::pin!(container_events(&daemon));
        let first = next(&mut events).await.expect("an event").expect("read");
        assert_eq!(
            first,
            ContainerEvent {
                action: Action::Start,
                id: "c0ffee".into(),
                name: "app".into(),
            }
        );
        let second = next(&mut events).await.expect("an event").expect("read");
        assert_eq!(second.action, Action::HealthStatus("healthy".into()));
        // Sent as it comes.
        engine.event(container_event("die", "c0ffee", "app"));
        let third = next(&mut events).await.expect("an event").expect("read");
        assert_eq!(third.action, Action::Die);
        engine.end_events();
        assert!(next(&mut events).await.is_none());
        let path = engine.paths().pop().expect("a request");
        assert!(path.starts_with("/v1.53/events?"), "{path}");
        let query = path.split_once('?').map_or("", |(_, q)| q);
        let filters: HashMap<String, String> = form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect();
        let filters: HashMap<String, Vec<String>> =
            serde_json::from_str(&filters["filters"]).expect("a JSON object");
        assert_eq!(filters["type"], ["container"]);
        // Every action the viewer names, and the health states through
        // `health_status`'s prefix; no exec event.
        let asked = &filters["event"];
        for named in [
            "create",
            "start",
            "die",
            "destroy",
            "rename",
            "health_status",
        ] {
            assert!(asked.iter().any(|a| a == named), "{named}: {asked:?}");
        }
        assert!(!asked.iter().any(|a| a.starts_with("exec")), "{asked:?}");
        assert_eq!(filters.len(), 2, "{filters:?}");
    }

    /// One container's starts and dies, for its viewer: the daemon keeps
    /// every other action and every other container's, but for names it
    /// takes the name as a prefix of.
    #[tokio::test]
    async fn a_viewer_gets_its_containers_starts_and_dies() {
        let engine = FakeEngine::start(Options::default()).expect("fake Engine API");
        let daemon = connect(&engine.host()).await.expect("connect");
        engine.event(container_event("start", "beef", "other"));
        engine.event(container_event("health_status: healthy", "c0ffee", "app"));
        engine.event(container_event("exec_start: true", "c0ffee", "app"));
        engine.event(container_event("stop", "c0ffee", "app"));
        engine.event(container_event("start", "c0ffee", "app"));
        engine.event(container_event("die", "d00d", "app2"));
        engine.event(container_event("die", "c0ffee", "app"));
        engine.end_events();
        let events: Vec<ContainerEvent> = starts_and_dies(&daemon, "app")
            .map(|e| e.expect("read"))
            .collect()
            .await;
        let seen: Vec<(Action, &str)> = events
            .iter()
            .map(|e| (e.action.clone(), e.name.as_str()))
            .collect();
        assert_eq!(
            seen,
            [
                (Action::Start, "app"),
                (Action::Die, "app2"),
                (Action::Die, "app")
            ]
        );
        let path = engine.paths().pop().expect("a request");
        let query = path.split_once('?').map_or("", |(_, q)| q);
        let filters: HashMap<String, String> = form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect();
        let filters: HashMap<String, Vec<String>> =
            serde_json::from_str(&filters["filters"]).expect("a JSON object");
        assert_eq!(filters["type"], ["container"]);
        assert_eq!(filters["container"], ["app"]);
        assert_eq!(filters["event"], ["start", "die"]);
    }

    #[tokio::test]
    async fn a_daemon_error_ends_the_events_with_its_message() {
        let engine = FakeEngine::start(Options {
            refuse_unversioned: true,
            ..Options::default()
        })
        .expect("fake Engine API");
        // A client left unversioned on purpose (`connect` versions every
        // request), so the daemon refuses its `/events`.
        let daemon = Daemon {
            docker: bollard::Docker::connect_with_host(&engine.host()).expect("client"),
            address: engine.host(),
            version: None,
            api_version: None,
            request_version: *bollard::API_DEFAULT_VERSION,
        };
        let mut events = std::pin::pin!(container_events(&daemon));
        let err = next(&mut events)
            .await
            .expect("an item")
            .expect_err("an error");
        let text = format!("{err:#}");
        assert!(
            text.contains("reading the container events")
                && text.contains("API without a version prefix is not supported"),
            "{text}"
        );
        assert!(next(&mut events).await.is_none());
    }
}
