//! The system sections that read files and syscalls, no children (motd
//! decision 16): Distribution, Kernel, Uptime, Load Averages, Processes,
//! CPU and Memory Usage. /proc is read through procfs
//! (<https://docs.rs/procfs/0.18.0/procfs/>): each `*_here` reads the
//! host's file with procfs's `Current`, and the formatting takes procfs's
//! types, which the tests read from fixtures with `FromRead`
//! (<https://docs.rs/procfs/0.18.0/procfs/trait.FromRead.html>).

use std::collections::HashSet;
use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

use procfs::{CpuInfo, Current, LoadAverage, Meminfo, Uptime};
use regex::Regex;

use crate::Paint;
use crate::text::one_line;

/// sb-go's value when a source can't answer (`motd/utils.go`,
/// `ExecCommand`).
pub const NOT_AVAILABLE: &str = "Not available";

/// os-release's two places, in the order freedesktop says to read them
/// (<https://www.freedesktop.org/software/systemd/man/latest/os-release.html>:
/// "/etc/os-release takes precedence over /usr/lib/os-release").
const OS_RELEASE: [&str; 2] = ["/etc/os-release", "/usr/lib/os-release"];

/// Distribution on this host: [`distribution`] of the first os-release
/// that exists.
#[must_use]
pub fn distribution_here() -> String {
    distribution(read_os_release(&OS_RELEASE.map(Path::new)).as_deref())
}

/// The first of `places` that exists, read with cli-core's 1 MiB cap. A
/// file that exists but can't be read ends the search, as it would for
/// `lsb_release`; the run log says why.
fn read_os_release(places: &[&Path]) -> Option<String> {
    for place in places {
        match sb_cli_core::config::read_small(place) {
            Ok(Some(text)) => return Some(text),
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(file = %place.display(), "os-release not read: {err}");
                return None;
            }
        }
    }
    tracing::warn!("no os-release file");
    None
}

/// Distribution from os-release's text: `PRETTY_NAME (VERSION_CODENAME)`,
/// as sb-go joins `lsb_release -ds` and `-cs` (`motd/info.go:85-93`);
/// without a codename, the name alone; without a name or the file, `Not
/// available`.
#[must_use]
pub fn distribution(os_release: Option<&str>) -> String {
    let Some(text) = os_release else {
        return NOT_AVAILABLE.to_string();
    };
    // Made safe first, so a value of escapes or spaces alone is absent.
    let shown = |key| {
        os_release_value(text, key)
            .map(|value| one_line(value.as_bytes()))
            .filter(|value| !value.is_empty())
    };
    let Some(name) = shown("PRETTY_NAME") else {
        return NOT_AVAILABLE.to_string();
    };
    match shown("VERSION_CODENAME") {
        Some(codename) => format!("{name} ({codename})"),
        None => name,
    }
}

/// `key`'s value in os-release's format
/// (<https://www.freedesktop.org/software/systemd/man/latest/os-release.html>,
/// "Syntax"): `KEY=value` lines, `#` comments, a value in double or single
/// quotes or bare, and in double quotes a backslash before `$`, `"`, `\`
/// or `` ` `` standing for that character. The last assignment wins, as
/// a shell's would.
///
/// Why not sysinfo: it exposes neither `PRETTY_NAME` nor
/// `VERSION_CODENAME` (motd spec, Tech Stack, "Considered and not used");
/// Uptime's `VERSION_ID` comes from the same parser, so one reading of
/// the file (with freedesktop's `/usr/lib` fallback) decides both. Why
/// not rs-release 0.1.12: it fails the whole file on a line without `=`
/// and undoes no backslash escapes
/// (<https://docs.rs/rs-release/0.1.12/src/rs_release/lib.rs.html>).
/// Why not `shlex::split` for the value: its double-quote rule is the
/// same, but it splits a bare value at spaces, reads `#` as a comment and
/// gives nothing for an unbalanced quote, where a damaged os-release
/// should still show its text.
fn os_release_value(text: &str, key: &str) -> Option<String> {
    let mut found = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name == key {
            found = Some(unquote(value));
        }
    }
    found
}

/// An os-release value without its quotes and escapes.
fn unquote(value: &str) -> String {
    if let Some(inner) = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some(next @ ('$' | '"' | '\\' | '`')) => out.push(next),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => out.push('\\'),
                }
            } else {
                out.push(c);
            }
        }
        out
    } else if let Some(inner) = value
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
    {
        inner.to_string()
    } else {
        value.to_string()
    }
}

/// Kernel on this host: `uname(2)`'s release, as `uname -r` prints it
/// (<https://docs.rs/rustix/1.1.6/rustix/system/fn.uname.html>).
#[must_use]
pub fn kernel_here() -> String {
    kernel(rustix::system::uname().release().to_bytes())
}

