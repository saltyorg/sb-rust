//! `sb docker ps`'s rules (docker spec, "`sb docker ps` behaviour"): the
//! status rule, sb-go's Traefik rule, the binding format and order, and the
//! display name. Pure functions over bollard's model types.

use std::collections::HashMap;
use std::fmt::Write as _;

use std::net::{IpAddr, Ipv6Addr};
use std::time::Duration;

use bollard::models::{
    ContainerInspectResponse, ContainerState, ContainerStateStatusEnum as Status, ContainerSummary,
    ContainerSummaryHealthStatusEnum as ListHealth, ContainerSummaryStateEnum as ListState,
    HealthStatusEnum as Health, PortMap, PortSummary,
};
use jiff::Timestamp;
use sb_cli_core::theme::Role;
use sb_term_ui::{Cell, Table};

/// A container's status text and the role it is drawn in: sb-go's wording,
/// but an exit code decides whether a stopped container is a problem
/// (proto-inline review), and a failed first start (created with an error,
/// moby's daemon/start.go) is one. The text alone tells the states apart.
/// bollard's state enum has no catch-all, so an unknown state fails
/// deserialisation (the list call or that inspect; docker spec); an empty
/// or absent one reads `unknown`.
#[must_use]
pub fn status(state: &ContainerState) -> (String, Role) {
    let health = state.health.as_ref().and_then(|h| h.status);
    let exit_code = state.exit_code.unwrap_or_default();
    let failed_start = state.error.as_deref().is_some_and(|e| !e.is_empty());
    match state.status {
        Some(Status::RUNNING) => match health {
            Some(Health::HEALTHY) => ("running (healthy)".into(), Role::Ok),
            Some(Health::STARTING) => ("running (starting)".into(), Role::Warn),
            Some(Health::UNHEALTHY) => ("running (unhealthy)".into(), Role::Problem),
            Some(Health::NONE | Health::EMPTY) | None => ("running".into(), Role::Ok),
        },
        Some(Status::EXITED) if state.oom_killed == Some(true) => {
            ("exited (out of memory)".into(), Role::Problem)
        }
        Some(Status::EXITED) if exit_code != 0 => {
            (format!("exited (code {exit_code})"), Role::Problem)
        }
        Some(Status::EXITED) => ("exited".into(), Role::Idle),
        Some(Status::CREATED) if failed_start => {
            ("created (failed to start)".into(), Role::Problem)
        }
        Some(Status::CREATED) => ("created".into(), Role::Idle),
        Some(Status::PAUSED) => ("paused".into(), Role::Idle),
        Some(Status::RESTARTING) => ("restarting".into(), Role::Problem),
        Some(Status::DEAD) => ("dead".into(), Role::Problem),
        Some(Status::REMOVING) => ("removing".into(), Role::Warn),
        Some(Status::STOPPING) => ("stopping".into(), Role::Warn),
        Some(Status::EMPTY) | None => ("unknown".into(), Role::Warn),
    }
}

/// The ports Traefik routes to, from Saltbox's labels: sb-go's rule
/// (`traefik.http.services.<name>.loadbalancer.server.port`, skipping the
/// `-http` redirect services, more than three dot-separated parts), with
/// empty values dropped, deduplicated, numbers in numeric order and then
/// any other text (Go's map order made sb-go's vary).
#[must_use]
pub fn traefik_ports<S: std::hash::BuildHasher>(
    labels: &HashMap<String, String, S>,
) -> Vec<String> {
    let mut ports: Vec<&String> = labels
        .iter()
        .filter(|(key, value)| {
            key.contains("traefik.http.services")
                && key.contains(".loadbalancer.server.port")
                && !key.contains("-http")
                // sb-go's `len(parts) > 3`, kept for parity: the two
                // substring checks above already imply more than three parts.
                && key.split('.').count() > 3
                && !value.is_empty()
        })
        .map(|(_, value)| value)
        .collect();
    ports.sort_by_key(|p| (number(p), p.as_str()));
    ports.dedup();
    ports.into_iter().cloned().collect()
}

/// One host port binding, as Docker gives it (an unset address as
/// `0.0.0.0`, as sb-go shows it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub host_ip: String,
    pub host_port: String,
    pub container_port: String,
    pub protocol: String,
}

impl Binding {
    fn is_ipv6(&self) -> bool {
        self.host_ip.parse::<Ipv6Addr>().is_ok()
    }

    /// The container port and protocol as Docker writes them (`80/tcp`).
    fn container(&self) -> String {
        if self.protocol.is_empty() {
            self.container_port.clone()
        } else {
            format!("{}/{}", self.container_port, self.protocol)
        }
    }
}

/// A port's number for ordering; anything else sorts after every number.
fn number(port: &str) -> u64 {
    port.parse::<u32>().map_or(u64::MAX, u64::from)
}

/// The container's host port bindings, from the inspect's `PortMap`:
/// those with a host port, ordered by container port number, then
/// protocol, then IPv4 before IPv6, then address, then host port (docker
/// spec, Port Bindings).
#[must_use]
pub fn bindings(ports: &PortMap) -> Vec<Binding> {
    let found: Vec<Binding> = ports
        .iter()
        .flat_map(|(port, list)| {
            let (container_port, protocol) = port.split_once('/').unwrap_or((port, ""));
            list.iter().flatten().filter_map(move |b| {
                let host_port = b.host_port.clone().filter(|p| !p.is_empty())?;
                let host_ip = b
                    .host_ip
                    .clone()
                    .filter(|ip| !ip.is_empty())
                    .unwrap_or_else(|| "0.0.0.0".into());
                Some(Binding {
                    host_ip,
                    host_port,
                    container_port: container_port.into(),
                    protocol: protocol.into(),
                })
            })
        })
        .collect();
    in_order(found)
}

/// The same bindings from the list's `PortSummary` entries, which moby
/// makes one per binding of the same `PortMap` (`daemon/container/view.go`):
/// an entry without a host port (an exposed port, or a routed one, port
/// 0) is left out, as the inspect's empty host port is; an empty address
/// reads `0.0.0.0` as there.
#[must_use]
pub fn listed_bindings(ports: &[PortSummary]) -> Vec<Binding> {
    let found: Vec<Binding> = ports
        .iter()
        .filter_map(|port| {
            let host_port = port.public_port.filter(|p| *p != 0)?;
            Some(Binding {
                host_ip: port
                    .ip
                    .clone()
                    .filter(|ip| !ip.is_empty())
                    .unwrap_or_else(|| "0.0.0.0".into()),
                host_port: host_port.to_string(),
                container_port: port.private_port.to_string(),
                protocol: port.typ.map_or_default(|typ| typ.to_string()),
            })
        })
        .collect();
    in_order(found)
}

