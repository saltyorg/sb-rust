//! A piped child under sb's own signal handling, for exec's real-signal
//! tests (exec plan Task 4): cli-core's `Shutdown` listens for SIGINT and
//! SIGTERM, exec's forced-exit hook is registered as sb's `main` will, and
//! one `sh` child (with a background grandchild) is captured with the
//! shutdown token; the run ends through cli-core's `finish_run`, as sb's
//! does. Not part of sb.
//!
//! `exec-demo FILE MODE GRACE_MS`: the child writes "leader grandchild"
//! pids to FILE (the leader's pid is the group's id). MODE `stops` (both
//! end on SIGTERM), `ignores` (both ignore it, so only a SIGKILL ends
//! them), `exits` (it ends on its own) or `fails` (it exits 3); a fourth
//! argument `pty` runs it on a PTY in a term-ui step instead of captured;
//! the outcome is checked, so a failed child fails the run.
//!
//! `attached` or `passed` as the fourth argument runs an attached child
//! instead (Task 8b): exec-demo itself as `--child MODE FILE` (see
//! [`attached_child`] for its modes), its outcome checked (`attached`) or
//! its code passed through (`passed`, decision 5).

use std::error::Error;
use std::time::Duration;

use sb_cli_core::cancel::Shutdown;
use sb_cli_core::exit::{RunEnd, finish_run};
use sb_exec::Cmd;
use sb_term_ui::{Labels, Options, Runner};

/// How exec-demo runs its child.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Way {
    Captured,
    Pty,
    /// Attached, its outcome checked (a failure ends exec-demo with 1).
    Attached,
    /// Attached, its own code passed through (decision 5).
    Passed,
}