/// Kernel from the release bytes, made safe to draw; empty is `Not
/// available`.
#[must_use]
pub fn kernel(release: &[u8]) -> String {
    let shown = one_line(release);
    if shown.is_empty() {
        NOT_AVAILABLE.to_string()
    } else {
        shown
    }
}

/// Which procps formats `uptime -p` on the host's release (motd spec,
/// Uptime: "forms pinned per release"). Ubuntu 22.04 ships procps-ng
/// 3.3.17, whose `sprint_uptime` takes each unit modulo the next
/// (<https://gitlab.com/procps-ng/procps/-/blob/v3.3.17/proc/whattime.c>);
/// 24.04 and 26.04 ship 4.0.4 (Launchpad: `2:4.0.4-4ubuntu3`,
/// `2:4.0.4-9ubuntu1`), whose `procps_uptime_sprint_short` takes each unit
/// out of what is left, only when strictly more is left
/// (<https://gitlab.com/procps-ng/procps/-/blob/v4.0.4/library/uptime.c>).
/// Ubuntu's one patch there (`library_0_days_users`) changes `day` to
/// `days` for zero days, which the short form never prints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Procps {
    V3,
    V4,
}

impl Procps {
    /// The form for os-release's `VERSION_ID`: 3.3 up to 22.04, 4.0
    /// after it and when the release is unknown (the newest form).
    #[must_use]
    pub fn for_release(version_id: Option<&str>) -> Self {
        let major = version_id
            .and_then(|id| id.split('.').next())
            .and_then(|major| major.parse::<u32>().ok());
        match major {
            Some(major) if major <= 22 => Self::V3,
            _ => Self::V4,
        }
    }
}

const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;
const WEEK: u64 = 7 * DAY;
const YEAR: u64 = 365 * DAY;
const DECADE: u64 = 10 * YEAR;

/// procps' `uptime -p` line for `uptime`, `up ` included, in `form`.
#[must_use]
pub fn uptime_pretty(uptime: Duration, form: Procps) -> String {
    let secs = uptime.as_secs();
    // [decades, years, weeks, days, hours, minutes], and whether less than
    // a minute is left (procps prints `0 minutes` then).
    let (units, under_a_minute) = match form {
        Procps::V3 => (
            [
                secs / DECADE,
                (secs / YEAR) % 10,
                (secs / WEEK) % 52,
                (secs / DAY) % 7,
                (secs / HOUR) % 24,
                (secs / MINUTE) % 60,
            ],
            // `uptime_secs < 60`: no whole minute yet.
            secs / MINUTE == 0,
        ),
        Procps::V4 => {
            let mut left = uptime;
            let units = [DECADE, YEAR, WEEK, DAY, HOUR, MINUTE].map(|unit| {
                let size = Duration::from_secs(unit);
                if left > size {
                    let count = left.as_secs() / unit;
                    left -= Duration::from_secs(count * unit);
                    count
                } else {
                    0
                }
            });
            (units, left < Duration::from_secs(MINUTE))
        }
    };
    let names = [
        ("decade", "decades"),
        ("year", "years"),
        ("week", "weeks"),
        ("day", "days"),
        ("hour", "hours"),
    ];
    let mut parts: Vec<String> = units
        .iter()
        .zip(names)
        .filter(|(count, _)| **count > 0)
        .map(|(count, (one, many))| format!("{count} {}", if *count > 1 { many } else { one }))
        .collect();
    let minutes = units[5];
    if minutes > 0 || under_a_minute {
        let unit = if minutes == 1 { "minute" } else { "minutes" };
        parts.push(format!("{minutes} {unit}"));
    }
    format!("up {}", parts.join(", "))
}

/// Uptime: procps' text without `up ` in the value colour, as sb-go trims
/// `uptime -p`'s output (`motd/info.go:101-113`; a line of `up ` alone
/// trims to `up` and stays); no uptime is `Not available`.
#[must_use]
pub fn uptime(uptime: Option<Duration>, form: Procps, paint: &Paint) -> String {
    let Some(uptime) = uptime else {
        return NOT_AVAILABLE.to_string();
    };
    let line = uptime_pretty(uptime, form);
    let line = line.trim();
    paint.value(line.strip_prefix("up ").unwrap_or(line))
}

/// Uptime on this host: /proc/uptime, in the form of the host's release.
#[must_use]
pub fn uptime_here(paint: &Paint) -> String {
    let text = read_os_release(&OS_RELEASE.map(Path::new));
    let version = text
        .as_deref()
        .and_then(|text| os_release_value(text, "VERSION_ID"));
    let form = Procps::for_release(version.as_deref());
    let now = current::<Uptime>("uptime").map(|up| up.uptime_duration());
    uptime(now, form, paint)
}

