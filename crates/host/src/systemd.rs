//! The systemd service list (host spec, "systemd services"; sb-go's
//! host/systemd.go), with sb-go's duration texts for motd.
//!
//! [`services`] reads systemctl's JSON tables, not sb-go's human-readable
//! columns (host spec, assumption 4): `list-units`, `list-unit-files` and
//! `list-timers` at once, then one `show` over the matched units for their
//! two timestamps. Each is a capture with a 10 s timeout taking the
//! caller's token, so the call takes at most two timeouts.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use jiff::civil::DateTime;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use sb_exec::Cmd;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio_util::sync::CancellationToken;

/// sb-go's prefix of the units Saltbox manages.
const MANAGED_PREFIX: &str = "saltbox_managed_";

/// Each systemctl call's limit (host spec, difference 7).
const TIMEOUT: Duration = Duration::from_secs(10);

/// Which services to list: sb-go's `ServiceFilter`, on the name without
/// `.service`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    /// Names starting with this.
    Prefix(String),
    /// This name exactly.
    Exact(String),
}

impl Filter {
    fn matches(&self, name: &str) -> bool {
        match self {
            Self::Prefix(prefix) => name.starts_with(prefix.as_str()),
            Self::Exact(exact) => name == exact,
        }
    }
}

/// sb-go's `DefaultFilters`: the Saltbox-managed units and `docker`.
#[must_use]
pub fn default_filters() -> Vec<Filter> {
    vec![
        Filter::Prefix(MANAGED_PREFIX.to_string()),
        Filter::Exact("docker".to_string()),
    ]
}

/// The default filters plus each of `names` exactly, `.service` stripped
/// and empty names skipped (sb-go's `FiltersWithAdditional`; motd.yml's
/// configured services).
#[must_use]
pub fn with_additional<S: AsRef<str>>(names: &[S]) -> Vec<Filter> {
    let mut filters = default_filters();
    for name in names {
        let name = name.as_ref();
        let name = name.strip_suffix(".service").unwrap_or(name);
        if !name.is_empty() {
            filters.push(Filter::Exact(name.to_string()));
        }
    }
    filters
}

/// One matched service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    /// The unit's name without `.service`.
    pub name: String,
    /// The active state (`active`, `inactive`, `failed`, `activating`, ...).
    pub active: String,
    /// The sub state (`running`, `dead`, `exited`, `auto-restart`, ...).
    pub sub: String,
    /// The unit file's state (`enabled`, `disabled`, `static`, ...), when
    /// `list-unit-files` lists one.
    pub unit_file_state: Option<String>,
    /// When the unit last entered its active state, when known.
    pub active_enter: Option<Timestamp>,
    /// When the unit last changed state, when known (the log picker's
    /// "failed 2h ago", tui spec).
    pub state_change: Option<Timestamp>,
    /// Its timer (`<name>.timer`), when one is loaded.
    pub timer: Option<Timer>,
}

/// A service's timer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timer {
    /// The timer's active state.
    pub active: String,
    /// The timer's sub state (`waiting`, ...).
    pub sub: String,
    /// Its next run, when one is scheduled.
    pub next: Option<Timestamp>,
}

impl Service {
    /// How long an active service has run at `now`: now minus its
    /// active-enter time, never below zero. `None` when it isn't active or
    /// the time is unknown. sb-go computed it from the monotonic timestamp
    /// and `/proc/uptime`; the wall-clock timestamp gives the same value in
    /// one step (host spec, difference 9).
    #[must_use]
    pub fn runtime(&self, now: Timestamp) -> Option<Duration> {
        if self.active != "active" {
            return None;
        }
        let since = now.duration_since(self.active_enter?);
        Some(Duration::try_from(since).unwrap_or(Duration::ZERO))
    }
}

/// The service list couldn't be read, in sb-go's words (one text for the
/// whole list, host spec difference 6).
#[derive(Debug, thiserror::Error)]
#[error("failed to list systemd services: {cause}")]
pub struct ServicesError {
    cause: String,
}

/// systemctl, as the service list runs it.
#[derive(Debug, Clone)]
pub struct Systemd {
    program: PathBuf,
    timeout: Duration,
}

impl Default for Systemd {
    fn default() -> Self {
        Self::new()
    }
}