/// `found` in the Port Bindings column's order.
fn in_order(mut found: Vec<Binding>) -> Vec<Binding> {
    // `IpAddr`'s order puts every IPv4 address before any IPv6 one and
    // compares addresses numerically (one that doesn't parse sorts first;
    // Docker gives none); the raw strings break the last ties, so the order
    // never depends on the map's.
    found.sort_by_cached_key(|b| {
        (
            number(&b.container_port),
            b.protocol.clone(),
            b.host_ip.parse::<IpAddr>().ok(),
            number(&b.host_port),
            b.host_ip.clone(),
            b.container_port.clone(),
            b.host_port.clone(),
        )
    });
    found
}

/// The Port Bindings column's lines: sb-go's `ip:host->port/proto`, every
/// IPv6 address bracketed (sb-go brackets only `::`). The host and
/// container ports are padded to one width, so the part after the address
/// is the same width on every line and the arrows line up in the table's
/// right-aligned column (term-ui aligns each line of a cell on its own).
#[must_use]
pub fn binding_lines(bindings: &[Binding]) -> Vec<String> {
    let host_width = bindings
        .iter()
        .map(|b| b.host_port.len())
        .max()
        .unwrap_or(0);
    let container_width = bindings
        .iter()
        .map(|b| b.container().len())
        .max()
        .unwrap_or(0);
    bindings
        .iter()
        .map(|b| {
            let ip = if b.is_ipv6() {
                format!("[{}]", b.host_ip)
            } else {
                b.host_ip.clone()
            };
            format!(
                "{ip}:{:>host_width$}->{:>container_width$}",
                b.host_port,
                b.container()
            )
        })
        .collect()
}

/// The Container column: the first name without its leading `/`, else the
/// short ID (sb-go's `containerDisplayName`).
pub fn display_name(id: &str, names: &[String]) -> String {
    names
        .first()
        .map(|name| name.strip_prefix('/').unwrap_or(name))
        .filter(|name| !name.is_empty())
        .map_or_else(|| short_id(id), str::to_string)
}

/// The first 12 characters of an ID, `unknown` when empty (sb-go's
/// `shortContainerID`).
#[must_use]
pub fn short_id(id: &str) -> String {
    match id {
        "" => "unknown".into(),
        id => id.chars().take(12).collect(),
    }
}

/// One container's row: what the table and the JSON show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub id: String,
    pub state: String,
    pub health: Option<String>,
    pub exit_code: i64,
    pub oom_killed: bool,
    pub status: String,
    pub role: Role,
    pub traefik: Vec<String>,
    pub bindings: Vec<Binding>,
    /// How often Docker restarted it (`RestartCount`).
    pub restart_count: i64,
    /// When it was created (`Created`). This and the two times below are
    /// `None` for Docker's zero time (never) and for a value that doesn't
    /// parse.
    pub created: Option<Timestamp>,
    /// When it last started (`State.StartedAt`).
    pub started_at: Option<Timestamp>,
    /// When it last stopped (`State.FinishedAt`).
    pub finished_at: Option<Timestamp>,
}

impl Row {
    /// The row for a container from the list (`summary`: its ID and names,
    /// as sb-go) and its inspect (state, labels, bindings).
    pub fn new(summary: &ContainerSummary, inspect: &ContainerInspectResponse) -> Self {
        let id = summary.id.clone().unwrap_or_default();
        let names = summary.names.as_deref().unwrap_or_default();
        let state = inspect.state.clone().unwrap_or_default();
        let (status, role) = status(&state);
        let labels = inspect
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref());
        let ports = inspect
            .network_settings
            .as_ref()
            .and_then(|settings| settings.ports.as_ref());
        Row {
            name: display_name(&id, names),
            state: state.status.map_or_default(|s| s.to_string()),
            // `none` and empty mean no health check (docker spec).
            health: state
                .health
                .and_then(|h| h.status)
                .filter(|h| !matches!(h, Health::NONE | Health::EMPTY))
                .map(|h| h.to_string()),
            exit_code: state.exit_code.unwrap_or_default(),
            oom_killed: state.oom_killed.unwrap_or_default(),
            status,
            role,
            traefik: labels.map_or_default(traefik_ports),
            bindings: ports.map_or_default(bindings),
            restart_count: inspect.restart_count.unwrap_or_default(),
            created: inspect.created.as_deref().and_then(docker_time),
            started_at: state.started_at.as_deref().and_then(docker_time),
            finished_at: state.finished_at.as_deref().and_then(docker_time),
            id,
        }
    }

    /// A running container's row from its list entry alone, where the
    /// entry says its health (Engine API 1.52 and later; the caller checks
    /// the version), with [`status`]'s text and role: the same row as
    /// [`Row::new`]'s but for the restart count (0) and the times (none),
    /// which only the inspect has. A running container's exit code is 0
    /// and it wasn't killed for memory (moby's `setRunning` clears both).
    /// `None` for any other container, or an entry without its health:
    /// that one needs its inspect.
    #[must_use]
    pub fn from_summary(summary: &ContainerSummary) -> Option<Self> {
        if summary.state != Some(ListState::RUNNING) {
            return None;
        }
        let health = match summary.health.as_ref()?.status? {
            ListHealth::HEALTHY => Health::HEALTHY,
            ListHealth::STARTING => Health::STARTING,
            ListHealth::UNHEALTHY => Health::UNHEALTHY,
            ListHealth::NONE => Health::NONE,
            ListHealth::EMPTY => Health::EMPTY,
        };
        let state = ContainerState {
            status: Some(Status::RUNNING),
            health: Some(bollard::models::Health {
                status: Some(health),
                ..bollard::models::Health::default()
            }),
            ..ContainerState::default()
        };
        let (status, role) = status(&state);
        let id = summary.id.clone().unwrap_or_default();
        let names = summary.names.as_deref().unwrap_or_default();
        Some(Row {
            name: display_name(&id, names),
            state: Status::RUNNING.to_string(),
            health: Some(health)
                .filter(|h| !matches!(h, Health::NONE | Health::EMPTY))
                .map(|h| h.to_string()),
            exit_code: 0,
            oom_killed: false,
            status,
            role,
            traefik: summary.labels.as_ref().map_or_default(traefik_ports),
            bindings: summary.ports.as_deref().map_or_default(listed_bindings),
            restart_count: 0,
            created: None,
            started_at: None,
            finished_at: None,
            id,
        })
    }
}