/// The host's `T` from /proc, or none with the reason in the run log.
fn current<T: Current>(name: &str) -> Option<T> {
    T::current()
        .inspect_err(|err| tracing::warn!(file = name, "not read: {err}"))
        .ok()
}

/// Load Averages: sb-go's `1 min: a | 5 min: b | 15 min: c`, the numbers
/// with the two decimals /proc/loadavg prints, in the value colour
/// (`motd/info.go:115-127`).
#[must_use]
pub fn load_averages(load: Option<&LoadAverage>, paint: &Paint) -> String {
    let Some(load) = load else {
        return NOT_AVAILABLE.to_string();
    };
    let shown = |value: f32| paint.value(&format!("{value:.2}"));
    format!(
        "1 min: {} | 5 min: {} | 15 min: {}",
        shown(load.one),
        shown(load.five),
        shown(load.fifteen)
    )
}

/// Load Averages on this host.
#[must_use]
pub fn load_averages_here(paint: &Paint) -> String {
    load_averages(current::<LoadAverage>("loadavg").as_ref(), paint)
}

/// Processes: `N running processes`, N in the value colour, counted as
/// sb-go counts /proc's numeric entries (`motd/info.go:334-351`).
#[must_use]
pub fn processes(count: Option<usize>, paint: &Paint) -> String {
    match count {
        Some(count) => format!("{} running processes", paint.value(&count.to_string())),
        None => NOT_AVAILABLE.to_string(),
    }
}

/// The processes under `root` (a /proc), through procfs's iterator over
/// its numeric entries
/// (<https://docs.rs/procfs/0.18.0/procfs/process/fn.all_processes_with_root.html>).
/// Every entry counts, one that ended while it was read too, as in
/// sb-go's directory listing.
fn count_processes(root: &Path) -> Option<usize> {
    procfs::process::all_processes_with_root(root)
        .inspect_err(|err| tracing::warn!("processes not counted: {err}"))
        .ok()
        .map(Iterator::count)
}

/// Processes on this host.
#[must_use]
pub fn processes_here(paint: &Paint) -> String {
    processes(count_processes(Path::new("/proc")), paint)
}

/// CPU: sb-go's model and counts (`motd/info.go:625-773`): the model
/// without its ` N-Core…` tail, then `(C cores, T threads)` with SMT or
/// `(C cores)` without, the numbers in the value colour; with several
/// sockets, one such line each. Sockets are the distinct `physical id`s
/// (at least one); cores are `cpu cores` per socket when given, else the
/// distinct (socket, `core id`) pairs, else the threads; threads are the
/// processors. Without a model name it is `Not available`.
#[must_use]
pub fn cpu(info: Option<&CpuInfo>, paint: &Paint) -> String {
    let Some(info) = info else {
        return NOT_AVAILABLE.to_string();
    };
    let field = |cpu: usize, key: &str| info.get_field(cpu, key);
    let cpus = 0..info.num_cores();
    let threads = cpus
        .clone()
        .filter(|&cpu| field(cpu, "processor").is_some())
        .count();
    // The ids as text, as sb-go counts them (procfs's `physical_id`
    // would parse them).
    let sockets = cpus
        .clone()
        .filter_map(|cpu| field(cpu, "physical id"))
        .collect::<HashSet<_>>()
        .len()
        .max(1);
    // The last one given wins, as sb-go's line scan keeps the last.
    let model = cpus.clone().rev().find_map(|cpu| info.model_name(cpu));
    let cores_per_socket = cpus
        .clone()
        .rev()
        .find_map(|cpu| field(cpu, "cpu cores")?.parse::<i64>().ok())
        .and_then(|cores| usize::try_from(cores).ok())
        .filter(|&cores| cores > 0);
    let cores = cores_per_socket.map_or_else(
        || {
            let pairs = cpus
                .filter_map(|cpu| Some((field(cpu, "physical id")?, field(cpu, "core id")?)))
                .collect::<HashSet<_>>()
                .len();
            if pairs == 0 { threads } else { pairs }
        },
        |per_socket| per_socket * sockets,
    );
    let Some(model) = model.map(model_name).filter(|model| !model.is_empty()) else {
        return NOT_AVAILABLE.to_string();
    };
    // sb-go's one-socket and many-socket forms are the same with one
    // socket: the counts per socket, one line per socket.
    let (cores, threads) = (cores / sockets, threads / sockets);
    let counts = if threads > cores {
        format!(
            "({} cores, {} threads)",
            paint.value(&cores.to_string()),
            paint.value(&threads.to_string())
        )
    } else {
        format!("({} cores)", paint.value(&cores.to_string()))
    };
    vec![format!("{model} {counts}"); sockets].join("\n")
}

