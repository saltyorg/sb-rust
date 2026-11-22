//! Package Status and Reboot Status (motd spec, Sections), as sb-go
//! reads them (`motd/info.go:372-622`):
//! - Package Status: update-notifier's `updates-available` file, then its
//!   `apt-check --human-readable --no-esm-messages`, then `apt list
//!   --upgradable`;
//! - Reboot Status: update-notifier's `/var/run/reboot-required` and
//!   `.pkgs` (sb-go then ran its `update-motd-reboot-required` script,
//!   which prints that same file when it is there, so without the file
//!   it printed nothing: Difference 37).
//!
//! Every child runs through sb-exec's `Cmd` under sb-go's 5 s
//! (`motd/utils.go`, `ExecCommand`), and its text through term-ui's
//! `DisplayText`. The texts are update-notifier's own, so they are
//! matched as sb-go matches them, not parsed: no crate reads them.

use serde_json::json;

use crate::Out;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sb_exec::{Cmd, Ending};
use tokio_util::sync::CancellationToken;

use crate::Paint;
use crate::text::{block, one_line};

/// sb-go's limit for each child (`motd/utils.go`, `ExecCommand`; motd
/// decision 11).
pub const CHILD_TIMEOUT: Duration = Duration::from_secs(5);

/// update-notifier's files and scripts; the tests point them at a
/// temporary directory (motd spec, Testing, "Children").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    /// The summary apt's daily update leaves for the MOTD.
    pub updates_available: PathBuf,
    /// update-notifier's counter, asked when the summary says nothing.
    pub apt_check: PathBuf,
    /// apt itself, the last source (found on `PATH`).
    pub apt: PathBuf,
    /// Its flag that a reboot is needed.
    pub reboot_required: PathBuf,
    /// The packages that asked for it, one per line.
    pub reboot_packages: PathBuf,
}

impl Paths {
    /// Where Ubuntu's update-notifier-common puts them.
    #[must_use]
    pub fn system() -> Self {
        Self {
            updates_available: PathBuf::from("/var/lib/update-notifier/updates-available"),
            apt_check: PathBuf::from("/usr/lib/update-notifier/apt-check"),
            apt: PathBuf::from("apt"),
            reboot_required: PathBuf::from("/var/run/reboot-required"),
            reboot_packages: PathBuf::from("/var/run/reboot-required.pkgs"),
        }
    }
}

/// Reboot Status, in the warning colour when a reboot is needed: with
/// the flag file there, `Reboot required (package: P)` for one package in
/// `.pkgs`, `Reboot required (N packages)` for several, and `Reboot
/// required` when the list is empty or can't be read; without it nothing,
/// so the line is hidden. sb-go then ran update-notifier's
/// `update-motd-reboot-required`, whose seven lines on 22.04, 24.04 and
/// 26.04 print the flag file only when it exists (Difference 37): a
/// child that could only print nothing.
#[must_use]
pub fn reboot(paths: &Paths, paint: &Paint) -> Out {
    if paths.reboot_required.exists() {
        let (line, packages) = required(&paths.reboot_packages);
        let value = json!({ "required": true, "packages": packages, "text": line });
        return Out::value(paint.warning(&line), value);
    }
    Out::value(String::new(), json!({}))
}

/// The flag file's text and the packages from `.pkgs`, read with
/// cli-core's 1 MiB cap, each made safe to draw.
fn required(packages: &Path) -> (String, Vec<String>) {
    let text = match sb_cli_core::config::read_small(packages) {
        Ok(text) => text.unwrap_or_default(),
        Err(err) => {
            tracing::warn!(file = %packages.display(), "reboot packages not read: {err}");
            String::new()
        }
    };
    let names: Vec<String> = text
        .split('\n')
        .filter(|name| !name.is_empty())
        .map(|name| one_line(name.as_bytes()))
        .collect();
    let line = match names.as_slice() {
        [] => "Reboot required".to_string(),
        [one] => format!("Reboot required (package: {one})"),
        many => format!("Reboot required ({} packages)", many.len()),
    };
    (line, names)
}