impl Systemd {
    /// `systemctl` from `PATH`, each call limited to 10 s.
    #[must_use]
    pub fn new() -> Self {
        Self {
            program: PathBuf::from("systemctl"),
            timeout: TIMEOUT,
        }
    }

    /// Run `program` as systemctl instead (tests).
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn with_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = program.into();
        self
    }

    /// Each call's limit instead (tests).
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn with_timeout(mut self, limit: Duration) -> Self {
        self.timeout = limit;
        self
    }

    /// The services `filters` match, sorted by name, without duplicates.
    ///
    /// - Loaded units come from `list-units`, `not-found` ones dropped;
    ///   unit files systemd hasn't loaded (a disabled leftover) from
    ///   `list-unit-files`, as `inactive`/`dead`.
    /// - A timer is `<name>.timer` from the same `list-units` call; its
    ///   next run comes from `list-timers` (the entry activating the
    ///   service, as sb-go's).
    /// - Both timestamps come from one `show` over the matched units.
    ///
    /// The three listings run at once (tokio's `join!`,
    /// <https://docs.rs/tokio/1.54.1/tokio/macro.join.html>), then `show`:
    /// at most two timeouts. Cancelling `token` ends every call.
    ///
    /// # Errors
    ///
    /// [`ServicesError`] when `list-units` or `list-unit-files` fails, is
    /// interrupted or answers with something that isn't its table. A
    /// failed `list-timers` or `show` leaves its fields empty instead, as
    /// sb-go ignores those errors; the run log gets why. So does a cancel
    /// that comes after both listings have answered: the list returns
    /// without next runs or times, and a caller that cancelled drops it.
    pub async fn services(
        &self,
        filters: &[Filter],
        token: &CancellationToken,
    ) -> Result<Vec<Service>, ServicesError> {
        let units = self.table::<UnitRow>(
            self.cmd()
                .arg("list-units")
                .arg("--type=service,timer")
                .arg("--all")
                .arg("--output=json"),
            token,
        );
        let files = self.table::<UnitFileRow>(
            self.cmd()
                .arg("list-unit-files")
                .arg("--type=service")
                .arg("--output=json"),
            token,
        );
        let timers = self.table::<TimerRow>(
            self.cmd()
                .arg("list-timers")
                .arg("--all")
                .arg("--output=json"),
            token,
        );
        let (units, files, timers) = tokio::join!(units, files, timers);
        let units = units.map_err(|cause| ServicesError { cause })?;
        let files = files.map_err(|cause| ServicesError { cause })?;
        let timers = timers.unwrap_or_else(|cause| {
            tracing::info!(cause, "systemd timers not read");
            Vec::new()
        });
        let mut found = matched(filters, &units, &files, &timers);
        if !found.is_empty() {
            match self.times(found.keys(), token).await {
                Ok(read) => {
                    for (name, service) in &mut found {
                        if let Some(&(active_enter, state_change)) = read.get(name.as_str()) {
                            service.active_enter = active_enter;
                            service.state_change = state_change;
                        }
                    }
                }
                Err(cause) => tracing::info!(cause, "systemd service times not read"),
            }
        }
        tracing::info!(count = found.len(), "systemd services listed");
        Ok(found.into_values().collect())
    }

    fn cmd(&self) -> Cmd {
        Cmd::new(self.program.as_os_str()).timeout(self.timeout)
    }

    /// `cmd`'s JSON table.
    async fn table<T: DeserializeOwned>(
        &self,
        cmd: Cmd,
        token: &CancellationToken,
    ) -> Result<Vec<T>, String> {
        let stdout = run(&cmd, token).await?;
        serde_json::from_str(&stdout).map_err(|err| format!("read systemctl's table: {err}"))
    }

    /// Each matched unit's active-enter and state-change times, by name
    /// without `.service`.
    async fn times(
        &self,
        names: impl Iterator<Item = &String>,
        token: &CancellationToken,
    ) -> Result<HashMap<String, Times>, String> {
        let mut cmd = self
            .cmd()
            .arg("show")
            .arg("--timestamp=utc")
            .arg("--property=Id,ActiveEnterTimestamp,StateChangeTimestamp");
        for name in names {
            cmd = cmd.value(format!("{name}.service"));
        }
        Ok(parse_show(&run(&cmd, token).await?))
    }
}