/// The model as sb-go shows it: everything from the first ` N-Core` on
/// removed (sb-go's `\s+\d+-Core.*`, with Go's classes written out:
/// `\s` is `[\t\n\f\r ]` and `\d` is `[0-9]` in its RE2 syntax,
/// <https://pkg.go.dev/regexp/syntax>), trimmed and made safe to draw.
fn model_name(raw: &str) -> String {
    static TAIL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[\t\n\f\r ]+[0-9]+-Core.*").expect("a valid pattern"));
    let model = TAIL.find(raw).map_or(raw, |found| &raw[..found.start()]);
    one_line(model.as_bytes())
}

/// CPU on this host.
#[must_use]
pub fn cpu_here(paint: &Paint) -> String {
    cpu(current::<CpuInfo>("cpuinfo").as_ref(), paint)
}

/// Memory Usage: sb-go's `U used, F free, C cached, A available, T total`
/// in GiB with one decimal and `G`, in the value colour
/// (`motd/info.go:849-913`). Cached counts the buffers; available is
/// `MemAvailable`, or free plus cached without it; used is total less
/// available.
#[must_use]
pub fn memory(info: Option<&Meminfo>, paint: &Paint) -> String {
    let Some(info) = info else {
        return NOT_AVAILABLE.to_string();
    };
    let cached = info.cached.saturating_add(info.buffers);
    let available = info
        .mem_available
        .unwrap_or_else(|| info.mem_free.saturating_add(cached));
    let used = info.mem_total.saturating_sub(available);
    let shown = |bytes: u64| paint.value(&gibibytes(bytes));
    format!(
        "{} used, {} free, {} cached, {} available, {} total",
        shown(used),
        shown(info.mem_free),
        shown(cached),
        shown(available),
        shown(info.mem_total)
    )
}

/// `bytes` in GiB as Go's `%.1fG` prints the exact quotient: rounded to
/// one decimal, a tie to the even digit
/// (<https://pkg.go.dev/strconv#FormatFloat>, which rounds correctly).
/// Integer arithmetic gives the same digits without a float cast.
fn gibibytes(bytes: u64) -> String {
    const GIB: u128 = 1 << 30;
    let scaled = u128::from(bytes) * 10;
    let (mut tenths, rest) = (scaled / GIB, scaled % GIB);
    if rest > GIB / 2 || (rest == GIB / 2 && tenths % 2 == 1) {
        tenths += 1;
    }
    format!("{}.{}G", tenths / 10, tenths % 10)
}

/// Memory Usage on this host.
#[must_use]
pub fn memory_here(paint: &Paint) -> String {
    memory(current::<Meminfo>("meminfo").as_ref(), paint)
}

#[cfg(test)]
mod tests {
    use super::*;

    use procfs::FromRead;

    /// A procfs type read from a fixture under `tests/fixtures`.
    fn fixture<T: FromRead>(name: &str) -> T {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        T::from_file(&path).unwrap_or_else(|err| panic!("{name}: {err}"))
    }

    fn painted() -> (Paint, sb_cli_core::theme::Theme) {
        let theme = sb_cli_core::theme::Theme::resolve(
            &sb_cli_core::theme::Palette::default(),
            sb_cli_core::theme::Profile::TrueColor,
        );
        (Paint::new(&theme, true), theme)
    }

    fn secs(text: &str) -> Duration {
        Uptime::from_read(format!("{text} 0.00\n").as_bytes())
            .unwrap()
            .uptime_duration()
    }