/// The child's stdout, trimmed and made safe to draw, or empty when it
/// can't start, fails or runs past [`CHILD_TIMEOUT`] (sb-go's `Not
/// available`, which every caller treats as no answer). It runs through
/// sb-exec, in its own process group, ended at the timeout; `what` names
/// it in the run log.
pub(crate) async fn child_output(cmd: Cmd, what: &str) -> String {
    let ran = cmd
        .timeout(CHILD_TIMEOUT)
        .capture(&CancellationToken::new())
        .await;
    match ran {
        Ok(captured) if captured.outcome.ending == Ending::Exited(0) => {
            block(captured.stdout.as_bytes())
        }
        Ok(captured) => {
            tracing::warn!(child = what, ending = %captured.outcome.ending, "motd child failed");
            String::new()
        }
        Err(err) => {
            tracing::warn!(child = what, "motd child not run: {err}");
            String::new()
        }
    }
}

/// sb-go's last answer when no source says anything.
const UNKNOWN: &str = "Update status unknown";

/// Package Status, and its `--json` value `{updates, text}` (motd spec,
/// `--json`): the line's first number, 0 for `System is up to date`, `null`
/// when the status is unknown. Read from the line, which is plain in a
/// `--json` run.
pub async fn packages_out(paths: &Paths, paint: &Paint) -> Out {
    let line = packages(paths, paint).await;
    let updates = if line == UNKNOWN {
        None
    } else if line == "System is up to date" {
        Some(0)
    } else {
        first_number(&line)
    };
    let value = json!({ "updates": updates, "text": line });
    Out::value(line, value)
}

