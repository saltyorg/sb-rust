//! `sb docker ps` end to end against sb-docker's fake Engine API: errors,
//! settings, output modes and the run log (docker spec, success criteria 3,
//! 3a, 4, 7 and 8). Root runs (`SB_TEST_ROOT=1`), logging under Cargo's
//! test temporary directory (`common::sb()`); every run reaches the fake
//! through `DOCKER_HOST` (criterion 8), never the host's daemon. An empty
//! `DOCKER_HOST` would mean the host's default socket, so that case stays
//! with sb-docker's `address` unit tests. Fitting 120 and 70 columns is
//! checked on the rendered table (sb-docker and term-ui); here, 50 on a
//! real terminal.

mod common;
mod root_gate;

use std::process::Output;

use sb_cli_core::theme::{Palette, Profile, Theme};
use sb_docker::testing::{FakeEngine, Options, fixture_rows, inspects};

fn fake(options: Options) -> FakeEngine {
    FakeEngine::start(options).expect("fake Engine API")
}

/// `sb docker ps` piped, with `env` added.
fn ps(host: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = common::sb();
    cmd.args(["docker", "ps"])
        .args(args)
        .env("DOCKER_HOST", host)
        .env_remove("CLICOLOR_FORCE")
        .env_remove("NO_COLOR");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("run sb")
}

/// `sb docker ps` on a pseudo-terminal `cols` wide (util-linux `script`,
/// as sb's colour tests), with `common::sb()`'s environment and `env`.
fn ps_on_a_terminal(host: &str, args: &str, cols: u16, env: &[(&str, &str)]) -> Output {
    let program = common::sb().get_program().to_string_lossy().into_owned();
    let mut cmd = std::process::Command::new("script");
    common::apply_sb_env(&mut cmd);
    cmd.args([
        "-qec",
        &format!("stty cols {cols} rows 30; exec '{program}' docker ps {args}"),
        "/dev/null",
    ]);
    cmd.env("DOCKER_HOST", host)
        .env("TERM", "xterm-256color")
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR")
        .env_remove("CLICOLOR_FORCE");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("run script")
}

fn fixture_id(name: &str) -> String {
    inspects()
        .into_iter()
        .find(|i| i["Name"] == format!("/{name}").as_str())
        .and_then(|i| i["Id"].as_str().map(str::to_string))
        .expect("fixture")
}

fn json_len(stdout: &[u8]) -> Option<usize> {
    serde_json::from_slice::<serde_json::Value>(stdout)
        .ok()?
        .as_array()
        .map(Vec::len)
}

fn plain_table(rows: &[sb_docker::ps::Row]) -> String {
    let plain = Theme::resolve(&Palette::default(), Profile::Ascii);
    format!(
        "{}\n",
        sb_docker::ps::table(rows).render(&plain, false, None)
    )
}

/// Removed between the list and its inspect: left out, exit 0.
#[test]
fn a_vanished_container_is_left_out() {
    if !root_gate::requested() {
        return;
    }
    let engine = fake(Options {
        vanished: [fixture_id("plex")].into(),
        ..Options::default()
    });
    let out = ps(&engine.host(), &[], &[]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains("plex") && text.contains("sonarr"), "{text}");
    assert!(out.stderr.is_empty(), "{out:?}");
    let json = ps(&engine.host(), &["--json"], &[]);
    assert_eq!(json.status.code(), Some(0), "{json:?}");
    assert_eq!(json_len(&json.stdout), Some(5), "{json:?}");
}

/// An older daemon that refuses versioned paths above its own answers
/// sb's unversioned requests: the table and the JSON, exit 0. Its answers
/// keep today's shape: no field sb reads postdates API 1.41, and a newer
/// daemon's shape (fields missing or added) is sb-docker's test.
#[test]
fn an_old_daemon_answers() {
    if !root_gate::requested() {
        return;
    }
    let engine = fake(Options {
        api_versions: ((1, 24), (1, 41)),
        ..Options::default()
    });
    let table = ps(&engine.host(), &[], &[]);
    assert_eq!(table.status.code(), Some(0), "{table:?}");
    assert!(
        String::from_utf8_lossy(&table.stdout).contains("plex"),
        "{table:?}"
    );
    assert!(table.stderr.is_empty(), "{table:?}");
    let json = ps(&engine.host(), &["--json"], &[]);
    assert_eq!(json.status.code(), Some(0), "{json:?}");
    assert_eq!(json_len(&json.stdout), Some(6), "{json:?}");
    assert!(json.stderr.is_empty(), "{json:?}");
    let paths = engine.paths();
    assert!(
        !paths.is_empty() && paths.iter().all(|p| !p.starts_with("/v1.")),
        "{paths:?}"
    );
}

/// One refused setting: the host, extra environment, what the error names.
type Case<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a str);

/// A TLS setting, an https or an ssh host: an error naming it, nothing on
/// stdout, exit 1, and no request reaches the daemon.
#[test]
fn tls_and_ssh_settings_are_refused_before_any_request() {
    if !root_gate::requested() {
        return;
    }
    let engine = fake(Options::default());
    let host = engine.host();
    let cases: [Case<'_>; 4] = [
        (&host, &[("DOCKER_TLS_VERIFY", "1")], "DOCKER_TLS_VERIFY"),
        (&host, &[("DOCKER_CERT_PATH", "/certs")], "DOCKER_CERT_PATH"),
        (
            "https://198.51.100.7:2376",
            &[],
            "DOCKER_HOST=https://198.51.100.7:2376",
        ),
        (
            "ssh://user@198.51.100.7",
            &[],
            "DOCKER_HOST=ssh://user@198.51.100.7",
        ),
    ];
    for (host, env, named) in cases {
        for args in [&[][..], &["--json"]] {
            let out = ps(host, args, env);
            assert_eq!(out.status.code(), Some(1), "{named}: {out:?}");
            assert!(out.stdout.is_empty(), "{named}: {out:?}");
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(err.contains(named), "{err}");
        }
    }
    assert!(engine.paths().is_empty(), "{:?}", engine.paths());
}

