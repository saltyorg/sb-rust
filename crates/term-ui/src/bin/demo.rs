//! The runner on stderr, for trying it and for the PTY tests: `cargo run
//! -p sb-term-ui --bin demo [-- --fail] [--verbose] [--sensitive]
//! [--panic]`. A binary rather than an example because `cargo test --tests` (the
//! coverage run) builds bins for the integration tests but not examples;
//! sb's build never includes it. It ends through cli-core's `finish_run`,
//! as sb's `main` does, and logs only under `SB_LOG_DIR`.

use std::path::Path;
use std::time::Duration;

use sb_cli_core::cancel::Shutdown;
use sb_cli_core::exit::{ExitCode, finish_run};
use sb_cli_core::log::{RunLog, outcome};
use sb_cli_core::theme::{self, Palette, Profile, Theme};
use sb_term_ui::{Labels, Line, Options, Runner, Step};

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

async fn demo(
    shutdown: &Shutdown,
    fail: bool,
    verbose: bool,
    sensitive: bool,
    log: Option<&RunLog>,
) -> anyhow::Result<()> {
    let pace = Duration::from_millis(120);
    let options = Options {
        verbose,
        ..Options::default()
    };
    let mut run = Runner::start("sb demo", shutdown, log, options).await;
    run.step(
        Labels::new("Reading accounts.yml", "accounts.yml read"),
        async |step| {
            work(step, 4, pace).await?;
            step.info("3 accounts");
            if sensitive {
                step.output(Line::sensitive("demo-secret-withheld"));
                step.info(Line::sensitive("demo-secret-shown").shown());
            }
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
        // `--panic`: a body that panics, for the test that a panic shows
        // only its own message (no failure block, no pointer).
        if std::env::args().any(|a| a == "--panic") {
            panic!("the demo was asked to panic");
        }
        if fail {
            let why =
                anyhow::anyhow!("HTTP 403 from the Cloudflare API\nthe token lacks Zone:Read");
            return Err(why.context("the Cloudflare token was refused"));
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
    let sensitive = std::env::args().any(|a| a == "--sensitive");
    // A run log only under SB_LOG_DIR (the tests' temporary directory),
    // never the host's /var/log/sb; opened as sb's `main` opens it.
    let log = std::env::var_os("SB_LOG_DIR").and_then(|dir| {
        let (log, subscriber) = RunLog::open(Path::new(&dir)).ok()?;
        tracing::subscriber::set_global_default(subscriber)
            .is_ok()
            .then_some(log)
    });
    let _run = log.as_ref().map(RunLog::enter);
    if let Some(log) = &log {
        log.started("demo", env!("CARGO_PKG_VERSION"));
    }
    let shutdown = Shutdown::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build();
    let result = match (shutdown.listen_for_signals(), runtime) {
        (Ok(()), Ok(runtime)) => {
            runtime.block_on(demo(&shutdown, fail, verbose, sensitive, log.as_ref()))
        }
        (Err(err), _) | (_, Err(err)) => Err(err.into()),
    };
    let error = result.as_ref().err().map(|e| e.as_ref() as _);
    // One frozen view of the ending for the log and the screen, as sb does.
    let ending = shutdown.snapshot();
    if let Some(log) = &log {
        log.finished(&outcome(&ending, error), error);
    }
    let pointer = log.as_ref().map(RunLog::pointer);
    finish_run(&ending, error, pointer.as_deref(), &mut anstream::stderr())
}