/// The first run of ASCII digits in `line`, as a number.
fn first_number(line: &str) -> Option<u64> {
    let start = line.find(|c: char| c.is_ascii_digit())?;
    let digits: String = line[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// Package Status: the first source that answers, as sb-go asks them.
pub async fn packages(paths: &Paths, paint: &Paint) -> String {
    if let Some(line) = from_summary(&paths.updates_available, paint) {
        return line;
    }
    let checked = child_output(
        Cmd::new(paths.apt_check.as_os_str())
            .arg("--human-readable")
            .arg("--no-esm-messages"),
        "apt-check",
    )
    .await;
    if let Some(line) = from_apt_check(&checked, paint) {
        return line;
    }
    let listed = child_output(
        Cmd::new(paths.apt.as_os_str())
            .arg("list")
            .arg("--upgradable"),
        "apt list",
    )
    .await;
    from_apt_list(&listed, paint).unwrap_or_else(|| UNKNOWN.to_string())
}

/// Whether a line speaks of ESM, which sb-go never shows.
fn esm(line: &str) -> bool {
    line.contains("ESM") || line.contains("esm")
}

/// `line` with its numbers in the value colour, as sb-go's
/// `re.ReplaceAllString(line, ValueStyle.Render(first))` writes it: every
/// run of digits becomes the line's first number. update-notifier's
/// lines carry one number (`apt_check.py`'s `%i updates can be applied
/// immediately.`), where that is the same as colouring each. Go's `\d`
/// is ASCII only, hence `[0-9]`.
///
/// The runs found by hand, as regex's `[0-9]+` finds them (a test compares
/// them): why not regex, its compile and the CPU line's pattern's were
/// 1.2 M instructions at each login (`system::core_tail`).
fn coloured(line: &str, paint: &Paint) -> String {
    let mut runs = digit_runs(line).peekable();
    let Some(first) = runs.peek() else {
        return line.to_string();
    };
    let number = paint.value(&line[first.clone()]);
    let mut out = String::with_capacity(line.len() + number.len());
    let mut last = 0;
    for run in runs {
        out.push_str(&line[last..run.start]);
        out.push_str(&number);
        last = run.end;
    }
    out.push_str(&line[last..]);
    out
}

/// Each run of ASCII digits in `line`, from the left.
fn digit_runs(line: &str) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
    let bytes = line.as_bytes();
    let mut at = 0;
    std::iter::from_fn(move || {
        let start = at + bytes[at..].iter().position(u8::is_ascii_digit)?;
        let end = bytes[start..]
            .iter()
            .position(|b| !b.is_ascii_digit())
            .map_or(bytes.len(), |len| start + len);
        at = end;
        Some(start..end)
    })
}

/// The `updates-available` file's answer: its first line that says
/// updates `can be applied immediately` and isn't about ESM, cut after
/// its first full stop, or `System is up to date` when a line says so;
/// none when the file is missing, empty, unreadable or says neither.
fn from_summary(path: &Path, paint: &Paint) -> Option<String> {
    let text = match sb_cli_core::config::read_small(path) {
        Ok(text) => text?,
        Err(err) => {
            tracing::warn!(file = %path.display(), "update summary not read: {err}");
            return None;
        }
    };
    if text.is_empty() {
        return None;
    }
    let lines: Vec<String> = text
        .split('\n')
        .map(|line| one_line(line.as_bytes()))
        .collect();
    if let Some(line) = lines
        .iter()
        .find(|line| line.contains("can be applied immediately") && !esm(line))
    {
        let cut = line.find('.').map_or(line.as_str(), |stop| &line[..=stop]);
        return Some(coloured(cut, paint));
    }
    lines
        .iter()
        .any(|line| line.contains("up to date") || line.contains("Up to date"))
        .then(|| "System is up to date".to_string())
}

/// apt-check's answer: its `packages can be updated` line, or else its
/// first line about an update or a package; none for ESM lines or no
/// output.
fn from_apt_check(output: &str, paint: &Paint) -> Option<String> {
    let lines: Vec<&str> = output.lines().map(str::trim).collect();
    lines
        .iter()
        .find(|line| line.contains("packages can be updated") && !esm(line))
        .or_else(|| {
            lines.iter().find(|line| {
                !line.is_empty()
                    && !esm(line)
                    && (line.contains("update") || line.contains("package"))
            })
        })
        .map(|line| coloured(line, paint))
}

/// apt's answer: the lines naming a package (`name/suite ...`, not
/// `Listing...`) counted, when there are any.
fn from_apt_list(output: &str, paint: &Paint) -> Option<String> {
    let lines: Vec<&str> = output.split('\n').collect();
    if lines.len() < 2 {
        return None;
    }
    let count = lines
        .iter()
        .filter(|line| line.contains('/') && !line.contains("Listing"))
        .count();
    (count > 0).then(|| {
        format!(
            "{} updates can be applied immediately",
            paint.value(&count.to_string())
        )
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Tests that write a stand-in script or start a child hold this from
    /// writing it until the run is over, so no other test's fork holds
    /// the script's write descriptor when it runs (exec fails with
    /// ETXTBSY otherwise), as sb-host's tests do.
    pub(crate) static SPAWNS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Paths in `dir`, none of them there yet.
    fn paths(dir: &Path) -> Paths {
        Paths {
            updates_available: dir.join("updates-available"),
            apt_check: dir.join("apt-check"),
            apt: dir.join("apt"),
            reboot_required: dir.join("reboot-required"),
            reboot_packages: dir.join("reboot-required.pkgs"),
        }
    }

    /// A stand-in program at `path` running `body` under sh, written by a
    /// child (ETXTBSY otherwise: `sb_cli_core::testing::write_program`).
    pub(crate) fn stand_in(path: &Path, body: &str) {
        sb_cli_core::testing::write_program(path, format!("#!/bin/sh\n{body}\n"));
    }

    fn painted() -> Paint {
        let theme = sb_cli_core::theme::Theme::resolve(
            &sb_cli_core::theme::Palette::default(),
            sb_cli_core::theme::Profile::TrueColor,
        );
        Paint::new(&theme, true)
    }

    #[test]
    fn the_system_paths_are_update_notifiers() {
        let system = Paths::system();
        assert_eq!(
            system.reboot_required,
            Path::new("/var/run/reboot-required")
        );
        assert_eq!(
            system.reboot_packages,
            Path::new("/var/run/reboot-required.pkgs")
        );
    }

    #[tokio::test]
    async fn the_flag_with_one_package_names_it() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        std::fs::write(&paths.reboot_required, "*** System restart required ***\n").unwrap();
        std::fs::write(&paths.reboot_packages, "linux-base\n").unwrap();
        assert_eq!(
            reboot(&paths, &Paint::plain()).text,
            "Reboot required (package: linux-base)"
        );
    }

    #[tokio::test]
    async fn the_flag_with_several_packages_counts_them() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        std::fs::write(&paths.reboot_required, "").unwrap();
        std::fs::write(&paths.reboot_packages, "linux-base\n\nlibc6\ndbus\n").unwrap();
        assert_eq!(
            reboot(&paths, &Paint::plain()).text,
            "Reboot required (3 packages)"
        );
    }

    /// No list, an empty one or an unreadable one: the plain text.
    #[tokio::test]
    async fn the_flag_without_a_package_list_says_reboot_required() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        std::fs::write(&paths.reboot_required, "").unwrap();
        assert_eq!(reboot(&paths, &Paint::plain()).text, "Reboot required");
        std::fs::write(&paths.reboot_packages, "\n\n").unwrap();
        assert_eq!(reboot(&paths, &Paint::plain()).text, "Reboot required");
        std::fs::remove_file(&paths.reboot_packages).unwrap();
        std::fs::create_dir(&paths.reboot_packages).unwrap();
        assert_eq!(reboot(&paths, &Paint::plain()).text, "Reboot required");
    }

    #[tokio::test]
    async fn a_package_name_is_made_safe_to_draw() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        std::fs::write(&paths.reboot_required, "").unwrap();
        std::fs::write(&paths.reboot_packages, "evil\x1b]0;title\x07pkg\n").unwrap();
        assert_eq!(
            reboot(&paths, &Paint::plain()).text,
            "Reboot required (package: evilpkg)"
        );
    }

    #[tokio::test]
    async fn a_needed_reboot_is_in_the_warning_colour() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        std::fs::write(&paths.reboot_required, "").unwrap();
        let paint = painted();
        assert_eq!(
            reboot(&paths, &paint).text,
            paint.warning("Reboot required")
        );
        assert_ne!(paint.warning("Reboot required"), "Reboot required");
    }

    /// No flag: the line is hidden and its value empty, with no child
    /// (update-notifier's script would print only the absent file).
    #[tokio::test]
    async fn no_flag_hides_the_line() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let out = reboot(&paths, &Paint::plain());
        assert_eq!(out.text, "");
        assert_eq!(out.json, crate::Json::Empty);
        // A `.pkgs` list alone needs no reboot.
        std::fs::write(&paths.reboot_packages, "libc6\n").unwrap();
        assert_eq!(reboot(&paths, &Paint::plain()).text, "");
    }

    #[test]
    fn the_system_package_sources_are_update_notifiers_and_apt() {
        let system = Paths::system();
        assert_eq!(
            system.updates_available,
            Path::new("/var/lib/update-notifier/updates-available")
        );
        assert_eq!(
            system.apt_check,
            Path::new("/usr/lib/update-notifier/apt-check")
        );
        assert_eq!(system.apt, Path::new("apt"));
    }

    /// update-notifier's summary as Ubuntu writes it: the count, cut
    /// after its full stop, the ESM lines skipped. No child runs.
    #[tokio::test]
    async fn the_summary_file_answers_first() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        stand_in(&paths.apt_check, "echo '9 packages can be updated.'");
        std::fs::write(
            &paths.updates_available,
            "\nExpanded Security Maintenance for Applications is not enabled.\n\n\
             2 additional security updates can be applied with ESM Apps. They can be applied immediately.\n\
             12 updates can be applied immediately. To see these additional updates run: apt list --upgradable\n",
        )
        .unwrap();
        assert_eq!(
            packages(&paths, &Paint::plain()).await,
            "12 updates can be applied immediately."
        );
        let paint = painted();
        assert_eq!(
            packages(&paths, &paint).await,
            format!("{} updates can be applied immediately.", paint.value("12"))
        );
    }

    #[tokio::test]
    async fn a_summary_without_a_full_stop_is_shown_whole() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        std::fs::write(
            &paths.updates_available,
            "  1 update can be applied immediately\x1b[2J  \n",
        )
        .unwrap();
        assert_eq!(
            packages(&paths, &Paint::plain()).await,
            "1 update can be applied immediately"
        );
        std::fs::write(
            &paths.updates_available,
            "Updates can be applied immediately. Run apt.\n",
        )
        .unwrap();
        assert_eq!(
            packages(&paths, &painted()).await,
            "Updates can be applied immediately."
        );
    }

    #[tokio::test]
    async fn a_summary_saying_up_to_date_says_so() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        stand_in(&paths.apt_check, "echo '9 packages can be updated.'");
        for text in ["Your system is up to date\n", "Up to date.\n"] {
            std::fs::write(&paths.updates_available, text).unwrap();
            assert_eq!(
                packages(&paths, &Paint::plain()).await,
                "System is up to date"
            );
        }
    }

    /// An empty summary, one saying nothing sb-go reads, or one that
    /// isn't text: apt-check is asked, its older line first.
    #[tokio::test]
    async fn apt_check_answers_when_the_summary_says_nothing() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        stand_in(
            &paths.apt_check,
            "echo '1 additional security update can be applied with ESM Apps.'\n\
             echo '  3 updates can be applied immediately.'\n\
             echo '7 packages can be updated.'",
        );
        stand_in(&paths.apt, "exit 1");
        assert_eq!(
            packages(&paths, &Paint::plain()).await,
            "7 packages can be updated."
        );
        for summary in [&b""[..], b"\n", b"nothing here\n", b"\xff\xfe"] {
            std::fs::write(&paths.updates_available, summary).unwrap();
            assert_eq!(
                packages(&paths, &Paint::plain()).await,
                "7 packages can be updated."
            );
        }
        stand_in(
            &paths.apt_check,
            "echo; echo 'Learn more about ESM'; echo '  3 updates can be applied immediately.  '",
        );
        let paint = painted();
        assert_eq!(
            packages(&paths, &paint).await,
            format!("{} updates can be applied immediately.", paint.value("3"))
        );
        stand_in(&paths.apt_check, "echo 'No package: all good'");
        assert_eq!(packages(&paths, &paint).await, "No package: all good");
    }

    /// sb-go's colouring writes the line's first number for each number.
    #[test]
    fn every_number_takes_the_first_numbers_colour() {
        let paint = painted();
        assert_eq!(
            coloured("4 of 10 updates", &paint),
            format!("{0} of {0} updates", paint.value("4"))
        );
        assert_eq!(
            coloured("4 of 10 updates", &Paint::plain()),
            "4 of 4 updates"
        );
        assert_eq!(coloured("no number", &paint), "no number");
    }

    /// The runs by hand are regex's `[0-9]+` on every short text of the
    /// characters that matter: each replaced with the first, as regex's
    /// `replace_all` replaces them.
    #[test]
    fn the_digit_runs_are_the_patterns() {
        let pattern = regex::Regex::new("[0-9]+").expect("compiles");
        let alphabet = ["1", "23", "a", " ", "\u{FF16}", "é"];
        // Every text of up to five parts.
        let mut texts = vec![String::new()];
        let mut last = texts.clone();
        for _ in 0..5 {
            last = last
                .iter()
                .flat_map(|text| alphabet.iter().map(move |part| format!("{text}{part}")))
                .collect();
            texts.extend(last.iter().cloned());
        }
        let paint = painted();
        for text in &texts {
            let expected = match pattern.find(text) {
                Some(first) => pattern
                    .replace_all(text, regex::NoExpand(&paint.value(first.as_str())))
                    .into_owned(),
                None => text.clone(),
            };
            assert_eq!(coloured(text, &paint), expected, "{text:?}");
        }
    }

    /// apt-check missing, failing, silent or only about ESM: apt counts.
    #[tokio::test]
    async fn apt_list_counts_when_apt_check_has_no_answer() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        stand_in(
            &paths.apt,
            "[ \"$1 $2\" = 'list --upgradable' ] || exit 3\n\
             echo 'Listing... Done'\n\
             echo 'curl/jammy-updates 7.81.0-1ubuntu1.20 amd64 [upgradable from: 7.81.0-1ubuntu1.18]'\n\
             echo 'not a package line'\n\
             echo 'tzdata/jammy-updates 2024a-0ubuntu0.22.04 all [upgradable from: 2023c]'",
        );
        let paint = painted();
        let two = format!("{} updates can be applied immediately", paint.value("2"));
        assert_eq!(packages(&paths, &paint).await, two);
        for body in [
            "echo '3 packages can be updated.'; exit 1",
            "true",
            "echo 'ESM updates: 4 packages'",
            "echo 'nothing to say'",
        ] {
            stand_in(&paths.apt_check, body);
            assert_eq!(packages(&paths, &paint).await, two, "{body}");
        }
    }

    /// Nothing answers: sb-go's `Update status unknown`.
    #[tokio::test]
    async fn no_source_says_update_status_unknown() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        assert_eq!(packages(&paths, &Paint::plain()).await, UNKNOWN);
        assert_eq!(UNKNOWN, "Update status unknown");
        for body in [
            "echo 'Listing... Done'",
            "echo 'Listing...'; echo",
            "echo 'Listing...'; echo 'WARNING: nothing to list'",
            "exit 1",
        ] {
            stand_in(&paths.apt, body);
            assert_eq!(packages(&paths, &Paint::plain()).await, UNKNOWN, "{body}");
        }
        // One package line alone is not counted: sb-go wants two lines.
        stand_in(&paths.apt, "echo 'curl/jammy 1 amd64'");
        assert_eq!(packages(&paths, &Paint::plain()).await, UNKNOWN);
        stand_in(&paths.apt, "echo 'Listing...'; echo 'curl/jammy 1 amd64'");
        assert_eq!(
            packages(&paths, &Paint::plain()).await,
            "1 updates can be applied immediately"
        );
    }

    /// apt-check that hangs is ended at 5 s and apt is asked.
    #[tokio::test]
    async fn a_hanging_apt_check_is_ended_at_the_limit() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        stand_in(
            &paths.apt_check,
            "echo '3 packages can be updated.'; exec sleep 60",
        );
        stand_in(&paths.apt, "echo 'Listing...'; echo 'a/b 1 all'");
        let start = std::time::Instant::now();
        assert_eq!(
            packages(&paths, &Paint::plain()).await,
            "1 updates can be applied immediately"
        );
        let took = start.elapsed();
        assert!(took >= CHILD_TIMEOUT, "{took:?}");
        assert!(took < Duration::from_secs(20), "{took:?}");
    }

    /// Reboot Status's and Package Status's `--json` values (motd spec,
    /// `--json`): the flag file's packages; the line's count.
    #[tokio::test]
    async fn the_values_carry_the_packages_and_the_count() {
        use crate::Json;
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        std::fs::write(&paths.reboot_required, "").unwrap();
        std::fs::write(&paths.reboot_packages, "linux-image\nlibc6\n").unwrap();
        assert_eq!(
            reboot(&paths, &Paint::plain()).json,
            Json::Value(json!({
                "required": true,
                "packages": ["linux-image", "libc6"],
                "text": "Reboot required (2 packages)"
            }))
        );
        assert_eq!(
            first_number("12 updates can be applied immediately."),
            Some(12)
        );
        assert_eq!(first_number("no number"), None);
    }

    /// Package Status's `--json` value for each answer: the line's count,
    /// 0 for `System is up to date`, `null` when the status is unknown.
    #[tokio::test]
    async fn the_package_value_counts_each_answer() {
        use crate::Json;
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let value = |updates: serde_json::Value, text: &str| {
            Json::Value(json!({ "updates": updates, "text": text }))
        };
        assert_eq!(
            packages_out(&paths, &Paint::plain()).await.json,
            value(serde_json::Value::Null, UNKNOWN)
        );
        std::fs::write(&paths.updates_available, "Your system is up to date\n").unwrap();
        assert_eq!(
            packages_out(&paths, &Paint::plain()).await.json,
            value(json!(0), "System is up to date")
        );
        std::fs::write(
            &paths.updates_available,
            "3 updates can be applied immediately.\n",
        )
        .unwrap();
        assert_eq!(
            packages_out(&paths, &Paint::plain()).await.json,
            value(json!(3), "3 updates can be applied immediately.")
        );
    }
}
