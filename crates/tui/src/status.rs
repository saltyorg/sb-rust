//! The service status rule (tui spec, "Service status"): a pure function
//! from sb-host's service record to the picker's symbol, text, role and
//! attention flag. host assumption 4 puts the rule here (tui decision 9).
//! It started as proto-logs' `classify_unit`; where the two differ, the
//! spec's table wins (a disabled leftover is Warn here, Problem there).

use std::time::Duration;

use jiff::Timestamp;
use sb_cli_core::theme::Role;
use sb_host::systemd::{Service, format_duration, format_next_in};

/// A source's status in the picker: the symbol drawn before the text, the
/// text, its role, and whether the source belongs under Needs attention.
/// The words alone tell the states apart, without colour.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub symbol: char,
    pub text: String,
    pub role: Role,
    pub attention: bool,
}

impl Status {
    fn new(symbol: char, text: impl Into<String>, role: Role) -> Self {
        Self {
            symbol,
            text: text.into(),
            role,
            attention: false,
        }
    }

    fn needing_attention(mut self) -> Self {
        self.attention = true;
        self
    }
}

/// `service`'s status at `now`, by the tui spec's table, first row that
/// fits:
///
/// | Facts | Symbol and text | Role | Attention |
/// |---|---|---|---|
/// | active `failed` | `✗ failed 2h 5m ago` (`✗ failed` without a time) | Problem | yes |
/// | sub `auto-restart` | `↻ restarting` | Problem | yes |
/// | `active`/`running` | `✓ running · 2d 3h` (`✓ running` without a time) | Ok | no |
/// | `active`/`exited` | `✓ finished` | Ok | no |
/// | `active`/other sub | `✓ <sub>` | Ok | no |
/// | `activating` | `◌ starting` | Warn | no |
/// | `deactivating` | `◌ stopping` | Warn | no |
/// | `inactive`, unit file `disabled`, no timer run due | `! disabled · not cleaned up` | Warn | yes |
/// | `inactive` with a timer's next run | `✓ scheduled · next run in 3h` | Ok | no |
/// | `inactive` otherwise | `○ stopped` | Idle | no |
/// | any other active state | `◌ <state>` | Warn | no |
///
/// "Ago" is `now` minus `StateChangeTimestamp`, the running time
/// sb-host's `Service::runtime` (`now` minus `ActiveEnterTimestamp`); a
/// time ahead of `now` (a clock moved
/// back) counts as zero. The next run is sb-host's `format_next_in`, so a
/// run already due reads `next run soon`. Durations are sb-host's
/// `format_duration`, so the picker and motd write times alike.
#[must_use]
pub fn service_status(service: &Service, now: Timestamp) -> Status {
    let since = |time: Option<Timestamp>| time.map(|time| format_duration(elapsed(time, now)));
    let next_run = service.timer.as_ref().and_then(|timer| timer.next);
    match (service.active.as_str(), service.sub.as_str()) {
        ("failed", _) => match since(service.state_change) {
            Some(ago) => Status::new('✗', format!("failed {ago} ago"), Role::Problem),
            None => Status::new('✗', "failed", Role::Problem),
        }
        .needing_attention(),
        (_, "auto-restart") => Status::new('↻', "restarting", Role::Problem).needing_attention(),
        ("active", "running") => match service.runtime(now).map(format_duration) {
            Some(up) => Status::new('✓', format!("running · {up}"), Role::Ok),
            None => Status::new('✓', "running", Role::Ok),
        },
        ("active", "exited") => Status::new('✓', "finished", Role::Ok),
        ("active", sub) => Status::new('✓', sub, Role::Ok),
        ("activating", _) => Status::new('◌', "starting", Role::Warn),
        ("deactivating", _) => Status::new('◌', "stopping", Role::Warn),
        ("inactive", _) => match next_run {
            Some(next) => {
                // sb-host's text: `soon` for a run already due.
                let when = match format_next_in(Some(next), now) {
                    soon if soon == "soon" => "next run soon".to_owned(),
                    left => format!("next run in {left}"),
                };
                Status::new('✓', format!("scheduled · {when}"), Role::Ok)
            }
            // A disabled unit that neither runs nor waits for a timer is
            // left over from an app that wasn't cleaned up.
            None if service.unit_file_state.as_deref() == Some("disabled") => {
                Status::new('!', "disabled · not cleaned up", Role::Warn).needing_attention()
            }
            None => Status::new('○', "stopped", Role::Idle),
        },
        (other, _) => Status::new('◌', other, Role::Warn),
    }
}

/// `now` minus `then`, zero when `then` is ahead (a clock moved back).
fn elapsed(then: Timestamp, now: Timestamp) -> Duration {
    Duration::try_from(now.duration_since(then)).unwrap_or(Duration::ZERO)
}

#[cfg(test)]
mod tests {
    use sb_host::systemd::Timer;

    use super::*;

    /// 2026-11-13 10:00:00 UTC.
    fn now() -> Timestamp {
        "2026-11-13T10:00:00Z".parse().expect("a time")
    }

    /// `secs` before [`now`].
    fn before(secs: i64) -> Timestamp {
        now() - jiff::SignedDuration::from_secs(secs)
    }

    fn service(active: &str, sub: &str) -> Service {
        Service {
            name: "app".into(),
            active: active.into(),
            sub: sub.into(),
            unit_file_state: Some("enabled".into()),
            active_enter: None,
            state_change: None,
            timer: None,
        }
    }

