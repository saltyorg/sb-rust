//! `sb docker logs`, and `sb logs`' Containers section, through the built
//! binary on the fake Engine API (tui spec, Testing Strategy, "End to
//! end"; `DOCKER_HOST` at the fake, never the host's daemon): the empty,
//! unreachable and no-terminal cases before the screen; on a PTY, the
//! picker's containers, a container's log that follows across a restart,
//! the section reloaded by an event, `q` and Ctrl-C with the terminal
//! restored, and no log content in the run log.

mod common;
mod logs_pty;

use std::collections::HashMap;

use assert_cmd::prelude::*;
use logs_pty::{Pty, World};
use sb_docker::testing::{FakeEngine, FakeEntry, FakeLog, Options, container_event};
use serde_json::{Value, json};

const ID: &str = "c0ffee0000000000000000000000000000000000000000000000000000000001";
const OTHER: &str = "c0ffee0000000000000000000000000000000000000000000000000000000002";
const NEW: &str = "c0ffee0000000000000000000000000000000000000000000000000000000003";

/// A string the run log must never hold: it is only in a log's content.
const MARKER: &str = "content-marker-5d1c";

fn inspect(id: &str, name: &str, status: &str) -> Value {
    json!({ "Id": id, "Name": format!("/{name}"), "State": { "Status": status } })
}

/// Entry `i`, at 10:00:0`i` on 2026-11-13, UTC.
fn entry(i: u8, text: &str) -> FakeEntry {
    let time = format!("2026-11-13T10:00:{i:02}Z").parse().expect("a time");
    FakeEntry::new(time, format!("{text}\n"))
}

/// A daemon with `web` running (a log of three lines, one with the
/// marker) and `cache` exited.
fn engine() -> FakeEngine {
    let log = FakeLog {
        tty: false,
        entries: vec![
            entry(0, "web starting"),
            entry(1, &format!("a line with {MARKER}")),
            entry(2, "web ready"),
        ],
    };
    FakeEngine::start(Options {
        inspects: vec![
            inspect(ID, "web", "running"),
            inspect(OTHER, "cache", "exited"),
        ],
        logs: HashMap::from([(ID.into(), log)]),
        ..Options::default()
    })
    .expect("the fake Engine API")
}

/// A world with no services, on `engine`.
fn world(engine: &FakeEngine) -> World {
    World::new("[]", "[]").on_docker(engine.host())
}

/// sb-go's short text stays (the command contract); the long one
/// describes the picker (tui spec, "Help texts").
#[test]
fn help_is_pinned() {
    let help = common::sb()
        .args(["docker", "logs", "--help", "--color=never"])
        .assert()
        .success();
    insta::assert_snapshot!(
        "docker_logs_help",
        String::from_utf8_lossy(&help.get_output().stdout)
    );
    common::sb().args(["docker", "logs", "x"]).assert().code(2);
}

