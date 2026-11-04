//! The runner on stderr, for trying it and for the PTY tests: `cargo run
//! -p sb-term-ui --bin demo [-- FLAGS]`, the flags being test hooks:
//! `--fail`, `--sensitive` (wins over `--fail`), `--new-token`,
//! `--child-fail`, `--panic`,
//! `--interrupt`, `--interrupt-twice`, `--terminate`,
//! `--leave-foreground`, `--table` (a table instead of the run, which
//! returns early: any output error is exit 1 there) and `--colour` /
//! `--no-colour`. A binary
//! rather than an example because `cargo test --tests` (the coverage run)
//! builds bins for the integration tests but not examples; sb's build
//! never includes it. It ends through cli-core's `finish_run`,
//! as sb's `main` does, and logs only under `SB_LOG_DIR`.

use std::path::Path;
use std::time::Duration;

use rustix::process::{Signal, getpid, kill_process, setpgid};
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::exit::{ExitCode, RunEnd, finish_run};
use sb_cli_core::log::{RunLog, Secret, outcome};
use sb_cli_core::theme::{self, Palette, Profile, Role, Theme};
use sb_term_ui::{Cell, Labels, Line, Options, Runner, Step, Table};

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
            // `--interrupt-twice`: a body slow to stop, so the second signal
            // arrives while it still runs (cli-core's hard exit).
            if arg("--interrupt-twice") {
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            anyhow::bail!("cancelled");
        }
        step.output(format!("checking item {n} of {lines}"));
    }
    Ok(())
}

/// A made-up token, for `--sensitive` and `--new-token` (the tests look for
/// its `cf-demo` prefix).
const DEMO_TOKEN: &str = "cf-demo-NOT-A-REAL-TOKEN";

/// Whether the demo was started with `flag`.
fn arg(flag: &str) -> bool {
    std::env::args().any(|a| a == flag)
}

/// The test hooks `--interrupt`, `--interrupt-twice` and `--terminate`
/// (the demo signals itself while the second step runs, as a user's Ctrl-C
/// or a `kill` would, once or twice), and `--leave-foreground`.
fn signal_self() {
    // `--leave-foreground`: move into a process group of its own mid-run,
    // as Ctrl-Z then `bg` would, so the terminal's foreground is another.
    if arg("--leave-foreground") {
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(300));
            let _ = setpgid(None, None);
        });
    }
    let (signal, times) = if arg("--terminate") {
        (Signal::TERM, 1)
    } else if arg("--interrupt-twice") {
        (Signal::INT, 2)
    } else if arg("--interrupt") {
        (Signal::INT, 1)
    } else {
        return;
    };
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(700));
        for _ in 0..times {
            let _ = kill_process(getpid(), signal);
            std::thread::sleep(Duration::from_millis(300));
        }
    });
}

