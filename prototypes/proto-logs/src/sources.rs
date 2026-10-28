//! Log sources (Saltbox systemd services and Docker containers) with a
//! readable status for the picker. Design: docs/ideas/log-picker.md.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use bollard::Docker;
use bollard::query_parameters::ListContainersOptionsBuilder;
use futures::future::join_all;
use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    Container(String),
    /// Unit name without the `.service` suffix, as sb-go shows it.
    Service(String),
}

impl Source {
    pub fn name(&self) -> &str {
        match self {
            Self::Container(n) | Self::Service(n) => n,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Container(_) => "container",
            Self::Service(_) => "service",
        }
    }

    pub fn label(&self) -> String {
        format!("{} {}", self.kind(), self.name())
    }

    fn key(&self) -> String {
        format!("{}:{}", self.kind(), self.name())
    }

    fn from_key(key: &str) -> Option<Self> {
        match key.split_once(':')? {
            ("container", n) => Some(Self::Container(n.into())),
            ("service", n) => Some(Self::Service(n.into())),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub source: Source,
    pub status: Status,
}

/// Lists services and containers with their status, both kinds at once.
pub async fn load(docker: &Docker, service_prefix: &str) -> Result<Vec<Entry>> {
    let now = jiff::Timestamp::now().as_second();
    let (services, containers) = tokio::join!(
        load_services(service_prefix, now),
        load_containers(docker, now)
    );
    let mut entries = services?;
    entries.extend(containers?);
    Ok(entries)
}

/// One row of `systemctl list-units --output=json` (and list-timers).
#[derive(Deserialize)]
struct UnitRow {
    unit: String,
    load: String,
    active: String,
    sub: String,
}

#[derive(Deserialize)]
struct TimerRow {
    /// Microseconds since the epoch; 0 or null when no run is scheduled.
    next: Option<i64>,
    activates: String,
}

/// The same units sb-go lists (`saltbox_managed_*` plus `docker`, skipping
/// units whose file is gone), read from systemctl's JSON table output
/// instead of its human-readable columns. `--output=json` for list-units is
/// in systemd since at least v249 (Ubuntu 22.04): `output_table()` in
/// https://github.com/systemd/systemd/blob/v249/src/systemctl/systemctl-util.c
async fn load_services(prefix: &str, now: i64) -> Result<Vec<Entry>> {
    let pattern = format!("{prefix}*");
    let units: Vec<UnitRow> = systemctl_json(&[
        "list-units",
        &pattern,
        "docker.service",
        "--type=service",
        "--all",
    ])
    .await?;
    let units: Vec<UnitRow> = units
        .into_iter()
        .filter(|u| u.load != "not-found" && u.unit.ends_with(".service"))
        .collect();
    let timers: Vec<TimerRow> = systemctl_json(&["list-timers", &pattern, "--all"]).await?;
    let next_run: HashMap<String, i64> = timers
        .into_iter()
        .filter_map(|t| Some((t.activates, t.next.filter(|n| *n > 0)? / 1_000_000)))
        .collect();
    let times = unit_times(units.iter().map(|u| u.unit.as_str())).await?;
    let mut entries: Vec<Entry> = units
        .into_iter()
        .map(|u| {
            let (changed, active_since) = times.get(&u.unit).copied().unwrap_or_default();
            let facts = UnitFacts {
                next_timer: next_run.get(&u.unit).copied(),
                active: u.active,
                sub: u.sub,
                changed,
                active_since,
            };
            let name = u.unit.trim_end_matches(".service").to_string();
            Entry {
                source: Source::Service(name),
                status: classify_unit(&facts, now),
            }
        })
        .collect();
    entries.sort_by(|a, b| a.source.name().cmp(b.source.name()));
    Ok(entries)
}

async fn systemctl_json<T: serde::de::DeserializeOwned>(args: &[&str]) -> Result<Vec<T>> {
    let out = tokio::process::Command::new("systemctl")
        .args(args)
        .args(["--output=json", "--no-pager"])
        .output()
        .await?;
    Ok(serde_json::from_slice(&out.stdout)?)
}

/// (StateChangeTimestamp, ActiveEnterTimestamp) per unit, in Unix seconds.
/// `systemctl show` output is "intended to be computer-parsable" (systemctl(1)).
async fn unit_times<'a>(
    units: impl Iterator<Item = &'a str>,
) -> Result<HashMap<String, (Option<i64>, Option<i64>)>> {
    let units: Vec<&str> = units.collect();
    if units.is_empty() {
        return Ok(HashMap::new());
    }
    let out = tokio::process::Command::new("systemctl")
        .arg("show")
        .args(&units)
        .args([
            "--property=Id,StateChangeTimestamp,ActiveEnterTimestamp",
            "--timestamp=unix",
            "--no-pager",
        ])
        .output()
        .await?;
    Ok(parse_show(&String::from_utf8_lossy(&out.stdout)))
}

fn parse_show(text: &str) -> HashMap<String, (Option<i64>, Option<i64>)> {
    let secs = |v: &str| v.strip_prefix('@')?.parse::<i64>().ok().filter(|s| *s > 0);
    text.split("\n\n")
        .filter_map(|block| {
            let mut id = None;
            let (mut changed, mut active) = (None, None);
            for line in block.lines() {
                match line.split_once('=') {
                    Some(("Id", v)) => id = Some(v.to_string()),
                    Some(("StateChangeTimestamp", v)) => changed = secs(v),
                    Some(("ActiveEnterTimestamp", v)) => active = secs(v),
                    _ => {}
                }
            }
            Some((id?, (changed, active)))
        })
        .collect()
}

/// Running and stopped containers, each inspected (in parallel) for exit
/// code, restart count and health, which the list endpoint doesn't carry.
async fn load_containers(docker: &Docker, now: i64) -> Result<Vec<Entry>> {
    let list = docker
        .list_containers(Some(ListContainersOptionsBuilder::new().all(true).build()))
        .await?;
    let names: Vec<String> = list
        .into_iter()
        .filter_map(|c| Some(c.names?.first()?.trim_start_matches('/').to_string()))
        .collect();
    let inspected = join_all(names.iter().map(|n| docker.inspect_container(n, None))).await;
    let secs = |t: Option<String>| {
        t?.parse::<jiff::Timestamp>()
            .ok()
            .map(jiff::Timestamp::as_second)
            .filter(|s| *s > 0)
    };
    let mut entries: Vec<Entry> = names
        .into_iter()
        .zip(inspected)
        .filter_map(|(name, info)| {
            let info = info.ok()?;
            let state = info.state.unwrap_or_default();
            let facts = ContainerFacts {
                state: state.status.map(|s| s.to_string()).unwrap_or_default(),
                exit_code: state.exit_code.unwrap_or_default(),
                oom_killed: state.oom_killed.unwrap_or_default(),
                restart_count: info.restart_count.unwrap_or_default(),
                health: state.health.and_then(|h| h.status).map(|s| s.to_string()),
                started: secs(state.started_at),
                finished: secs(state.finished_at),
            };
            Some(Entry {
                source: Source::Container(name),
                status: classify_container(&facts, now),
            })
        })
        .collect();
    entries.sort_by(|a, b| a.source.name().cmp(b.source.name()));
    Ok(entries)
}

/// Picker sections, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Attention,
    Recent,
    Services,
    Containers,
}

