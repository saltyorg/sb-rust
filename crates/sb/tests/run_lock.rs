//! The run lock (cli-core spec, "Run lock"): commands that change the host
//! run one at a time. Each test shares one lock directory
//! (`SB_TEST_RUN_LOCK_DIR`) between its runs of sb, `sb install
//! --no-cache` with a stand-in playbook that holds until the test releases
//! it, or runs a nested sb. Root runs (`SB_TEST_ROOT=1`): the locking
//! commands need root. Always this test build, never `SB_TEST_BINARY`'s:
//! only a build with cli-core's `testing` feature takes the lock in the
//! test's directory; any other takes the host's `/run/sb`.

mod common;
mod root_gate;

use std::fs::File;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process};

const PLAYBOOK: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/run-lock/fake-playbook"
);

/// `systemd-detect-virt -c` outside a container (install's LXC check).
const NOT_LXC: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/detect-virt/none"
);

/// A test's directory: the Saltbox root, the run logs, the shared lock
/// directory, and each run's output files.
struct World {
    dir: tempfile::TempDir,
}

/// One run of sb, killed if the test fails before it ends.
struct Run {
    child: Option<Child>,
    stdout: PathBuf,
    stderr: PathBuf,
}

impl Drop for Run {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Run {
    fn pid(&self) -> u32 {
        self.child.as_ref().expect("running").id()
    }

    fn signal(&self, signal: Signal) {
        let pid = i32::try_from(self.pid()).ok().and_then(Pid::from_raw);
        kill_process(pid.expect("pid"), signal).expect("signal");
    }

    fn stderr(&self) -> String {
        std::fs::read_to_string(&self.stderr).unwrap_or_default()
    }