/// One of the inspect's RFC 3339 times, parsed with jiff
/// (<https://docs.rs/jiff/0.2.38/jiff/struct.Timestamp.html#impl-FromStr-for-Timestamp>).
/// Docker's zero time, `0001-01-01T00:00:00Z` (Go's zero `time.Time`),
/// means never: `None`, as is anything that doesn't parse.
fn docker_time(text: &str) -> Option<Timestamp> {
    /// `0001-01-01T00:00:00Z` in Unix seconds.
    const ZERO: Timestamp = Timestamp::constant(-62_135_596_800, 0);
    text.parse().ok().filter(|time| *time != ZERO)
}

/// A container's status in the log picker (tui spec, "Container
/// status"): `ps`'s text with its time and restart details, the symbol
/// drawn before it, its role, and whether it belongs in Needs attention.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerStatus {
    pub symbol: char,
    pub text: String,
    pub role: Role,
    pub attention: bool,
}

/// How long a container counts as just restarted (the 10-minute rule).
const RECENT_RESTART: Duration = Duration::from_mins(10);

/// `row`'s status in the log picker at `now` (the docker spec's picker
/// status): [`status`]'s text and role, then
/// - running: ` · up <duration>`; restarted and up for less than 10
///   minutes, ` · restarted N× · up <duration>`, `↻`, role Problem;
/// - exited or dead: ` · <duration> ago`;
/// - restarting: ` (N×)`, `↻`.
///
/// A time Docker doesn't give leaves its part out. `format` writes the
/// durations (the picker passes sb-host's, so motd and the picker write
/// times alike; sb-docker doesn't depend on sb-host). Attention is role
/// Problem.
pub fn picker_status(
    row: &Row,
    now: Timestamp,
    format: impl Fn(Duration) -> String,
) -> PickerStatus {
    let since = |time: Option<Timestamp>| time.map(|time| elapsed(time, now));
    let mut text = row.status.clone();
    let mut role = row.role;
    let mut cycling = false;
    match row.state.as_str() {
        "running" => {
            if let Some(up) = since(row.started_at) {
                if row.restart_count > 0 && up < RECENT_RESTART {
                    let _ = write!(text, " · restarted {}×", row.restart_count);
                    role = Role::Problem;
                    cycling = true;
                }
                let _ = write!(text, " · up {}", format(up));
            }
        }
        "exited" | "dead" => {
            if let Some(ago) = since(row.finished_at) {
                let _ = write!(text, " · {} ago", format(ago));
            }
        }
        "restarting" => {
            let _ = write!(text, " ({}×)", row.restart_count);
            cycling = true;
        }
        _ => {}
    }
    let symbol = if cycling {
        '↻'
    } else {
        match role {
            Role::Ok => '✓',
            Role::Problem => '✗',
            Role::Warn => '◌',
            // `status` gives only the four state roles; Idle and anything
            // else read as stopped.
            _ => '○',
        }
    };
    PickerStatus {
        symbol,
        text,
        role,
        attention: role == Role::Problem,
    }
}

/// `now` minus `then`, zero when `then` is ahead (a clock moved back).
fn elapsed(then: Timestamp, now: Timestamp) -> Duration {
    Duration::try_from(now.duration_since(then)).unwrap_or(Duration::ZERO)
}

