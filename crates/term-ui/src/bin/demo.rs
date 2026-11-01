//! The runner on stderr, for trying it and for the PTY tests: `cargo run
//! -p sb-term-ui --bin demo [-- --fail] [--verbose]`. A binary rather than an example
//! because `cargo test --tests` (the coverage run) builds bins for the
//! integration tests but not examples; sb's build never includes it. It
//! ends through cli-core's `finish_run`, as sb's `main` does, and writes no
//! log.

use std::time::Duration;

use sb_cli_core::cancel::Shutdown;
use sb_cli_core::exit::{ExitCode, finish_run};
use sb_cli_core::theme::{self, Palette, Profile, Theme};
use sb_term_ui::{Labels, Options, Runner, Step};

/// `lines` output lines, one every `pace`, stopping when cancelled.
async fn work(step: &mut Step, lines: usize, pace: Duration) -> anyhow::Result<()> {
    let token = step.token();
    for n in 1..=lines {
        // https://docs.rs/tokio-util/0.7.20/tokio_util/sync/struct.CancellationToken.html#method.run_until_cancelled
        if token
            .run_until_cancelled(tokio::time::sleep(pace))
            .await
            .is_none()
        {
            anyhow::bail!("cancelled");
        }
        step.output(format!("checking item {n} of {lines}"));
    }
    Ok(())
}

async fn demo(shutdown: &Shutdown, fail: bool, verbose: bool) -> anyhow::Result<()> {
    let pace = Duration::from_millis(120);
    let options = Options {
        verbose,
        ..Options::default()
    };
    let mut run = Runner::start("sb demo", shutdown, None, options).await;
    run.step(
        Labels::new("Reading accounts.yml", "accounts.yml read"),
        async |step| {
            work(step, 4, pace).await?;
            step.info("3 accounts");
            Ok(())
        },
    )
    .await?;
    run.step(
        Labels::new("Checking settings.yml", "settings.yml checked"),
        async |step| {
            step.warn("unknown key: downloads.extra");
            work(step, 11, pace).await
        },
    )
    .await?;
    let labels =
        Labels::new("Checking credentials", "credentials valid").failure("credentials invalid");
    run.step(labels, async |step| {
        work(step, 6, pace).await?;
        if fail {
            anyhow::bail!("the Cloudflare token was refused");
        }
        Ok(())
    })
    .await?;
    run.finish();
    Ok(())
}

fn main() -> ExitCode {
    theme::install(Theme::resolve(&Palette::default(), Profile::TrueColor));
    sb_term_ui::follow_colour_choice(Profile::TrueColor);
    let fail = std::env::args().any(|a| a == "--fail");
    let verbose = std::env::args().any(|a| a == "--verbose");
    let shutdown = Shutdown::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build();
    let result = match (shutdown.listen_for_signals(), runtime) {
        (Ok(()), Ok(runtime)) => runtime.block_on(demo(&shutdown, fail, verbose)),
        (Err(err), _) | (_, Err(err)) => Err(err.into()),
    };
    let error = result.as_ref().err().map(|e| e.as_ref() as _);
    finish_run(&shutdown.snapshot(), error, None, &mut anstream::stderr())
}