    /// Its end within 20 s, with what it printed.
    fn end(mut self) -> (ExitStatus, String, String) {
        let mut child = self.child.take().expect("running");
        let deadline = Instant::now() + Duration::from_secs(20);
        let status = loop {
            if let Some(status) = child.try_wait().expect("wait") {
                break status;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("sb didn't end: {}", self.stderr());
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let stdout = std::fs::read_to_string(&self.stdout).unwrap_or_default();
        (status, stdout, self.stderr())
    }
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("dir");
        std::fs::create_dir_all(dir.path().join("saltbox")).expect("root");
        Self { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// `sb <args>` on this test build, its playbook (when it gets that
    /// far) named `name` and in `mode`.
    fn start(&self, name: &str, mode: &str, args: &[&str]) -> Run {
        let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("sb"));
        common::apply_sb_env(&mut cmd);
        self.run(cmd, name, mode, args)
    }

    /// `sb <args>` on `common::sb()`'s build, `SB_TEST_BINARY`'s too: a
    /// command that never takes the lock takes it in no build.
    fn start_reader(&self, args: &[&str]) -> Run {
        self.run(common::sb(), "reader", "", args)
    }

    fn run(&self, mut cmd: std::process::Command, name: &str, mode: &str, args: &[&str]) -> Run {
        let stdout = self.path(&format!("stdout.{name}"));
        let stderr = self.path(&format!("stderr.{name}"));
        let sb = assert_cmd::cargo::cargo_bin("sb");
        cmd.args(args)
            .env("PATH", {
                let own = std::env::var_os("PATH").unwrap_or_default();
                std::env::join_paths(
                    std::iter::once(PathBuf::from(NOT_LXC)).chain(std::env::split_paths(&own)),
                )
                .expect("a PATH")
            })
            .env("SB_SALTBOX_DIR", self.path("saltbox"))
            .env("SB_LOG_DIR", self.path("logs"))
            .env("SB_ANSIBLE_PLAYBOOK", PLAYBOOK)
            .env("SB_TEST_RUN_LOCK_DIR", self.path("lock"))
            .env("RUN_LOCK_TEST_DIR", self.dir.path())
            .env("RUN_LOCK_NAME", name)
            .env("RUN_LOCK_MODE", mode)
            .env("RUN_LOCK_SB", sb)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(File::create(&stdout).expect("stdout"))
            .stderr(File::create(&stderr).expect("stderr"));
        Run {
            child: Some(cmd.spawn().expect("run sb")),
            stdout,
            stderr,
        }
    }

    /// `sb <args>` on this test build, with git kept from looking above
    /// the test's directory (`GIT_CEILING_DIRECTORIES`), so the Saltbox
    /// root is no repository at all.
    fn start_in_ceiling(&self, name: &str, args: &[&str]) -> Run {
        let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("sb"));
        common::apply_sb_env(&mut cmd);
        cmd.env("GIT_CEILING_DIRECTORIES", self.dir.path());
        self.run(cmd, name, "", args)
    }

    /// `sb install --no-cache <name>`, a locking command.
    fn install(&self, name: &str, mode: &str) -> Run {
        self.start(name, mode, &["install", "--no-cache", name])
    }

    /// Whether `name`'s playbook ran.
    fn ran(&self, name: &str) -> bool {
        self.path(&format!("ran.{name}")).exists()
    }

    /// Lets the holding playbooks end.
    fn release(&self) {
        std::fs::write(self.path("release"), "").expect("release");
    }

    /// Every run log line.
    fn logged(&self) -> String {
        let mut text = String::new();
        if let Ok(entries) = std::fs::read_dir(self.path("logs")) {
            for entry in entries.flatten() {
                text.push_str(&std::fs::read_to_string(entry.path()).unwrap_or_default());
            }
        }
        text
    }
}

/// Waits up to 10 s for `done`.
fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The line a waiting sb prints for a holder `sb install` with `pid`.
fn waiting(pid: u32) -> String {
    format!("another sb command is running (sb install, pid {pid}); waiting… (Ctrl-C to give up)\n")
}

/// A holder's playbook is running: a second locking command prints the
/// line once and waits, and runs once the holder is done, logging how long
/// it waited.
#[test]
fn a_second_locking_command_waits_then_runs() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let holder = world.install("first", "hold");
    wait_for("the holder's playbook", || world.ran("first"));
    let second = world.install("second", "");
    let holder_pid = holder.pid();
    wait_for("the waiting line", || {
        second.stderr() == waiting(holder_pid)
    });
    std::thread::sleep(Duration::from_millis(300));
    assert!(!world.ran("second"), "the second ran under the holder");
    world.release();
    let (status, _, stderr) = holder.end();
    assert!(status.success(), "{stderr}");
    let (status, stdout, stderr) = second.end();
    assert!(status.success(), "{stdout}\n{stderr}");
    assert!(world.ran("second"));
    assert_eq!(stderr, waiting(holder_pid), "printed once");
    let logged = world.logged();
    assert!(logged.contains("run lock taken"), "{logged}");
    assert!(logged.contains("waited_ms="), "{logged}");
}

/// A command that never takes the lock runs while it is held.
#[test]
fn a_read_only_command_is_never_blocked() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let holder = world.install("first", "hold");
    wait_for("the holder's playbook", || world.ran("first"));
    // validate-config fails, as the world has no Saltbox configuration
    // (1); version and diag succeed (0): none of them waits.
    for (args, code) in [
        (&["validate-config"][..], 1),
        (&["version"], 0),
        (&["diag"], 0),
    ] {
        let (status, stdout, stderr) = world.start_reader(args).end();
        assert!(
            !stderr.contains("another sb command is running"),
            "{args:?}: {stdout}\n{stderr}"
        );
        assert_eq!(status.code(), Some(code), "{args:?}: {stdout}\n{stderr}");
    }
    world.release();
    let (status, _, stderr) = holder.end();
    assert!(status.success(), "{stderr}");
}

/// `announcements preview` takes the lock only when it runs its
/// migrations (`--run-migrations`), as update's migrations change the
/// host; the dry preview never waits.
#[test]
fn a_preview_waits_only_when_it_runs_migrations() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let holder = world.install("first", "hold");
    wait_for("the holder's playbook", || world.ran("first"));
    let holder_pid = holder.pid();
    // Saltbox's checkout only (never the host's Sandbox one), and git
    // stopped at the test's directory, so no repository above it counts.
    let preview = |name: &str, extra: &[&str]| {
        let mut args = vec!["announcements", "preview", "--repo", "saltbox"];
        args.extend_from_slice(extra);
        world.start_in_ceiling(name, &args)
    };
    let (_, stdout, stderr) = preview("dry", &[]).end();
    assert!(
        !stderr.contains("another sb command is running"),
        "{stdout}\n{stderr}"
    );
    let running = preview("running", &["--run-migrations"]);
    wait_for("the waiting line", || {
        running.stderr() == waiting(holder_pid)
    });
    world.release();
    let (status, _, stderr) = holder.end();
    assert!(status.success(), "{stderr}");
    let (_, _, stderr) = running.end();
    assert!(stderr.starts_with(&waiting(holder_pid)), "{stderr}");
    // It took the lock once the holder was done.
    let logged = world.logged();
    assert_eq!(logged.matches("run lock taken").count(), 2, "{logged}");
    assert!(
        logged.contains("command=\"announcements preview\""),
        "{logged}"
    );
}

