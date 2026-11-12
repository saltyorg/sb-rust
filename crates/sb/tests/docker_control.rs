//! `sb docker start`, `stop` and `restart` through the built binary
//! (docker spec, "Start, stop and restart"): help and parse errors as a
//! normal run, and the commands end to end as root against sb-docker's
//! fake controller, with a stand-in `systemctl` first on `PATH` and the
//! controller's URL and unit file pointed at the test's own (root runs:
//! `SB_TEST_ROOT=1`, logging under the test's directory). No test reaches
//! the real controller: `common::apply_sb_env` points both overrides away
//! from it, and the last test checks that.

mod common;
mod nobody;
mod root_gate;

use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::PermissionsExt;

use assert_cmd::prelude::OutputAssertExt as _;
use std::path::PathBuf;
use std::process::Output;
use std::sync::Mutex;

use sb_docker::testing::controller::{Answer, FakeController};

const UNIT: &str = "saltbox_managed_docker_controller.service";
const JOB_S: &str = r#"{"job_id":"job-s","status":"pending"}"#;
const JOB_T: &str = r#"{"job_id":"job-t","status":"pending"}"#;

/// Held from writing a stand-in program until the run that uses it is
/// over, by every test that writes and runs one: it narrows the window in
/// which another test's fork holds the program's write descriptor when it
/// runs (exec fails with ETXTBSY otherwise). A busy file is never skipped
/// for the real one on `PATH`: the search stops at it.
static SPAWNS: Mutex<()> = Mutex::new(());

fn status(text: &str) -> Answer {
    Answer::ok(format!(r#"{{"status":"{text}"}}"#))
}

/// One test's world: a directory holding the stand-in `systemctl`, the
/// controller's unit file and the run log.
struct World {
    dir: tempfile::TempDir,
}

impl World {
    /// A world whose `systemctl` runs `script` after checking sb-go's
    /// arguments, and whose unit file exists.
    fn new(script: &str) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).expect("bin");
        let program = bin.join("systemctl");
        std::fs::write(
            &program,
            format!("#!/bin/sh\n[ \"$1 $2\" = 'is-active {UNIT}' ] || exit 9\n{script}\n"),
        )
        .expect("systemctl");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        std::fs::write(dir.path().join("unit.service"), "").expect("unit");
        std::fs::create_dir(dir.path().join("logs")).expect("logs");
        Self { dir }
    }

    fn unit(&self) -> PathBuf {
        self.dir.path().join("unit.service")
    }

    fn logs(&self) -> PathBuf {
        self.dir.path().join("logs")
    }

    /// The stand-in first on `PATH`, then the system's.
    fn path(&self) -> String {
        format!("{}:/usr/bin:/bin", self.dir.path().join("bin").display())
    }

    /// The run log's text.
    fn log(&self) -> String {
        std::fs::read_dir(self.logs())
            .expect("log dir")
            .map(|e| std::fs::read_to_string(e.expect("entry").path()).expect("read"))
            .collect()
    }

    /// sb with `args`, as root, its controller at `url`, piped.
    fn sb(&self, url: &str, args: &[&str]) -> Output {
        common::sb()
            .args(args)
            .env("SB_DOCKER_CONTROLLER_URL", url)
            .env("SB_DOCKER_CONTROLLER_UNIT", self.unit())
            .env("SB_LOG_DIR", self.logs())
            .env("PATH", self.path())
            .env("NO_COLOR", "1")
            .output()
            .expect("run sb")
    }
}

/// A loopback URL nothing listens on (a port just freed).
fn refused() -> String {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("free port")
        .port();
    format!("http://127.0.0.1:{port}/")
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The help texts: sb-go's short and long ones, `--ignore` on stop and
/// restart only; the docker group lists the three, sorted as cobra sorts.
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
    insta::assert_snapshot!("docker_start_help", help(&["docker", "start", "--help"]));
    insta::assert_snapshot!("docker_stop_help", help(&["docker", "stop", "--help"]));
    insta::assert_snapshot!(
        "docker_restart_help",
        help(&["docker", "restart", "--help"])
    );
}