/// The services `filters` match, with [`Systemd::new`].
///
/// # Errors
///
/// As [`Systemd::services`].
pub async fn services(
    filters: &[Filter],
    token: &CancellationToken,
) -> Result<Vec<Service>, ServicesError> {
    Systemd::new().services(filters, token).await
}

/// `cmd`'s stdout when it exits 0, else why not.
async fn run(cmd: &Cmd, token: &CancellationToken) -> Result<String, String> {
    let ran = cmd
        .capture(token)
        .await
        .map_err(|err| format!("{err}: {}", err.cause()))?;
    match ran.check() {
        Ok(ran) => Ok(ran.stdout),
        Err(failed) => Err(match failed.last_lines().last() {
            // On one line, controls escaped, as sb-cli-core's
            // `escape_controls`: std's `str::escape_debug`
            // (<https://doc.rust-lang.org/std/primitive.str.html#method.escape_debug>).
            Some(line) => format!("{failed}: {}", line.escape_debug()),
            None => failed.to_string(),
        }),
    }
}

/// One row of `systemctl list-units --output=json`.
#[derive(Debug, Deserialize)]
struct UnitRow {
    unit: String,
    load: String,
    active: String,
    sub: String,
}

/// One row of `systemctl list-unit-files --output=json`: the "UNIT FILE"
/// header becomes `unit_file` (`string_to_json_field_name()` in systemd's
/// src/shared/format-table.c), seen on 249, 255 and 259 (the fixtures).
#[derive(Debug, Deserialize)]
struct UnitFileRow {
    unit_file: String,
    state: String,
}

/// One row of `systemctl list-timers --output=json`: `next` in
/// microseconds since the epoch, 0 or null when nothing is scheduled.
#[derive(Debug, Deserialize)]
struct TimerRow {
    next: Option<u64>,
    activates: Option<String>,
}

/// The active-enter and state-change times.
type Times = (Option<Timestamp>, Option<Timestamp>);

/// The matched services by name, before their times. sb-go's rules: a
/// `not-found` unit is dropped (its unit file too, if a listing still
/// names one), and a name nothing loaded or on disk matches isn't there.
fn matched(
    filters: &[Filter],
    units: &[UnitRow],
    files: &[UnitFileRow],
    timers: &[TimerRow],
) -> BTreeMap<String, Service> {
    let wanted = |name: &str| filters.iter().any(|filter| filter.matches(name));
    let file_state: HashMap<&str, &str> = files
        .iter()
        .filter_map(|row| Some((row.unit_file.strip_suffix(".service")?, row.state.as_str())))
        .collect();
    // An alias's file (`state: alias`) names another unit, which
    // list-units lists under its own name: no service of its own, as
    // sb-go's pattern lists match the primary name only.
    let unloaded = files
        .iter()
        .filter(|row| row.state != "alias")
        .filter_map(|row| row.unit_file.strip_suffix(".service"));
    let next_run: HashMap<&str, Timestamp> = timers
        .iter()
        .filter_map(|row| {
            let service = row.activates.as_deref()?.strip_suffix(".service")?;
            Some((service, from_micros(row.next?)?))
        })
        .collect();
    let timer_units: HashMap<&str, &UnitRow> = units
        .iter()
        .filter(|row| row.load != "not-found")
        .filter_map(|row| Some((row.unit.strip_suffix(".timer")?, row)))
        .collect();
    let service = |name: &str, active: &str, sub: &str| Service {
        name: name.to_string(),
        active: active.to_string(),
        sub: sub.to_string(),
        unit_file_state: file_state.get(name).map(|state| (*state).to_string()),
        active_enter: None,
        state_change: None,
        timer: timer_units.get(name).map(|timer| Timer {
            active: timer.active.clone(),
            sub: timer.sub.clone(),
            next: next_run.get(name).copied(),
        }),
    };

    let mut listed = HashSet::new();
    let mut found = BTreeMap::new();
    for row in units {
        let Some(name) = row.unit.strip_suffix(".service") else {
            continue;
        };
        listed.insert(name);
        if row.load != "not-found" && wanted(name) {
            found.insert(name.to_string(), service(name, &row.active, &row.sub));
        }
    }
    for name in unloaded {
        if !listed.contains(name) && wanted(name) {
            found.insert(name.to_string(), service(name, "inactive", "dead"));
        }
    }
    found
}

