//! `sb diag` and `sb install` interrupted (ansible spec, steps 7 and 8,
//! success criterion 4; module build M30, M31): Ctrl-C while the fake
//! playbook runs attached.
//! sb runs in a process group of its own, as a shell's foreground job, and
//! the terminal's Ctrl-C is SIGINT to that group, which is what these
//! tests send, as exec's own attached tests do (why not through a PTY: the
//! kernel's delivery to the group is the same, and the test then needs no
//! terminal; `crates/exec/tests/demo_attached.rs`). Root runs
//! (`SB_TEST_ROOT=1`).

mod common;
mod root_gate;

use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::Path;
use std::process::{Child, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process, kill_process_group};

const FAKE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../ansible/tests/fixtures/fake-ansible-playbook"
);

/// `systemd-detect-virt -c` outside a container.
const NOT_LXC: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/detect-virt/none"
);

/// sb's group, killed with SIGKILL if a test fails before sb ends (the fake is in
/// it too).
struct Group {
    pid: Pid,
    child: Option<Child>,
}

impl Drop for Group {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = kill_process_group(self.pid, Signal::KILL);
            let _ = child.wait();
        }
    }
}

fn pid(raw: u32) -> Option<Pid> {
    i32::try_from(raw).ok().and_then(Pid::from_raw)
}

/// `sb <args>` in a group of its own with the fake in `mode`, waiting until
/// the fake has written its pid.
fn start(dir: &Path, mode: &str, args: &[&str]) -> Group {
    start_with(dir, mode, args, None)
}

/// [`start`], with the fake's exit code (also its code after a SIGTERM).
fn start_with(dir: &Path, mode: &str, args: &[&str], exit: Option<&str>) -> Group {
    let root = dir.join("saltbox");
    std::fs::create_dir_all(root.join("inventories/host_vars")).expect("root");
    let appdata = dir.join("appdata");
    std::fs::create_dir_all(appdata.join("sandbox")).expect("sandbox");
    std::fs::write(
        root.join("inventories/host_vars/localhost.yml"),
        format!("server_appdata_path: {}\n", appdata.display()),
    )
    .expect("inventory");
    let logs = dir.join("logs");
    std::fs::create_dir_all(&logs).expect("logs");
    let mut cmd = common::sb();
    cmd.args(args)
        // install's LXC check: not in a container.
        .env("PATH", {
            let own = std::env::var_os("PATH").unwrap_or_default();
            std::env::join_paths(
                std::iter::once(std::path::PathBuf::from(NOT_LXC))
                    .chain(std::env::split_paths(&own)),
            )
            .expect("a PATH")
        })
        .env("SB_SALTBOX_DIR", &root)
        .env("SB_LOG_DIR", &logs)
        .env("SB_ANSIBLE_PLAYBOOK", FAKE)
        .env("FAKE_ANSIBLE_LOG", dir.join("argv"))
        .env("FAKE_ANSIBLE_MODE", mode)
        .env("FAKE_ANSIBLE_EXIT", exit.unwrap_or_default())
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().expect("run sb");
    let group = Group {
        pid: pid(child.id()).expect("pid"),
        child: Some(child),
    };
    let pid_file = dir.join("argv.pid");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !pid_file.exists() {
        assert!(Instant::now() < deadline, "the fake didn't start");
        std::thread::sleep(Duration::from_millis(10));
    }
    group
}

/// sb's end within 15 s, with what it printed (read on a thread, so a
/// hung sb fails the test instead of hanging it). Why not `assert_cmd`'s
/// `timeout`: it waits inside `output()`, so no signal can be sent while
/// sb runs, and on a timeout it kills sb alone, not the group the fake is
/// in (as `crates/exec/tests/demo_attached.rs` waits).
fn end(mut group: Group) -> (ExitStatus, String, String) {
    let child = group.child.take().expect("running");
    let (sent, read) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sent.send(child.wait_with_output());
    });
    let Ok(output) = read.recv_timeout(Duration::from_secs(15)) else {
        let _ = kill_process_group(group.pid, Signal::KILL);
        panic!("sb didn't end");
    };
    let output = output.expect("sb's output");
    (
        output.status,
        String::from_utf8_lossy(&output.stdout).into_owned(),
        console::strip_ansi_codes(&String::from_utf8_lossy(&output.stderr)).into_owned(),
    )
}

fn press_ctrl_c(group: &Group) {
    kill_process_group(group.pid, Signal::INT).expect("SIGINT");
}

