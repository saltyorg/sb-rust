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
//! them) or `exits` (it ends on its own); a fourth argument `pty` runs it
//! on a PTY in a term-ui step instead of captured.

use std::error::Error;
use std::time::Duration;

use sb_cli_core::cancel::Shutdown;
use sb_cli_core::exit::{RunEnd, finish_run};
use sb_exec::Cmd;
use sb_term_ui::{Labels, Options, Runner};

fn main() -> RunEnd {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = || {
        eprintln!("usage: exec-demo FILE stops|ignores|exits GRACE_MS [pty]");
        RunEnd::Code(2.into())
    };
    let (file, mode, grace, pty) = match &args[..] {
        [file, mode, grace] => (file, mode, grace, false),
        [file, mode, grace, way] if way == "pty" => (file, mode, grace, true),
        _ => return usage(),
    };
    let Ok(grace) = grace.parse() else {
        return usage();
    };
    let script = match mode.as_str() {
        "stops" => "sleep 30 & echo $$ $! > \"$1\"; wait",
        "ignores" => "trap '' TERM; (exec sleep 30) & echo $$ $! > \"$1\"; wait",
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
    let cmd = Cmd::new("sh")
        .arg("-c")
        .arg(script)
        .arg("sh")
        .value(file.as_str())
        .grace(grace);
    let result: Result<(), Box<dyn Error>> = match (shutdown.listen_for_signals(), runtime) {
        (Ok(()), Ok(runtime)) if pty => runtime.block_on(on_a_pty(&cmd, &shutdown)),
        (Ok(()), Ok(runtime)) => runtime
            .block_on(cmd.capture(&shutdown.token()))
            .map(|_| ())
            .map_err(Box::from),
        (Err(err), _) | (_, Err(err)) => Err(err.into()),
    };
    let error = result
        .as_ref()
        .err()
        .map(|e| e.as_ref() as &(dyn Error + 'static));
    finish_run(&shutdown.snapshot(), error, None, &mut std::io::stderr())
}

/// `pty`: the child on a PTY, in one step of a term-ui runner (piped here,
/// so it draws plainly), as a command would run it.
async fn on_a_pty(cmd: &Cmd, shutdown: &Shutdown) -> Result<(), Box<dyn Error>> {
    let mut run = Runner::start("exec-demo", shutdown, None, Options::default()).await;
    let ended = run
        .step(Labels::new("Running", "ran"), async |step| {
            cmd.pty(step).await?;
            Ok(())
        })
        .await;
    run.finish();
    ended.map_err(Box::from)
}