/// A list-timers time: microseconds since the epoch (systemd's `usec_t`),
/// 0 meaning none.
fn from_micros(micros: u64) -> Option<Timestamp> {
    if micros == 0 {
        return None;
    }
    Timestamp::from_microsecond(i64::try_from(micros).ok()?).ok()
}

/// `systemctl show`'s blocks (one per unit, a blank line between),
/// `Id=<unit>` and the two timestamps, by name without `.service`. Text,
/// not JSON: `--output` sets the journal's format and `show` has no table;
/// systemctl(1) says `show`'s output is "intended to be computer-parsable"
/// (<https://www.freedesktop.org/software/systemd/man/latest/systemctl.html#show%20PATTERN%E2%80%A6%7CJOB%E2%80%A6>).
fn parse_show(text: &str) -> HashMap<String, Times> {
    let mut times = HashMap::new();
    for block in text.split("\n\n") {
        let (mut id, mut active_enter, mut state_change) = (None, None, None);
        for line in block.lines() {
            match line.split_once('=') {
                Some(("Id", value)) => id = value.strip_suffix(".service"),
                Some(("ActiveEnterTimestamp", value)) => active_enter = parse_utc(value),
                Some(("StateChangeTimestamp", value)) => state_change = parse_utc(value),
                _ => {}
            }
        }
        if let Some(id) = id {
            times.insert(id.to_string(), (active_enter, state_change));
        }
    }
    times
}

/// A `--timestamp=utc` value: `Thu 2026-01-15 09:30:00 UTC`. systemd
/// writes the weekday in English whatever the locale (`format_timestamp`
/// in src/basic/time-util.c, v249 on) and the time in UTC, so there is no
/// locale or time zone to read (host spec, assumption 4). `n/a` (249's
/// text for no time), an empty value or anything else reads as unknown.
/// Parsed with jiff's `strptime`
/// (<https://docs.rs/jiff/0.2.38/jiff/civil/struct.DateTime.html#method.strptime>),
/// which checks the weekday against the date; the ` UTC` is taken off
/// first, because jiff's `%Z` is for formatting only (its strtime table).
fn parse_utc(value: &str) -> Option<Timestamp> {
    let local = value.strip_suffix(" UTC")?;
    let time = DateTime::strptime("%a %Y-%m-%d %H:%M:%S", local).ok()?;
    let at = TimeZone::UTC.to_timestamp(time).ok()?;
    (at > Timestamp::UNIX_EPOCH).then_some(at)
}

/// sb-go's `FormatDuration`: `Ns`, `Nm Ns`, `Nh Nm` or `Nd Nh`, a zero
/// minor unit left out, whole units only.
#[must_use]
pub fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    let (minute, hour, day) = (60, 60 * 60, 24 * 60 * 60);
    let (major, minor, units) = if secs < minute {
        return format!("{secs}s");
    } else if secs < hour {
        (secs / minute, secs % minute, ("m", "s"))
    } else if secs < day {
        (secs / hour, secs / minute % 60, ("h", "m"))
    } else {
        (secs / day, secs / hour % 24, ("d", "h"))
    };
    if minor > 0 {
        format!("{major}{} {minor}{}", units.0, units.1)
    } else {
        format!("{major}{}", units.0)
    }
}

