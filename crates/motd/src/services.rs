//! Services (motd spec, Sections; decision 14): host's service list with
//! motd.yml's `systemd` additions and display names, written as sb-go's
//! `GetSystemdServicesInfo` writes it (`motd/systemd.go`).
//!
//! Library-backed: the list is sb-host's `Systemd::services` (systemctl's
//! JSON tables through sb-exec), its durations sb-host's
//! `format_duration` and `format_next_in`; the time is jiff's. motd's own
//! code is sb-go's display names, sort and line texts (domain logic), and
//! the names and states from systemctl go through term-ui's `DisplayText`
//! (decision 24).

use std::collections::BTreeMap;

use jiff::Timestamp;
use sb_host::systemd::{Service, Systemd, format_duration, format_next_in, with_additional};
use tokio_util::sync::CancellationToken;

use crate::Paint;
use crate::config;
use crate::text::one_line;

/// sb-go's prefix of the units Saltbox manages, cut from a name it has
/// no display name for.
const MANAGED_PREFIX: &str = "saltbox_managed_";

/// sb-go's `defaultDisplayNames`.
const DEFAULT_NAMES: [(&str, &str); 5] = [
    ("docker", "Docker"),
    (
        "saltbox_managed_docker_controller",
        "Saltbox Docker Controller",
    ),
    (
        "saltbox_managed_docker_controller_helper",
        "Saltbox Docker Controller Helper",
    ),
    (
        "saltbox_managed_docker_update_hosts",
        "Saltbox Docker Hosts Manager",
    ),
    ("saltbox_managed_mergerfs", "Mergerfs"),
];

/// Services: hidden when motd.yml's `systemd` has `enabled: false`;
/// otherwise the services that sb-go's default filters and motd.yml's
/// `additional_services` match (sb-host's `with_additional`), one line
/// each, sorted by display name, or an error line. Without a usable
/// motd.yml (none, or one with issues) the default filters and names
/// apply (decision 3). `token` is the section's cap: cancelling it ends
/// every systemctl call.
pub async fn services(
    systemd: &Systemd,
    settings: Option<&config::Systemd>,
    paint: &Paint,
    token: &CancellationToken,
) -> String {
    if settings.is_some_and(|settings| !settings.enabled) {
        return String::new();
    }
    let additional = settings.map_or(&[][..], |s| s.additional_services.as_slice());
    let filters = with_additional(additional);
    match systemd.services(&filters, token).await {
        Ok(found) => {
            let names = settings.map(|s| &s.display_names);
            lines(&found, names, Timestamp::now(), paint)
        }
        Err(err) => {
            tracing::warn!(error = %err, "systemd services not listed");
            paint.error(&format!(
                "Error: {}",
                one_line(format!("error getting services: {err}").as_bytes())
            ))
        }
    }
}