/// Ctrl-C with Ansible's own handling (it prints "User interrupted
/// execution" and exits 99): sb waits for it, passes 99 through with
/// exec's block and `Ansible was interrupted.`, and doesn't end by SIGINT.
#[test]
fn ctrl_c_passes_ansibles_99_through() {
    if !root_gate::requested() {
        return;
    }
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("dir");
    let group = start(dir.path(), "interrupt", &["diag"]);
    press_ctrl_c(&group);
    let (status, stdout, stderr) = end(group);
    assert_eq!(status.code(), Some(99), "{status:?} {stderr}");
    assert!(stderr.contains("User interrupted execution"), "{stderr}");
    assert!(stderr.contains("exited with code 99"), "{stderr}");
    assert!(
        stderr.trim_end().ends_with("Ansible was interrupted."),
        "{stderr}"
    );
    assert!(!stdout.contains("executed successfully"), "{stdout}");
}

/// Ctrl-C with a playbook that dies of it: sb ends by SIGINT (130 to a
/// shell), with cli-core's interruption and no hint.
#[test]
fn ctrl_c_a_playbook_dies_of_ends_sb_by_sigint() {
    if !root_gate::requested() {
        return;
    }
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("dir");
    let group = start(dir.path(), "wait", &["diag"]);
    press_ctrl_c(&group);
    let (status, _, stderr) = end(group);
    assert_eq!(
        status.signal(),
        Some(Signal::INT.as_raw()),
        "{status:?} {stderr}"
    );
    assert!(stderr.contains("interrupted"), "{stderr}");
    assert!(!stderr.contains("Scroll up"), "{stderr}");
    let log = std::fs::read_to_string(dir.path().join("logs/sb.log")).unwrap_or_default();
    assert!(log.contains(r#"outcome="interrupted (exit 130)""#), "{log}");
}

/// A SIGTERM to sb alone reaches the playbook as a SIGTERM; sb then ends
/// by its signal (143 to a shell).
#[test]
fn a_sigterm_to_sb_reaches_the_playbook() {
    if !root_gate::requested() {
        return;
    }
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("dir");
    let group = start(dir.path(), "wait", &["diag"]);
    kill_process(group.pid, Signal::TERM).expect("SIGTERM");
    let (status, _, stderr) = end(group);
    assert_eq!(
        status.signal(),
        Some(Signal::TERM.as_raw()),
        "{status:?} {stderr}"
    );
    assert!(
        dir.path().join("argv.term").exists(),
        "the playbook got SIGTERM"
    );
}

/// Ctrl-C during `sb install`'s first playbook: Ansible's 99 passes
/// through and the later playbooks don't run (spec, step 7).
#[test]
fn ctrl_c_during_install_stops_the_later_playbooks() {
    if !root_gate::requested() {
        return;
    }
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("dir");
    let group = start(dir.path(), "interrupt", &["install", "plex", "sandbox-b"]);
    press_ctrl_c(&group);
    let (status, _, stderr) = end(group);
    assert_eq!(status.code(), Some(99), "{status:?} {stderr}");
    assert!(
        stderr.trim_end().ends_with("Ansible was interrupted."),
        "{stderr}"
    );
    assert!(dir.path().join("argv.0").exists(), "Saltbox ran");
    assert!(!dir.path().join("argv.1").exists(), "Sandbox didn't");
}

/// A SIGTERM to sb that the playbook survives with exit 0: exec counts the
/// run as interrupted whatever its code, so sb runs no later playbook,
/// prints no `Executing...` line for it, and ends by its signal.
#[test]
fn a_sigterm_between_playbooks_runs_no_more() {
    if !root_gate::requested() {
        return;
    }
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("dir");
    let group = start_with(
        dir.path(),
        "wait",
        &["install", "plex", "sandbox-b"],
        Some("0"),
    );
    kill_process(group.pid, Signal::TERM).expect("SIGTERM");
    let (status, stdout, stderr) = end(group);
    assert_eq!(
        status.signal(),
        Some(Signal::TERM.as_raw()),
        "{status:?} {stderr}"
    );
    assert!(
        dir.path().join("argv.term").exists(),
        "the playbook got SIGTERM"
    );
    assert!(!dir.path().join("argv.1").exists(), "Sandbox didn't run");
    assert_eq!(
        stdout.matches("Executing Ansible playbook").count(),
        1,
        "{stdout}"
    );
}