impl Section {
    pub fn title(self) -> &'static str {
        match self {
            Self::Attention => "NEEDS ATTENTION",
            Self::Recent => "RECENT",
            Self::Services => "SERVICES",
            Self::Containers => "CONTAINERS",
        }
    }
}

/// Entry indexes per section. Services and Containers always list every
/// matching source; Needs attention and Recent are shortcuts on top, so a
/// source can appear twice but is always where its kind says.
pub fn sections(entries: &[Entry], recent: &[Source], filter: &str) -> Vec<(Section, Vec<usize>)> {
    let needle = filter.to_lowercase();
    let shown: Vec<usize> = (0..entries.len())
        .filter(|&i| entries[i].source.name().to_lowercase().contains(&needle))
        .collect();
    let pick = |f: &dyn Fn(&Entry) -> bool| -> Vec<usize> {
        shown.iter().copied().filter(|&i| f(&entries[i])).collect()
    };
    let recent_rows: Vec<usize> = recent
        .iter()
        .filter_map(|r| shown.iter().copied().find(|&i| entries[i].source == *r))
        .collect();
    [
        (
            Section::Attention,
            pick(&|e| e.status.health == Health::Problem),
        ),
        (Section::Recent, recent_rows),
        (
            Section::Services,
            pick(&|e| matches!(e.source, Source::Service(_))),
        ),
        (
            Section::Containers,
            pick(&|e| matches!(e.source, Source::Container(_))),
        ),
    ]
    .into_iter()
    .filter(|(_, rows)| !rows.is_empty())
    .collect()
}

/// How many recently opened sources the picker remembers.
pub const RECENT_MAX: usize = 5;