    fn timer(next: Option<Timestamp>) -> Timer {
        Timer {
            active: "active".into(),
            sub: "waiting".into(),
            next,
        }
    }

    /// The symbol, text, role and attention of `service` at [`now`].
    fn shown(service: &Service) -> (char, String, Role, bool) {
        let s = service_status(service, now());
        (s.symbol, s.text, s.role, s.attention)
    }

    fn row(symbol: char, text: &str, role: Role, attention: bool) -> (char, String, Role, bool) {
        (symbol, text.to_owned(), role, attention)
    }

    #[test]
    fn a_failed_unit_says_how_long_ago_from_its_state_change() {
        let mut s = service("failed", "failed");
        s.state_change = Some(before(2 * 3600 + 5 * 60));
        // The running time's timestamp isn't the failure's.
        s.active_enter = Some(before(9 * 3600));
        assert_eq!(shown(&s), row('✗', "failed 2h 5m ago", Role::Problem, true));
    }

    #[test]
    fn a_failed_unit_without_a_time_says_failed() {
        assert_eq!(
            shown(&service("failed", "failed")),
            row('✗', "failed", Role::Problem, true)
        );
    }

    #[test]
    fn auto_restart_is_restarting_whatever_the_active_state() {
        for active in ["activating", "active", "deactivating"] {
            assert_eq!(
                shown(&service(active, "auto-restart")),
                row('↻', "restarting", Role::Problem, true),
                "{active}"
            );
        }
    }

    #[test]
    fn a_running_unit_shows_how_long_it_has_run() {
        let mut s = service("active", "running");
        s.active_enter = Some(before(2 * 86_400 + 3 * 3600));
        s.state_change = Some(before(60));
        assert_eq!(shown(&s), row('✓', "running · 2d 3h", Role::Ok, false));
    }

    #[test]
    fn a_running_unit_without_a_time_says_running() {
        assert_eq!(
            shown(&service("active", "running")),
            row('✓', "running", Role::Ok, false)
        );
    }

    #[test]
    fn an_active_exited_unit_is_finished() {
        assert_eq!(
            shown(&service("active", "exited")),
            row('✓', "finished", Role::Ok, false)
        );
    }

    #[test]
    fn another_active_sub_state_is_shown_as_it_is() {
        assert_eq!(
            shown(&service("active", "listening")),
            row('✓', "listening", Role::Ok, false)
        );
    }

    #[test]
    fn activating_and_deactivating_are_starting_and_stopping() {
        assert_eq!(
            shown(&service("activating", "start")),
            row('◌', "starting", Role::Warn, false)
        );
        assert_eq!(
            shown(&service("deactivating", "stop-sigterm")),
            row('◌', "stopping", Role::Warn, false)
        );
    }

    /// The spec's role, Warn, where proto-logs had Problem.
    #[test]
    fn a_disabled_inactive_unit_without_a_timer_run_is_a_leftover() {
        let mut s = service("inactive", "dead");
        s.unit_file_state = Some("disabled".into());
        let leftover = row('!', "disabled · not cleaned up", Role::Warn, true);
        assert_eq!(shown(&s), leftover);
        // A timer with no run due doesn't schedule it.
        s.timer = Some(timer(None));
        assert_eq!(shown(&s), leftover);
    }

    #[test]
    fn an_inactive_unit_with_a_timer_run_is_scheduled() {
        let mut s = service("inactive", "dead");
        s.timer = Some(timer(Some(now() + jiff::SignedDuration::from_hours(3))));
        let scheduled = row('✓', "scheduled · next run in 3h", Role::Ok, false);
        assert_eq!(shown(&s), scheduled);
        // A disabled unit run by its timer isn't a leftover.
        s.unit_file_state = Some("disabled".into());
        assert_eq!(shown(&s), scheduled);
    }

    #[test]
    fn a_timer_run_already_due_is_soon() {
        let mut s = service("inactive", "dead");
        s.timer = Some(timer(Some(now())));
        assert_eq!(
            shown(&s),
            row('✓', "scheduled · next run soon", Role::Ok, false)
        );
    }

    #[test]
    fn an_inactive_unit_otherwise_is_stopped() {
        let mut s = service("inactive", "dead");
        assert_eq!(shown(&s), row('○', "stopped", Role::Idle, false));
        s.unit_file_state = None;
        assert_eq!(shown(&s), row('○', "stopped", Role::Idle, false));
        s.unit_file_state = Some("static".into());
        assert_eq!(shown(&s), row('○', "stopped", Role::Idle, false));
    }

    #[test]
    fn any_other_active_state_is_shown_as_it_is() {
        assert_eq!(
            shown(&service("reloading", "reload")),
            row('◌', "reloading", Role::Warn, false)
        );
        assert_eq!(
            shown(&service("maintenance", "clean")),
            row('◌', "maintenance", Role::Warn, false)
        );
    }

    /// A clock moved back: the time ahead of now counts as zero.
    #[test]
    fn a_time_ahead_of_now_counts_as_zero() {
        let mut s = service("failed", "failed");
        s.state_change = Some(now() + jiff::SignedDuration::from_secs(30));
        assert_eq!(shown(&s).1, "failed 0s ago");
        let mut s = service("active", "running");
        s.active_enter = Some(now() + jiff::SignedDuration::from_secs(30));
        assert_eq!(shown(&s).1, "running · 0s");
    }
}