/// One line per service, sorted by display name (byte order, as Go's
/// `<`), each name padded to the longest; empty for none.
fn lines(
    found: &[Service],
    names: Option<&BTreeMap<String, String>>,
    now: Timestamp,
    paint: &Paint,
) -> String {
    let mut shown: Vec<(String, &Service)> = found
        .iter()
        .map(|service| (display_name(&service.name, names), service))
        .collect();
    // Stable, so equal display names keep sb-host's order by unit name.
    shown.sort_by(|a, b| a.0.cmp(&b.0));
    // sb-go pads by bytes (`len`), as here; std's `{:<width$}` would
    // count chars.
    let width = shown.iter().map(|(name, _)| name.len()).max().unwrap_or(0);
    shown
        .iter()
        .map(|(name, service)| {
            let pad = " ".repeat(width - name.len());
            format!("{name}{pad}   {}", status(service, now, paint))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// sb-go's `getDisplayName`: motd.yml's name, then sb-go's own, then the
/// name without `saltbox_managed_`, then the name; made safe to draw.
fn display_name(name: &str, names: Option<&BTreeMap<String, String>>) -> String {
    let chosen = names
        .and_then(|names| names.get(name).map(String::as_str))
        .or_else(|| {
            DEFAULT_NAMES
                .iter()
                .find(|(unit, _)| *unit == name)
                .map(|(_, shown)| *shown)
        })
        .unwrap_or_else(|| name.strip_prefix(MANAGED_PREFIX).unwrap_or(name));
    one_line(chosen.as_bytes())
}

/// sb-go's `formatServiceLine` status: `active/<sub> • <runtime>` in the
/// success colour, `failed` in the error colour, an inactive service's
/// timer (`scheduled • timer …`, `inactive • timer …`, `• next in …`),
/// and any other state in the warning colour.
fn status(service: &Service, now: Timestamp, paint: &Paint) -> String {
    let active = one_line(service.active.as_bytes());
    let sub = one_line(service.sub.as_bytes());
    match active.as_str() {
        "active" => {
            let mut status = joined(&active, &sub);
            if let Some(runtime) = service.runtime(now) {
                status = format!("{status} • {}", format_duration(runtime));
            }
            paint.success(&status)
        }
        "failed" => paint.error("failed"),
        "inactive" => match &service.timer {
            Some(timer) => {
                let timer_active = one_line(timer.active.as_bytes());
                let state = joined(&timer_active, &one_line(timer.sub.as_bytes()));
                let scheduled = timer_active == "active";
                let mut status = if scheduled {
                    format!("scheduled • timer {state}")
                } else {
                    format!("inactive • timer {state}")
                };
                let next = format_next_in(timer.next, now);
                if !next.is_empty() {
                    status = format!("{status} • next in {next}");
                }
                if scheduled {
                    paint.success(&status)
                } else {
                    paint.warning(&status)
                }
            }
            None => paint.warning("inactive"),
        },
        _ => paint.warning(&active),
    }
}

/// `state/sub`, or `state` when the sub state is empty or the same.
fn joined(state: &str, sub: &str) -> String {
    if sub.is_empty() || sub == state {
        state.to_string()
    } else {
        format!("{state}/{sub}")
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use sb_host::systemd::Timer;

    use super::*;
    use crate::packages::tests::{SPAWNS, stand_in};

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    fn service(name: &str, active: &str, sub: &str) -> Service {
        Service {
            name: name.to_string(),
            active: active.to_string(),
            sub: sub.to_string(),
            unit_file_state: None,
            active_enter: None,
            state_change: None,
            timer: None,
        }
    }

    fn painted() -> Paint {
        let theme = sb_cli_core::theme::Theme::resolve(
            &sb_cli_core::theme::Palette::default(),
            sb_cli_core::theme::Profile::TrueColor,
        );
        Paint::new(&theme, true)
    }

    const NOW: &str = "2026-11-13T12:00:00Z";

    /// sb-go's display names: motd.yml's first, then its defaults, then
    /// the prefix cut, then the name as it is; control characters gone.
    #[test]
    fn display_names_are_sb_gos() {
        let mut names = BTreeMap::new();
        names.insert("rclone_vfs".to_string(), "Rclone".to_string());
        names.insert("docker".to_string(), "Engine".to_string());
        names.insert("odd".to_string(), "a\x1b]0;t\x07b".to_string());
        let names = Some(&names);
        assert_eq!(display_name("rclone_vfs", names), "Rclone");
        assert_eq!(display_name("docker", names), "Engine");
        assert_eq!(display_name("docker", None), "Docker");
        assert_eq!(display_name("odd", names), "ab");
        for (unit, shown) in DEFAULT_NAMES {
            assert_eq!(display_name(unit, None), shown);
        }
        assert_eq!(
            display_name("saltbox_managed_docker_controller", None),
            "Saltbox Docker Controller"
        );
        assert_eq!(display_name("saltbox_managed_web", names), "web");
        assert_eq!(display_name("mergerfs", None), "mergerfs");
        assert_eq!(display_name("saltbox_managed_", None), "");
    }

    /// Each state's text as sb-go's `formatServiceLine` writes it.
    #[test]
    fn states_read_as_sb_gos() {
        let now = at(NOW);
        let plain = Paint::plain();
        let mut running = service("docker", "active", "running");
        assert_eq!(status(&running, now, &plain), "active/running");
        running.active_enter = Some(at("2026-11-10T09:30:00Z"));
        assert_eq!(status(&running, now, &plain), "active/running • 3d 2h");
        let exited = service("x", "active", "active");
        assert_eq!(status(&exited, now, &plain), "active");
        assert_eq!(status(&service("x", "active", ""), now, &plain), "active");
        assert_eq!(
            status(&service("x", "failed", "failed"), now, &plain),
            "failed"
        );
        assert_eq!(
            status(&service("x", "inactive", "dead"), now, &plain),
            "inactive"
        );
        assert_eq!(
            status(&service("x", "activating", "auto-restart"), now, &plain),
            "activating"
        );
        let mut job = service("x", "inactive", "dead");
        job.timer = Some(Timer {
            active: "active".to_string(),
            sub: "waiting".to_string(),
            next: Some(now + Duration::from_mins(130)),
        });
        assert_eq!(
            status(&job, now, &plain),
            "scheduled • timer active/waiting • next in 2h 10m"
        );
        job.timer = Some(Timer {
            active: "inactive".to_string(),
            sub: "dead".to_string(),
            next: None,
        });
        assert_eq!(status(&job, now, &plain), "inactive • timer inactive/dead");
        job.timer = Some(Timer {
            active: "active".to_string(),
            sub: "active".to_string(),
            next: Some(at("2026-11-13T11:00:00Z")),
        });
        assert_eq!(
            status(&job, now, &plain),
            "scheduled • timer active • next in soon"
        );
        // A failed timer's own state isn't the service's: still `inactive`.
        let mut failed = job.clone();
        failed.active = "failed".to_string();
        assert_eq!(status(&failed, now, &plain), "failed");
        // States from systemctl are drawn safe.
        assert_eq!(
            status(&service("x", "re\x1b[2Jloading", ""), now, &plain),
            "reloading"
        );
    }

    /// The colours: success for active and a scheduled timer, error for
    /// failed, warning for the rest.
    #[test]
    fn states_take_sb_gos_colours() {
        let now = at(NOW);
        let paint = painted();
        assert_eq!(
            status(&service("x", "active", "running"), now, &paint),
            paint.success("active/running")
        );
        assert_eq!(
            status(&service("x", "failed", "failed"), now, &paint),
            paint.error("failed")
        );
        assert_eq!(
            status(&service("x", "inactive", "dead"), now, &paint),
            paint.warning("inactive")
        );
        assert_eq!(
            status(&service("x", "deactivating", "stop"), now, &paint),
            paint.warning("deactivating")
        );
        let mut job = service("x", "inactive", "dead");
        job.timer = Some(Timer {
            active: "active".to_string(),
            sub: "waiting".to_string(),
            next: None,
        });
        assert_eq!(
            status(&job, now, &paint),
            paint.success("scheduled • timer active/waiting")
        );
        job.timer = Some(Timer {
            active: "inactive".to_string(),
            sub: String::new(),
            next: None,
        });
        assert_eq!(
            status(&job, now, &paint),
            paint.warning("inactive • timer inactive")
        );
    }

    /// Sorted by display name, padded to the longest by bytes, three
    /// spaces before the status; nothing for no service.
    #[test]
    fn lines_are_sorted_by_display_name_and_padded() {
        let now = at(NOW);
        let found = [
            service("docker", "active", "running"),
            service("rclone_vfs", "failed", "failed"),
            service("saltbox_managed_web", "inactive", "dead"),
        ];
        let mut names = BTreeMap::new();
        names.insert("rclone_vfs".to_string(), "Rclone".to_string());
        assert_eq!(
            lines(&found, Some(&names), now, &Paint::plain()),
            "Docker   active/running\nRclone   failed\nweb      inactive"
        );
        assert_eq!(
            lines(&found, None, now, &Paint::plain()),
            "Docker       active/running\nrclone_vfs   failed\nweb          inactive"
        );
        assert_eq!(lines(&[], None, now, &Paint::plain()), "");
        // By bytes, as sb-go's `len`: `Plëx` is 5 bytes, 4 characters.
        names.insert("docker".to_string(), "Plëx".to_string());
        assert_eq!(
            lines(&found[..1], Some(&names), now, &Paint::plain()),
            "Plëx   active/running"
        );
        let mut two = found[..2].to_vec();
        two[1].name = "docker2".to_string();
        names.insert("docker2".to_string(), "Docker".to_string());
        assert_eq!(
            lines(&two, Some(&names), now, &Paint::plain()),
            "Docker   failed\nPlëx    active/running"
        );
        // The name isn't painted (sb-go's `DefaultStyle`), the status is.
        let paint = painted();
        assert_eq!(
            lines(&found[..1], None, now, &paint),
            format!("Docker   {}", paint.success("active/running"))
        );
    }

    /// A stand-in systemctl that lists `units` (unit, active, sub), has no
    /// unit files or timers, and knows no times.
    fn systemctl(dir: &Path, units: &[(&str, &str, &str)]) -> Systemd {
        let rows: Vec<String> = units
            .iter()
            .map(|(unit, active, sub)| {
                format!(
                    r#"{{"unit":"{unit}.service","load":"loaded","active":"{active}","sub":"{sub}","description":"x"}}"#
                )
            })
            .collect();
        let path = dir.join("systemctl");
        stand_in(
            &path,
            &format!(
                r#"case "$1" in
  list-units) echo '[{}]' ;;
  list-unit-files|list-timers) echo '[]' ;;
  show) ;;
  *) exit 9 ;;
esac"#,
                rows.join(",")
            ),
        );
        Systemd::new().with_program(path)
    }

    fn settings(enabled: bool, additional: &[&str], names: &[(&str, &str)]) -> config::Systemd {
        config::Systemd {
            enabled,
            additional_services: additional.iter().map(ToString::to_string).collect(),
            display_names: names
                .iter()
                .map(|(unit, shown)| ((*unit).to_string(), (*shown).to_string()))
                .collect(),
        }
    }

    /// motd.yml's additions join the default filters, its display names
    /// apply; without motd.yml only the defaults show.
    #[tokio::test]
    async fn configured_names_join_the_defaults() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let systemd = systemctl(
            dir.path(),
            &[
                ("docker", "active", "running"),
                ("rclone_vfs", "failed", "failed"),
                ("ssh", "active", "running"),
                ("saltbox_managed_mergerfs", "inactive", "dead"),
            ],
        );
        let token = CancellationToken::new();
        let plain = Paint::plain();
        let configured = settings(
            true,
            &["rclone_vfs.service", ""],
            &[("rclone_vfs", "Rclone")],
        );
        assert_eq!(
            services(&systemd, Some(&configured), &plain, &token).await,
            "Docker     active/running\nMergerfs   inactive\nRclone     failed"
        );
        assert_eq!(
            services(&systemd, None, &plain, &token).await,
            "Docker     active/running\nMergerfs   inactive"
        );
    }

    /// `enabled: false` hides the section without asking systemctl; no
    /// service matched hides it too.
    #[tokio::test]
    async fn disabled_or_nothing_matched_is_hidden() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let systemd = Systemd::new().with_program(dir.path().join("absent"));
        let token = CancellationToken::new();
        let off = settings(false, &[], &[]);
        assert_eq!(
            services(&systemd, Some(&off), &Paint::plain(), &token).await,
            ""
        );
        let systemd = systemctl(dir.path(), &[("ssh", "active", "running")]);
        assert_eq!(services(&systemd, None, &Paint::plain(), &token).await, "");
    }

    /// A list that can't be read is sb-go's error line, first line only,
    /// in the error colour.
    #[tokio::test]
    async fn a_failing_list_is_an_error_line() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("systemctl");
        stand_in(&path, "echo 'Failed to connect to bus\nsecond' >&2; exit 1");
        let systemd = Systemd::new().with_program(&path);
        let token = CancellationToken::new();
        let text = services(&systemd, None, &Paint::plain(), &token).await;
        assert!(
            text.starts_with("Error: error getting services: failed to list systemd services: "),
            "{text}"
        );
        assert!(!text.contains('\n'), "{text}");
        let paint = painted();
        let text = services(&systemd, None, &paint, &token).await;
        assert!(text.starts_with(&paint.error("Error: ")[..10]), "{text:?}");
        assert!(text.ends_with("\x1b[0m"), "{text:?}");
    }

    /// The section's cap reaches systemctl through the token: cancelling
    /// it ends a hanging call at once, not at its 10 s.
    #[tokio::test]
    async fn cancelling_the_token_ends_a_hanging_list() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("systemctl");
        stand_in(&path, "exec sleep 60");
        let systemd = Systemd::new().with_program(&path);
        let token = CancellationToken::new();
        let cancel = token.clone();
        let plain = Paint::plain();
        let start = std::time::Instant::now();
        let (text, ()) = tokio::join!(services(&systemd, None, &plain, &token), async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            cancel.cancel();
        });
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
        assert!(
            text.starts_with("Error: error getting services: "),
            "{text}"
        );
    }
}
