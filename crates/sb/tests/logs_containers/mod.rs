//! `sb logs`' Containers section, a module of logs.rs (which has the
//! services), through the built binary on the fake Engine API (tui spec,
//! Testing Strategy, "End to end"; `DOCKER_HOST` at the fake, never the
//! host's daemon): on a PTY, the picker's containers, a container's log
//! that follows across a restart, the section reloaded by an event, the
//! kind filter, `q` and Ctrl-C with the terminal restored, and no log
//! content in the run log. `sb docker logs` is gone (tui decision 1): its
//! parse error is in docker.rs.

use std::collections::HashMap;

use crate::logs_pty::{self, Pty, World};
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

/// With no services the picker lists the containers only; Enter opens
/// one's log, which follows across a restart; Esc goes back to a picker
/// an event has reloaded; `q` exits 0 with the terminal restored; the run
/// log has the source and none of its content.
#[test]
fn a_containers_log_follows_across_a_restart() {
    let engine = engine();
    let world = world(&engine);
    let mut pty = Pty::spawn(&["logs"], world.envs());
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
    let mut pty = Pty::spawn(&["logs"], world.envs());
    assert!(pty.showing("Containers (2)"), "{}", pty.contents());
    pty.send(b"j\r");
    assert!(pty.showing("web ready"), "{}", pty.contents());
    pty.send(b"\x03");
    let status = pty.exit();
    assert_eq!(status.exit_code(), 130, "{}", pty.contents());
    pty.assert_restored();
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

/// `Tab` shows the services only, then the containers only (tui spec,
/// "Entry points").
#[test]
fn tab_shows_one_kind_at_a_time() {
    let units = r#"[{"unit":"docker.service","load":"loaded","active":"active","sub":"running","description":"Docker"}]"#;
    let files = r#"[{"unit_file":"docker.service","state":"enabled","vendor_preset":null}]"#;
    let engine = engine();
    let world = World::new(units, files).on_docker(engine.host());
    let mut pty = Pty::spawn(&["logs"], world.envs());
    assert!(pty.showing("Containers (2)"), "{}", pty.contents());
    assert!(
        pty.contents().contains("Services (1)"),
        "{}",
        pty.contents()
    );
    pty.send(b"\t");
    assert!(pty.showing("1 service · all healthy"), "{}", pty.contents());
    assert!(
        pty.contents().contains("Services (1)"),
        "{}",
        pty.contents()
    );
    assert!(
        !pty.contents().contains("Containers ("),
        "{}",
        pty.contents()
    );
    pty.send(b"\t");
    assert!(pty.showing("Containers (2)"), "{}", pty.contents());
    assert!(!pty.contents().contains("Services ("), "{}", pty.contents());
    assert!(
        !pty.contents().contains("1 service ·"),
        "{}",
        pty.contents()
    );
    pty.send(b"q");
    assert!(pty.exit().success());
    pty.assert_restored();
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
    let script = r#"exec "$0" logs > "$1" 2> "$2""#;
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

/// Home and End in a container's log (tui spec, "Home and End: the
/// log's ends"), as for a service's: `g` the log's first line, read from
/// the start, the header saying so, not following; `G` the newest line,
/// following again.
#[test]
fn home_and_end_reach_a_containers_ends() {
    // One a second from 10:00:00 on 2026-11-13, UTC.
    let entries = (0..3_000_u32)
        .map(|i| {
            let time = format!(
                "2026-11-13T{:02}:{:02}:{:02}Z",
                10 + i / 3_600,
                i / 60 % 60,
                i % 60
            );
            FakeEntry::new(time.parse().expect("a time"), format!("line {i}\n"))
        })
        .collect();
    let engine = FakeEngine::start(Options {
        inspects: vec![inspect(ID, "web", "running")],
        logs: HashMap::from([(
            ID.into(),
            FakeLog {
                tty: false,
                entries,
            },
        )]),
        ..Options::default()
    })
    .expect("the fake Engine API");
    let world = world(&engine);
    let mut pty = Pty::spawn(&["logs"], world.envs());
    assert!(pty.showing("Containers (1)"), "{}", pty.contents());
    pty.send(b"\r");
    assert!(pty.showing("stdout: line 2999"), "{}", pty.contents());
    pty.send(b"g");
    assert!(pty.showing("stdout: line 0"), "{}", pty.contents());
    assert!(pty.showing("--- start of log ---"), "{}", pty.contents());
    assert!(
        pty.showing("at the start · 2026-11-13T"),
        "{}",
        pty.contents()
    );
    // The whole frame: the marker on the top row, the window's last row
    // 26 entries on (the footer takes the screen's last row).
    assert!(pty.showing("stdout: line 26"), "{}", pty.contents());
    assert!(!pty.contents().contains("following"), "{}", pty.contents());
    pty.send(b"G");
    assert!(pty.showing("stdout: line 2999"), "{}", pty.contents());
    assert!(pty.showing("following"), "{}", pty.contents());
    pty.send(b"q");
    assert!(pty.exit().success());
    pty.assert_restored();
}
