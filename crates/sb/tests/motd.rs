//! `sb motd` through the built binary (motd spec, Testing strategy, "End
//! to end"; module build M36, M37): the layout of the system sections,
//! colour under Saltbox's `CLICOLOR_FORCE=1` with stdout piped, the
//! selection error and `-v`'s parse error. Root runs (`SB_TEST_ROOT=1`),
//! each with its own log directory; a run as another user would relaunch
//! through sudo. The system sections read the host's os-release, /proc
//! and `uname(2)`, read-only; nothing here contacts a service.

mod common;
mod root_gate;

use std::path::Path;
use std::process::Output;

/// `sb motd <args>` as root, stdout piped, logging to `logs`, with `env`.
fn motd(args: &[&str], logs: &Path, env: &[(&str, &str)]) -> Output {
    let mut cmd = common::sb();
    cmd.arg("motd")
        .args(args)
        .env("SB_LOG_DIR", logs)
        .env_remove("NO_COLOR")
        .env_remove("SB_COLOR_PROFILE")
        .env_remove("CLICOLOR")
        .env_remove("CLICOLOR_FORCE");
    for (name, value) in env {
        cmd.env(name, value);
    }
    cmd.output().expect("run sb")
}

fn temp() -> tempfile::TempDir {
    tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("temp dir")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn distro_and_kernel_show_in_sb_gos_layout() {
    if !root_gate::requested() {
        return;
    }
    let logs = temp();
    let out = motd(&["--kernel", "--distro"], logs.path(), &[]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    let lines: Vec<&str> = stdout.split('\n').collect();
    assert_eq!(
        lines.len(),
        4,
        "two lines, the blank line, the end: {stdout:?}"
    );
    assert!(lines[0].starts_with("Distribution:  "), "{stdout:?}");
    assert!(lines[0].len() > "Distribution:  ".len(), "{stdout:?}");
    let release = rustix::system::uname();
    let kernel = String::from_utf8_lossy(release.release().to_bytes()).into_owned();
    assert_eq!(lines[1], format!("Kernel:        {kernel}"));
    assert_eq!(&lines[2..], ["", ""]);
    assert!(!out.stdout.contains(&0x1b), "piped output is plain");
    assert_eq!(text(&out.stderr), "");
}

/// Saltbox's 01-motd exports `CLICOLOR_FORCE=1`, and `pam_motd` pipes
/// stdout: the keys are coloured (criterion 8). `--color=never` wins.
#[test]
fn clicolor_force_colours_piped_output_and_never_wins() {
    if !root_gate::requested() {
        return;
    }
    let logs = temp();
    let forced = motd(&["--kernel"], logs.path(), &[("CLICOLOR_FORCE", "1")]);
    assert_eq!(forced.status.code(), Some(0));
    assert!(
        text(&forced.stdout).starts_with("\x1b["),
        "{:?}",
        text(&forced.stdout)
    );
    let never = motd(
        &["--kernel", "--color=never"],
        logs.path(),
        &[("CLICOLOR_FORCE", "1")],
    );
    assert_eq!(never.status.code(), Some(0));
    assert!(!never.stdout.contains(&0x1b), "{:?}", text(&never.stdout));
}

/// Five of the /proc sections, in sb-go's order and padding, plain when
/// piped; with `CLICOLOR_FORCE=1` their values are coloured too
/// (criterion 8).
#[test]
fn system_sections_show_in_order_plain_unless_forced() {
    if !root_gate::requested() {
        return;
    }
    let logs = temp();
    let flags = ["--memory", "--cpu-info", "--processes", "--cpu", "--uptime"];
    let out = motd(&flags, logs.path(), &[]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(
        !out.stdout.contains(&0x1b),
        "piped output is plain: {stdout:?}"
    );
    let keys: Vec<&str> = stdout
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with(' '))
        .map(|line| line.get(..16).unwrap_or(line))
        .collect();
    assert_eq!(
        keys,
        [
            "Uptime:         ",
            "Load Averages:  ",
            "Processes:      ",
            "CPU:            ",
            "Memory Usage:   ",
        ],
        "{stdout:?}"
    );
    assert!(stdout.contains(" running processes\n"), "{stdout:?}");
    assert!(stdout.contains("1 min: "), "{stdout:?}");
    assert!(stdout.ends_with("\n\n"), "{stdout:?}");

    let forced = motd(&flags, logs.path(), &[("CLICOLOR_FORCE", "1")]);
    assert_eq!(forced.status.code(), Some(0));
    let forced = text(&forced.stdout);
    let processes = forced
        .lines()
        .find(|line| line.contains("running processes"))
        .expect("a Processes line");
    // The key and the count are each painted.
    assert!(processes.matches("\x1b[").count() >= 4, "{processes:?}");
}

/// Disk Usage: a line per file system and its bar, or sb-go's text for
/// none; plain when piped.
#[test]
fn disk_usage_shows_in_sb_gos_layout() {
    if !root_gate::requested() {
        return;
    }
    let logs = temp();
    let out = motd(&["--disk"], logs.path(), &[]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(stdout.starts_with("Disk Usage:  "), "{stdout:?}");
    assert!(!out.stdout.contains(&0x1b), "{stdout:?}");
    assert!(
        stdout.contains("No valid disk partitions found")
            || (stdout.contains("% used out of ")
                && (stdout.contains('█') || stdout.contains('░'))),
        "{stdout:?}"
    );
    assert!(stdout.ends_with("\n\n"), "{stdout:?}");
}

/// With no section chosen, sb-go's text and exit 1; nothing on stdout.
#[test]
fn nothing_selected_is_sb_gos_error() {
    if !root_gate::requested() {
        return;
    }
    let logs = temp();
    let out = motd(&["--title", "x"], logs.path(), &[]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "");
    assert!(
        text(&out.stderr)
            .contains("no information selected to display (use --all or specific flags)"),
        "{}",
        text(&out.stderr)
    );
}

/// `--share` is refused until its task lands (M47): never a run that
/// ignores it. Exit 1, nothing on stdout.
#[test]
fn unbuilt_share_is_refused() {
    if !root_gate::requested() {
        return;
    }
    let logs = temp();
    let (args, flag) = (&["--all", "--share"][..], "--share");
    let out = motd(args, logs.path(), &[]);
    assert_eq!(out.status.code(), Some(1), "{args:?}");
    assert_eq!(text(&out.stdout), "", "{args:?}");
    assert!(
        text(&out.stderr).contains(&format!("sb motd {flag} is not available")),
        "{}",
        text(&out.stderr)
    );
}

/// `-v` is clap's parse error, exit 2 (motd decision 9), as is an
/// argument (`cobra.NoArgs`).
#[test]
fn verbose_and_arguments_are_parse_errors() {
    if !root_gate::requested() {
        return;
    }
    let logs = temp();
    for args in [&["--all", "-v"][..], &["--all", "extra"]] {
        let out = motd(args, logs.path(), &[]);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert_eq!(text(&out.stdout), "", "{args:?}");
    }
}

/// The run log names the command and records each section (criterion 12).
#[test]
fn the_run_log_records_the_sections() {
    if !root_gate::requested() {
        return;
    }
    let logs = temp();
    let out = motd(&["--distro", "--kernel"], logs.path(), &[]);
    assert_eq!(out.status.code(), Some(0));
    let mut log = String::new();
    for entry in std::fs::read_dir(logs.path()).expect("log dir") {
        log.push_str(&std::fs::read_to_string(entry.expect("entry").path()).expect("log"));
    }
    assert!(log.contains("command=\"motd\""), "{log}");
    assert!(log.contains("sections=\"distro,kernel\""), "{log}");
    assert!(log.contains("section=\"distro\""), "{log}");
    assert!(log.contains("section=\"kernel\""), "{log}");
}

/// A Saltbox root holding the pinned motd schema and, if given, motd.yml.
fn saltbox(motd: Option<&str>) -> tempfile::TempDir {
    let root = temp();
    std::fs::create_dir(root.path().join("schema")).expect("schema dir");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../config/tests/fixtures/saltbox-schemas/motd.schema.yml"),
        root.path().join("schema/motd.schema.yml"),
    )
    .expect("schema");
    if let Some(motd) = motd {
        std::fs::write(root.path().join("motd.yml"), motd).expect("motd.yml");
    }
    root
}

fn root_env(root: &tempfile::TempDir) -> (&'static str, &str) {
    ("SB_SALTBOX_DIR", root.path().to_str().expect("UTF-8 path"))
}

/// `--generate-config` prints sb-go's example from the root's schema,
/// byte for byte, whatever else is given (criterion 5).
#[test]
fn generate_config_prints_sb_gos_example() {
    if !root_gate::requested() {
        return;
    }
    let logs = temp();
    let root = saltbox(None);
    let golden = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../motd/tests/fixtures/generate-config.golden"),
    )
    .expect("golden");
    for args in [
        &["--generate-config"][..],
        &["--generate-config", "--all", "--share"],
    ] {
        let out = motd(args, logs.path(), &[root_env(&root)]);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        assert_eq!(text(&out.stdout), golden, "{args:?}");
        assert_eq!(text(&out.stderr), "");
    }
}

/// A schema that won't load is sb-go's error with the file and the
/// cause, exit 1.
#[test]
fn generate_config_without_a_schema_fails() {
    if !root_gate::requested() {
        return;
    }
    let logs = temp();
    let out = motd(&["--generate-config"], logs.path(), &[]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "");
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("failed to generate example config: failed to load schema: schema "),
        "{stderr}"
    );
    assert!(stderr.contains("motd.schema.yml doesn't exist"), "{stderr}");
}