    /// Each uptime with procps 3.3.17's and 4.0.4's `uptime -p` text, as
    /// their own code prints it: copied from proc/whattime.c and
    /// library/uptime.c into a C program fed these values.
    const UPTIME_FORMS: [(&str, &str, &str); 27] = [
        ("0.50", "up 0 minutes", "up 0 minutes"),
        ("59.99", "up 0 minutes", "up 0 minutes"),
        ("60.00", "up 1 minute", "up "),
        ("60.50", "up 1 minute", "up 1 minute"),
        ("61.00", "up 1 minute", "up 1 minute"),
        ("119.99", "up 1 minute", "up 1 minute"),
        ("120.00", "up 2 minutes", "up 2 minutes"),
        ("3599.99", "up 59 minutes", "up 59 minutes"),
        ("3600.00", "up 1 hour", "up 60 minutes"),
        ("3600.50", "up 1 hour", "up 1 hour, 0 minutes"),
        ("3661.00", "up 1 hour, 1 minute", "up 1 hour, 1 minute"),
        ("7200.00", "up 2 hours", "up 2 hours, 0 minutes"),
        ("7230.00", "up 2 hours", "up 2 hours, 0 minutes"),
        ("86400.00", "up 1 day", "up 24 hours, 0 minutes"),
        ("86400.50", "up 1 day", "up 1 day, 0 minutes"),
        (
            "90061.00",
            "up 1 day, 1 hour, 1 minute",
            "up 1 day, 1 hour, 1 minute",
        ),
        ("604800.00", "up 1 week", "up 7 days, 0 minutes"),
        ("604800.50", "up 1 week", "up 1 week, 0 minutes"),
        (
            "1483506.00",
            "up 2 weeks, 3 days, 4 hours, 5 minutes",
            "up 2 weeks, 3 days, 4 hours, 5 minutes",
        ),
        ("31449700.00", "up 1 minute", "up 52 weeks, 1 minute"),
        (
            "31536000.00",
            "up 1 year, 1 day",
            "up 52 weeks, 24 hours, 0 minutes",
        ),
        ("31536000.50", "up 1 year, 1 day", "up 1 year, 0 minutes"),
        (
            "34560000.00",
            "up 1 year, 5 weeks, 1 day",
            "up 1 year, 5 weeks, 0 minutes",
        ),
        (
            "315360000.00",
            "up 1 decade, 1 week, 3 days",
            "up 10 years, 0 minutes",
        ),
        (
            "315360061.25",
            "up 1 decade, 1 week, 3 days, 1 minute",
            "up 1 decade, 1 minute",
        ),
        (
            "352000000.00",
            "up 1 decade, 1 year, 10 weeks, 1 hour, 46 minutes",
            "up 1 decade, 1 year, 8 weeks, 3 days, 1 hour, 46 minutes",
        ),
        (
            "1234567.89",
            "up 2 weeks, 6 hours, 56 minutes",
            "up 2 weeks, 6 hours, 56 minutes",
        ),
    ];

    #[test]
    fn uptime_takes_procps_form_on_each_release() {
        for (uptime, v3, v4) in UPTIME_FORMS {
            assert_eq!(
                uptime_pretty(secs(uptime), Procps::V3),
                v3,
                "3.3.17 {uptime}"
            );
            assert_eq!(
                uptime_pretty(secs(uptime), Procps::V4),
                v4,
                "4.0.4 {uptime}"
            );
        }
    }

    #[test]
    fn the_form_follows_the_release() {
        assert_eq!(Procps::for_release(Some("22.04")), Procps::V3);
        assert_eq!(Procps::for_release(Some("20.04")), Procps::V3);
        assert_eq!(Procps::for_release(Some("24.04")), Procps::V4);
        assert_eq!(Procps::for_release(Some("26.04")), Procps::V4);
        assert_eq!(Procps::for_release(Some("23.10")), Procps::V4);
        assert_eq!(Procps::for_release(Some("garbage")), Procps::V4);
        assert_eq!(Procps::for_release(None), Procps::V4);
    }

    /// sb-go drops `up ` and paints the rest (a bare `up` stays).
    #[test]
    fn uptime_is_procps_text_without_up_in_the_value_colour() {
        let plain = Paint::plain();
        assert_eq!(
            uptime(Some(secs("1483506.00")), Procps::V3, &plain),
            "2 weeks, 3 days, 4 hours, 5 minutes"
        );
        assert_eq!(uptime(Some(secs("60.00")), Procps::V4, &plain), "up");
        assert_eq!(uptime(None, Procps::V4, &plain), "Not available");
        let (paint, _) = painted();
        assert_eq!(
            uptime(Some(secs("120.00")), Procps::V4, &paint),
            paint.value("2 minutes")
        );
        assert_eq!(uptime(None, Procps::V4, &paint), "Not available");
    }

    #[test]
    fn load_averages_are_sb_gos_line_with_two_decimals() {
        let load = LoadAverage::from_read(&b"0.07 1.50 12.34 2/1234 5678\n"[..]).unwrap();
        assert_eq!(
            load_averages(Some(&load), &Paint::plain()),
            "1 min: 0.07 | 5 min: 1.50 | 15 min: 12.34"
        );
        let (paint, _) = painted();
        assert_eq!(
            load_averages(Some(&load), &paint),
            format!(
                "1 min: {} | 5 min: {} | 15 min: {}",
                paint.value("0.07"),
                paint.value("1.50"),
                paint.value("12.34")
            )
        );
        assert_eq!(load_averages(None, &paint), "Not available");
    }