/// `rows` sorted by name, as sb-go.
#[must_use]
pub fn sorted(mut rows: Vec<Row>) -> Vec<Row> {
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

/// The table in sb-go's columns: headings centred and bold (term-ui), the
/// port columns right-aligned, each status in its role, the bindings one
/// per line (on a narrow terminal each wraps at any character, not at its
/// padding), columns capped at 100 as sb-go.
#[must_use]
pub fn table(rows: &[Row]) -> Table {
    let mut table = Table::new(["Container", "Status", "Traefik Port", "Port Bindings"])
        .right_align(2)
        .right_align(3)
        .wrap_anywhere(3)
        .max_column_width(100);
    for row in rows {
        table.row([
            Cell::new(row.name.clone()),
            Cell::new(row.status.clone()).role(row.role),
            Cell::new(
                row.traefik
                    .iter()
                    .map(|port| one_short_line(port))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            Cell::new(binding_lines(&row.bindings).join("\n")),
        ]);
    }
    table
}

/// A Traefik value as the table shows it: one line (newline and carriage
/// return as spaces) of at most 64 characters, cut with `…` (a port is
/// at most five digits; a label is the image's text). `--json` keeps it
/// whole. Why not console's `truncate_str`
/// (<https://docs.rs/console/0.16.6/console/fn.truncate_str.html>): it
/// counts display width, not characters, and console is only a
/// dev-dependency of sb-docker; the cell is sanitized by `Table` anyway.
fn one_short_line(value: &str) -> String {
    const MAX: usize = 64;
    let line: String = value
        .chars()
        .map(|c| if matches!(c, '\n' | '\r') { ' ' } else { c })
        .collect();
    if line.chars().count() <= MAX {
        return line;
    }
    let mut cut: String = line.chars().take(MAX - 1).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::fixture_rows;
    use bollard::models::{
        ContainerStateStatusEnum as S, Health, HealthStatusEnum as H, PortBinding,
    };

    fn state(status: Option<S>, health: Option<H>) -> ContainerState {
        ContainerState {
            status,
            health: health.map(|status| Health {
                status: Some(status),
                ..Health::default()
            }),
            ..ContainerState::default()
        }
    }

    fn exited(code: i64, oom: bool) -> ContainerState {
        ContainerState {
            exit_code: Some(code),
            oom_killed: Some(oom),
            ..state(Some(S::EXITED), None)
        }
    }

    /// Every row of the spec's status table.
    #[test]
    fn running_states_follow_their_health() {
        let cases = [
            (Some(H::HEALTHY), "running (healthy)", Role::Ok),
            (Some(H::STARTING), "running (starting)", Role::Warn),
            (Some(H::UNHEALTHY), "running (unhealthy)", Role::Problem),
            (Some(H::NONE), "running", Role::Ok),
            (Some(H::EMPTY), "running", Role::Ok),
            (None, "running", Role::Ok),
        ];
        for (health, text, role) in cases {
            assert_eq!(
                status(&state(Some(S::RUNNING), health)),
                (text.to_string(), role),
                "{health:?}"
            );
        }
    }

    /// An exit code decides whether a stopped container is a problem.
    #[test]
    fn exited_containers_name_their_code() {
        assert_eq!(status(&exited(0, false)), ("exited".into(), Role::Idle));
        assert_eq!(
            status(&exited(137, false)),
            ("exited (code 137)".into(), Role::Problem)
        );
        assert_eq!(
            status(&exited(137, true)),
            ("exited (out of memory)".into(), Role::Problem)
        );
    }

    /// A failed first start leaves the container `created` with its error
    /// (moby's daemon/start.go; platform note).
    #[test]
    fn a_failed_start_is_a_problem() {
        let failed = ContainerState {
            error: Some("port is already allocated".into()),
            exit_code: Some(128),
            ..state(Some(S::CREATED), None)
        };
        assert_eq!(
            status(&failed),
            ("created (failed to start)".into(), Role::Problem)
        );
        let empty_error = ContainerState {
            error: Some(String::new()),
            ..state(Some(S::CREATED), None)
        };
        assert_eq!(status(&empty_error), ("created".into(), Role::Idle));
    }

    #[test]
    fn the_other_states() {
        let cases = [
            (Some(S::CREATED), "created", Role::Idle),
            (Some(S::PAUSED), "paused", Role::Idle),
            (Some(S::RESTARTING), "restarting", Role::Problem),
            (Some(S::DEAD), "dead", Role::Problem),
            (Some(S::REMOVING), "removing", Role::Warn),
            (Some(S::STOPPING), "stopping", Role::Warn),
            (Some(S::EMPTY), "unknown", Role::Warn),
            (None, "unknown", Role::Warn),
        ];
        for (s, text, role) in cases {
            assert_eq!(status(&state(s, None)), (text.to_string(), role), "{s:?}");
        }
    }

    fn labels(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// sb-go's rule: the service's load-balancer port, not the `-http`
    /// redirect services; empty values dropped, deduplicated, numbers in
    /// numeric order, then any other text.
    #[test]
    fn traefik_ports_follow_sb_go_rule() {
        let l = labels(&[
            (
                "traefik.http.services.plex.loadbalancer.server.port",
                "32400",
            ),
            (
                "traefik.http.services.plex-http.loadbalancer.server.port",
                "80",
            ),
            ("traefik.http.services.web.loadbalancer.server.port", "8080"),
            ("traefik.http.services.api.loadbalancer.server.port", "8080"),
            ("traefik.http.services.alt.loadbalancer.server.port", "900"),
            ("traefik.http.services.odd.loadbalancer.server.port", "web"),
            ("traefik.http.services.none.loadbalancer.server.port", ""),
            ("traefik.http.routers.plex.rule", "Host(`x`)"),
            ("com.example.loadbalancer.server.port", "1"),
        ]);
        assert_eq!(traefik_ports(&l), ["900", "8080", "32400", "web"]);
        assert_eq!(traefik_ports(&HashMap::new()), [] as [String; 0]);
    }

    fn port_map(entries: &[(&str, &[(&str, &str)])]) -> PortMap {
        entries
            .iter()
            .map(|(port, bs)| {
                let list = bs
                    .iter()
                    .map(|(ip, host)| PortBinding {
                        host_ip: (!ip.is_empty()).then(|| ip.to_string()),
                        host_port: Some(host.to_string()),
                    })
                    .collect();
                (port.to_string(), Some(list))
            })
            .collect()
    }

    fn order(ports: &PortMap) -> Vec<String> {
        bindings(ports)
            .into_iter()
            .map(|b| format!("{}:{}->{}", b.host_ip, b.host_port, b.container()))
            .collect()
    }

    /// Bindings without a host port are left out; an unset address is
    /// 0.0.0.0.
    #[test]
    fn bindings_keep_only_published_ports() {
        let mut ports = port_map(&[
            ("80/tcp", &[("", "8080")]),
            ("9000/tcp", &[("0.0.0.0", "")]),
        ]);
        ports.insert("9999/tcp".into(), None);
        assert_eq!(order(&ports), ["0.0.0.0:8080->80/tcp"]);
    }

    /// The list's ports give the inspect's bindings, in the same order:
    /// each binding's entry (an empty address as `0.0.0.0`, IPv6 kept),
    /// without the entries of a port with no host port (exposed only, or
    /// routed with port 0). Built from the inspect's map as moby builds
    /// the list's (`testing::summary`).
    #[test]
    fn listed_bindings_are_the_inspects() {
        let mut ports = port_map(&[
            ("32400/tcp", &[("0.0.0.0", "32400"), ("::", "32400")]),
            (
                "53/udp",
                &[
                    ("", "5353"),
                    ("198.51.100.10", "53"),
                    ("198.51.100.9", "53"),
                ],
            ),
            ("53/tcp", &[("2001:db8::1", "53")]),
            ("9000/tcp", &[("0.0.0.0", "")]),
        ]);
        ports.insert("1900/udp".into(), None);
        let inspect = serde_json::json!({
            "Id": "c1",
            "NetworkSettings": { "Ports": serde_json::to_value(&ports).expect("ports") },
        });
        let summary: ContainerSummary =
            serde_json::from_value(crate::testing::summary(&inspect)).expect("summary");
        let listed = summary.ports.as_deref().expect("ports");
        assert_eq!(listed.len(), 8);
        assert_eq!(listed_bindings(listed), bindings(&ports));
        assert_eq!(bindings(&ports).len(), 6);
        let routed = PortSummary {
            ip: Some("0.0.0.0".into()),
            private_port: 80,
            public_port: Some(0),
            typ: None,
        };
        assert_eq!(listed_bindings(&[routed]), []);
    }

    /// A running container's row from its list entry is the inspect's
    /// row, but for the restart count and the times, for every health the
    /// list can say; any other state, and an entry without its health
    /// (older API), needs the inspect.
    #[test]
    fn a_running_containers_row_comes_from_its_list_entry() {
        let inspects = crate::testing::inspects();
        let mut seen = 0;
        for inspect in &inspects {
            let summary: ContainerSummary =
                serde_json::from_value(crate::testing::summary(inspect)).expect("summary");
            let full: ContainerInspectResponse =
                serde_json::from_value(inspect.clone()).expect("inspect");
            let row = Row::from_summary(&summary);
            if inspect.pointer("/State/Status") != Some(&serde_json::json!("running")) {
                assert_eq!(row, None, "{inspect}");
                continue;
            }
            seen += 1;
            let mut expected = Row::new(&summary, &full);
            expected.restart_count = 0;
            expected.created = None;
            expected.started_at = None;
            expected.finished_at = None;
            assert_eq!(row, Some(expected));
            let mut older = summary.clone();
            older.health = None;
            assert_eq!(Row::from_summary(&older), None);
        }
        assert_eq!(seen, 3);
        for (word, text, role) in [
            ("healthy", "running (healthy)", Role::Ok),
            ("starting", "running (starting)", Role::Warn),
            ("unhealthy", "running (unhealthy)", Role::Problem),
            ("none", "running", Role::Ok),
            ("", "running", Role::Ok),
        ] {
            let summary: ContainerSummary = serde_json::from_value(serde_json::json!({
                "Id": "0123456789abcdef",
                "State": "running",
                "Health": { "Status": word },
            }))
            .expect("summary");
            let row = Row::from_summary(&summary).expect("a row");
            assert_eq!((row.status.as_str(), row.role), (text, role), "{word}");
            assert_eq!(row.name, "0123456789ab");
            assert_eq!(
                row.health.as_deref(),
                Some(word).filter(|w| !matches!(*w, "none" | ""))
            );
            assert_eq!((row.exit_code, row.oom_killed), (0, false));
        }
        for state in ["restarting", "paused", "exited", "created", "dead"] {
            let summary: ContainerSummary = serde_json::from_value(serde_json::json!({
                "Id": "c1", "State": state, "Health": { "Status": "healthy" },
            }))
            .expect("summary");
            assert_eq!(Row::from_summary(&summary), None, "{state}");
        }
    }

    /// Container ports in numeric order, not text order.
    #[test]
    fn bindings_order_container_ports_by_number() {
        let ports = port_map(&[
            ("10000/tcp", &[("0.0.0.0", "1")]),
            ("9000/tcp", &[("0.0.0.0", "2")]),
        ]);
        assert_eq!(
            order(&ports),
            ["0.0.0.0:2->9000/tcp", "0.0.0.0:1->10000/tcp"]
        );
    }

    /// Then by protocol.
    #[test]
    fn bindings_order_by_protocol_next() {
        let ports = port_map(&[
            ("53/udp", &[("0.0.0.0", "1")]),
            ("53/tcp", &[("0.0.0.0", "2")]),
        ]);
        assert_eq!(order(&ports), ["0.0.0.0:2->53/tcp", "0.0.0.0:1->53/udp"]);
    }

    /// Then IPv4 before IPv6 (an IPv6 address that sorts first as text),
    /// then addresses numerically (.9 before .10).
    #[test]
    fn bindings_order_ipv4_first_and_addresses_by_number() {
        let ports = port_map(&[(
            "80/tcp",
            &[
                ("2001:db8::1", "80"),
                ("198.51.100.10", "80"),
                ("198.51.100.9", "80"),
            ],
        )]);
        assert_eq!(
            order(&ports),
            [
                "198.51.100.9:80->80/tcp",
                "198.51.100.10:80->80/tcp",
                "2001:db8::1:80->80/tcp"
            ]
        );
    }

    /// The address decides before the host port; the host port breaks a
    /// tie on the same address, numerically.
    #[test]
    fn bindings_order_by_address_then_host_port() {
        let ports = port_map(&[(
            "80/tcp",
            &[
                ("198.51.100.20", "80"),
                ("198.51.100.10", "9000"),
                ("198.51.100.10", "10000"),
            ],
        )]);
        assert_eq!(
            order(&ports),
            [
                "198.51.100.10:9000->80/tcp",
                "198.51.100.10:10000->80/tcp",
                "198.51.100.20:80->80/tcp"
            ]
        );
    }

    /// sb-go's format: host and container ports padded to one width (the
    /// arrows line up in the right-aligned column); every IPv6 address
    /// bracketed.
    #[test]
    fn binding_lines_line_up() {
        let ports = port_map(&[
            ("80/tcp", &[("", "8080"), ("::1", "8080")]),
            ("32400/tcp", &[("::", "32400")]),
        ]);
        assert_eq!(
            binding_lines(&bindings(&ports)),
            [
                "0.0.0.0: 8080->   80/tcp",
                "[::1]: 8080->   80/tcp",
                "[::]:32400->32400/tcp",
            ]
        );
        assert_eq!(binding_lines(&[]), [] as [String; 0]);
        // A port Docker gave without a protocol keeps no slash.
        let bare = port_map(&[("80", &[("", "80")])]);
        assert_eq!(binding_lines(&bindings(&bare)), ["0.0.0.0:80->80"]);
    }

    /// sb-go's `containerDisplayName` and `TestShortContainerID` cases.
    #[test]
    fn names_fall_back_to_the_short_id() {
        assert_eq!(display_name("abc", &["/plex".into()]), "plex");
        assert_eq!(display_name("abc", &["/".into()]), "abc");
        assert_eq!(display_name("abc", &["//odd".into()]), "/odd");
        assert_eq!(display_name("123456789012345", &[]), "123456789012");
        assert_eq!(short_id("short"), "short");
        assert_eq!(short_id(""), "unknown");
        assert_eq!(short_id("123456789012345"), "123456789012");
        assert_eq!(short_id("123456789012"), "123456789012");
    }

    /// A row from an inspect (`state` is its `State` object), with the
    /// picker's fields given.
    fn picker_row(inspect: &serde_json::Value) -> Row {
        let summary: ContainerSummary =
            serde_json::from_value(crate::testing::summary(inspect)).expect("summary");
        let inspect: ContainerInspectResponse =
            serde_json::from_value(inspect.clone()).expect("inspect");
        Row::new(&summary, &inspect)
    }

    fn at(text: &str) -> Timestamp {
        text.parse().expect("timestamp")
    }

    #[test]
    fn rows_carry_the_restart_count_and_the_times() {
        let row = picker_row(&serde_json::json!({
            "Id": "a".repeat(64),
            "Name": "/sonarr",
            "Created": "2026-11-01T10:00:00.123456789Z",
            "RestartCount": 3,
            "State": {
                "Status": "running",
                "StartedAt": "2026-11-13T09:58:00.5Z",
                "FinishedAt": "2026-11-13T09:57:59Z",
            },
        }));
        assert_eq!(row.restart_count, 3);
        assert_eq!(row.created, Some(at("2026-11-01T10:00:00.123456789Z")));
        assert_eq!(row.started_at, Some(at("2026-11-13T09:58:00.5Z")));
        assert_eq!(row.finished_at, Some(at("2026-11-13T09:57:59Z")));
    }

    /// Docker writes its zero time for "never"; that, a value that doesn't
    /// parse and a missing one read as no time.
    #[test]
    fn the_zero_time_and_unparsable_times_are_none() {
        let row = picker_row(&serde_json::json!({
            "Id": "b".repeat(64),
            "Name": "/lidarr",
            "Created": "yesterday",
            "State": {
                "Status": "created",
                "StartedAt": "0001-01-01T00:00:00Z",
            },
        }));
        assert_eq!(row.restart_count, 0);
        assert_eq!(
            (row.created, row.started_at, row.finished_at),
            (None, None, None)
        );
    }

    /// `"5s"`-style text, enough to see which duration was formatted.
    fn secs(d: Duration) -> String {
        format!("{}s", d.as_secs())
    }

    const NOW: &str = "2026-11-13T10:00:00Z";

    fn picker(state: &serde_json::Value, restarts: i64) -> PickerStatus {
        let row = picker_row(&serde_json::json!({
            "Id": "c".repeat(64),
            "Name": "/radarr",
            "RestartCount": restarts,
            "State": state,
        }));
        picker_status(&row, at(NOW), secs)
    }

    fn want(symbol: char, text: &str, role: Role, attention: bool) -> PickerStatus {
        PickerStatus {
            symbol,
            text: text.into(),
            role,
            attention,
        }
    }

    #[test]
    fn a_running_container_shows_its_uptime() {
        let state = serde_json::json!({ "Status": "running", "StartedAt": "2026-11-13T09:00:00Z" });
        assert_eq!(
            picker(&state, 0),
            want('✓', "running · up 3600s", Role::Ok, false)
        );
        let unhealthy = serde_json::json!({
            "Status": "running",
            "StartedAt": "2026-11-13T09:00:00Z",
            "Health": { "Status": "unhealthy" },
        });
        assert_eq!(
            picker(&unhealthy, 0),
            want('✗', "running (unhealthy) · up 3600s", Role::Problem, true)
        );
        let starting = serde_json::json!({
            "Status": "running",
            "StartedAt": "2026-11-13T09:59:00Z",
            "Health": { "Status": "starting" },
        });
        assert_eq!(
            picker(&starting, 0),
            want('◌', "running (starting) · up 60s", Role::Warn, false)
        );
    }

    /// The 10-minute rule: restarted and up for less than 10 minutes.
    #[test]
    fn a_recent_restart_needs_attention_for_ten_minutes() {
        let up = |since: &str| serde_json::json!({ "Status": "running", "StartedAt": since });
        assert_eq!(
            picker(&up("2026-11-13T09:51:00Z"), 3),
            want('↻', "running · restarted 3× · up 540s", Role::Problem, true)
        );
        assert_eq!(
            picker(&up("2026-11-13T09:50:01Z"), 1),
            want('↻', "running · restarted 1× · up 599s", Role::Problem, true)
        );
        assert_eq!(
            picker(&up("2026-11-13T09:50:00Z"), 3),
            want('✓', "running · up 600s", Role::Ok, false)
        );
        assert_eq!(
            picker(&up("2026-11-13T09:49:00Z"), 3),
            want('✓', "running · up 660s", Role::Ok, false)
        );
    }

    /// A crash loop with a health check: the rule makes a starting
    /// container a problem.
    #[test]
    fn a_recent_restart_with_a_health_check_is_a_problem() {
        let state = serde_json::json!({
            "Status": "running",
            "StartedAt": "2026-11-13T09:59:00Z",
            "Health": { "Status": "starting" },
        });
        assert_eq!(
            picker(&state, 2),
            want(
                '↻',
                "running (starting) · restarted 2× · up 60s",
                Role::Problem,
                true
            )
        );
    }

    /// A count below zero (never from Docker) is no restart.
    #[test]
    fn a_negative_restart_count_is_no_restart() {
        let state = serde_json::json!({ "Status": "running", "StartedAt": "2026-11-13T09:59:00Z" });
        assert_eq!(
            picker(&state, -1),
            want('✓', "running · up 60s", Role::Ok, false)
        );
    }

    #[test]
    fn a_running_container_without_a_start_time_shows_no_uptime() {
        let state = serde_json::json!({ "Status": "running", "StartedAt": "0001-01-01T00:00:00Z" });
        assert_eq!(picker(&state, 2), want('✓', "running", Role::Ok, false));
    }

    /// A start time ahead of the clock (a clock moved back) is up 0 s.
    #[test]
    fn a_start_time_in_the_future_is_up_no_time() {
        let state = serde_json::json!({ "Status": "running", "StartedAt": "2026-11-13T10:05:00Z" });
        assert_eq!(
            picker(&state, 1),
            want('↻', "running · restarted 1× · up 0s", Role::Problem, true)
        );
    }

    #[test]
    fn stopped_containers_show_how_long_ago() {
        let exited = |code: i64| {
            serde_json::json!({
                "Status": "exited",
                "ExitCode": code,
                "FinishedAt": "2026-11-13T08:00:00Z",
            })
        };
        assert_eq!(
            picker(&exited(3), 0),
            want('✗', "exited (code 3) · 7200s ago", Role::Problem, true)
        );
        assert_eq!(
            picker(&exited(0), 4),
            want('○', "exited · 7200s ago", Role::Idle, false)
        );
        let dead = serde_json::json!({ "Status": "dead", "FinishedAt": "2026-11-13T09:59:30Z" });
        assert_eq!(
            picker(&dead, 0),
            want('✗', "dead · 30s ago", Role::Problem, true)
        );
        let never = serde_json::json!({ "Status": "exited", "FinishedAt": "0001-01-01T00:00:00Z" });
        assert_eq!(picker(&never, 0), want('○', "exited", Role::Idle, false));
    }

    #[test]
    fn a_restarting_container_shows_its_count() {
        let state =
            serde_json::json!({ "Status": "restarting", "StartedAt": "2026-11-13T09:59:00Z" });
        assert_eq!(
            picker(&state, 5),
            want('↻', "restarting (5×)", Role::Problem, true)
        );
    }

    /// The other states keep `ps`'s text, with the role's symbol.
    #[test]
    fn the_other_states_keep_their_text() {
        let cases = [
            ("created", '○', "created", Role::Idle, false),
            ("paused", '○', "paused", Role::Idle, false),
            ("removing", '◌', "removing", Role::Warn, false),
            ("stopping", '◌', "stopping", Role::Warn, false),
        ];
        for (state, symbol, text, role, attention) in cases {
            let state = serde_json::json!({
                "Status": state,
                "StartedAt": "2026-11-13T09:59:00Z",
                "FinishedAt": "2026-11-13T09:59:30Z",
            });
            assert_eq!(
                picker(&state, 2),
                want(symbol, text, role, attention),
                "{text}"
            );
        }
        let unknown = serde_json::json!({ "StartedAt": "2026-11-13T09:59:00Z" });
        assert_eq!(picker(&unknown, 1), want('◌', "unknown", Role::Warn, false));
        let failed =
            serde_json::json!({ "Status": "created", "Error": "port is already allocated" });
        assert_eq!(
            picker(&failed, 0),
            want('✗', "created (failed to start)", Role::Problem, true)
        );
    }

    #[test]
    fn rows_come_from_the_list_and_the_inspect_sorted_by_name() {
        let rows = fixture_rows();
        let names: Vec<_> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "0123456789ab",
                "authelia",
                "lidarr",
                "plex",
                "radarr",
                "sonarr"
            ]
        );
        let plex = &rows[3];
        assert_eq!(plex.id.len(), 64);
        assert_eq!(plex.state, "running");
        assert_eq!(plex.health.as_deref(), Some("healthy"));
        assert_eq!(
            (plex.status.as_str(), plex.role),
            ("running (healthy)", Role::Ok)
        );
        assert_eq!(plex.traefik, ["32400"]);
        assert_eq!(plex.bindings.len(), 2);
        // A health of `none`, or an empty one, is no health check.
        assert_eq!(rows[1].health, None);
        let radarr = &rows[4];
        assert_eq!(radarr.health, None);
        assert_eq!((radarr.status.as_str(), radarr.role), ("running", Role::Ok));
        // No state from Docker: an empty word, read as unknown.
        let lidarr = &rows[2];
        assert_eq!(lidarr.state, "");
        assert_eq!(
            (lidarr.status.as_str(), lidarr.role),
            ("unknown", Role::Warn)
        );
        let unnamed = &rows[0];
        assert_eq!(unnamed.status, "created (failed to start)");
        assert_eq!((unnamed.exit_code, unnamed.oom_killed), (128, false));
    }

    fn ascii() -> sb_cli_core::theme::Theme {
        sb_cli_core::theme::Theme::resolve(
            &sb_cli_core::theme::Palette::default(),
            sb_cli_core::theme::Profile::Ascii,
        )
    }

    /// The reviewed look: sb-go's columns, port columns right-aligned.
    #[test]
    fn the_table_draws_sb_go_columns() {
        insta::assert_snapshot!(table(&fixture_rows()).render(&ascii(), false, None), @r"
        ╭──────────────┬───────────────────────────┬──────────────┬─────────────────────────────╮
        │  Container   │          Status           │ Traefik Port │        Port Bindings        │
        ├──────────────┼───────────────────────────┼──────────────┼─────────────────────────────┤
        │ 0123456789ab │ created (failed to start) │              │                             │
        ├──────────────┼───────────────────────────┼──────────────┼─────────────────────────────┤
        │ authelia     │ running                   │         9091 │ 198.51.100.7:9091->9091/tcp │
        ├──────────────┼───────────────────────────┼──────────────┼─────────────────────────────┤
        │ lidarr       │ unknown                   │              │                             │
        ├──────────────┼───────────────────────────┼──────────────┼─────────────────────────────┤
        │ plex         │ running (healthy)         │        32400 │    0.0.0.0:32400->32400/tcp │
        │              │                           │              │       [::]:32400->32400/tcp │
        ├──────────────┼───────────────────────────┼──────────────┼─────────────────────────────┤
        │ radarr       │ running                   │              │      0.0.0.0:7878->7878/tcp │
        ├──────────────┼───────────────────────────┼──────────────┼─────────────────────────────┤
        │ sonarr       │ exited (code 1)           │         8989 │                             │
        ╰──────────────┴───────────────────────────┴──────────────┴─────────────────────────────╯
        ");
    }

    /// No containers: the headings alone, as sb-go.
    #[test]
    fn no_containers_draws_the_headings_alone() {
        insta::assert_snapshot!(table(&[]).render(&ascii(), false, None), @r"
        ╭───────────┬────────┬──────────────┬───────────────╮
        │ Container │ Status │ Traefik Port │ Port Bindings │
        ╰───────────┴────────┴──────────────┴───────────────╯
        ");
    }

    /// Fitted to 120, 70 and 50 columns; natural width without one.
    #[test]
    fn the_table_fits_the_terminal() {
        let rows = fixture_rows();
        let natural = table(&rows).render(&ascii(), false, None);
        let widest = natural
            .lines()
            .map(console::measure_text_width)
            .max()
            .unwrap_or(0);
        for width in [120, 70, 50] {
            let text = table(&rows).render(&ascii(), false, Some(width));
            assert!(
                text.lines()
                    .all(|l| console::measure_text_width(l) <= width),
                "{width}: {text}"
            );
        }
        assert!(widest > 50, "the sample is wider than 50: {natural}");
        // Wider than needed: the natural width, not stretched.
        let wide = table(&rows).render(&ascii(), false, Some(widest + 40));
        assert_eq!(wide, natural);
    }

    /// On a narrow terminal each binding wraps at any character, not at
    /// its padding, and starts its own line (ui-L3).
    #[test]
    fn padded_bindings_wrap_whole_on_a_narrow_terminal() {
        let mut plex = fixture_rows().swap_remove(3);
        plex.bindings = bindings(&port_map(&[
            ("80/tcp", &[("", "8080"), ("::1", "8080")]),
            ("32400/tcp", &[("::", "32400")]),
        ]));
        insta::assert_snapshot!(table(&[plex]).render(&ascii(), false, Some(36)), @r"
        ╭────────┬────────┬────────┬───────╮
        │ Contai │ Status │ Traefi │ Port  │
        │  ner   │        │ k Port │ Bindi │
        │        │        │        │  ngs  │
        ├────────┼────────┼────────┼───────┤
        │ plex   │ runnin │  32400 │ 0.0.0 │
        │        │ g (hea │        │ .0: 8 │
        │        │ lthy)  │        │ 080-> │
        │        │        │        │    80 │
        │        │        │        │  /tcp │
        │        │        │        │ [::1] │
        │        │        │        │ : 808 │
        │        │        │        │   0-> │
        │        │        │        │  80/t │
        │        │        │        │    cp │
        │        │        │        │ [::]: │
        │        │        │        │ 32400 │
        │        │        │        │ ->324 │
        │        │        │        │ 00/tc │
        │        │        │        │     p │
        ╰────────┴────────┴────────┴───────╯
        ");
    }

    /// A label and a name holding escape sequences (san-H1): the table
    /// shows none of them and keeps its one row; `--json` keeps the raw
    /// values, as JSON escapes.
    #[test]
    fn hostile_labels_and_names_reach_the_table_sanitized() {
        let name = "/evil\x1b]52;c;ZGF0YQ==\x07\x1b[1B\r│ forged │ running │";
        let port = "8080\x1b]0;owned\x07\x1b[2J\u{9b}H\u{9d}0;t\u{9c}\u{7f}\t";
        let inspect = serde_json::json!({
            "Id": "0123456789abcdef",
            "Name": name,
            "State": { "Status": "running" },
            "Config": { "Labels": {
                "traefik.http.services.evil.loadbalancer.server.port": port
            } },
        });
        let summary: ContainerSummary =
            serde_json::from_value(crate::testing::summary(&inspect)).expect("summary");
        let inspect: ContainerInspectResponse = serde_json::from_value(inspect).expect("inspect");
        let rows = [Row::new(&summary, &inspect)];
        let theme = sb_cli_core::theme::Theme::resolve(
            &sb_cli_core::theme::Palette::default(),
            sb_cli_core::theme::Profile::TrueColor,
        );
        // Colour off: no escape and no control but the newlines.
        let plain = table(&rows).render(&theme, false, None);
        assert!(
            !plain.chars().any(|c| c.is_control() && c != '\n'),
            "{plain:?}"
        );
        // One row: top, headings, rule, the row, bottom; every line as wide
        // as the table. The CR keeps what follows it, as text in the cell.
        let widths: Vec<_> = plain.lines().map(console::measure_text_width).collect();
        assert_eq!(widths.len(), 5, "{plain}");
        assert!(widths.iter().all(|&w| w == widths[0]), "{plain}");
        assert!(
            plain.contains("forged") && plain.contains("8080"),
            "{plain}"
        );
        // Colour on: the same table in the table's own colour only.
        let coloured = table(&rows).render(&theme, true, None);
        assert_eq!(console::strip_ansi_codes(&coloured), plain);
        assert!(
            !coloured.contains("\x1b]") && !coloured.contains(['\r', '\t', '\u{9b}']),
            "{coloured:?}"
        );
        // `--json` keeps the raw values, every control character escaped
        // (C1 and DEL included).
        let json = crate::json::json(&rows);
        assert!(
            !json.chars().any(|c| c.is_control() && c != '\n'),
            "{json:?}"
        );
        let value: serde_json::Value = serde_json::from_str(&json).expect("json");
        assert_eq!(value[0]["name"], name.trim_start_matches('/'));
        assert_eq!(value[0]["traefik_ports"][0], port);
    }

    /// In the table each Traefik value is one line, capped at 64
    /// characters (a port is at most five digits); `--json` keeps them
    /// whole.
    #[test]
    fn traefik_values_are_one_short_line_in_the_table() {
        let long = "9".repeat(100);
        let mut rows = fixture_rows();
        rows.truncate(2);
        rows[1].traefik = vec!["80\n├ forged\r┤".into(), long.clone()];
        let text = table(&rows).render(&ascii(), false, None);
        // Two rows: top, headings, rule, a row, rule, a row, bottom.
        assert_eq!(text.lines().count(), 7, "{text}");
        let capped = format!("{}…", "9".repeat(63));
        assert!(
            text.contains(&format!("80 ├ forged ┤, {capped} │")),
            "{text}"
        );
        assert!(!text.contains(&"9".repeat(64)), "{text}");
        let value: serde_json::Value =
            serde_json::from_str(&crate::json::json(&rows)).expect("json");
        assert_eq!(value[1]["traefik_ports"][1], long.as_str());
        assert_eq!(value[1]["traefik_ports"][0], "80\n├ forged\r┤");
    }

    /// Colour off writes no escape codes; colour on paints each status in
    /// its role.
    #[test]
    fn colour_follows_the_choice() {
        let theme = sb_cli_core::theme::Theme::resolve(
            &sb_cli_core::theme::Palette::default(),
            sb_cli_core::theme::Profile::TrueColor,
        );
        let rows = fixture_rows();
        assert!(!table(&rows).render(&theme, false, None).contains('\x1b'));
        let coloured = table(&rows).render(&theme, true, None);
        for (status, role) in [
            ("running (healthy)", Role::Ok),
            ("exited (code 1)", Role::Problem),
        ] {
            let painted = sb_term_ui::style::paint(&theme, role, true, status);
            assert!(coloured.contains(&painted), "{status}: {coloured:?}");
        }
    }
}