/// sb-go's `formatNextIn`: how long until `next` at `now`; `soon` for a
/// run already due, empty for an unknown time.
#[must_use]
pub fn format_next_in(next: Option<Timestamp>, now: Timestamp) -> String {
    let Some(next) = next else {
        return String::new();
    };
    let left = next.duration_since(now);
    if left <= SignedDuration::ZERO {
        return "soon".to_string();
    }
    format_duration(Duration::try_from(left).unwrap_or(Duration::ZERO))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Instant;

    use super::*;
    use crate::testing::{SPAWNS, stand_in};

    const RELEASES: [&str; 3] = ["22.04", "24.04", "26.04"];

    fn fixtures(release: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/fixtures/systemd")
            .join(release)
    }

    fn at(text: &str) -> Timestamp {
        text.parse().expect("timestamp")
    }

    /// A stand-in systemctl answering each call with `release`'s capture,
    /// after checking its arguments are the spec's.
    fn systemctl(dir: &Path, release: &str) -> PathBuf {
        let f = fixtures(release);
        let f = f.display();
        stand_in(
            dir,
            "systemctl",
            &format!(
                r#"case "$*" in
  "list-units --type=service,timer --all --output=json") cat '{f}/list-units.json' ;;
  "list-unit-files --type=service --output=json") cat '{f}/list-unit-files.json' ;;
  "list-timers --all --output=json") cat '{f}/list-timers.json' ;;
  "show --timestamp=utc --property=Id,ActiveEnterTimestamp,StateChangeTimestamp "*) cat '{f}/show.txt' ;;
  *) echo "unexpected: $*" >&2; exit 9 ;;
esac"#
            ),
        )
    }

    async fn list(program: &Path, filters: &[Filter]) -> Result<Vec<Service>, ServicesError> {
        Systemd::new()
            .with_program(program)
            .services(filters, &CancellationToken::new())
            .await
    }

    #[tokio::test]
    async fn each_release_lists_the_managed_services_and_docker() {
        let _spawns = SPAWNS.lock().await;
        for release in RELEASES {
            let dir = tempfile::tempdir().expect("tempdir");
            let program = systemctl(dir.path(), release);
            let found = list(&program, &default_filters()).await.expect(release);
            let names: Vec<&str> = found.iter().map(|s| s.name.as_str()).collect();
            assert_eq!(
                names,
                [
                    "docker",
                    "saltbox_managed_crash",
                    "saltbox_managed_job",
                    "saltbox_managed_left",
                    "saltbox_managed_once",
                    "saltbox_managed_restart",
                    "saltbox_managed_web",
                ],
                "{release}"
            );
            let by = |name: &str| found.iter().find(|s| s.name == name).expect(name);
            let states = |s: &Service| (s.active.clone(), s.sub.clone(), s.unit_file_state.clone());
            let state = |a: &str, b: &str, c: &str| (a.into(), b.into(), Some(c.to_string()));
            assert_eq!(
                states(by("docker")),
                state("active", "running", "static"),
                "{release}"
            );
            assert_eq!(
                states(by("saltbox_managed_crash")),
                state("failed", "failed", "static")
            );
            assert_eq!(
                states(by("saltbox_managed_restart")),
                state("activating", "auto-restart", "static")
            );
            assert_eq!(
                states(by("saltbox_managed_once")),
                state("active", "exited", "static")
            );
            assert_eq!(
                states(by("saltbox_managed_web")),
                state("active", "running", "enabled")
            );
            // The disabled leftover, never loaded: from list-unit-files.
            let left = by("saltbox_managed_left");
            assert_eq!(
                states(left),
                state("inactive", "dead", "disabled"),
                "{release}"
            );
            assert_eq!(
                (left.active_enter, left.state_change, &left.timer),
                (None, None, &None)
            );
            // The timer, its state from list-units, its next run from
            // list-timers (the next midnight after the capture).
            let job = by("saltbox_managed_job");
            assert_eq!(
                job.timer,
                Some(Timer {
                    active: "active".into(),
                    sub: "waiting".into(),
                    next: Some(at("2026-01-16T00:00:00Z")),
                }),
                "{release}"
            );
            assert_eq!(job.active_enter, None, "{release}");
            // Both timestamps from show, on the day of the capture.
            let web = by("saltbox_managed_web");
            let enter = web.active_enter.expect("active enter");
            assert_eq!(
                enter.strftime("%Y-%m-%d").to_string(),
                "2026-01-15",
                "{release}"
            );
            assert!(web.state_change.is_some(), "{release}");
            assert!(
                by("saltbox_managed_crash").state_change.is_some(),
                "{release}"
            );
            assert_eq!(web.timer, None);
        }
    }

    #[tokio::test]
    async fn configured_names_are_added_and_unknown_ones_left_out() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let program = systemctl(dir.path(), "24.04");
        let filters = with_additional(&["other.service", "nothing-here"]);
        let found = list(&program, &filters).await.expect("list");
        let names: Vec<&str> = found.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"other"), "{names:?}");
        assert!(!names.contains(&"nothing-here"), "{names:?}");
        assert!(!names.contains(&"saltbox_managed_gone"), "{names:?}");
    }

    #[tokio::test]
    async fn an_alias_unit_file_is_no_service_of_its_own() {
        let _spawns = SPAWNS.lock().await;
        for release in RELEASES {
            let dir = tempfile::tempdir().expect("tempdir");
            let program = systemctl(dir.path(), release);
            // procps.service is an alias of systemd-sysctl.service on each
            // release: list-unit-files names it, list-units doesn't.
            let found = list(&program, &with_additional(&["procps"]))
                .await
                .expect("list");
            assert!(found.iter().all(|s| s.name != "procps"), "{release}");
        }
    }

    #[tokio::test]
    async fn a_failed_listing_is_the_error_with_its_last_line() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let program = stand_in(
            dir.path(),
            "systemctl",
            r#"case "$1" in list-units) printf 'Failed to connect to bus\033[31m\n' >&2; exit 1 ;; *) echo '[]' ;; esac"#,
        );
        let error = list(&program, &default_filters())
            .await
            .expect_err("failed");
        let text = error.to_string();
        assert!(
            text.starts_with("failed to list systemd services: "),
            "{text}"
        );
        assert!(
            text.ends_with(": Failed to connect to bus\\u{1b}[31m"),
            "{text}"
        );
        assert!(text.contains("exited with code 1"), "{text}");
    }

    #[tokio::test]
    async fn a_failed_unit_file_listing_is_the_error() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let program = stand_in(
            dir.path(),
            "systemctl",
            r#"case "$1" in list-unit-files) exit 3 ;; *) echo '[]' ;; esac"#,
        );
        let error = list(&program, &default_filters())
            .await
            .expect_err("failed");
        assert!(error.to_string().contains("exited with code 3"), "{error}");
    }

    #[tokio::test]
    async fn a_listing_that_isnt_a_table_is_the_error() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let program = stand_in(dir.path(), "systemctl", "echo 'UNIT LOAD ACTIVE SUB'");
        let error = list(&program, &default_filters())
            .await
            .expect_err("failed");
        assert!(
            error
                .to_string()
                .starts_with("failed to list systemd services: read systemctl's table: "),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_missing_program_is_the_error() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let error = list(&dir.path().join("missing"), &default_filters())
            .await
            .expect_err("failed");
        assert!(
            error
                .to_string()
                .starts_with("failed to list systemd services: could not run "),
            "{error}"
        );
    }

    #[tokio::test]
    async fn failed_timers_and_show_leave_their_fields_empty() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let f = fixtures("26.04");
        let f = f.display();
        let program = stand_in(
            dir.path(),
            "systemctl",
            &format!(
                r#"case "$1" in
  list-units) cat '{f}/list-units.json' ;;
  list-unit-files) cat '{f}/list-unit-files.json' ;;
  *) exit 1 ;;