/// Moves `source` to the front of the recent list.
pub fn remember(recent: &mut Vec<Source>, source: &Source) {
    recent.retain(|r| r != source);
    recent.insert(0, source.clone());
    recent.truncate(RECENT_MAX);
}

/// `$XDG_STATE_HOME` (or ~/.local/state) via dirs::state_dir; one source per line.
fn recent_path() -> Option<PathBuf> {
    Some(dirs::state_dir()?.join("sb-rust-proto").join("recent-logs"))
}

pub fn load_recent() -> Vec<Source> {
    recent_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| t.lines().filter_map(Source::from_key).collect())
        .unwrap_or_default()
}

/// Best effort: a picker that can't save its history still works.
pub fn save_recent(recent: &[Source]) {
    let Some(path) = recent_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let text: Vec<String> = recent.iter().map(Source::key).collect();
    let _ = std::fs::write(path, text.join("\n") + "\n");
}

/// What the picker should make of a source's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    /// Running normally.
    Ok,
    /// Failed, crashed, restarting or unhealthy: listed under Needs attention.
    Problem,
    /// In transition (starting, stopping, paused).
    Busy,
    /// Not running, and that is fine (stopped cleanly, waiting for a timer).
    Idle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub health: Health,
    pub symbol: char,
    pub text: String,
}

/// State of one systemd unit, from `systemctl list-units` and `systemctl show`.
#[derive(Default)]
pub struct UnitFacts {
    pub active: String,
    pub sub: String,
    /// Unix seconds of the last state change and of entering "active".
    pub changed: Option<i64>,
    pub active_since: Option<i64>,
    /// Unix seconds of the next run of a timer that starts this unit.
    pub next_timer: Option<i64>,
}

/// State of one container, from the Docker inspect response.
#[derive(Default)]
pub struct ContainerFacts {
    pub state: String,
    pub exit_code: i64,
    pub oom_killed: bool,
    pub restart_count: i64,
    pub health: Option<String>,
    pub started: Option<i64>,
    pub finished: Option<i64>,
}

/// A container restarted by Docker counts as a problem until it has stayed up
/// this long.
const SETTLE_SECS: i64 = 600;

/// Unit states as listed in systemd.unit(5) and systemctl(1) (ACTIVE and SUB).
pub fn classify_unit(f: &UnitFacts, now: i64) -> Status {
    let since = |t: Option<i64>| t.map(|t| short_duration(now - t));
    match (f.active.as_str(), f.sub.as_str()) {
        ("failed", _) => match since(f.changed) {
            Some(ago) => status(Health::Problem, '✗', format!("failed {ago} ago")),
            None => status(Health::Problem, '✗', "failed".into()),
        },
        (_, "auto-restart") => status(Health::Problem, '↻', "restarting".into()),
        ("active", "running") => match since(f.active_since) {
            Some(up) => status(Health::Ok, '✓', format!("running · {up}")),
            None => status(Health::Ok, '✓', "running".into()),
        },
        ("active", "exited") => status(Health::Ok, '✓', "finished".into()),
        ("active", sub) => status(Health::Ok, '✓', sub.into()),
        ("activating", _) => status(Health::Busy, '◌', "starting".into()),
        ("deactivating", _) => status(Health::Busy, '◌', "stopping".into()),
        ("inactive", _) => match f.next_timer {
            Some(next) => status(
                Health::Idle,
                '○',
                format!("idle · next run in {}", short_duration(next - now)),
            ),
            None => status(Health::Idle, '○', "stopped".into()),
        },
        (other, _) => status(Health::Busy, '◌', other.into()),
    }
}

