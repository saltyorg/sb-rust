//! An attached child under sb's real signal handling, through exec-demo
//! (exec plan Task 8b; spec "Attached children", decisions 2, 5, 7 and
//! 12). exec-demo runs in a process group of its own, its attached child
//! in that same group, as a shell's foreground job; the terminal's Ctrl-C
//! and Ctrl-\ are SIGINT and SIGQUIT to that group, which is what these
//! tests send (why not through a PTY: the kernel's delivery is the same,
//! and the test then needs no terminal; as `tests/demo_signals.rs`). The
//! child is exec-demo's own `--child` mode, not dumpable, so one that dies
//! of SIGQUIT writes no core.

use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process, kill_process_group, test_kill_process};

/// exec-demo, killed (with its group) if a test fails before it ends,
/// and its stderr, read on a thread: a child left running holds the pipe
/// open, which must fail a test, not hang it.
struct Demo(Child, Receiver<String>);

impl Drop for Demo {
    fn drop(&mut self) {
        let _ = kill_process_group(pid(self.0.id()), Signal::KILL);
        let _ = self.0.wait();
    }
}

fn pid(raw: u32) -> Pid {
    Pid::from_raw(i32::try_from(raw).expect("pid")).expect("pid")
}

/// exec-demo running `mode`'s child attached (`way`: `attached`, or
/// `passed` for decision 5's pass-through), in a group of its own.
fn start(file: &Path, mode: &str, grace_ms: u64, way: &str) -> Demo {
    let mut child = Command::new(env!("CARGO_BIN_EXE_exec-demo"))
        .arg(file)
        .args([mode, &grace_ms.to_string(), way])
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("exec-demo");
    let mut pipe = child.stderr.take().expect("stderr");
    let (sent, read) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut pipe, &mut text);
        let _ = sent.send(text);
    });
    Demo(child, read)
}

