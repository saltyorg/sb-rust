//! A hangup cancels the run cleanly (decision 2; exec-H1, cli-M1): SIGHUP
//! is handled as SIGTERM is. sb stops its piped child with the usual
//! grace (exec puts it in its own process group, so the hangup never
//! reaches it), writes the run log's ending, then dies by SIGHUP. Under
//! `nohup` (SIGHUP ignored at startup) sb leaves it ignored (decision 8).
//! The child is a stand-in `systemctl` that `sb docker start` checks the
//! controller's service with; it leaves a marker, so a test knows the
//! stand-in ran and not the host's. The controller is sb-docker's fake.
//! Root runs (`SB_TEST_ROOT=1`).

mod common;
mod processes;
mod root_gate;

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use processes::{KillOnDrop, StandIn, gone, wait_for};
use rustix::process::{Pid, Signal, kill_process};
use sb_docker::testing::controller::FakeController;

const UNIT: &str = "saltbox_managed_docker_controller.service";

/// A test's world: a stand-in `systemctl` that writes its pid and sleeps,
/// the controller's unit file and the run log, in one temporary directory.
struct World {
    dir: tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        Self::with_stand_in("exec sleep 30")
    }

    /// A world whose stand-in runs `script` once it has written its pid
    /// (`$here` is the world's directory).
    fn with_stand_in(script: &str) -> Self {
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("temp dir");
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).expect("bin");
        let program = bin.join("systemctl");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\n[ \"$1 $2\" = 'is-active {UNIT}' ] || exit 9\n\
                 here=\"$(dirname \"$0\")/..\"\necho $$ > \"$here/systemctl.pid\"\n{script}\n"
            ),
        )
        .expect("systemctl");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        std::fs::write(dir.path().join("unit.service"), "").expect("unit");
        std::fs::create_dir(dir.path().join("logs")).expect("logs");
        Self { dir }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// The environment of a run against the fake at `url`, for `cmd` (sb
    /// itself, or what starts it).
    fn env(&self, cmd: &mut Command, url: &str) {
        common::apply_sb_env(cmd);
        cmd.env("SB_LOG_DIR", self.file("logs"))
            .env("SB_DOCKER_CONTROLLER_URL", url)
            .env("SB_DOCKER_CONTROLLER_UNIT", self.file("unit.service"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.file("bin").display()),
            );
    }

    /// The stand-in's pid once it runs.
    fn stand_in(&self) -> StandIn {
        let pid_file = self.file("systemctl.pid");
        StandIn(
            wait_for(|| {
                std::fs::read_to_string(&pid_file)
                    .ok()
                    .and_then(|text| text.trim().parse().ok())
            })
            .expect("systemctl running"),
        )
    }

    /// The run log's text.
    fn log(&self) -> String {
        std::fs::read_to_string(self.file("logs").join("sb.log")).unwrap_or_default()
    }
}

/// Fail at once, clearly, when the suite itself runs with SIGHUP ignored
/// (under `nohup`): sb would inherit that and, as decision 8 asks, leave
/// it ignored, so these tests would wait out their stand-in instead.
/// `SigIgn` in `/proc/self/status` (`proc_pid_status(5)`): SIGHUP is bit 0.
fn assert_sighup_not_ignored() {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let ignored = status
        .lines()
        .find_map(|line| line.strip_prefix("SigIgn:"))
        .and_then(|mask| u64::from_str_radix(mask.trim(), 16).ok())
        .is_some_and(|mask| mask & 1 != 0);
    assert!(
        !ignored,
        "the tests run with SIGHUP ignored (under nohup?): sb leaves it ignored"
    );
}

fn signal(pid: u32, signal: Signal) {
    let pid = Pid::from_raw(i32::try_from(pid).expect("pid")).expect("pid");
    kill_process(pid, signal).expect("kill");
}