/// No `-v`/`--verbose` (docker decision 6), and no positional arguments:
/// parse errors, exit 2, before anything runs.
#[test]
fn verbose_and_arguments_are_refused() {
    for args in [
        &["docker", "start", "-v"][..],
        &["docker", "stop", "--verbose"],
        &["docker", "restart", "-v"],
        &["docker", "start", "plex"],
        &["docker", "start", "--ignore", "plex"],
    ] {
        common::sb().args(args).assert().code(2);
    }
}

/// A start: the steps, the job asked for and waited on, exit 0; the run
/// log records the check's answer, the request and its answer, each
/// poll's status and the outcome (Task 4's run-log criterion).
#[test]
fn start_runs_and_logs_each_step() {
    if !root_gate::requested() {
        return;
    }
    let _spawns = SPAWNS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new("echo active");
    let fake = FakeController::start().expect("fake");
    // Completed at the first poll: a second would wait sb-go's 5 s.
    fake.script("/start", Answer::ok(JOB_T));
    fake.script("/job_status/job-t", status("completed"));
    let out = world.sb(fake.url(), &["docker", "start"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let shown = text(&out);
    for line in [
        "Docker containers started",
        "Docker controller service ready",
        "Requesting Docker start job",
        "Waiting for Docker start job",
    ] {
        assert!(shown.contains(line), "{line}\n{shown}");
    }
    let log = world.log();
    for record in [
        "controller URL overridden",
        "controller unit file overridden",
        "controller service ready",
        "answer=active",
        "controller request",
        r#"path="/start""#,
        "controller answer",
        "status=200",
        "controller job",
        "job=job-t",
        "job status",
        "status=completed",
    ] {
        assert!(log.contains(record), "{record}\n{log}");
    }
    for record in ["docker start", "run finished", r#"outcome="done""#] {
        assert!(log.contains(record), "{record}\n{log}");
    }
}

/// A stop and a restart send the ignored names, split on commas, empty
/// ones dropped and spaces kept; restart stops before it starts.
#[test]
fn stop_and_restart_send_the_ignored_names() {
    if !root_gate::requested() {
        return;
    }
    let _spawns = SPAWNS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new("echo active");
    let fake = FakeController::start().expect("fake");
    fake.script("/stop", Answer::ok(JOB_S));
    fake.script("/job_status/job-s", status("completed"));
    fake.script("/start", Answer::ok(JOB_T));
    fake.script("/job_status/job-t", status("completed"));
    let out = world.sb(
        fake.url(),
        &[
            "docker", "stop", "--ignore", "a,,b", "--ignore", " c", "--ignore", "",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(text(&out).contains("Docker containers stopped"), "{out:?}");
    let seen = fake.seen();
    assert_eq!(seen[0].path, "/stop");
    assert_eq!(
        seen[0].raw_query.as_deref(),
        Some("ignore=a&ignore=b&ignore=+c")
    );
    let log = world.log();
    assert!(log.contains(r#"ignored=["a", "b", " c"]"#), "{log}");
    let out = world.sb(
        fake.url(),
        &["docker", "restart", "--ignore", "plex,sonarr"],
    );
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(
        text(&out).contains("Docker containers restarted"),
        "{out:?}"
    );
    let seen: Vec<(String, Option<String>)> = fake
        .seen()
        .into_iter()
        .skip(2)
        .map(|s| (s.path, s.raw_query))
        .collect();
    assert_eq!(
        seen,
        [
            (
                "/stop".to_string(),
                Some("ignore=plex&ignore=sonarr".to_string())
            ),
            ("/job_status/job-s".to_string(), None),
            ("/start".to_string(), None),
            ("/job_status/job-t".to_string(), None),
        ]
    );
}

/// Each failure that answers at once: its block, sb-go's text, exit 1.
/// (The timeouts and the poll limit are the unit tests'.)
#[test]
fn each_failure_shows_its_block_and_exits_1() {
    if !root_gate::requested() {
        return;
    }
    let _spawns = SPAWNS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // The service isn't running.
    let world = World::new("echo inactive; exit 3");
    let fake = FakeController::start().expect("fake");
    let out = world.sb(fake.url(), &["docker", "start"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let shown = text(&out);
    assert!(
        shown.contains("the Docker controller service is not running"),
        "{shown}"
    );
    assert!(
        shown.contains("✗ Docker controller service check"),
        "{shown}"
    );
    assert_eq!(fake.seen().len(), 0);
    // No unit file.
    let world = World::new("echo active");
    std::fs::remove_file(world.unit()).expect("remove unit");
    let out = world.sb(fake.url(), &["docker", "stop"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        text(&out).contains("the Docker controller service does not exist"),
        "{out:?}"
    );
    // The request is refused.
    let world = World::new("echo active");
    let fake = FakeController::start().expect("fake");
    // Restart asks for its stop first.
    fake.script("/stop", Answer::with_status(503, r#"{"error":"blocked"}"#));
    let out = world.sb(fake.url(), &["docker", "restart"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let shown = text(&out);
    assert!(
        shown.contains("docker controller request failed with status code: 503"),
        "{shown}"
    );
    assert!(shown.contains("✗ Requesting Docker stop job"), "{shown}");
    // A failed stop starts nothing.
    let asked: Vec<String> = fake.seen().into_iter().map(|s| s.path).collect();
    assert_eq!(asked, ["/stop"]);
    // Nothing listening: sb-go's send error.
    let out = world.sb(&refused(), &["docker", "start"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        text(&out).contains("send Docker controller request: "),
        "{out:?}"
    );
    // The job fails.
    let fake = FakeController::start().expect("fake");
    fake.script("/stop", Answer::ok(JOB_S));
    fake.script("/job_status/job-s", status("failed"));
    let out = world.sb(fake.url(), &["docker", "stop"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(text(&out).contains("job failed"), "{out:?}");
}

/// A restart whose stop's polls keep failing (docker spec decision 10):
/// three in a row end the wait (about 10 s at sb's 5 s interval), the
/// notice says the controller may still be stopping containers, and the
/// restart says why it requested no start.
#[test]
fn a_restart_whose_stop_polls_fail_says_why_no_start() {
    if !root_gate::requested() {
        return;
    }
    let _spawns = SPAWNS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new("echo active");
    let fake = FakeController::start().expect("fake");
    fake.script("/stop", Answer::ok(JOB_S));
    fake.script("/job_status/job-s", Answer::with_status(502, ""));
    let out = world.sb(fake.url(), &["docker", "restart"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let shown = text(&out);
    assert!(
        shown.contains(
            "the controller may still be stopping containers; sb docker ps shows where they are"
        ),
        "{shown}"
    );
    assert!(
        shown.contains("the start wasn't requested: sb couldn't tell whether the stop finished"),
        "{shown}"
    );
    assert!(
        shown.contains("job status check failed with status code: 502"),
        "{shown}"
    );
    let asked: Vec<String> = fake.seen().into_iter().map(|s| s.path).collect();
    assert_eq!(
        asked,
        [
            "/stop",
            "/job_status/job-s",
            "/job_status/job-s",
            "/job_status/job-s"
        ]
    );
}

/// No `systemctl` on `PATH`: "not running" (sb-go's text for a check that
/// can't run), nothing asked of the controller.
#[test]
fn no_systemctl_on_path_is_not_running() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new("echo active");
    let fake = FakeController::start().expect("fake");
    let out = common::sb()
        .args(["docker", "start"])
        .env("SB_DOCKER_CONTROLLER_URL", fake.url())
        .env("SB_DOCKER_CONTROLLER_UNIT", world.unit())
        .env("SB_LOG_DIR", world.logs())
        .env("PATH", world.dir.path().join("empty"))
        .env("NO_COLOR", "1")
        .output()
        .expect("run sb");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        text(&out).contains("the Docker controller service is not running"),
        "{out:?}"
    );
    assert_eq!(fake.seen().len(), 0);
}

/// A controller URL that doesn't parse: sb-go's text, before the runner
/// starts (no step drawn), exit 1.
#[test]
fn a_malformed_controller_url_fails_before_the_runner() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new("echo active");
    let out = world.sb("not a url", &["docker", "start"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let shown = text(&out);
    assert!(shown.contains("parse Docker controller URL: "), "{shown}");
    assert!(!shown.contains("Starting Docker containers"), "{shown}");
    // Not UTF-8: refused too, not taken for unset (the real controller).
    let out = common::sb()
        .args(["docker", "start"])
        .env(
            "SB_DOCKER_CONTROLLER_URL",
            std::ffi::OsStr::from_bytes(b"http://\xff/"),
        )
        .env("SB_DOCKER_CONTROLLER_UNIT", world.unit())
        .env("SB_LOG_DIR", world.logs())
        .env("PATH", world.path())
        .output()
        .expect("run sb");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        text(&out).contains("parse Docker controller URL: SB_DOCKER_CONTROLLER_URL isn't UTF-8"),
        "{out:?}"
    );
}

/// On a terminal: the same run, live, with the steps' labels shown.
#[test]
fn start_on_a_terminal() {
    if !root_gate::requested() {
        return;
    }
    let _spawns = SPAWNS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new("echo active");
    let fake = FakeController::start().expect("fake");
    fake.script("/start", Answer::ok(JOB_T));
    fake.script("/job_status/job-t", status("completed"));
    let program = common::sb().get_program().to_string_lossy().into_owned();
    let mut cmd = std::process::Command::new("script");
    common::apply_sb_env(&mut cmd);
    let out = cmd
        .args([
            "-qec",
            &format!("stty cols 100 rows 30; exec '{program}' docker start"),
            "/dev/null",
        ])
        .env("SB_DOCKER_CONTROLLER_URL", fake.url())
        .env("SB_DOCKER_CONTROLLER_UNIT", world.unit())
        .env("SB_LOG_DIR", world.logs())
        .env("PATH", world.path())
        .env("TERM", "xterm-256color")
        .output()
        .expect("run script");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let shown = String::from_utf8_lossy(&out.stdout);
    assert!(shown.contains("Docker containers started"), "{shown}");
    assert_eq!(fake.seen().len(), 2);
}

/// Not root: the relaunch through sudo with cli-core's arguments. The
/// stand-in sudo records them and exits 7, never running sb (the relaunch
/// drops the overrides, so a relaunched run would reach the real
/// controller).
#[test]
fn restart_relaunches_through_sudo_when_not_root() {
    if !root_gate::requested() {
        return;
    }
    let _spawns = SPAWNS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let sudo_dir = tempfile::tempdir().expect("temp dir");
    let record = sudo_dir.path().join("args");
    std::fs::write(&record, "").expect("record file");
    std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o666)).expect("chmod");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$$\" \"$@\" > '{}'\nexit 7\n",
        record.display()
    );
    let logs = tempfile::tempdir().expect("temp dir");
    let (pid, out) = nobody::sb_as_nobody(
        &["docker", "restart", "--ignore", "plex"],
        logs.path(),
        sudo_dir.path(),
        &script,
        &[],
    );
    assert_eq!(out.status.code(), Some(7), "{out:?}");
    let recorded = std::fs::read_to_string(&record).expect("recorded");
    let lines: Vec<&str> = recorded.lines().collect();
    assert_eq!(lines[0], pid.to_string(), "exec'd, not spawned: {recorded}");
    assert_eq!(lines[1..3], ["-u", "root"], "{recorded}");
    assert_eq!(
        lines[lines.len() - 4..],
        ["docker", "restart", "--ignore", "plex"],
        "{recorded}"
    );
}

/// With only the shared test environment (no overrides of the test's
/// own), a run never reaches the real controller: its unit file there
/// doesn't exist, so the check stops it first.
#[test]
fn the_shared_environment_keeps_runs_off_the_real_controller() {
    if !root_gate::requested() {
        return;
    }
    let _spawns = SPAWNS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let world = World::new("echo active");
    let out = common::sb()
        .args(["docker", "start"])
        .env("SB_LOG_DIR", world.logs())
        .env("PATH", world.path())
        .env("NO_COLOR", "1")
        .output()
        .expect("run sb");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        text(&out).contains("the Docker controller service does not exist"),
        "{out:?}"
    );
}