/// No containers: exactly the headings, or `[]`; exit 0 either way.
#[test]
fn no_containers() {
    if !root_gate::requested() {
        return;
    }
    let engine = fake(Options {
        inspects: Vec::new(),
        ..Options::default()
    });
    let out = ps(&engine.host(), &[], &[]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), plain_table(&[]));
    assert!(out.stderr.is_empty(), "{out:?}");
    let json = ps(&engine.host(), &["--json"], &[]);
    assert_eq!(json.status.code(), Some(0), "{json:?}");
    assert_eq!(String::from_utf8_lossy(&json.stdout), "[]\n");
    assert!(json.stderr.is_empty(), "{json:?}");
}

/// Piped (`auto`): exactly the table at its natural width, no escape
/// codes, whatever COLUMNS says (criterion 3).
#[test]
fn piped_output_is_the_plain_table_at_natural_width() {
    if !root_gate::requested() {
        return;
    }
    let engine = fake(Options::default());
    let out = ps(&engine.host(), &[], &[("COLUMNS", "40")]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        plain_table(&fixture_rows())
    );
    assert!(out.stderr.is_empty(), "{out:?}");
}

/// On a terminal: coloured by default; no escape codes with `NO_COLOR` or
/// `--color=never`, nor ever in the JSON; fitted to the terminal's width
/// (criteria 3 and 3a).
#[test]
fn on_a_terminal_colour_and_width_follow_the_terminal() {
    if !root_gate::requested() {
        return;
    }
    let engine = fake(Options::default());
    let host = engine.host();
    let coloured = ps_on_a_terminal(&host, "", 120, &[]);
    assert!(coloured.status.success(), "{coloured:?}");
    assert!(coloured.stdout.contains(&0x1b), "{coloured:?}");
    for (args, env) in [("", &[("NO_COLOR", "1")][..]), ("--color=never", &[])] {
        let out = ps_on_a_terminal(&host, args, 120, env);
        assert!(out.status.success(), "{out:?}");
        assert!(!out.stdout.contains(&0x1b), "{args} {env:?}: {out:?}");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("plex"),
            "{args} {env:?}: {out:?}"
        );
    }
    let json = ps_on_a_terminal(&host, "--json", 120, &[]);
    assert!(json.status.success(), "{json:?}");
    assert!(!json.stdout.contains(&0x1b), "{json:?}");
    let lines: Vec<u8> = json
        .stdout
        .iter()
        .copied()
        .filter(|&b| b != b'\r')
        .collect();
    assert_eq!(json_len(&lines), Some(6), "{json:?}");
    let narrow = ps_on_a_terminal(&host, "--color=never", 50, &[]);
    assert!(narrow.status.success(), "{narrow:?}");
    let text = String::from_utf8_lossy(&narrow.stdout);
    assert!(text.contains("plex"), "{text}");
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        assert!(console::measure_text_width(line) <= 50, "{line:?}\n{text}");
    }
}

/// A run's log records the daemon, its versions, the counts, each failed
/// inspect and how the run ended (criterion 7). The fake reports API
/// 1.55, above bollard's 1.53, so logging bollard's version instead would
/// show.
#[test]
fn the_run_log_records_the_daemon_and_the_counts() {
    if !root_gate::requested() {
        return;
    }
    let failing = [fixture_id("sonarr"), fixture_id("plex")];
    let engine = fake(Options {
        api_versions: ((1, 40), (1, 55)),
        failing: failing.iter().cloned().collect(),
        ..Options::default()
    });
    let logs = tempfile::tempdir().expect("temp dir");
    let out = ps(
        &engine.host(),
        &[],
        &[("SB_LOG_DIR", logs.path().to_str().expect("utf-8 path"))],
    );
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let log = std::fs::read_to_string(logs.path().join("sb.log")).expect("run log");
    let line = |marker: &str| {
        log.lines()
            .find(|l| l.contains(marker))
            .unwrap_or_else(|| panic!("no line with {marker:?}:\n{log}"))
    };
    let host = engine.host();
    assert!(
        line("docker daemon").contains(&format!(
            r#"sb_docker::connect: docker daemon address="{host}" version="0.0.0-fake" api_version="1.55""#
        )),
        "{log}"
    );
    let listed = line("containers listed");
    assert!(listed.contains("listed=6 inspected=4 failed=2"), "{log}");
    assert!(listed.contains("elapsed_ms="), "{log}");
    // Each failed inspect has its own warning, with its cause.
    for id in &failing {
        let warning = line(&format!("container=\"{}\"", &id[..12]));
        assert!(
            warning.contains(" WARN ") && warning.contains("inspect failed"),
            "{log}"
        );
        assert!(warning.contains("fake inspect failure"), "{log}");
    }
    assert!(
        line("run started").contains(r#"command="docker ps""#),
        "{log}"
    );
    assert!(
        line("run finished").contains(r#"outcome="failed""#),
        "{log}"
    );
}