/// A stand-in for Saltbox's backup in the test's Saltbox root: its lock
/// (`<root>/backup.lock`, as the backup roles touch it), made `age` ago,
/// and, when `running`, a root process named and given arguments as the
/// cron job's (`ansible-playbook <root>/backup.yml`): a shell under that
/// name, blocked in `read` on a pipe the test holds, so it starts no child
/// to leave behind, killed on drop.
struct Backup {
    lock: PathBuf,
    playbook: Option<Child>,
}

impl Backup {
    fn new(world: &World, age: Duration, running: bool) -> Self {
        let lock = world.path("saltbox/backup.lock");
        File::create(&lock)
            .expect("backup.lock")
            .set_modified(std::time::SystemTime::now() - age)
            .expect("mtime");
        let playbook = running.then(|| {
            std::process::Command::new("sh")
                .arg0("ansible-playbook")
                .args(["-c", "read -r line", "sh"])
                .arg(world.path("saltbox/backup.yml"))
                .stdin(std::process::Stdio::piped())
                .spawn()
                .expect("stand-in playbook")
        });
        Self { lock, playbook }
    }

    /// The backup dies outright, its lock left behind.
    fn kill(&mut self) {
        if let Some(mut child) = self.playbook.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Backup {
    fn drop(&mut self) {
        self.kill();
    }
}

/// The backup line's shape: its start is the lock's mtime, local.
fn backup_waiting(stderr: &str) -> bool {
    stderr.starts_with("a Saltbox backup is running (since ")
        && stderr.ends_with("); waiting… (Ctrl-C to give up)\n")
}

/// While Saltbox's backup runs (its lock there and its playbook alive,
/// however old the lock: three hours here, past Saltbox's own age
/// limits), a locking command prints one line and waits; it runs once the
/// backup removes its lock, and the run log records the wait.
#[test]
fn a_locking_command_waits_for_a_running_saltbox_backup() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let backup = Backup::new(&world, Duration::from_hours(3), true);
    let run = world.install("first", "");
    wait_for("the backup line", || backup_waiting(&run.stderr()));
    std::thread::sleep(Duration::from_millis(300));
    assert!(!world.ran("first"), "it ran under the backup");
    std::fs::remove_file(&backup.lock).expect("backup done");
    let (status, stdout, stderr) = run.end();
    assert!(status.success(), "{stdout}\n{stderr}");
    assert!(world.ran("first"));
    assert_eq!(
        stderr.matches("a Saltbox backup is running").count(),
        1,
        "{stderr}"
    );
    let logged = world.logged();
    assert!(logged.contains("waited for a Saltbox backup"), "{logged}");
}

/// A backup that dies outright leaves its lock: once its playbook is gone
/// the wait ends, and sb leaves Saltbox's file in place.
#[test]
fn a_dead_backups_lock_ends_the_wait() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let mut backup = Backup::new(&world, Duration::from_secs(60), true);
    let run = world.install("first", "");
    wait_for("the backup line", || backup_waiting(&run.stderr()));
    backup.kill();
    let (status, stdout, stderr) = run.end();
    assert!(status.success(), "{stdout}\n{stderr}");
    assert!(world.ran("first"));
    assert!(backup.lock.exists(), "sb removed Saltbox's file");
}

/// A lock with no backup playbook running holds nothing, however young.
#[test]
fn a_lock_without_a_running_backup_is_not_waited_for() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let backup = Backup::new(&world, Duration::ZERO, false);
    let (status, stdout, stderr) = world.install("first", "").end();
    assert!(status.success(), "{stdout}\n{stderr}");
    assert!(!stderr.contains("Saltbox backup"), "{stderr}");
    assert!(backup.lock.exists(), "sb removed Saltbox's file");
}

/// An sb started under a locked sb doesn't wait for a backup that began
/// after its parent's own wait: the parent's lock is its, waited for
/// already.
#[test]
fn a_nested_sb_doesnt_wait_for_a_backup() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let outer = world.install("outer", "nested-after");
    wait_for("the outer playbook", || world.ran("outer"));
    let _backup = Backup::new(&world, Duration::from_secs(60), true);
    std::fs::write(world.path("go"), "").expect("go");
    let (status, stdout, stderr) = outer.end();
    assert!(status.success(), "{stdout}\n{stderr}");
    assert!(world.ran("inner"));
    assert!(!stderr.contains("Saltbox backup"), "{stderr}");
    let logged = world.logged();
    assert!(!logged.contains("waited for a Saltbox backup"), "{logged}");
}

