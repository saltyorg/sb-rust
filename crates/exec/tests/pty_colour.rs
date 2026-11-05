//! A PTY child's colours through term-ui's in-memory screen, in a test
//! binary of its own: it installs a colour profile, which is process-wide
//! (cli-core's theme), and the other PTY tests run under the plain theme
//! (`ascii`), whose rules differ (no `COLORTERM`; a line's own colour
//! codes stripped, term-ui spec, maintainer 2026-11-05).

use std::time::Duration;

use indicatif::InMemoryTerm;
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::theme::{self, Palette, Profile, Theme};
use sb_exec::{Cmd, Ending};
use sb_term_ui::{Labels, Options, Runner, Target};

/// Run `cmd` on a PTY in one step, the step failing when the child does
/// (so its lines stay on screen). Returns the screen, with its escape
/// codes when `colour` (`contents` renders them away).
async fn on_pty(cmd: Cmd, colour: bool) -> String {
    let term = InMemoryTerm::new(40, 140);
    let options = Options {
        target: Target::InMemory {
            term: term.clone(),
            colour,
        },
    };
    let shutdown = Shutdown::new();
    let mut run = Runner::start("sb test", &shutdown, None, options).await;
    let _ = run
        .step(Labels::new("Running", "ran"), async |step| {
            let done = cmd.pty(step).await?;
            if done.outcome.ending != Ending::Exited(0) {
                anyhow::bail!("the child failed");
            }
            Ok(())
        })
        .await;
    run.finish();
    if colour {
        String::from_utf8_lossy(&term.contents_formatted()).into_owned()
    } else {
        term.contents()
    }
}

fn sh(script: &'static str) -> Cmd {
    Cmd::new("sh").arg("-c").arg(script)
}

fn has_line(screen: &str, line: &str) -> bool {
    screen.lines().any(|l| l.trim() == line)
}

/// Colours stay for a coloured target and go otherwise (term-ui's).
#[tokio::test]
async fn colours_follow_the_target() {
    theme::install(Theme::resolve(&Palette::default(), Profile::TrueColor));
    let cmd = || sh("printf '\\033[31mred\\033[0m\\n'; exit 1");
    let coloured = tokio::time::timeout(Duration::from_secs(20), on_pty(cmd(), true))
        .await
        .expect("ended");
    assert!(coloured.contains("\u{1b}[31m"), "{coloured:?}");
    let plain = tokio::time::timeout(Duration::from_secs(20), on_pty(cmd(), false))
        .await
        .expect("ended");
    assert!(
        !plain.contains("\u{1b}[31m") && has_line(&plain, "red"),
        "{plain:?}"
    );
}
