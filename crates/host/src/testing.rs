//! Stand-in programs for sb-host's tests (host spec, Testing Strategy):
//! shell scripts written to a temporary directory by a child, so that
//! running one never fails with ETXTBSY
//! (`sb_cli_core::testing::write_program`).

use std::path::{Path, PathBuf};

/// Tests that write a stand-in program and run it hold this from writing
/// it until its run is over, so no other test's fork holds the program's
/// write descriptor when it runs (exec fails with ETXTBSY otherwise).
pub(crate) static SPAWNS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// What a step left: its body's error chain, if any, and the screen.
pub(crate) struct Ran {
    pub(crate) error: Option<String>,
    pub(crate) screen: String,
}

/// `body` in a step of a runner drawing to memory, wide enough for long
/// lines. `body` gets the runner's shutdown too, to signal Ctrl-C.
pub(crate) async fn in_step(
    body: impl AsyncFnOnce(&mut sb_term_ui::Step, &sb_cli_core::cancel::Shutdown) -> anyhow::Result<()>,
) -> Ran {
    use sb_term_ui::{Labels, Options, Runner, Target};
    let term = indicatif::InMemoryTerm::new(60, 400);
    let options = Options {
        target: Target::InMemory {
            term: term.clone(),
            colour: false,
        },
    };
    let shutdown = sb_cli_core::cancel::Shutdown::new();
    let mut runner = Runner::start("sb test", &shutdown, None, options);
    let mut error = None;
    let _ = runner
        .step(Labels::new("Running apt", "Ran apt"), async |step| {
            let result = body(step, &shutdown).await;
            error = result.as_ref().err().map(|err| format!("{err:#}"));
            result
        })
        .await;
    runner.finish();
    Ran {
        error,
        screen: term.contents(),
    }
}

/// An executable `name` in `dir` running `script` with `sh`.
pub(crate) fn stand_in(dir: &Path, name: &str, script: &str) -> PathBuf {
    let path = dir.join(name);
    sb_cli_core::testing::write_program(&path, format!("#!/bin/sh\n{script}\n"));
    path
}