/// The child's pid, once it wrote it.
fn child_pid(file: &Path) -> Pid {
    let start = Instant::now();
    loop {
        if let Ok(text) = std::fs::read_to_string(file)
            && text.ends_with('\n')
        {
            let raw: u32 = text
                .split_whitespace()
                .next()
                .and_then(|p| p.parse().ok())
                .expect("pid");
            return pid(raw);
        }
        assert!(start.elapsed() < Duration::from_secs(10), "no pid file");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// What the terminal does for Ctrl-C or Ctrl-\: the signal to the
/// foreground group, exec-demo's and its attached child's.
fn press(demo: &Demo, signal: Signal) {
    kill_process_group(pid(demo.0.id()), signal).expect("group signal");
}

/// exec-demo's end, within 15 s, and its stderr.
fn end(demo: &mut Demo) -> (ExitStatus, String) {
    let start = Instant::now();
    loop {
        if let Some(status) = demo.0.try_wait().expect("try_wait") {
            // A child still holding the pipe gives no text: the test's own
            // checks then fail on what it shows.
            let stderr = demo
                .1
                .recv_timeout(Duration::from_secs(2))
                .unwrap_or_else(|_| "(stderr still held open)".to_string());
            return (status, stderr);
        }
        assert!(start.elapsed() < Duration::from_secs(15), "exec-demo hung");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Whether `child` still runs: it exists and isn't a zombie (an orphan
/// waits for pid 1, or a subreaper, to reap it). A `/proc` it can't read
/// (`hidepid`) fails the test rather than read as gone.
fn alive(child: Pid) -> bool {
    if test_kill_process(child).is_err() {
        return false;
    }
    let raw = child.as_raw_nonzero();
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{raw}/stat")) else {
        // Only if it went between the two looks: else `/proc` is hidden.
        assert!(
            test_kill_process(child).is_err(),
            "can't read /proc/{raw}/stat for a live pid"
        );
        return false;
    };
    let state = stat
        .rsplit(')')
        .next()
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_default();
    state != "Z"
}

/// Let a `survives` child exit (it waits for FILE.go).
fn release(file: &Path) {
    std::fs::write(format!("{}.go", file.display()), "").expect("release");
}

fn file() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("pids");
    (dir, file)
}

/// One Ctrl-C, or two, or a Ctrl-\, with a child that survives them: sb
/// waits (held signals don't count, and Ctrl-\ isn't sb's hard escape
/// while held), and the run ends with the child's 0 once it is released.
#[test]
fn held_signals_with_a_surviving_child_wait() {
    let cases: [&[Signal]; 3] = [&[Signal::INT], &[Signal::INT, Signal::INT], &[Signal::QUIT]];
    for presses in cases {
        let (_dir, file) = file();
        let mut demo = start(&file, "survives", 5000, "attached");
        child_pid(&file);
        for &signal in presses {
            press(&demo, signal);
            std::thread::sleep(Duration::from_millis(200));
        }
        assert!(
            demo.0.try_wait().expect("try_wait").is_none(),
            "{presses:?}: sb waits"
        );
        release(&file);
        let (status, stderr) = end(&mut demo);
        assert_eq!(status.code(), Some(0), "{presses:?}: {status:?} {stderr}");
    }
}

/// A child that dies of Ctrl-C: sb dies of SIGINT too.
#[test]
fn ctrl_c_a_child_dies_of_ends_sb_by_sigint() {
    let (_dir, file) = file();
    let mut demo = start(&file, "dies", 5000, "attached");
    child_pid(&file);
    press(&demo, Signal::INT);
    let (status, stderr) = end(&mut demo);
    assert_eq!(status.signal(), Some(2), "{status:?} {stderr}");
}

/// Ctrl-\ with a child that dies of it, and with one that ignores Ctrl-C
/// (it keeps running after a Ctrl-C, then ends on Ctrl-\): sb dies of
/// SIGQUIT, with no core. The no-core check only bites where cores are on
/// (an apport or systemd-coredump host, or a core limit above 0).
#[test]
fn ctrl_backslash_ends_sb_by_sigquit_without_a_core() {
    for (mode, ctrl_c_first) in [("dies", false), ("ignores-int", true)] {
        let (_dir, file) = file();
        let mut demo = start(&file, mode, 5000, "attached");
        let child = child_pid(&file);
        if ctrl_c_first {
            press(&demo, Signal::INT);
            std::thread::sleep(Duration::from_millis(300));
            assert!(alive(child), "{mode}: it survived Ctrl-C");
        }
        press(&demo, Signal::QUIT);
        let (status, stderr) = end(&mut demo);
        assert_eq!(status.signal(), Some(3), "{mode}: {status:?} {stderr}");
        assert!(!status.core_dumped(), "{mode}: {status:?}");
    }
}

/// A child signalled on its own (a signal sb never got) isn't sb's
/// interruption: the child's failure, exit 1, named on stderr.
#[test]
fn a_child_signalled_on_its_own_fails_the_run() {
    let (_dir, file) = file();
    let mut demo = start(&file, "self", 5000, "attached");
    let (status, stderr) = end(&mut demo);
    assert_eq!(status.code(), Some(1), "{status:?} {stderr}");
    assert!(stderr.contains("killed by SIGINT"), "{stderr}");
}

/// A SIGINT to sb alone isn't forwarded (only the terminal's reaches the
/// child); a SIGTERM to sb is, by pid, and sb then dies of SIGTERM.
#[test]
fn sigint_to_sb_alone_stays_and_sigterm_is_forwarded() {
    let (_dir, file) = file();
    let mut demo = start(&file, "dies", 5000, "attached");
    let child = child_pid(&file);
    kill_process(pid(demo.0.id()), Signal::INT).expect("SIGINT");
    std::thread::sleep(Duration::from_millis(300));
    assert!(alive(child), "not forwarded");
    assert!(demo.0.try_wait().expect("try_wait").is_none(), "sb waits");
    kill_process(pid(demo.0.id()), Signal::TERM).expect("SIGTERM");
    let (status, stderr) = end(&mut demo);
    assert_eq!(status.signal(), Some(15), "{status:?} {stderr}");
    assert!(!alive(child), "the child is gone");
}

/// A child ignoring the forwarded SIGTERM is `SIGKILLed` after the grace; a
/// Ctrl-C and a Ctrl-\ during that grace aren't counted (the child catches
/// both), so sb waits it out rather than force its exit.
#[test]
fn a_child_ignoring_sigterm_is_killed_after_the_grace() {
    let grace = Duration::from_secs(4);
    let (_dir, file) = file();
    let mut demo = start(&file, "ignores-term", 4000, "attached");
    let child = child_pid(&file);
    let sent = Instant::now();
    kill_process(pid(demo.0.id()), Signal::TERM).expect("SIGTERM");
    std::thread::sleep(Duration::from_millis(200));
    press(&demo, Signal::INT);
    std::thread::sleep(Duration::from_millis(100));
    press(&demo, Signal::QUIT);
    assert!(
        sent.elapsed() < grace.checked_sub(Duration::from_secs(1)).unwrap(),
        "pressed within the grace"
    );
    let (status, stderr) = end(&mut demo);
    assert_eq!(status.signal(), Some(15), "{status:?} {stderr}");
    assert!(
        sent.elapsed() >= grace.checked_sub(Duration::from_millis(100)).unwrap(),
        "waited for the grace"
    );
    assert!(!alive(child), "SIGKILLed");
}

/// A second SIGTERM forces the exit at once (decision 12): the child gets
/// SIGKILL and sb dies of SIGTERM, long before the grace ends.
#[test]
fn a_second_sigterm_forces_the_exit_and_kills_the_child() {
    let (_dir, file) = file();
    let mut demo = start(&file, "ignores-term", 60_000, "attached");
    let child = child_pid(&file);
    let sent = Instant::now();
    kill_process(pid(demo.0.id()), Signal::TERM).expect("SIGTERM");
    // Well past cli-core's 250 ms repeat window, which runs from when sb
    // read the first: sooner, the same signal is a copy of the first, not
    // a second one (decision 9). The child ignores SIGTERM, so there's no
    // effect of the first to wait for; a second leaves room for a loaded
    // host.
    std::thread::sleep(Duration::from_secs(1));
    kill_process(pid(demo.0.id()), Signal::TERM).expect("SIGTERM");
    let (status, stderr) = end(&mut demo);
    assert_eq!(status.signal(), Some(15), "{status:?} {stderr}");
    assert!(
        sent.elapsed() < Duration::from_secs(5),
        "{:?}",
        sent.elapsed()
    );
    let gone = Instant::now();
    while alive(child) {
        assert!(
            gone.elapsed() < Duration::from_secs(5),
            "the child was left"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// An interrupted pass-through run writes no line of its own: sb ends by
/// the signal (cli-core's block), not with a code.
#[test]
fn an_interrupted_pass_through_ends_by_the_signal() {
    let (_dir, file) = file();
    let mut demo = start(&file, "dies", 5000, "passed");
    child_pid(&file);
    press(&demo, Signal::INT);
    let (status, stderr) = end(&mut demo);
    assert_eq!(status.signal(), Some(2), "{status:?} {stderr}");
    assert!(!stderr.contains("exited with"), "{stderr}");
}

/// Decision 5's pass-through: exec-demo exits with the child's own code,
/// after one line naming the program and its outcome.
#[test]
fn the_pass_through_exits_with_the_childs_code() {
    let (_dir, file) = file();
    let mut demo = start(&file, "fails", 5000, "passed");
    let (status, stderr) = end(&mut demo);
    assert_eq!(status.code(), Some(3), "{status:?} {stderr}");
    let lines: Vec<&str> = stderr.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "{stderr}");
    assert!(lines[0].contains("exited with code 3"), "{stderr}");
    let (_dir, file) = self::file();
    let mut demo = start(&file, "exits", 5000, "passed");
    let (status, stderr) = end(&mut demo);
    assert_eq!(status.code(), Some(0), "{status:?} {stderr}");
    assert!(stderr.trim().is_empty(), "{stderr}");
}