fn main() -> RunEnd {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [child, mode, file] = &args[..]
        && child == "--child"
    {
        return attached_child(mode, file);
    }
    let usage = || {
        eprintln!(
            "usage: exec-demo FILE MODE GRACE_MS [pty|attached|passed]\n       exec-demo --child MODE FILE"
        );
        RunEnd::Code(2.into())
    };
    let (file, mode, grace, way) = match &args[..] {
        [file, mode, grace] => (file, mode, grace, Way::Captured),
        [file, mode, grace, way] => match way.as_str() {
            "pty" => (file, mode, grace, Way::Pty),
            "attached" => (file, mode, grace, Way::Attached),
            "passed" => (file, mode, grace, Way::Passed),
            _ => return usage(),
        },
        _ => return usage(),
    };
    let Ok(grace) = grace.parse() else {
        return usage();
    };
    let script = match mode.as_str() {
        "stops" => "sleep 30 & echo $$ $! > \"$1\"; wait",
        "ignores" => "trap '' TERM; (exec sleep 30) & echo $$ $! > \"$1\"; wait",
        "fails" => "true & echo $$ $! > \"$1\"; wait; exit 3",
        _ => "true & echo $$ $! > \"$1\"; wait",
    };
    let grace = Duration::from_millis(grace);
    let shutdown = Shutdown::new();
    let _hook = sb_exec::on_forced_exit(&shutdown);
    // Built by hand rather than with `#[tokio::main]`, so a failure to build
    // it ends through `finish_run` as sb's own does, not in a panic.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let cmd = if matches!(way, Way::Attached | Way::Passed) {
        // exec-demo itself, as `--child MODE FILE` (see `attached_child`).
        let exe = std::env::current_exe().unwrap_or_default();
        Cmd::new(exe)
            .arg("--child")
            .value(mode.as_str())
            .value(file.as_str())
            .grace(grace)
    } else {
        Cmd::new("sh")
            .arg("-c")
            .arg(script)
            .arg("sh")
            .value(file.as_str())
            .grace(grace)
    };
    let result: Result<(), Box<dyn Error>> = match (shutdown.listen_for_signals(), runtime) {
        (Ok(()), Ok(runtime)) if way == Way::Pty => runtime.block_on(on_a_pty(&cmd, &shutdown)),
        (Ok(()), Ok(runtime)) if way == Way::Attached => runtime
            .block_on(cmd.attached(&shutdown))
            .map_err(Box::from)
            .and_then(|ran| ran.check().map(drop).map_err(Box::from)),
        (Ok(()), Ok(runtime)) if way == Way::Passed => runtime
            .block_on(cmd.attached(&shutdown))
            .map_err(Box::from)
            .and_then(|ran| {
                ran.pass_through(&mut anstream::stderr())
                    .map(drop)
                    .map_err(Box::from)
            }),
        (Ok(()), Ok(runtime)) => runtime
            .block_on(cmd.capture(&shutdown.token()))
            .map_err(Box::from)
            .and_then(|captured| captured.check().map(drop).map_err(Box::from)),
        (Err(err), _) | (_, Err(err)) => Err(err.into()),
    };
    let error = result
        .as_ref()
        .err()
        .map(|e| e.as_ref() as &(dyn Error + 'static));
    finish_run(&shutdown.snapshot(), error, None, &mut std::io::stderr())
}

/// The attached child (`--child MODE FILE`): exec-demo itself, made not
/// dumpable so a death by SIGQUIT writes no core, its pid written twice to
/// FILE (the tests read "leader grandchild"). MODE:
/// - `dies`: the default dispositions (SIGINT, SIGQUIT, SIGTERM end it);
/// - `survives`: SIGINT and SIGQUIT caught; it exits 0 once FILE.go
///   exists (the test's release, so its exit follows the test, not a
///   timer);
/// - `ignores-int`: SIGINT caught (Ctrl-\ or SIGTERM end it);
/// - `ignores-term`: SIGTERM, SIGINT and SIGQUIT caught (only SIGKILL ends
///   it);
/// - `self`: it SIGINTs itself (a signal sb never got);
/// - `fails`: exit 3; `exits`: exit 0.
///
/// Caught signals go through signal-hook's safe `flag::register`, as
/// cli-core's. A child left running ends after a minute, should its test
/// be killed before it could clean up.
fn attached_child(mode: &str, file: &str) -> RunEnd {
    use rustix::process::{DumpableBehavior, Signal, getpid, kill_process, set_dumpable_behavior};
    use signal_hook::consts::{SIGINT, SIGQUIT, SIGTERM};
    let caught: &[i32] = match mode {
        "survives" => &[SIGINT, SIGQUIT],
        "ignores-int" => &[SIGINT],
        "ignores-term" => &[SIGTERM, SIGINT, SIGQUIT],
        _ => &[],
    };
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ready = set_dumpable_behavior(DumpableBehavior::NotDumpable).is_ok()
        && caught.iter().all(|&signal| {
            signal_hook::flag::register(signal, std::sync::Arc::clone(&flag)).is_ok()
        });
    let pid = getpid().as_raw_nonzero().get();
    if !ready || std::fs::write(file, format!("{pid} {pid}\n")).is_err() {
        return RunEnd::Code(2.into());
    }
    let go = format!("{file}.go");
    let until = std::time::Instant::now() + Duration::from_secs(60);
    let wait = |released: &dyn Fn() -> bool| {
        while !released() && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    match mode {
        "dies" | "ignores-int" | "ignores-term" => {
            wait(&|| false);
            RunEnd::Code(0.into())
        }
        "survives" => {
            wait(&|| std::path::Path::new(&go).exists());
            RunEnd::Code(0.into())
        }
        "self" => {
            let _ = kill_process(getpid(), Signal::INT);
            wait(&|| false);
            RunEnd::Code(0.into())
        }
        "fails" => RunEnd::Code(3.into()),
        _ => RunEnd::Code(0.into()),
    }
}

/// `pty`: the child on a PTY, in one step of a term-ui runner (piped here,
/// so it draws plainly), as a command would run it.
async fn on_a_pty(cmd: &Cmd, shutdown: &Shutdown) -> Result<(), Box<dyn Error>> {
    let mut run = Runner::start("exec-demo", shutdown, None, Options::default()).await;
    let ended = run
        .step(Labels::new("Running", "ran"), async |step| {
            // A failed child fails the step: its outcome is checked, not
            // dropped (the outcome types are `#[must_use]`).
            let _ = cmd.pty(step).await?.check()?;
            Ok(())
        })
        .await;
    run.finish();
    ended.map_err(Box::from)
}