async fn demo(
    shutdown: &Shutdown,
    fail: bool,
    sensitive: bool,
    log: Option<&RunLog>,
) -> anyhow::Result<()> {
    let pace = Duration::from_millis(120);
    let options = Options::default();
    let mut run = Runner::start("sb demo", shutdown, log, options).await;
    run.step(
        Labels::new("Reading accounts.yml", "accounts.yml read"),
        async |step| {
            work(step, 4, pace).await?;
            step.info("3 accounts");
            // `--new-token`: a step whose job is to show a secret to its
            // user (as a command creating an API token would): shown on a
            // terminal only, its value never logged (decision 8).
            if arg("--new-token") {
                step.info(Line::new("new token: ").secret(DEMO_TOKEN).shown());
            }
            // `--child-fail`: the step fails as a failed child does (exec's
            // `Failed`), its reason in the times column.
            if arg("--child-fail") {
                let child = sb_term_ui::ChildFailure::new(
                    "systemctl is-active [ARG] exited with code 3",
                    "exit 3",
                    vec!["inactive".into()],
                );
                return Err(child.into());
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
    // Two checks at once (a group); with `--fail` the first fails early,
    // stopping the other, and its parent reads "sub-step failed".
    run.step(
        Labels::new("Checking credentials", "credentials valid"),
        async |step| {
            // `--panic`: a body that panics, for the test that a panic shows
            // only its own message (no failure block, no pointer).
            if arg("--panic") {
                panic!("the demo was asked to panic");
            }
            let mut group = step.group();
            let cloudflare = Labels::new("Checking Cloudflare token", "Cloudflare token valid")
                .failure("Cloudflare token invalid");
            group.spawn(cloudflare, move |mut step| async move {
                work(&mut step, if fail || sensitive { 2 } else { 6 }, pace).await?;
                // `--sensitive`: the token read from settings.yml is malformed,
                // and validation names the bad value, which is the secret:
                // just the value is marked, in the line showing what was read
                // and in the error, so `[REDACTED]` stands in its place there
                // and in the log; the rest stays readable. (The length it
                // gives is a deliberate choice for this message.)
                if sensitive {
                    let read = Line::new("cloudflare.token: ").secret(DEMO_TOKEN);
                    step.output(read);
                    let found = DEMO_TOKEN.len();
                    let token = Secret::new(DEMO_TOKEN);
                    let why = anyhow::anyhow!("got {token}: expected 40 characters, found {found}");
                    return Err(
                        why.context("settings.yml: cloudflare.token is not a valid API token")
                    );
                }
                if fail {
                    let why = anyhow::anyhow!(
                        "HTTP 403 from the Cloudflare API\nthe token lacks Zone:Read"
                    );
                    return Err(why.context("the Cloudflare token was refused"));
                }
                Ok(())
            });
            let docker_hub = Labels::new("Checking Docker Hub login", "Docker Hub login valid");
            // A sub-step inside a sibling: in plain mode its lines carry the
            // sibling's label too.
            group.spawn(docker_hub, move |mut step| async move {
                let login = Labels::new("Logging in", "logged in");
                step.step(login, async |step| work(step, 4, pace).await)
                    .await?;
                work(&mut step, 1, pace).await
            });
            group.join().await?;
            Ok(())
        },
    )
    .await?;
    run.finish();
    Ok(())
}

/// `--table`: a `docker ps`-like table on stdout, in the reviewed look.
fn table() -> ExitCode {
    let mut table = Table::new(["Container", "Status", "Traefik Port", "Port Bindings"])
        .right_align(2)
        .right_align(3)
        .max_column_width(100);
    table.row([
        Cell::new("authelia"),
        Cell::new("running").role(Role::Ok),
        Cell::new("9091"),
        Cell::new(""),
    ]);
    table.row([
        Cell::new("plex"),
        Cell::new("running").role(Role::Ok),
        Cell::new("32400"),
        Cell::new("0.0.0.0:32400->32400/tcp"),
    ]);
    table.row([
        Cell::new("sonarr"),
        Cell::new("exited (1)").role(Role::Problem),
        Cell::new("8989"),
        Cell::new(""),
    ]);
    match table.print() {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(1),
    }
}

fn main() -> RunEnd {
    theme::install(Theme::resolve(&Palette::default(), Profile::TrueColor));
    // `--colour` / `--no-colour`: sb's `--color=always|never`, as anstream's
    // global choice (the real binary sets it from its flag).
    if arg("--colour") {
        anstream::ColorChoice::Always.write_global();
    } else if arg("--no-colour") {
        anstream::ColorChoice::Never.write_global();
    }
    sb_term_ui::follow_colour_choice(Profile::TrueColor);
    if arg("--table") {
        return table().into();
    }
    let fail = std::env::args().any(|a| a == "--fail");
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
    signal_self();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build();
    let result = match (shutdown.listen_for_signals(), runtime) {
        (Ok(()), Ok(runtime)) => runtime.block_on(demo(&shutdown, fail, sensitive, log.as_ref())),
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