/// Container states from the Docker Engine API's ContainerState.Status:
/// https://docs.docker.com/reference/api/engine/version/v1.53/#tag/Container/operation/ContainerInspect
pub fn classify_container(f: &ContainerFacts, now: i64) -> Status {
    let up = f.started.map(|t| now - t);
    let up_text = up.map(short_duration).unwrap_or_default();
    let ago = f
        .finished
        .map(|t| format!(" · {} ago", short_duration(now - t)))
        .unwrap_or_default();
    match f.state.as_str() {
        "running" if f.health.as_deref() == Some("unhealthy") => {
            status(Health::Problem, '✗', format!("unhealthy · up {up_text}"))
        }
        "running" if f.restart_count > 0 && up.is_some_and(|u| u < SETTLE_SECS) => status(
            Health::Problem,
            '↻',
            format!("restarted {}× · up {up_text}", f.restart_count),
        ),
        "running" if f.health.as_deref() == Some("starting") => status(
            Health::Busy,
            '◌',
            format!("health check starting · up {up_text}"),
        ),
        "running" if up.is_some() => status(Health::Ok, '✓', format!("running · {up_text}")),
        "running" => status(Health::Ok, '✓', "running".into()),
        "restarting" => status(
            Health::Problem,
            '↻',
            format!("restarting ({}×)", f.restart_count),
        ),
        "exited" if f.oom_killed => status(Health::Problem, '✗', format!("out of memory{ago}")),
        "exited" if f.exit_code != 0 => status(
            Health::Problem,
            '✗',
            format!("exited with code {}{ago}", f.exit_code),
        ),
        "exited" => match f.finished {
            Some(t) => status(
                Health::Idle,
                '○',
                format!("stopped {} ago", short_duration(now - t)),
            ),
            None => status(Health::Idle, '○', "stopped".into()),
        },
        "created" => status(Health::Idle, '○', "never started".into()),
        "dead" => status(Health::Problem, '✗', "dead".into()),
        other => status(Health::Busy, '◌', other.into()),
    }
}

/// The two largest units of a duration, like sb-go's runtime column ("3d 2h"),
/// leaving out a zero second unit ("2d", not "2d 0h").
pub fn short_duration(secs: i64) -> String {
    let s = secs.max(0);
    let (d, h, m) = (s / 86400, s % 86400 / 3600, s % 3600 / 60);
    if d > 0 && h > 0 {
        format!("{d}d {h}h")
    } else if d > 0 {
        format!("{d}d")
    } else if h > 0 && m > 0 {
        format!("{h}h {m}m")
    } else if h > 0 {
        format!("{h}h")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{s}s")
    }
}