esac"#
            ),
        );
        let found = list(&program, &default_filters()).await.expect("list");
        let job = found
            .iter()
            .find(|s| s.name == "saltbox_managed_job")
            .expect("job");
        assert_eq!(job.timer.as_ref().map(|t| t.next), Some(None));
        assert!(
            found
                .iter()
                .all(|s| s.active_enter.is_none() && s.state_change.is_none())
        );
    }

    #[tokio::test]
    async fn no_match_runs_no_show() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let program = stand_in(
            dir.path(),
            "systemctl",
            r#"case "$1" in show) exit 9 ;; *) echo '[]' ;; esac"#,
        );
        let found = list(&program, &default_filters()).await.expect("list");
        assert_eq!(found, []);
    }

    #[tokio::test]
    async fn the_listings_run_at_once_so_the_call_takes_at_most_two_timeouts() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let f = fixtures("22.04");
        let f = f.display();
        // list-units and list-unit-files answer after most of a timeout;
        // list-timers and show never end.
        let program = stand_in(
            dir.path(),
            "systemctl",
            &format!(
                r#"case "$1" in
  list-units) sleep 1.2; cat '{f}/list-units.json' ;;
  list-unit-files) sleep 1.2; cat '{f}/list-unit-files.json' ;;
  *) exec sleep 60 ;;