/// Commands that never take the run lock don't wait for a backup either.
#[test]
fn a_read_only_command_doesnt_wait_for_a_backup() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let _backup = Backup::new(&world, Duration::from_secs(60), true);
    // validate-config fails, as the world has no Saltbox configuration
    // (1); version and diag succeed (0): none of them waits.
    for (args, code) in [
        (&["validate-config"][..], 1),
        (&["version"], 0),
        (&["diag"], 0),
    ] {
        let (status, stdout, stderr) = world.start_reader(args).end();
        assert!(
            !stderr.contains("Saltbox backup"),
            "{args:?}: {stdout}\n{stderr}"
        );
        assert_eq!(status.code(), Some(code), "{args:?}: {stdout}\n{stderr}");
    }
}

/// Ctrl-C while waiting for a backup ends sb by SIGINT, before its
/// command has run; the backup's file is left as it was.
#[test]
fn ctrl_c_while_waiting_for_a_backup_ends_by_sigint() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let backup = Backup::new(&world, Duration::from_secs(60), true);
    let run = world.install("first", "");
    wait_for("the backup line", || backup_waiting(&run.stderr()));
    run.signal(Signal::INT);
    let (status, _, stderr) = run.end();
    assert_eq!(status.signal(), Some(2), "{status:?} {stderr}");
    assert!(!world.ran("first"));
    assert!(backup.lock.exists());
}

/// Ctrl-C while waiting ends sb by SIGINT (`$?` 130), before its command
/// has run, and the holder keeps the lock.
#[test]
fn ctrl_c_while_waiting_ends_by_sigint_with_nothing_run() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let holder = world.install("first", "hold");
    wait_for("the holder's playbook", || world.ran("first"));
    let second = world.install("second", "");
    let holder_pid = holder.pid();
    wait_for("the waiting line", || {
        second.stderr() == waiting(holder_pid)
    });
    second.signal(Signal::INT);
    let (status, _, stderr) = second.end();
    assert_eq!(status.signal(), Some(2), "{status:?} {stderr}");
    assert!(!world.ran("second"));
    let record = std::fs::read_to_string(world.path("lock/run.lock")).expect("record");
    assert_eq!(record, format!("{holder_pid}\nsb install\n"));
    world.release();
    let (status, _, stderr) = holder.end();
    assert!(status.success(), "{stderr}");
}

/// A holder killed outright (SIGKILL, no cleanup) leaves no lock: the
/// waiting sb runs.
#[test]
fn a_killed_holder_releases_the_lock() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let holder = world.install("first", "hold");
    wait_for("the holder's playbook", || world.ran("first"));
    let second = world.install("second", "");
    let holder_pid = holder.pid();
    wait_for("the waiting line", || {
        second.stderr() == waiting(holder_pid)
    });
    holder.signal(Signal::KILL);
    let (status, stdout, stderr) = second.end();
    assert!(status.success(), "{stdout}\n{stderr}");
    assert!(world.ran("second"));
    // The killed holder's playbook, left behind, is stopped here.
    let orphan = std::fs::read_to_string(world.path("ran.first")).expect("pid");
    let orphan = orphan.trim().parse::<i32>().ok().and_then(Pid::from_raw);
    let _ = kill_process(orphan.expect("pid"), Signal::KILL);
    drop(holder);
}

/// An sb started under a locked sb (here by its playbook) runs under its
/// parent's lock instead of waiting for it.
#[test]
fn a_nested_sb_runs_under_its_parents_lock() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let (status, stdout, stderr) = world.install("outer", "nested").end();
    assert!(status.success(), "{stdout}\n{stderr}");
    assert!(world.ran("inner"), "{stdout}\n{stderr}");
    assert!(
        !stderr.contains("another sb command is running"),
        "{stderr}"
    );
    let logged = world.logged();
    assert!(logged.contains("run lock held by an ancestor"), "{logged}");
}

/// The lock's directory is made 0700 and its file 0600.
#[test]
fn the_lock_is_root_only() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let (status, _, stderr) = world.install("only", "").end();
    assert!(status.success(), "{stderr}");
    let mode = |path: &Path| std::fs::metadata(path).expect("meta").permissions().mode() & 0o777;
    assert_eq!(mode(&world.path("lock")), 0o700);
    assert_eq!(mode(&world.path("lock/run.lock")), 0o600);
}