/// `kill -HUP`: as SIGTERM, the stand-in stopped (not left behind), the
/// run log's ending written, sb dead by SIGHUP itself.
#[test]
fn a_sighup_cancels_as_sigterm_does() {
    if !root_gate::requested() {
        return;
    }
    assert_sighup_not_ignored();
    let world = World::new();
    let fake = FakeController::start().expect("fake");
    let mut cmd = common::sb();
    world.env(&mut cmd, fake.url());
    cmd.args(["docker", "start"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut sb = KillOnDrop(cmd.spawn().expect("run sb"), None);
    let stand_in = world.stand_in();
    signal(sb.0.id(), Signal::HUP);
    let status = sb.0.wait().expect("wait");
    assert_eq!(status.signal(), Some(Signal::HUP.as_raw()), "{status:?}");
    assert!(
        wait_for(|| gone(stand_in.0).then_some(())).is_some(),
        "the stand-in was left running"
    );
    let log = world.log();
    assert!(
        log.contains("run finished") && log.contains("outcome=\"interrupted (exit 129)\""),
        "{log}"
    );
}

/// A real hangup: the terminal's master side closes (its `script` is
/// killed), so the kernel hangs up the session, and sb's writes to stderr
/// and its terminal restore fail with EIO. sb still writes the run log's
/// ending, stops the stand-in and dies by SIGHUP: a wrapper shell that
/// catches the hangup itself (so sb inherits the default) records `$?`.
/// One hangup reaches sb here; an interactive bash login shell sends its
/// jobs a second one, which the repeat window (Task 13) is for.
#[test]
fn a_real_hangup_logs_the_ending_and_leaves_no_child() {
    if !root_gate::requested() {
        return;
    }
    assert_sighup_not_ignored();
    let world = World::new();
    let fake = FakeController::start().expect("fake");
    let program = common::sb().get_program().to_string_lossy().into_owned();
    let wrapper = world.file("run.sh");
    std::fs::write(
        &wrapper,
        "#!/bin/sh\ntrap : HUP\n\
         sh -c 'echo $$ > \"$1\"; exec \"$0\" docker start' \"$SB\" \"$DIR/sb.pid\"\n\
         echo \"EXIT=$?\" > \"$DIR/exit\"\n",
    )
    .expect("wrapper");
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    // The session leader is `script`'s non-interactive `sh`: the kernel
    // hangs it up, and as it dies of it the kernel sends SIGHUP once to
    // the foreground group, the wrapper's and sb's (a single hangup; an
    // interactive bash would also re-send one to its jobs). `; :` keeps
    // that shell from exec'ing the wrapper, which would make the wrapper
    // (which survives a hangup) the leader.
    let mut cmd = Command::new("script");
    world.env(&mut cmd, fake.url());
    cmd.args(["-qec", &format!("{}; :", wrapper.display()), "/dev/null"])
        .env("SHELL", "/bin/sh")
        .env("TERM", "xterm-256color")
        .env("SB", &program)
        .env("DIR", world.dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut script = KillOnDrop(cmd.spawn().expect("run script"), None);
    let sb_pid: i32 = wait_for(|| {
        std::fs::read_to_string(world.file("sb.pid"))
            .ok()
            .and_then(|text| text.trim().parse().ok())
    })
    .expect("sb's pid");
    let stand_in = world.stand_in();
    // The master closes with `script`.
    script.0.kill().expect("kill script");
    let _ = script.0.wait();
    let exit = wait_for(|| std::fs::read_to_string(world.file("exit")).ok())
        .expect("the wrapper's record of sb's end");
    assert_eq!(exit.trim(), "EXIT=129", "{}", world.log());
    assert!(gone(sb_pid));
    assert!(
        wait_for(|| gone(stand_in.0).then_some(())).is_some(),
        "the stand-in was left running"
    );
    let log = world.log();
    assert!(
        log.contains("run finished") && log.contains("outcome=\"interrupted (exit 129)\""),
        "{log}"
    );
}

/// A hangup under an interactive bash as the session leader comes twice:
/// bash re-sends SIGHUP to its jobs, then, as it dies, the kernel hangs up
/// the foreground group, well under a millisecond apart (the platform
/// note's probe). The copy falls in the repeat window (decision 9), so it
/// doesn't force the exit: the stand-in gets its SIGTERM and the grace
/// to finish (it takes 300 ms and then marks that it did, which a forced
/// exit's SIGKILL would stop), and the run log's ending is written. The
/// doubling decides the outcome in only part of the runs, so the test
/// hangs up several sessions.
#[test]
fn a_doubled_hangup_from_bash_still_takes_the_grace() {
    if !root_gate::requested() {
        return;
    }
    assert_sighup_not_ignored();
    let program = common::sb().get_program().to_string_lossy().into_owned();
    for run in 1..=5 {
        let world = World::with_stand_in(
            "trap 'sleep 0.3; : > \"$here/done\"; exit 0' TERM\n\
             i=0; while [ $i -lt 300 ]; do sleep 0.1; i=$((i+1)); done",
        );
        let fake = FakeController::start().expect("fake");
        let mut cmd = Command::new("script");
        world.env(&mut cmd, fake.url());
        // bash's history goes to the world, not root's home.
        cmd.args(["-qec", "bash --norc -i", "/dev/null"])
            .env("SHELL", "/bin/bash")
            .env("HISTFILE", world.file("history"))
            .env("TERM", "xterm-256color")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut script = KillOnDrop(cmd.spawn().expect("run script"), None);
        let line = format!(
            "sh -c 'echo $$ > \"$1\"; exec \"$0\" docker start' '{program}' '{}'\n",
            world.file("sb.pid").display()
        );
        let mut input = script.0.stdin.take().expect("script's input");
        std::io::Write::write_all(&mut input, line.as_bytes()).expect("type the command");
        let sb_pid: i32 = wait_for(|| {
            std::fs::read_to_string(world.file("sb.pid"))
                .ok()
                .and_then(|text| text.trim().parse().ok())
        })
        .expect("sb's pid");
        let stand_in = world.stand_in();
        // The master closes with `script`: bash is hung up.
        script.0.kill().expect("kill script");
        let _ = script.0.wait();
        drop(input);
        assert!(
            wait_for(|| gone(sb_pid).then_some(())).is_some(),
            "run {run}: sb still running"
        );
        let log = world.log();
        assert!(
            log.contains("run finished") && log.contains("outcome=\"interrupted (exit 129)\""),
            "run {run}: no ending in the run log\n{log}"
        );
        assert!(
            world.file("done").exists(),
            "run {run}: the stand-in didn't get its grace"
        );
        assert!(gone(stand_in.0), "run {run}: the stand-in was left running");
    }
}

/// A session scope stopped (a reboot during an SSH session, `loginctl
/// terminate-session`): systemd sends every process in it SIGTERM, then
/// SIGHUP (`SendSIGHUP=yes`, as logind's session scopes have), about a
/// millisecond apart. The SIGHUP falls in the first SIGTERM's repeat window
/// (decision 9), so it doesn't force the exit: the stand-in (which ignores
/// the SIGHUP it gets too) takes its 300 ms grace, the run log's ending is
/// written, and sb ends by the SIGTERM it read first.
#[test]
fn a_session_scope_stop_takes_the_grace() {
    if !root_gate::requested() {
        return;
    }
    // The gate's sandbox has no systemd to ask: its /run is empty
    // (scripts/check.sh, sandbox_argv, the only place that sets
    // SB_TEST_NO_SYSTEMD), and every test run of the gate is in it. This
    // test runs on a systemd of its own instead, in a container:
    // crates/sb/tests/hangup-systemd.sh, by hand.
    // Set anywhere systemd runs, it fails rather than skips:
    // /run/systemd/system is sd_booted(3)'s check,
    // https://www.freedesktop.org/software/systemd/man/latest/sd_booted.html
    let no_systemd = std::env::var_os("SB_TEST_NO_SYSTEMD");
    assert!(
        no_systemd.as_ref().is_none_or(|v| v == "1"),
        "SB_TEST_NO_SYSTEMD must be 1 or unset, got {no_systemd:?}"
    );
    if no_systemd.is_some() {
        assert!(
            !std::path::Path::new("/run/systemd/system").exists(),
            "SB_TEST_NO_SYSTEMD=1 where systemd is running"
        );
        eprintln!("not run: SB_TEST_NO_SYSTEMD=1, no systemd to start a scope");
        return;
    }
    assert_sighup_not_ignored();
    let world = World::with_stand_in(
        "trap '' HUP\ntrap 'sleep 0.3; : > \"$here/done\"; exit 0' TERM\n\
         i=0; while [ $i -lt 300 ]; do sleep 0.1; i=$((i+1)); done",
    );
    let fake = FakeController::start().expect("fake");
    let unit = format!("sb-test-hangup-{}", std::process::id());
    let program = common::sb().get_program().to_os_string();
    let mut cmd = Command::new("systemd-run");
    world.env(&mut cmd, fake.url());
    // A scope keeps the caller's environment, and sb is its process
    // (systemd-run execs it).
    cmd.args(["--scope", "-q", "--unit", &unit, "-p", "SendSIGHUP=yes"])
        .arg(program)
        .args(["docker", "start"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut sb = KillOnDrop(cmd.spawn().expect("run systemd-run"), None);
    let stand_in = world.stand_in();
    let stopped = Command::new("systemctl")
        .args(["stop", &format!("{unit}.scope")])
        .status()
        .expect("systemctl stop");
    assert!(stopped.success(), "{stopped:?}");
    // sb ends by the one it read first: SIGTERM, unless both were pending
    // when its signals thread read them, which hands them over in any
    // order (cli-core's accepted limits).
    let status = sb.0.wait().expect("wait");
    let code = match status.signal() {
        Some(signal) if signal == Signal::TERM.as_raw() => 143,
        Some(signal) if signal == Signal::HUP.as_raw() => 129,
        _ => panic!("sb didn't end by SIGTERM or SIGHUP: {status:?}"),
    };
    let log = world.log();
    assert!(
        log.contains("run finished")
            && log.contains(&format!("outcome=\"interrupted (exit {code})\"")),
        "no ending in the run log\n{log}"
    );
    assert!(
        world.file("done").exists(),
        "the stand-in didn't get its grace"
    );
    assert!(gone(stand_in.0), "the stand-in was left running");
}

/// Under `nohup` SIGHUP stays ignored (decision 8): a hangup doesn't
/// cancel the run; a SIGTERM then still does.
#[test]
fn under_nohup_a_hangup_does_not_cancel() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let fake = FakeController::start().expect("fake");
    let program = common::sb().get_program().to_os_string();
    let mut cmd = Command::new("nohup");
    world.env(&mut cmd, fake.url());
    cmd.arg(program)
        .args(["docker", "start"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // nohup execs sb: the same pid.
    let mut sb = KillOnDrop(cmd.spawn().expect("run nohup"), None);
    let stand_in = world.stand_in();
    signal(sb.0.id(), Signal::HUP);
    std::thread::sleep(Duration::from_millis(500));
    assert!(sb.0.try_wait().expect("try_wait").is_none(), "sb ended");
    assert!(!gone(stand_in.0), "the stand-in was stopped");
    signal(sb.0.id(), Signal::TERM);
    let status = sb.0.wait().expect("wait");
    assert_eq!(status.signal(), Some(Signal::TERM.as_raw()), "{status:?}");
    let log = world.log();
    assert!(log.contains("outcome=\"interrupted (exit 143)\""), "{log}");
}