esac"#
            ),
        );
        let start = Instant::now();
        let found = Systemd::new()
            .with_program(&program)
            .with_timeout(Duration::from_secs(2))
            .services(&default_filters(), &CancellationToken::new())
            .await
            .expect("list");
        let took = start.elapsed();
        assert_eq!(found.len(), 7);
        assert!(found.iter().all(|s| s.active_enter.is_none()));
        // Two timeouts (4 s) and their kills; one after another the calls
        // would take 1.2 + 1.2 + 2 + 2 s.
        assert!(took >= Duration::from_secs(4), "{took:?}");
        assert!(took < Duration::from_millis(5800), "{took:?}");
    }

    #[tokio::test]
    async fn cancelling_ends_the_call() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let program = stand_in(dir.path(), "systemctl", "exec sleep 60");
        let token = CancellationToken::new();
        let cancel = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            cancel.cancel();
        });
        let start = Instant::now();
        let error = Systemd::new()
            .with_program(&program)
            .services(&default_filters(), &token)
            .await
            .expect_err("interrupted");
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "{:?}",
            start.elapsed()
        );
        assert!(error.to_string().contains("was interrupted"), "{error}");
    }

    #[test]
    fn the_default_filters_are_sb_gos() {
        let filters = default_filters();
        let wanted = |name: &str| filters.iter().any(|f| f.matches(name));
        assert!(wanted("saltbox_managed_x"));
        assert!(wanted("docker"));
        assert!(!wanted("dockerd"));
        assert!(!wanted("x_saltbox_managed_"));
    }

    #[test]
    fn additional_names_lose_service_and_skip_empty() {
        let filters = with_additional(&["plex.service", "", ".service", "jellyfin"]);
        assert_eq!(
            filters[2..],
            [
                Filter::Exact("plex".into()),
                Filter::Exact("jellyfin".into())
            ]
        );
    }

    #[test]
    fn a_not_found_unit_is_dropped_even_if_a_file_is_listed() {
        let units = [UnitRow {
            unit: "saltbox_managed_x.service".into(),
            load: "not-found".into(),
            active: "inactive".into(),
            sub: "dead".into(),
        }];
        let files = [UnitFileRow {
            unit_file: "saltbox_managed_x.service".into(),
            state: "disabled".into(),
        }];
        assert!(matched(&default_filters(), &units, &files, &[]).is_empty());
    }

    #[test]
    fn a_not_found_timer_is_no_timer() {
        let row = |unit: &str, load: &str| UnitRow {
            unit: unit.into(),
            load: load.into(),
            active: "inactive".into(),
            sub: "dead".into(),
        };
        let units = [
            row("saltbox_managed_x.service", "loaded"),
            row("saltbox_managed_x.timer", "not-found"),
        ];
        let found = matched(&default_filters(), &units, &[], &[]);
        assert_eq!(found["saltbox_managed_x"].timer, None);
        assert_eq!(found["saltbox_managed_x"].unit_file_state, None);
    }

    #[test]
    fn a_timer_without_a_next_run_has_none() {
        let timers = [
            TimerRow {
                next: Some(0),
                activates: Some("a.service".into()),
            },
            TimerRow {
                next: None,
                activates: Some("b.service".into()),
            },
            TimerRow {
                next: Some(1_000_000),
                activates: None,
            },
        ];
        let units: Vec<UnitRow> = ["a", "b"]
            .iter()
            .flat_map(|n| ["service", "timer"].map(|kind| format!("{n}.{kind}")))
            .map(|unit| UnitRow {
                unit,
                load: "loaded".into(),
                active: "active".into(),
                sub: "waiting".into(),
            })
            .collect();
        let filters = with_additional(&["a", "b"]);
        let found = matched(&filters, &units, &[], &timers);
        assert_eq!(found["a"].timer.as_ref().and_then(|t| t.next), None);
        assert_eq!(found["b"].timer.as_ref().and_then(|t| t.next), None);
    }

    #[test]
    fn show_values_read_as_times_or_unknown() {
        let text = "Id=a.service\nStateChangeTimestamp=Thu 2026-01-15 10:00:03 UTC\n\
                    ActiveEnterTimestamp=Thu 2026-11-12 10:00:00 UTC\n\n\
                    Id=b.service\nStateChangeTimestamp=n/a\nActiveEnterTimestamp=\n\n\
                    Id=c.service\n\n\
                    StateChangeTimestamp=Thu 2026-01-15 10:00:03 UTC\n";
        let times = parse_show(text);
        assert_eq!(
            times["a"],
            (
                Some(at("2026-11-12T10:00:00Z")),
                Some(at("2026-01-15T10:00:03Z"))
            )
        );
        assert_eq!(times["b"], (None, None));
        assert_eq!(times["c"], (None, None));
        assert_eq!(times.len(), 3);
    }

    #[test]
    fn utc_values_need_the_utc_suffix_and_a_real_date() {
        assert_eq!(
            parse_utc("Thu 2026-01-15 10:00:03 UTC"),
            Some(at("2026-01-15T10:00:03Z"))
        );
        assert_eq!(parse_utc("Thu 2026-01-15 10:00:03 CET"), None);
        assert_eq!(parse_utc("@1768470000"), None);
        assert_eq!(parse_utc("n/a"), None);
        assert_eq!(parse_utc(""), None);
        assert_eq!(parse_utc("Thu 1970-01-01 00:00:00 UTC"), None);
        // jiff checks the weekday against the date.
        assert_eq!(parse_utc("Mon 2026-01-15 10:00:03 UTC"), None);
    }

    #[test]
    fn micros_of_zero_or_less_are_no_time() {
        assert_eq!(from_micros(0), None);
        assert_eq!(from_micros(u64::MAX), None);
        assert_eq!(
            from_micros(1_768_521_600_000_000),
            Some(at("2026-01-16T00:00:00Z"))
        );
    }

    #[test]
    fn durations_read_as_sb_gos() {
        let cases = [
            (0, "0s"),
            (59, "59s"),
            (60, "1m"),
            (61, "1m 1s"),
            (3599, "59m 59s"),
            (3600, "1h"),
            (3660, "1h 1m"),
            (3661, "1h 1m"),
            (86_399, "23h 59m"),
            (86_400, "1d"),
            (90_000, "1d 1h"),
            (93_599, "1d 1h"),
            (10 * 86_400 + 23 * 3600, "10d 23h"),
        ];
        for (secs, text) in cases {
            assert_eq!(format_duration(Duration::from_secs(secs)), text, "{secs}");
        }
        assert_eq!(format_duration(Duration::from_millis(59_999)), "59s");
    }

    #[test]
    fn next_runs_read_as_sb_gos() {
        let now = at("2026-11-13T12:00:00Z");
        assert_eq!(format_next_in(None, now), "");
        assert_eq!(format_next_in(Some(now), now), "soon");
        assert_eq!(
            format_next_in(Some(at("2026-11-13T11:00:00Z")), now),
            "soon"
        );
        assert_eq!(
            format_next_in(Some(at("2026-11-13T15:30:00Z")), now),
            "3h 30m"
        );
        assert_eq!(
            format_next_in(Some(at("2026-11-13T12:00:00.5Z")), now),
            "0s"
        );
    }

    #[test]
    fn the_runtime_is_now_minus_the_active_enter_time() {
        let service = Service {
            name: "web".into(),
            active: "active".into(),
            sub: "running".into(),
            unit_file_state: None,
            active_enter: Some(at("2026-11-11T09:00:00Z")),
            state_change: None,
            timer: None,
        };
        let now = at("2026-11-13T12:00:00Z");
        let runtime = service.runtime(now).expect("runtime");
        assert_eq!(format_duration(runtime), "2d 3h");
        // A clock behind the unit's time reads as zero, as sb-go's clamp.
        assert_eq!(
            service.runtime(at("2026-11-10T00:00:00Z")),
            Some(Duration::ZERO)
        );
        let unknown = Service {
            active_enter: None,
            ..service.clone()
        };
        assert_eq!(unknown.runtime(now), None);
        let inactive = Service {
            active: "inactive".into(),
            ..service
        };
        assert_eq!(inactive.runtime(now), None);
    }
}
