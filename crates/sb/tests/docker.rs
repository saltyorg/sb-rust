//! `sb docker` and `sb docker ps` through the built binary (docker spec,
//! success criteria 3a, 4 and 8): help and parse errors as a normal run,
//! and the command end to end against sb-docker's fake Engine API (root
//! runs: `SB_TEST_ROOT=1`, logging under the test's directory).

mod common;
mod nobody;
mod root_gate;

use std::os::unix::fs::PermissionsExt;

use sb_docker::testing::{FakeEngine, Options, fixture_rows, inspects};

/// `sb docker` alone: its help on stdout (the same as `--help`), exit 0,
/// as sb-go. Colour off through the environment: a global flag without a
/// subcommand is a parse error (below).
#[test]
fn docker_alone_prints_its_help() {
    let run = |args: &[&str]| {
        common::sb()
            .args(args)
            .env("NO_COLOR", "1")
            .output()
            .expect("run sb")
    };
    let bare = run(&["docker"]);
    assert_eq!(bare.status.code(), Some(0), "{bare:?}");
    assert!(bare.stderr.is_empty(), "{bare:?}");
    assert_eq!(bare.stdout, run(&["docker", "--help"]).stdout);
    let text = String::from_utf8_lossy(&bare.stdout);
    assert!(
        text.contains("Manage Docker containers managed by Saltbox"),
        "{text}"
    );
}

/// A global flag but no subcommand, an unknown subcommand, or `help`
/// (sb-go's docker group has none): cli-core's parse error, exit 2 (docker
/// spec, differences from sb-go).
#[test]
fn a_missing_or_unknown_subcommand_is_a_parse_error() {
    for args in [
        &["docker", "--color=never"][..],
        &["docker", "nope"],
        &["docker", "help"],
    ] {
        let out = common::sb().args(args).output().expect("run sb");
        assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
    }
}

/// `sb docker logs` is gone (the maintainer, 2026-11-18; tui decision 1):
/// clap's unknown-subcommand error, exit 2, with a hint naming `sb logs`,
/// with or without an argument or `--help` after it.
#[test]
fn docker_logs_is_gone_and_names_sb_logs() {
    for args in [
        &["docker", "logs"][..],
        &["docker", "logs", "--help"],
        &["docker", "logs", "plex"],
    ] {
        let out = common::sb().args(args).output().expect("run sb");
        assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("unrecognized subcommand 'logs'"),
            "{args:?}: {stderr}"
        );
        assert!(
            stderr.contains(
                "\n  hint: for container logs, use 'sb logs': it shows Saltbox's services \
                 and Docker containers\n"
            ),
            "{args:?}: {stderr}"
        );
        assert!(out.stdout.is_empty(), "{args:?}: {out:?}");
    }
    let help = common::sb()
        .args(["docker", "--help", "--color=never"])
        .output()
        .expect("run sb");
    let text = String::from_utf8_lossy(&help.stdout);
    assert!(!text.contains("logs"), "{text}");
}

