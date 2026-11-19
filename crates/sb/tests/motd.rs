//! `sb motd` through the built binary (motd spec, Testing strategy, "End
//! to end"; module build M36, M37): the layout of the system sections,
//! colour under Saltbox's `CLICOLOR_FORCE=1` with stdout piped, the
//! selection error, an argument's parse error and `-v`'s notice; the banner through stand-in
//! toilet and boxes first on `PATH` (M46). Root runs (`SB_TEST_ROOT=1`),
//! each with its own log directory; a run as another user would relaunch
//! through sudo. The system sections read the host's os-release, /proc
//! and `uname(2)`, read-only; nothing here contacts a service.

mod common;
mod root_gate;

use std::path::Path;
use std::process::Output;

/// `sb motd <args>` as root, stdout piped, logging to `logs`, with `env`.
/// No banner unless `args` give a title or a banner file: an empty
/// `--title` otherwise, so the host's toilet and boxes never run.
fn motd(args: &[&str], logs: &Path, env: &[(&str, &str)]) -> Output {
    let mut cmd = common::sb();
    cmd.arg("motd");
    if !args.contains(&"--title") {
        cmd.args(["--title", ""]);
    }
    cmd.args(args)
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
    // Without motd.yml the key takes motd's built-in label colour,
    // #58A6FF (decision 5, amended 2026-11-19).
    assert!(
        text(&forced.stdout).starts_with("\x1b[38;2;88;166;255mKernel:"),
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

/// `--share` masks Last login's source and every address in the
/// values (criterion 7); without it the source shows as given. `last` is
/// a stand-in first on `PATH`.
#[test]
fn share_masks_the_last_login_source() {
    use std::os::unix::fs::PermissionsExt as _;
    if !root_gate::requested() {
        return;
    }
    let logs = temp();
    let tools = temp();
    let last = tools.path().join("last");
    std::fs::write(
        &last,
        "#!/bin/sh\necho 'sbuser   pts/0        192.0.2.9        Sat Nov 14 23:10   still logged in'\n",
    )
    .expect("stand-in");
    std::fs::set_permissions(&last, std::fs::Permissions::from_mode(0o755)).expect("mode");
    let path = tools.path().display().to_string();
    let out = motd(&["--login", "--share"], logs.path(), &[("PATH", &path)]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        "Last login:  sbuser at Sat Nov 14 23:10 (still logged in) from xxx.xxx.xxx.xxx\n\n"
    );
    let out = motd(&["--login"], logs.path(), &[("PATH", &path)]);
    assert!(
        text(&out.stdout).contains("from 192.0.2.9\n"),
        "{}",
        text(&out.stdout)
    );
}

/// An argument is clap's parse error, exit 2 (`cobra.NoArgs`). sb-go's
/// `-v` is accepted (the maintainer, 2026-11-18; motd decision 9's
/// amendment): the same output, and one notice on stderr; without it, as
/// the login runs motd, no notice.
#[test]
fn arguments_are_parse_errors_and_verbose_a_notice() {
    if !root_gate::requested() {
        return;
    }
    let logs = temp();
    let out = motd(&["--all", "extra"], logs.path(), &[]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(text(&out.stdout), "");
    let login = motd(&["--kernel"], logs.path(), &[]);
    assert_eq!(login.status.code(), Some(0), "{}", text(&login.stderr));
    assert!(
        !text(&login.stderr).contains("isn't needed"),
        "{}",
        text(&login.stderr)
    );
    for flag in ["-v", "-vv", "--verbose"] {
        let asked = motd(&["--kernel", flag], logs.path(), &[]);
        assert_eq!(
            asked.status.code(),
            Some(0),
            "{flag}: {}",
            text(&asked.stderr)
        );
        assert_eq!(text(&asked.stdout), text(&login.stdout), "{flag}");
        let stderr = text(&asked.stderr);
        assert_eq!(
            stderr
                .matches("! -v isn't needed: sb logs full detail to ")
                .count(),
            1,
            "{flag}: {stderr}"
        );
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
/// byte for byte but for motd's built-in colours (Difference 36),
/// whatever else is given (criterion 5).
#[test]
fn generate_config_prints_the_example() {
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

/// A theme file's role colours don't reach motd's keys: the theme file
/// has no motd entry (decision 5, amended 2026-11-19), so the label keeps
/// motd's built-in #58A6FF; its `color_profile` still applies.
#[test]
fn a_theme_file_leaves_motds_colours() {
    if !root_gate::requested() {
        return;
    }
    let logs = temp();
    let config = logs.path().join("config.toml");
    std::fs::write(
        &config,
        "[theme.colors]\naccent = \"#102030\"\nok = \"#102030\"\n",
    )
    .expect("write config");
    let config = config.to_str().expect("utf-8 path");
    let out = motd(
        &["--kernel"],
        logs.path(),
        &[("SB_CONFIG", config), ("CLICOLOR_FORCE", "1")],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(
        stdout.starts_with("\x1b[38;2;88;166;255mKernel:"),
        "{stdout:?}"
    );
    assert!(!stdout.contains("16;32;48"), "{stdout:?}");
    std::fs::write(
        logs.path().join("config.toml"),
        "[theme]\ncolor_profile = \"ansi\"\n",
    )
    .expect("write config");
    let out = motd(
        &["--kernel"],
        logs.path(),
        &[("SB_CONFIG", config), ("CLICOLOR_FORCE", "1")],
    );
    // Bright blue, the nearest VGA colour.
    assert!(
        text(&out.stdout).starts_with("\x1b[94mKernel:"),
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

/// A directory of stand-ins for `PATH`: toilet prints its arguments, and
/// boxes its own, then its stdin between `#` lines. Without `boxes`, only
/// toilet is there; with neither, the directory is empty.
fn stand_ins(toilet: bool, boxes: bool) -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = temp();
    let write = |name: &str, body: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("stand-in");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("mode");
    };
    if toilet {
        write("toilet", r#"echo "toilet $*""#);
    }
    if boxes {
        write("boxes", r#"echo "boxes $*"; echo '#'; cat; echo '#'"#);
    }
    dir
}

/// The title through toilet and boxes, then a blank line (sb-go's
/// `fmt.Println`), then the sections (motd spec, Command flow step 5).
#[test]
fn the_banner_comes_first_through_toilet_and_boxes() {
    if !root_gate::requested() {
        return;
    }
    let logs = temp();
    let tools = stand_ins(true, true);
    // The stand-ins first; the boxes stand-in needs `cat`.
    let path = format!(
        "{}:{}",
        tools.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = motd(
        &["--title", "Saltbox", "--kernel"],
        logs.path(),
        &[("PATH", &path)],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(
        stdout.starts_with("boxes -d peek -a hc -p h8\n#\ntoilet -f ivrit Saltbox\n#\n\nKernel:  "),
        "{stdout:?}"
    );
    assert_eq!(text(&out.stderr), "");
    let out = motd(
        &[
            "--title", "Saltbox", "--type", "none", "--font", "term", "--kernel",
        ],
        logs.path(),
        &[("PATH", &path)],
    );
    assert!(
        text(&out.stdout).starts_with("toilet -f term Saltbox\n\nKernel:  "),
        "{:?}",
        text(&out.stdout)
    );
}

/// Without toilet, sb-go's fallback and its warning; without boxes,
/// toilet's banner alone and a warning (Command flow step 5).
#[test]
fn missing_tools_fall_back_with_a_warning() {
    if !root_gate::requested() {
        return;
    }
    let logs = temp();
    let none = stand_ins(false, false);
    let path = none.path().display().to_string();
    let out = motd(
        &["--title", "Saltbox", "--kernel"],
        logs.path(),
        &[("PATH", &path)],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).starts_with("--- Saltbox ---\n\nKernel:  "),
        "{:?}",
        text(&out.stdout)
    );
    assert_eq!(
        text(&out.stderr),
        "warning: toilet command not found, using fallback banner\n"
    );
    let toilet = stand_ins(true, false);
    let path = toilet.path().display().to_string();
    let out = motd(
        &["--title", "Saltbox", "--kernel"],
        logs.path(),
        &[("PATH", &path)],
    );
    assert!(
        text(&out.stdout).starts_with("toilet -f ivrit Saltbox\n\nKernel:  "),
        "{:?}",
        text(&out.stdout)
    );
    assert_eq!(
        text(&out.stderr),
        "warning: boxes command not found, using toilet only\n"
    );
}

/// A banner file is printed raw, or through toilet with its arguments
/// as argv; an unreadable one is sb-go's error, exit 1.
#[test]
fn a_banner_file_is_printed_or_drawn() {
    if !root_gate::requested() {
        return;
    }
    let logs = temp();
    let tools = stand_ins(true, true);
    let path = tools.path().display().to_string();
    let file = tools.path().join("banner.txt");
    std::fs::write(&file, "Welcome\n").expect("banner");
    let file = file.display().to_string();
    let out = motd(
        &["--banner-file", &file, "--kernel"],
        logs.path(),
        &[("PATH", &path)],
    );
    assert!(
        text(&out.stdout).starts_with("Welcome\n\nKernel:  "),
        "{:?}",
        text(&out.stdout)
    );
    let out = motd(
        &[
            "--banner-file",
            &file,
            "--banner-file-toilet",
            "-f 'my font'",
            "--kernel",
        ],
        logs.path(),
        &[("PATH", &path)],
    );
    assert!(
        text(&out.stdout).starts_with("toilet -f my font\n\nKernel:  "),
        "{:?}",
        text(&out.stdout)
    );
    let missing = tools.path().join("missing").display().to_string();
    let out = motd(&["--banner-file", &missing, "--kernel"], logs.path(), &[]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "");
    assert!(
        text(&out.stderr).contains(&format!("could not read banner file '{missing}': ")),
        "{}",
        text(&out.stderr)
    );
}

/// An unknown type or font is sb-go's error with its list, exit 1,
/// before anything is shown (Command flow step 3).
#[test]
fn an_unknown_type_or_font_is_refused() {
    if !root_gate::requested() {
        return;
    }
    let logs = temp();
    let out = motd(
        &["--title", "x", "--type", "nope", "--kernel"],
        logs.path(),
        &[],
    );
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "");
    let stderr = text(&out.stderr);
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(
        lines[0], "✗ invalid banner type specified: nope",
        "{stderr}"
    );
    assert_eq!(lines[1..3], ["Available types:", ""], "{stderr}");
    assert!(
        lines[3].starts_with("  ada-box           ada-cmt"),
        "{stderr}"
    );
    assert!(stderr.contains("unicornthink"), "{stderr}");
    assert!(
        lines[lines.len() - 1].starts_with("  details: "),
        "{stderr}"
    );
    let out = motd(
        &["--title", "x", "--font", "no-such-font", "--kernel"],
        logs.path(),
        &[],
    );
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("invalid font specified: no-such-font"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Available fonts (from /usr/share/figlet):"),
        "{stderr}"
    );
    assert!(stderr.contains("term"), "{stderr}");
}

/// `--json` (motd spec, `--json`; decision 28): one line of JSON on
/// stdout, the chosen sections in order, no banner and no colour even
/// under `CLICOLOR_FORCE=1`; motd.yml's issues on stderr.
#[test]
fn json_prints_the_sections_document() {
    if !root_gate::requested() {
        return;
    }
    let logs = temp();
    let bare = saltbox(Some(
        "plex:\n  - name: Plex\n    url: http://plex:32400\n    token: t\ncolors:\n  text:\n    label: pink\n",
    ));
    let out = motd(
        // The banner's flags aren't read: an unknown type is no error.
        &[
            "--kernel", "--distro", "--json", "--title", "Saltbox", "--type", "bogus",
        ],
        logs.path(),
        &[("CLICOLOR_FORCE", "1"), root_env(&bare)],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(
        stdout.ends_with('\n') && stdout.lines().count() == 1,
        "{stdout:?}"
    );
    assert!(!out.stdout.contains(&0x1b), "{stdout:?}");
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("JSON");
    assert_eq!(document["schema"], 1);
    let sections = document["sections"].as_array().expect("sections");
    let names: Vec<&str> = sections
        .iter()
        .map(|s| s["section"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["distribution", "kernel"]);
    let release = rustix::system::uname();
    let kernel = String::from_utf8_lossy(release.release().to_bytes()).into_owned();
    assert_eq!(sections[1]["value"]["release"], kernel.as_str());
    assert!(
        text(&out.stderr).contains("motd.yml has 2 issues; run sb validate-config"),
        "{}",
        text(&out.stderr)
    );
}

/// `--json` takes the selection's error, with nothing on stdout, and
/// never goes with `--generate-config` (a usage error).
#[test]
fn json_without_sections_or_with_generate_config_fails() {
    if !root_gate::requested() {
        return;
    }
    let logs = temp();
    let none = motd(&["--json"], logs.path(), &[]);
    assert_eq!(none.status.code(), Some(1));
    assert_eq!(text(&none.stdout), "");
    assert!(
        text(&none.stderr).contains("no information selected to display"),
        "{}",
        text(&none.stderr)
    );
    let both = motd(&["--json", "--generate-config"], logs.path(), &[]);
    assert_eq!(both.status.code(), Some(2), "{}", text(&both.stderr));
    assert_eq!(text(&both.stdout), "");
}