fn status(health: Health, symbol: char, text: String) -> Status {
    Status {
        health,
        symbol,
        text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000;

    fn unit(active: &str, sub: &str) -> UnitFacts {
        UnitFacts {
            active: active.into(),
            sub: sub.into(),
            ..UnitFacts::default()
        }
    }

    fn container(state: &str) -> ContainerFacts {
        ContainerFacts {
            state: state.into(),
            ..ContainerFacts::default()
        }
    }

    #[test]
    fn durations_show_the_two_largest_units() {
        assert_eq!(short_duration(42), "42s");
        assert_eq!(short_duration(12 * 60 + 5), "12m");
        assert_eq!(short_duration(3 * 3600 + 12 * 60), "3h 12m");
        assert_eq!(short_duration(2 * 86400 + 4 * 3600 + 59), "2d 4h");
        assert_eq!(short_duration(-5), "0s");
        assert_eq!(short_duration(2 * 86400 + 59), "2d");
        assert_eq!(short_duration(3 * 3600), "3h");
    }

    #[test]
    fn running_unit_shows_uptime() {
        let f = UnitFacts {
            active_since: Some(NOW - 2 * 86400),
            ..unit("active", "running")
        };
        assert_eq!(
            classify_unit(&f, NOW),
            status(Health::Ok, '✓', "running · 2d".into())
        );
    }

    #[test]
    fn failed_unit_needs_attention_with_age() {
        let f = UnitFacts {
            changed: Some(NOW - 7200),
            ..unit("failed", "failed")
        };
        assert_eq!(
            classify_unit(&f, NOW),
            status(Health::Problem, '✗', "failed 2h ago".into())
        );
    }

    #[test]
    fn auto_restarting_unit_needs_attention() {
        let s = classify_unit(&unit("activating", "auto-restart"), NOW);
        assert_eq!((s.health, s.symbol), (Health::Problem, '↻'));
    }

    #[test]
    fn inactive_unit_with_timer_is_idle_with_next_run() {
        let f = UnitFacts {
            next_timer: Some(NOW + 3 * 3600),
            ..unit("inactive", "dead")
        };
        assert_eq!(
            classify_unit(&f, NOW),
            status(Health::Idle, '○', "idle · next run in 3h".into())
        );
        assert_eq!(
            classify_unit(&unit("inactive", "dead"), NOW),
            status(Health::Idle, '○', "stopped".into())
        );
    }

    #[test]
    fn finished_oneshot_unit_is_ok() {
        let s = classify_unit(&unit("active", "exited"), NOW);
        assert_eq!((s.health, s.text.as_str()), (Health::Ok, "finished"));
    }

    #[test]
    fn running_container_shows_uptime() {
        let f = ContainerFacts {
            started: Some(NOW - 90),
            ..container("running")
        };
        assert_eq!(
            classify_container(&f, NOW),
            status(Health::Ok, '✓', "running · 1m".into())
        );
    }

    #[test]
    fn unhealthy_container_needs_attention() {
        let f = ContainerFacts {
            health: Some("unhealthy".into()),
            started: Some(NOW - 3600),
            ..container("running")
        };
        assert_eq!(
            classify_container(&f, NOW),
            status(Health::Problem, '✗', "unhealthy · up 1h".into())
        );
    }

    #[test]
    fn recently_restarted_container_needs_attention_until_it_settles() {
        let fresh = ContainerFacts {
            restart_count: 3,
            started: Some(NOW - 30),
            ..container("running")
        };
        assert_eq!(
            classify_container(&fresh, NOW),
            status(Health::Problem, '↻', "restarted 3× · up 30s".into())
        );
        let settled = ContainerFacts {
            started: Some(NOW - 3600),
            ..fresh
        };
        assert_eq!(classify_container(&settled, NOW).health, Health::Ok);
    }

    #[test]
    fn restarting_container_needs_attention() {
        let f = ContainerFacts {
            restart_count: 4,
            ..container("restarting")
        };
        assert_eq!(
            classify_container(&f, NOW),
            status(Health::Problem, '↻', "restarting (4×)".into())
        );
    }

    #[test]
    fn exit_code_decides_whether_a_stopped_container_is_a_problem() {
        let clean = ContainerFacts {
            finished: Some(NOW - 600),
            ..container("exited")
        };
        assert_eq!(
            classify_container(&clean, NOW),
            status(Health::Idle, '○', "stopped 10m ago".into())
        );
        let crashed = ContainerFacts {
            exit_code: 137,
            ..clean
        };
        assert_eq!(
            classify_container(&crashed, NOW),
            status(
                Health::Problem,
                '✗',
                "exited with code 137 · 10m ago".into()
            )
        );
        let oom = ContainerFacts {
            oom_killed: true,
            ..crashed
        };
        assert_eq!(
            classify_container(&oom, NOW).text,
            "out of memory · 10m ago"
        );
    }

    #[test]
    fn show_output_is_split_per_unit_and_skips_unset_times() {
        let text = "Id=a.service\nStateChangeTimestamp=@100\nActiveEnterTimestamp=@90\n\n\
                    Id=b.service\nStateChangeTimestamp=\nActiveEnterTimestamp=@0\n";
        let parsed = parse_show(text);
        assert_eq!(parsed["a.service"], (Some(100), Some(90)));
        assert_eq!(parsed["b.service"], (None, None));
    }

    fn entry(source: Source, health: Health) -> Entry {
        Entry {
            source,
            status: status(health, '?', String::new()),
        }
    }

    #[test]
    fn sections_keep_kinds_complete_and_add_shortcuts() {
        let entries = vec![
            entry(Source::Service("docker".into()), Health::Ok),
            entry(
                Source::Service("saltbox_managed_backup".into()),
                Health::Problem,
            ),
            entry(Source::Container("plex".into()), Health::Ok),
            entry(Source::Container("sonarr".into()), Health::Problem),
        ];
        let recent = vec![
            Source::Container("plex".into()),
            Source::Container("gone".into()),
        ];
        let got = sections(&entries, &recent, "");
        assert_eq!(
            got,
            vec![
                (Section::Attention, vec![1, 3]),
                (Section::Recent, vec![2]),
                (Section::Services, vec![0, 1]),
                (Section::Containers, vec![2, 3]),
            ]
        );
        // Filtering is case-insensitive and drops sections left empty.
        assert_eq!(
            sections(&entries, &recent, "PLE"),
            vec![(Section::Recent, vec![2]), (Section::Containers, vec![2])]
        );
    }

    #[test]
    fn remember_moves_to_front_without_duplicates_and_caps() {
        let mut recent: Vec<Source> = (0..RECENT_MAX)
            .map(|i| Source::Container(i.to_string()))
            .collect();
        remember(&mut recent, &Source::Container("3".into()));
        assert_eq!(recent[0], Source::Container("3".into()));
        assert_eq!(recent.len(), RECENT_MAX);
        remember(&mut recent, &Source::Service("new".into()));
        assert_eq!(recent.len(), RECENT_MAX);
        assert!(!recent.contains(&Source::Container("4".into())));
        assert_eq!(
            Source::from_key(&Source::Service("x:y".into()).key()),
            Some(Source::Service("x:y".into()))
        );
    }

    #[test]
    fn never_started_container_is_idle() {
        assert_eq!(
            classify_container(&container("created"), NOW),
            status(Health::Idle, '○', "never started".into())
        );
    }
}