/// The help texts: sb-go's short and long ones.
#[test]
fn help_texts_are_sb_gos() {
    let help = |args: &[&str]| {
        let out = common::sb()
            .args(args)
            .arg("--color=never")
            .output()
            .expect("run sb");
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    insta::assert_snapshot!("docker_help", help(&["docker", "--help"]));
    insta::assert_snapshot!("docker_ps_help", help(&["docker", "ps", "--help"]));
}

fn fake(options: Options) -> FakeEngine {
    FakeEngine::start(options).expect("fake Engine API")
}

/// `sb docker ps` against `engine`, piped.
fn ps(engine: &FakeEngine, args: &[&str]) -> std::process::Output {
    common::sb()
        .args(["docker", "ps"])
        .args(args)
        .env("DOCKER_HOST", engine.host())
        .env_remove("CLICOLOR_FORCE")
        .output()
        .expect("run sb")
}

/// The fixture container named `name`'s ID.
fn fixture_id(name: &str) -> String {
    inspects()
        .into_iter()
        .find(|i| i["Name"] == format!("/{name}").as_str())
        .and_then(|i| i["Id"].as_str().map(str::to_string))
        .expect("fixture")
}

/// Piped: the table, plain, every container in name order; exit 0.
#[test]
fn ps_prints_the_table() {
    if !root_gate::requested() {
        return;
    }
    let engine = fake(Options::default());
    let out = ps(&engine, &[]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains('\x1b'), "{text}");
    let order: Vec<_> = [
        "0123456789ab",
        "authelia",
        "lidarr",
        "plex",
        "radarr",
        "sonarr",
    ]
    .iter()
    .map(|name| text.find(name).unwrap_or_else(|| panic!("{name}: {text}")))
    .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{text}");
    assert!(text.contains("0.0.0.0:32400->32400/tcp"), "{text}");
    assert!(out.stderr.is_empty(), "{out:?}");
}

/// `--json`: exactly sb-docker's JSON for the fixtures, never coloured.
#[test]
fn ps_json_prints_the_documented_shape() {
    if !root_gate::requested() {
        return;
    }
    let engine = fake(Options::default());
    let out = ps(&engine, &["--json", "--color=always"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        sb_docker::json::json(&fixture_rows())
    );
    assert!(out.stderr.is_empty(), "{out:?}");
}

/// Unreachable: one error block naming the address, nothing on stdout,
/// exit 1 (with and without `--json`).
#[test]
fn an_unreachable_daemon_is_one_error() {
    if !root_gate::requested() {
        return;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let host = format!("unix://{}", dir.path().join("none.sock").display());
    for args in [&[][..], &["--json"]] {
        let out = common::sb()
            .args(["docker", "ps"])
            .args(args)
            .env("DOCKER_HOST", &host)
            .output()
            .expect("run sb");
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        assert!(out.stdout.is_empty(), "{out:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains(&format!("couldn't reach Docker at {host}")),
            "{err}"
        );
        assert_eq!(err.matches('✗').count(), 1, "{err}");
    }
}

/// A failing inspect: the rest on stdout (table or JSON), then one error
/// naming the container's short ID, exit 1.
#[test]
fn a_failing_inspect_shows_the_rest_then_the_error() {
    if !root_gate::requested() {
        return;
    }
    let sonarr = fixture_id("sonarr");
    let engine = fake(Options {
        failing: [sonarr.clone()].into(),
        ..Options::default()
    });
    let json: serde_json::Value =
        serde_json::from_slice(&ps(&engine, &["--json"]).stdout).expect("the rest as JSON");
    assert_eq!(json.as_array().map(Vec::len), Some(5), "{json}");
    for args in [&[][..], &["--json"]] {
        let out = ps(&engine, args);
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("plex") && !text.contains("sonarr"), "{text}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(err.matches('✗').count(), 1, "{err}");
        assert!(err.contains("failed to inspect 1 container(s)"), "{err}");
        assert!(
            err.contains(&format!("error inspecting container {}", &sonarr[..12])),
            "{err}"
        );
    }
}

/// Not root: `sb docker ps` relaunches through sudo with cli-core's
/// arguments (cli-core's relaunch path end to end, docker
/// success criterion 5). The stand-in sudo records what it was given and
/// its process ID, and exits 7; sb exec'd it, so the ID is sb's own and 7
/// is sb's exit code.
#[test]
fn ps_relaunches_through_sudo_when_not_root() {
    if !root_gate::requested() {
        return;
    }
    let sudo_dir = tempfile::tempdir().expect("temp dir");
    let record = sudo_dir.path().join("args");
    std::fs::write(&record, "").expect("record file");
    std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o666)).expect("chmod");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$$\" \"$@\" \"SB_RELAUNCHED=$SB_RELAUNCHED\" > '{}'\nexit 7\n",
        record.display()
    );
    let logs = tempfile::tempdir().expect("temp dir");
    let (pid, out, program) = nobody::sb_as_nobody(
        &["docker", "ps", "--json"],
        logs.path(),
        sudo_dir.path(),
        &script,
        &[],
    );
    assert_eq!(out.status.code(), Some(7), "{out:?}");
    let recorded = std::fs::read_to_string(&record).expect("recorded");
    let lines: Vec<_> = recorded.lines().collect();
    assert_eq!(lines[0], pid.to_string(), "exec'd, not spawned: {recorded}");
    // sudo is asked for root explicitly, so it runs sb as root or refuses
    // (exec decision 6); no relaunch marker is set.
    assert_eq!(lines[1..3], ["-u", "root"], "{recorded}");
    let lines = [&lines[..1], &lines[3..]].concat();
    assert_eq!(
        lines[1], "--preserve-env=VISUAL,EDITOR,SB_COLOR_PROFILE,NO_COLOR",
        "{recorded}"
    );
    // The program that ran: the relaunch names sb's own executable.
    let sb = std::fs::canonicalize(&program).expect("sb's path");
    assert_eq!(lines[2], sb.display().to_string(), "{recorded}");
    assert_eq!(
        lines[3..],
        ["docker", "ps", "--json", "SB_RELAUNCHED="],
        "{recorded}"
    );
}

/// The run that relaunches prints no startup warning: the root run it
/// hands over to does, once.
#[test]
fn a_relaunching_run_leaves_startup_warnings_to_the_root_run() {
    if !root_gate::requested() {
        return;
    }
    let sudo_dir = tempfile::tempdir().expect("temp dir");
    let logs = tempfile::tempdir().expect("temp dir");
    let (_, out, _) = nobody::sb_as_nobody(
        &["--color=never", "docker", "ps"],
        logs.path(),
        sudo_dir.path(),
        "#!/bin/sh\nexit 7\n",
        &[("SB_COLOR_PROFILE", "16bit")],
    );
    assert_eq!(out.status.code(), Some(7), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("SB_COLOR_PROFILE"), "{stderr}");
    // The control through the same harness: a command that doesn't
    // relaunch warns, so the variable did reach sb.
    let (_, version, _) = nobody::sb_as_nobody(
        &["--color=never", "version"],
        logs.path(),
        sudo_dir.path(),
        "#!/bin/sh\nexit 7\n",
        &[("SB_COLOR_PROFILE", "16bit")],
    );
    let version = String::from_utf8_lossy(&version.stderr);
    assert_eq!(version.matches("SB_COLOR_PROFILE").count(), 1, "{version}");
    // The control: the root run (here, sb run as root) warns, once.
    let root = common::sb()
        .args(["--color=never", "docker", "ps"])
        .env("SB_COLOR_PROFILE", "16bit")
        .output()
        .expect("run sb");
    assert!(
        String::from_utf8_lossy(&root.stderr).contains("SB_COLOR_PROFILE"),
        "{root:?}"
    );
    let once = String::from_utf8_lossy(&root.stderr);
    assert_eq!(once.matches("SB_COLOR_PROFILE").count(), 1, "{once}");
}