/// motd.yml's label colour paints the keys as truecolor under
/// `CLICOLOR_FORCE=1` (criterion 8), and no `Config:` line shows.
#[test]
fn motd_yml_colours_paint_the_keys() {
    if !root_gate::requested() {
        return;
    }
    let logs = temp();
    let root = saltbox(Some("colors:\n  text:\n    label: '#f7c'\n"));
    let out = motd(
        &["--kernel"],
        logs.path(),
        &[root_env(&root), ("CLICOLOR_FORCE", "1")],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(
        stdout.starts_with("\x1b[38;2;255;119;204mKernel:"),
        "{stdout:?}"
    );
    assert!(!stdout.contains("Config:"), "{stdout:?}");
    // The whole run opened motd.yml once (criterion 5).
    let mut log = String::new();
    for entry in std::fs::read_dir(logs.path()).expect("log dir") {
        log.push_str(&std::fs::read_to_string(entry.expect("entry").path()).expect("log"));
    }
    assert_eq!(log.matches("config file checked").count(), 1, "{log}");
    // Under ansi256 the file's colour is converted (criterion 8).
    let out = motd(
        &["--kernel"],
        logs.path(),
        &[
            root_env(&root),
            ("CLICOLOR_FORCE", "1"),
            ("SB_COLOR_PROFILE", "ansi256"),
        ],
    );
    assert!(
        text(&out.stdout).starts_with("\x1b[38;5;"),
        "{:?}",
        text(&out.stdout)
    );
}

/// An invalid motd.yml (the older bare list among them, decision 4) or
/// an unloadable schema shows the one `Config:` line first, and the
/// system sections still show (criterion 5); without motd.yml there is
/// none.
#[test]
fn an_invalid_motd_yml_shows_the_config_line() {
    if !root_gate::requested() {
        return;
    }
    let logs = temp();
    let bare = saltbox(Some(
        "plex:\n  - name: Plex\n    url: http://plex:32400\n    token: t\ncolors:\n  text:\n    label: pink\n",
    ));
    let out = motd(&["--kernel"], logs.path(), &[root_env(&bare)]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines[0], "Config:  motd.yml has 2 issues; run sb validate-config",
        "{stdout:?}"
    );
    assert!(lines[1].starts_with("Kernel:  "), "{stdout:?}");

    let unloadable = saltbox(Some("colors: {}\n"));
    std::fs::write(
        unloadable.path().join("schema/motd.schema.yml"),
        "colors:\n  kind: object\n",
    )
    .expect("schema");
    let out = motd(&["--kernel"], logs.path(), &[root_env(&unloadable)]);
    assert!(
        text(&out.stdout)
            .starts_with("Config:  motd.yml's schema can't be loaded; run sb validate-config\n"),
        "{:?}",
        text(&out.stdout)
    );

    let none = saltbox(None);
    let out = motd(&["--kernel"], logs.path(), &[root_env(&none)]);
    assert!(
        text(&out.stdout).starts_with("Kernel:  "),
        "{:?}",
        text(&out.stdout)
    );
}