    #[test]
    fn processes_count_procs_numeric_entries() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["1", "42", "31337", "self", "sys", "12abc"] {
            std::fs::create_dir(dir.path().join(name)).unwrap();
        }
        std::fs::write(dir.path().join("loadavg"), "").unwrap();
        assert_eq!(count_processes(dir.path()), Some(3));
        assert_eq!(count_processes(&dir.path().join("missing")), None);
        assert_eq!(processes(Some(3), &Paint::plain()), "3 running processes");
        let (paint, _) = painted();
        assert_eq!(
            processes(Some(3), &paint),
            format!("{} running processes", paint.value("3"))
        );
        assert_eq!(processes(None, &paint), "Not available");
    }

    /// The expected values are sb-go's: its `GetCpuInfo` and
    /// `GetMemoryInfo`, copied into a Go program reading these fixtures,
    /// styles removed.
    #[test]
    fn cpu_matches_sb_go_on_the_fixtures() {
        let plain = Paint::plain();
        let shown = |name| cpu(Some(&fixture::<CpuInfo>(name)), &plain);
        assert_eq!(
            shown("cpuinfo-smt"),
            "AMD Ryzen 5 3600 (6 cores, 12 threads)"
        );
        assert_eq!(
            shown("cpuinfo-two-sockets"),
            "Intel(R) Xeon(R) CPU E5-2680 v4 @ 2.40GHz (4 cores, 8 threads)\n\
             Intel(R) Xeon(R) CPU E5-2680 v4 @ 2.40GHz (4 cores, 8 threads)"
        );
        assert_eq!(
            shown("cpuinfo-vm"),
            "QEMU Virtual CPU version 2.5+ (4 cores)"
        );
        assert_eq!(
            shown("cpuinfo-core-ids"),
            "Synthetic CPU (2 cores, 4 threads)"
        );
        assert_eq!(shown("cpuinfo-no-model"), "Not available");
        assert_eq!(cpu(None, &plain), "Not available");
    }

    #[test]
    fn cpu_counts_are_in_the_value_colour() {
        let (paint, _) = painted();
        assert_eq!(
            cpu(Some(&fixture::<CpuInfo>("cpuinfo-smt")), &paint),
            format!(
                "AMD Ryzen 5 3600 ({} cores, {} threads)",
                paint.value("6"),
                paint.value("12")
            )
        );
        assert_eq!(
            cpu(Some(&fixture::<CpuInfo>("cpuinfo-vm")), &paint),
            format!("QEMU Virtual CPU version 2.5+ ({} cores)", paint.value("4"))
        );
    }

    /// One socket without SMT, a two-socket box without it, a `cpu cores`
    /// that isn't a positive number, and a model to make safe.
    #[test]
    fn cpu_topology_edges() {
        let plain = Paint::plain();
        let block = |n: usize, socket: &str, extra: &str| {
            format!(
                "processor\t: {n}\nmodel name\t: Chip \x1b[2J4-Core X\nphysical id\t: {socket}\n{extra}\n\n"
            )
        };
        let read = |text: String| CpuInfo::from_read(text.as_bytes()).unwrap();
        let two = read(format!(
            "{}{}",
            block(0, "0", "cpu cores\t: 2"),
            block(1, "0", "cpu cores\t: 2")
        ));
        assert_eq!(cpu(Some(&two), &plain), "Chip 4-Core X (2 cores)");
        let sockets = read(format!(
            "{}{}",
            block(0, "0", "cpu cores\t: 1"),
            block(1, "1", "cpu cores\t: 1")
        ));
        assert_eq!(
            cpu(Some(&sockets), &plain),
            "Chip 4-Core X (1 cores)\nChip 4-Core X (1 cores)"
        );
        let odd = read(format!(
            "{}{}",
            block(0, "0", "cpu cores\t: -2"),
            block(1, "0", "cpu cores\t: x")
        ));
        assert_eq!(cpu(Some(&odd), &plain), "Chip 4-Core X (2 cores)");
        let zero = read(format!(
            "{}{}",
            block(0, "0", "cpu cores\t: 0"),
            block(1, "0", "cpu cores\t: 0")
        ));
        assert_eq!(cpu(Some(&zero), &plain), "Chip 4-Core X (2 cores)");
        // A positive `cpu cores` wins over the core ids.
        let given = read(format!(
            "{}{}",
            block(0, "0", "cpu cores\t: 4\ncore id\t: 0"),
            block(1, "0", "cpu cores\t: 4\ncore id\t: 0")
        ));
        assert_eq!(cpu(Some(&given), &plain), "Chip 4-Core X (4 cores)");
    }

    #[test]
    fn the_model_loses_sb_gos_core_tail_only() {
        assert_eq!(
            model_name("AMD Ryzen 9 5950X 16-Core Processor"),
            "AMD Ryzen 9 5950X"
        );
        assert_eq!(
            model_name("AMD EPYC 7302P 16-Core  Processor"),
            "AMD EPYC 7302P"
        );
        assert_eq!(
            model_name("Intel(R) Core(TM) i7-8700 CPU @ 3.20GHz"),
            "Intel(R) Core(TM) i7-8700 CPU @ 3.20GHz"
        );
        // Go's \s and \d are ASCII: a full-width digit is no match.
        assert_eq!(model_name("Chip \u{FF16}-Core"), "Chip \u{FF16}-Core");
        assert_eq!(model_name("Chip\t12-Core"), "Chip");
        // Nor is a vertical tab Go's `\s`; it is then dropped as a control.
        assert_eq!(model_name("Chip\x0b12-Core"), "Chip12-Core");
        assert_eq!(model_name("X-Core 8-Core"), "X-Core");
    }

    #[test]
    fn memory_matches_sb_go_on_the_fixtures() {
        let plain = Paint::plain();
        assert_eq!(
            memory(Some(&fixture::<Meminfo>("meminfo")), &plain),
            "5.5G used, 1.2G free, 6.5G cached, 10.0G available, 15.5G total"
        );
        assert_eq!(
            memory(Some(&fixture::<Meminfo>("meminfo-no-available")), &plain),
            "7.9G used, 1.2G free, 6.5G cached, 7.7G available, 15.5G total"
        );
        assert_eq!(memory(None, &plain), "Not available");
    }

    #[test]
    fn memory_values_are_in_the_value_colour() {
        let (paint, _) = painted();
        let shown = memory(Some(&fixture::<Meminfo>("meminfo")), &paint);
        assert_eq!(
            shown,
            format!(
                "{} used, {} free, {} cached, {} available, {} total",
                paint.value("5.5G"),
                paint.value("1.2G"),
                paint.value("6.5G"),
                paint.value("10.0G"),
                paint.value("15.5G")
            )
        );
    }

    /// Go's `%.1f` of the exact quotient: ties go to the even digit.
    #[test]
    fn gibibytes_round_as_go_prints_them() {
        const KIB: u64 = 1024;
        assert_eq!(gibibytes(0), "0.0G");
        assert_eq!(gibibytes(262_144 * KIB), "0.2G"); // 0.25
        assert_eq!(gibibytes(786_432 * KIB), "0.8G"); // 0.75
        assert_eq!(gibibytes(262_145 * KIB), "0.3G"); // just over 0.25
        assert_eq!(gibibytes(1_572_864 * KIB), "1.5G");
        assert_eq!(gibibytes(1_625_292 * KIB), "1.5G"); // 1.5499992
        assert_eq!(gibibytes(1_625_293 * KIB), "1.6G"); // 1.5500002
        assert_eq!(gibibytes(16_303_296 * KIB), "15.5G");
        assert_eq!(gibibytes(1_048_576 * 1_048_576 * KIB), "1048576.0G");
    }

    /// A `MemAvailable` above `MemTotal` (never seen) can't wrap round.
    #[test]
    fn used_memory_never_wraps() {
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/meminfo"),
        )
        .unwrap()
        .replace("MemAvailable:   10485760", "MemAvailable:   99999999");
        let info = Meminfo::from_read(text.as_bytes()).unwrap();
        assert!(memory(Some(&info), &Paint::plain()).starts_with("0.0G used"));
    }

    /// A smoke test: the host's own /proc parses and each section shows
    /// (read-only); the values are the fixtures' tests.
    #[test]
    fn the_host_sections_read_its_proc() {
        let plain = Paint::plain();
        assert_eq!(
            load_averages_here(&plain).split(" | ").count(),
            3,
            "{}",
            load_averages_here(&plain)
        );
        assert!(processes_here(&plain).ends_with(" running processes"));
        let memory = memory_here(&plain);
        assert!(
            memory.contains("G used, ") && memory.ends_with("G total"),
            "{memory}"
        );
        assert_eq!(
            cpu_here(&plain),
            cpu(Some(&CpuInfo::current().unwrap()), &plain)
        );
        // procps' parts, each `N unit`, or the bare `up` of 4.0.4's
        // exact minute.
        let uptime = uptime_here(&plain);
        let units = [
            "decade", "decades", "year", "years", "week", "weeks", "day", "days", "hour", "hours",
            "minute", "minutes",
        ];
        assert!(
            uptime == "up"
                || uptime.split(", ").all(|part| part
                    .split_once(' ')
                    .is_some_and(|(n, unit)| n.parse::<u64>().is_ok() && units.contains(&unit))),
            "{uptime}"
        );
    }

    const NOBLE: &str = "PRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\n\
NAME=\"Ubuntu\"\n\
VERSION_ID=\"24.04\"\n\
VERSION=\"24.04.1 LTS (Noble Numbat)\"\n\
VERSION_CODENAME=noble\n\
ID=ubuntu\n\
ID_LIKE=debian\n\
UBUNTU_CODENAME=noble\n";

    #[test]
    fn distribution_is_the_pretty_name_and_codename() {
        assert_eq!(distribution(Some(NOBLE)), "Ubuntu 24.04.1 LTS (noble)");
    }

    #[test]
    fn without_a_codename_the_name_stands_alone() {
        assert_eq!(
            distribution(Some("PRETTY_NAME='Debian GNU/Linux'\nVERSION_CODENAME=\n")),
            "Debian GNU/Linux"
        );
    }

    #[test]
    fn without_a_name_or_a_file_it_is_not_available() {
        assert_eq!(distribution(None), "Not available");
        assert_eq!(
            distribution(Some("VERSION_CODENAME=noble\n")),
            "Not available"
        );
        assert_eq!(
            distribution(Some("PRETTY_NAME=\"\"\nVERSION_CODENAME=noble\n")),
            "Not available"
        );
    }

    #[test]
    fn comments_and_odd_lines_are_skipped_and_the_last_assignment_wins() {
        let text = "# PRETTY_NAME=commented\n\
                    garbage line\n\
                    \n\
                    PRETTY_NAME=first\n\
                      PRETTY_NAME=\"Second One\"  \n\
                    VERSION_CODENAME=jammy\n";
        assert_eq!(distribution(Some(text)), "Second One (jammy)");
    }

    #[test]
    fn double_quotes_take_freedesktops_escapes() {
        assert_eq!(
            unquote(r#""a \"b\" \$c \\d \`e\` \n""#),
            r#"a "b" $c \d `e` \n"#
        );
        assert_eq!(unquote(r#""trailing\""#), r"trailing\");
        assert_eq!(unquote(r"'single \$ kept'"), r"single \$ kept");
        assert_eq!(unquote("bare"), "bare");
        assert_eq!(unquote("\"unbalanced"), "\"unbalanced");
        assert_eq!(unquote("'unbalanced"), "'unbalanced");
    }

    /// The file's text is drawn safely: escapes go, one line only.
    #[test]
    fn os_release_text_is_made_safe_to_draw() {
        let text = "PRETTY_NAME=\"Evil\x1b]0;title\x07 OS\"\nVERSION_CODENAME=\"x\x1b[2Jy\"\n";
        assert_eq!(distribution(Some(text)), "Evil OS (xy)");
    }

    /// A value that is only escapes or spaces counts as absent.
    #[test]
    fn a_name_or_codename_with_nothing_drawable_is_absent() {
        assert_eq!(
            distribution(Some("PRETTY_NAME=\"\x1b[2J \"\nVERSION_CODENAME=noble\n")),
            "Not available"
        );
        assert_eq!(
            distribution(Some("PRETTY_NAME=Ubuntu\nVERSION_CODENAME=\" \x1b[0m\"\n")),
            "Ubuntu"
        );
    }

    #[test]
    fn kernel_is_the_release_made_safe() {
        assert_eq!(kernel(b"6.8.0-1-generic"), "6.8.0-1-generic");
        assert_eq!(kernel(b"6.8\x1b[31m.0"), "6.8.0");
        assert_eq!(kernel(b""), "Not available");
    }

    /// The host's own kernel is uname -r's (read-only, no child).
    #[test]
    fn kernel_here_is_unames_release() {
        let release = rustix::system::uname();
        assert_eq!(
            kernel_here(),
            String::from_utf8_lossy(release.release().to_bytes())
        );
    }

    /// The host's Distribution is its own os-release's (read-only).
    #[test]
    fn distribution_here_reads_the_hosts_os_release() {
        let text = ["/etc/os-release", "/usr/lib/os-release"]
            .iter()
            .find_map(|place| std::fs::read_to_string(place).ok());
        assert_eq!(distribution_here(), distribution(text.as_deref()));
    }

    #[test]
    fn the_first_os_release_that_exists_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let etc = dir.path().join("etc-os-release");
        let lib = dir.path().join("lib-os-release");
        std::fs::write(&lib, "PRETTY_NAME=lib\n").unwrap();
        assert_eq!(
            read_os_release(&[&etc, &lib]).as_deref(),
            Some("PRETTY_NAME=lib\n")
        );
        std::fs::write(&etc, "PRETTY_NAME=etc\n").unwrap();
        assert_eq!(
            read_os_release(&[&etc, &lib]).as_deref(),
            Some("PRETTY_NAME=etc\n")
        );
    }

    #[test]
    fn a_missing_or_unreadable_os_release_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        assert_eq!(read_os_release(&[&missing]), None);
        // A directory at the path can't be read: the search ends there.
        let lib = dir.path().join("lib");
        std::fs::write(&lib, "PRETTY_NAME=lib\n").unwrap();
        assert_eq!(read_os_release(&[dir.path(), &lib]), None);
    }
}