/// No containers: the message, exit 0, before any screen.
#[test]
fn no_containers_print_the_message_and_exit_0() {
    let engine = FakeEngine::start(Options {
        inspects: Vec::new(),
        ..Options::default()
    })
    .expect("the fake Engine API");
    let out = world(&engine).piped(&["docker", "logs"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "No containers found.\n"
    );
    assert!(out.status.success(), "{out:?}");
}

/// No daemon: the error block, exit 1, as `sb docker ps`.
#[test]
fn an_unreachable_daemon_is_an_error() {
    let world = World::new("[]", "[]");
    let out = world.piped(&["docker", "logs"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("couldn't reach Docker at"), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
}

/// Containers but no terminal at all (no `/dev/tty`, stdout a pipe): the
/// error, exit 1, nothing on stdout.
#[test]
fn no_terminal_is_an_error_with_nothing_on_stdout() {
    let engine = engine();
    let out = world(&engine).without_terminal(&["docker", "logs"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("sb docker logs needs a terminal to choose a log"),
        "{stderr}"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
}

/// The picker lists the containers only; Enter opens one's log, which
/// follows across a restart; Esc goes back to a picker an event has
/// reloaded; `q` exits 0 with the terminal restored; the run log has the
/// source and none of its content.
#[test]
fn a_containers_log_follows_across_a_restart() {
    let engine = engine();
    let world = world(&engine);
    let mut pty = Pty::spawn(&["docker", "logs"], world.envs());
    assert!(pty.showing("Containers (2)"), "{}", pty.contents());
    let screen = pty.contents();
    assert!(screen.contains("› cache"), "{screen}");
    assert!(screen.contains("web"), "{screen}");
    assert!(screen.contains("✓ running"), "{screen}");
    assert!(!screen.contains("Services"), "{screen}");
    pty.send(b"j\r");
    assert!(pty.showing("stdout: web ready"), "{}", pty.contents());
    assert!(pty.showing("--- following ---"), "{}", pty.contents());
    assert!(pty.contents().contains("stdout: web starting"));
    engine.append(ID, entry(3, "appended while following"));
    assert!(
        pty.showing("stdout: appended while following"),
        "{}",
        pty.contents()
    );
    // The container stops, then starts again.
    engine.end_follows(ID);
    assert!(
        pty.showing("container stopped; waiting for it to start again"),
        "{}",
        pty.contents()
    );
    engine.resume_follows(ID);
    engine.append(ID, entry(4, "after the restart"));
    engine.event(container_event("start", ID, "web"));
    assert!(pty.showing("container started"), "{}", pty.contents());
    assert!(
        pty.showing("stdout: after the restart"),
        "{}",
        pty.contents()
    );
    // A new container: the picker's list reloads on its event.
    engine.set_inspects(vec![
        inspect(ID, "web", "running"),
        inspect(OTHER, "cache", "exited"),
        inspect(NEW, "queue", "running"),
    ]);
    engine.event(container_event("create", NEW, "queue"));
    // Reloaded while the viewer shows (the second list, the first was
    // before the screen): the picker shows it when it comes back.
    let lists = || {
        engine
            .paths()
            .iter()
            .filter(|p| p.contains("/containers/json"))
            .count()
    };
    let deadline = std::time::Instant::now() + logs_pty::WAIT;
    while lists() < 2 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(lists(), 2);
    // The list's answer reaches the picker's channel.
    std::thread::sleep(std::time::Duration::from_millis(300));
    pty.send(b"\x1b");
    assert!(pty.showing("Containers (3)"), "{}", pty.contents());
    assert!(pty.contents().contains("queue"));
    pty.send(b"q");
    let status = pty.exit();
    assert!(status.success(), "{status:?}");
    pty.assert_restored();
    let log = world.log();
    assert!(log.contains("source opened"), "{log}");
    assert!(log.contains("container log page"), "{log}");
    assert!(!log.contains(MARKER), "{log}");
}

/// Ctrl-C in the viewer: exit 130, quietly, the terminal restored.
#[test]
fn ctrl_c_exits_130_with_the_terminal_restored() {
    let engine = engine();
    let world = world(&engine);
    let mut pty = Pty::spawn(&["docker", "logs"], world.envs());
    assert!(pty.showing("Containers (2)"), "{}", pty.contents());
    pty.send(b"j\r");
    assert!(pty.showing("web ready"), "{}", pty.contents());
    pty.send(b"\x03");
    let status = pty.exit();
    assert_eq!(status.exit_code(), 130, "{}", pty.contents());
    pty.assert_restored();
}

/// `sb docker logs` raises the soft `RLIMIT_NOFILE` too, as `sb logs`.
#[test]
fn the_open_files_limit_is_raised() {
    let engine = engine();
    let world = world(&engine);
    let mut pty = Pty::spawn_shell(
        r#"ulimit -Sn 1024 && exec "$0" docker logs"#,
        &[],
        world.envs(),
    );
    assert!(pty.showing("Containers (2)"), "{}", pty.contents());
    let (soft, hard) = pty.open_files_limit();
    assert_eq!(
        soft,
        Some(logs_pty::raised_open_files(hard)),
        "hard {hard:?}"
    );
    // Raised from the shell's 1024, where the hard limit allows it.
    if hard.is_none_or(|hard| hard > 1024) {
        assert_ne!(soft, Some(1024));
    }
    pty.send(b"q");
    assert!(pty.exit().success());
}

/// `sb logs` lists both kinds, services first, and opens a container's
/// log from there too.
#[test]
fn sb_logs_lists_services_and_containers() {
    let units = r#"[{"unit":"docker.service","load":"loaded","active":"active","sub":"running","description":"Docker"}]"#;
    let files = r#"[{"unit_file":"docker.service","state":"enabled","vendor_preset":null}]"#;
    let engine = engine();
    let world = World::new(units, files).on_docker(engine.host());
    let mut pty = Pty::spawn(&["logs"], world.envs());
    assert!(pty.showing("Containers (2)"), "{}", pty.contents());
    let screen = pty.contents();
    assert!(screen.contains("Services (1)"), "{screen}");
    assert!(
        screen.find("Services (1)") < screen.find("Containers (2)"),
        "{screen}"
    );
    assert!(screen.contains("1 service · 2 containers"), "{screen}");
    // docker, cache, web.
    pty.send(b"jj\r");
    assert!(pty.showing("stdout: web ready"), "{}", pty.contents());
    pty.send(b"q");
    assert!(pty.exit().success());
    pty.assert_restored();
}

/// `sb docker logs` opens on the containers; `Tab` clears the filter, so
/// the services show too (tui spec, "Entry points").
#[test]
fn tab_shows_the_services_too() {
    let units = r#"[{"unit":"docker.service","load":"loaded","active":"active","sub":"running","description":"Docker"}]"#;
    let files = r#"[{"unit_file":"docker.service","state":"enabled","vendor_preset":null}]"#;
    let engine = engine();
    let world = World::new(units, files).on_docker(engine.host());
    let mut pty = Pty::spawn(&["docker", "logs"], world.envs());
    assert!(pty.showing("Containers (2)"), "{}", pty.contents());
    assert!(!pty.contents().contains("Services"), "{}", pty.contents());
    pty.send(b"\t");
    assert!(pty.showing("Services (1)"), "{}", pty.contents());
    assert!(
        pty.contents().contains("1 service · 2 containers"),
        "{}",
        pty.contents()
    );
    pty.send(b"q");
    assert!(pty.exit().success());
    pty.assert_restored();
}

/// A service list that fails doesn't end `sb docker logs`: the Services
/// section, after `Tab`, is the one row saying why.
#[test]
fn a_failing_service_list_is_the_services_row() {
    let engine = engine();
    let world = World::with_script("echo 'systemd is down' >&2\nexit 3\n").on_docker(engine.host());
    let mut pty = Pty::spawn(&["docker", "logs"], world.envs());
    assert!(pty.showing("Containers (2)"), "{}", pty.contents());
    pty.send(b"\t");
    assert!(pty.showing("Services unavailable: "), "{}", pty.contents());
    pty.send(b"q");
    assert!(pty.exit().success());
    let log = world.log();
    assert!(log.contains("services unavailable"), "{log}");
}

/// Piped stdout: the picker on the terminal, then the container's log on
/// stdout; the stop and start notices on stderr, the follow going on
/// across the restart; Ctrl-C ends it by SIGINT.
#[test]
fn piped_stdout_streams_a_containers_log_across_a_restart() {
    let engine = engine();
    let world = world(&engine);
    let out = world.dir.path().join("out.txt");
    let err = world.dir.path().join("err.txt");
    let args = [out.display().to_string(), err.display().to_string()];
    let script = r#"exec "$0" docker logs > "$1" 2> "$2""#;
    let mut pty = Pty::spawn_shell(script, &[&args[0], &args[1]], world.envs());
    assert!(pty.showing("Containers (2)"), "{}", pty.contents());
    pty.send(b"j\r");
    let text = logs_pty::file_holding(&out, "web ready");
    // Each line `<time> stdout: <message>`, the time local.
    let shown: Vec<&str> = text
        .lines()
        .map(|line| line.split_once(' ').expect("a time").1)
        .collect();
    assert_eq!(
        shown,
        [
            "stdout: web starting",
            &format!("stdout: a line with {MARKER}"),
            "stdout: web ready"
        ]
    );
    engine.end_follows(ID);
    logs_pty::file_holding(&err, "container stopped; waiting for it to start again\n");
    engine.resume_follows(ID);
    engine.append(ID, entry(4, "after the restart"));
    engine.event(container_event("start", ID, "web"));
    logs_pty::file_holding(&out, "stdout: after the restart\n");
    logs_pty::file_holding(&err, "container started\n");
    pty.send(b"\x03");
    let status = pty.exit();
    assert_eq!(status.signal(), Some("Interrupt"), "{}", pty.contents());
    pty.assert_restored();
    assert!(!world.log().contains(MARKER), "{}", world.log());
}
