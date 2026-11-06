//! `sb docker start`, `stop` and `restart` as term-ui steps (sb-go's
//! `cmd/dockerStart.go`, `dockerStop.go` and `dockerRestart.go`): the
//! spec's labels, the service check, then each job asked for and waited
//! on; an interrupted request or wait says the controller may still be
//! at work (decision 5).

use sb_cli_core::exit::Exit;
use sb_term_ui::{Labels, Runner, Step};

use super::{Action, ControlError, Controller, Job};

/// `sb docker start`: the service check, then a start job.
///
/// # Errors
///
/// The [`Exit`] the run ends with when a step doesn't finish (term-ui's
/// endings: 1 for a failure, an interruption or a stop).
pub async fn start(runner: &mut Runner, controller: &Controller) -> Result<(), Exit> {
    let labels = Labels::new("Starting Docker containers", "Docker containers started")
        .failure("Docker container start");
    runner
        .step(labels, async |step| {
            check(step, controller).await?;
            job(step, controller, Action::Start, &[]).await?;
            Ok(())
        })
        .await
}

/// `sb docker stop`: the service check, then a stop job ignoring
/// `ignore`'s names.
///
/// # Errors
///
/// As [`start`].
pub async fn stop(
    runner: &mut Runner,
    controller: &Controller,
    ignore: &[String],
) -> Result<(), Exit> {
    let labels = Labels::new("Stopping Docker containers", "Docker containers stopped")
        .failure("Docker container stop");
    runner
        .step(labels, async |step| {
            check(step, controller).await?;
            job(step, controller, Action::Stop, ignore).await?;
            Ok(())
        })
        .await
}

/// `sb docker restart`: the service check, then a stop job ignoring
/// `ignore`'s names, then a start job; a failed stop starts nothing.
///
/// # Errors
///
/// As [`start`].
pub async fn restart(
    runner: &mut Runner,
    controller: &Controller,
    ignore: &[String],
) -> Result<(), Exit> {
    let labels = Labels::new(
        "Restarting Docker containers",
        "Docker containers restarted",
    )
    .failure("Docker container restart");
    runner
        .step(labels, async |step| {
            check(step, controller).await?;
            let stopping = Labels::new("Stopping Docker containers", "Stopped Docker containers")
                .failure("Stop Docker containers");
            step.step(stopping, async |step| {
                job(step, controller, Action::Stop, ignore).await
            })
            .await?;
            let starting = Labels::new("Starting Docker containers", "Started Docker containers")
                .failure("Start Docker containers");
            step.step(starting, async |step| {
                job(step, controller, Action::Start, &[]).await
            })
            .await?;
            Ok(())
        })
        .await
}

/// The service check, under `step`. It takes the step's token, so exec
/// stops `systemctl` gracefully; no notice when interrupted, as no job
/// was asked for.
async fn check(step: &mut Step, controller: &Controller) -> Result<(), Exit> {
    let labels = Labels::new(
        "Checking Docker controller service",
        "Docker controller service ready",
    )
    .failure("Docker controller service check");
    step.step(labels, async |step| {
        Ok(controller.check_service(&step.token()).await?)
    })
    .await
}

/// One job under `step`: its request, then its wait, each a step whose
/// one label (sb-go gives these no others) stands for every ending.
async fn job(
    step: &mut Step,
    controller: &Controller,
    action: Action,
    ignore: &[String],
) -> anyhow::Result<()> {
    let (requesting, waiting) = match action {
        Action::Start => (
            "Requesting Docker start job",
            "Waiting for Docker start job",
        ),
        Action::Stop => ("Requesting Docker stop job", "Waiting for Docker stop job"),
    };
    let asked: Job = step
        .step(Labels::new(requesting, requesting), async |step| {
            stoppable(step, action, controller.request(action, ignore)).await
        })
        .await?;
    // A signal just as the job was accepted: the request ended done and
    // the wait would be skipped unseen (its token already cancelled), so
    // the notice goes here.
    if step.token().is_cancelled() {
        return Err(interrupted(step, action).into());
    }
    step.step(Labels::new(waiting, waiting), async |step| {
        stoppable(step, action, controller.wait(&asked)).await
    })
    .await?;
    Ok(())
}

/// `work` under `step`, ended early when the step is cancelled (Ctrl-C,
/// the command's request), as term-ui asks of a body
/// (<https://docs.rs/tokio-util/0.7.20/tokio_util/sync/struct.CancellationToken.html#method.run_until_cancelled>;
/// the request and the wait are cancelled by dropping them): then
/// decision 5's notice and `ControlError::Interrupted`.
async fn stoppable<T>(
    step: &mut Step,
    action: Action,
    work: impl Future<Output = Result<T, ControlError>>,
) -> anyhow::Result<T> {
    match step.token().run_until_cancelled(work).await {
        Some(done) => Ok(done?),
        None => Err(interrupted(step, action).into()),
    }
}

/// Decision 5's notice under `step`: the controller has no cancel, so a
/// job asked for may go on without sb.
fn interrupted(step: &mut Step, action: Action) -> ControlError {
    let doing = match action {
        Action::Start => "starting",
        Action::Stop => "stopping",
    };
    step.info(format!(
        "the controller may still be {doing} containers; sb docker ps shows where they are"
    ));
    ControlError::Interrupted
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use sb_cli_core::cancel::Shutdown;
    use sb_term_ui::{Options, Target};

    use super::*;
    use crate::control::{SPAWNS, systemctl};
    use crate::testing::controller::{Answer, FakeController};

    const JOB_S: &str = r#"{"job_id":"job-s","status":"pending"}"#;
    const JOB_T: &str = r#"{"job_id":"job-t","status":"pending"}"#;

    fn status(text: &str) -> Answer {
        Answer::ok(format!(r#"{{"status":"{text}"}}"#))
    }

    /// The command.
    #[derive(Clone, Copy)]
    enum Command {
        Start,
        Stop,
        Restart,
    }

    /// A controller on `fake`, its unit file present and `script` as
    /// `systemctl`, polling every 20 ms.
    fn controller(dir: &Path, fake: &FakeController, script: &str) -> Controller {
        let unit = dir.join("unit.service");
        std::fs::write(&unit, "").expect("unit");
        Controller::saltbox()
            .expect("client")
            .with_base(fake.url())
            .expect("base")
            .with_unit_file(unit)
            .with_systemctl(systemctl(dir, script))
            .with_polls(Duration::from_millis(20), 60)
    }

    /// `command` on an in-memory runner: its result and the screen.
    async fn run(
        command: Command,
        controller: &Controller,
        ignore: &[String],
        shutdown: &Shutdown,
    ) -> (Result<(), Exit>, String) {
        let term = indicatif::InMemoryTerm::new(40, 140);
        let options = Options {
            target: Target::InMemory {
                term: term.clone(),
                colour: false,
            },
        };
        let mut runner = Runner::start("sb docker", shutdown, None, options);
        let result = match command {
            Command::Start => start(&mut runner, controller).await,
            Command::Stop => stop(&mut runner, controller, ignore).await,
            Command::Restart => restart(&mut runner, controller, ignore).await,
        };
        runner.finish();
        (result, term.contents())
    }

    /// The screen's tree with each line's time (and its ` · `) taken off
    /// and trailing space trimmed, so a test compares the whole tree:
    /// nesting, order and each text once.
    fn tree(screen: &str) -> String {
        let mut out = String::new();
        for line in screen.lines() {
            let mut line = line.trim_end();
            if let Some((rest, last)) = line.rsplit_once(' ')
                && last.ends_with('s')
                && last.starts_with(|c: char| c.is_ascii_digit())
            {
                line = rest.trim_end();
                line = line.strip_suffix(" ·").unwrap_or(line).trim_end();
            }
            out.push_str(line);
            out.push('\n');
        }
        out
    }

    /// A start: sb-go's labels, the service check, the job asked for and
    /// waited on.
    #[tokio::test]
    async fn start_checks_then_asks_and_waits() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let fake = FakeController::start().expect("fake");
        fake.script("/start", Answer::ok(JOB_T));
        fake.script("/job_status/job-t", status("running"));
        fake.script("/job_status/job-t", status("completed"));
        let c = controller(dir.path(), &fake, "echo active");
        let (result, screen) = run(Command::Start, &c, &[], &Shutdown::new()).await;
        assert_eq!(
            tree(&screen),
            concat!(
                "sb docker\n",
                "╰── ✓ Docker containers started\n",
                "    ├── ✓ Docker controller service ready\n",
                "    ├── ✓ Requesting Docker start job\n",
                "    ╰── ✓ Waiting for Docker start job\n",
            ),
            "{screen}"
        );
        assert!(result.is_ok(), "{screen}");
        let paths: Vec<String> = fake.seen().into_iter().map(|s| s.path).collect();
        assert_eq!(paths, ["/start", "/job_status/job-t", "/job_status/job-t"]);
    }

    /// A stop sends the ignored names; its labels are the stop's.
    #[tokio::test]
    async fn stop_sends_the_ignored_names() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let fake = FakeController::start().expect("fake");
        fake.script("/stop", Answer::ok(JOB_S));
        fake.script("/job_status/job-s", status("completed"));
        let c = controller(dir.path(), &fake, "echo active");
        let ignore = ["plex", "sonarr"].map(String::from);
        let (result, screen) = run(Command::Stop, &c, &ignore, &Shutdown::new()).await;
        assert_eq!(
            tree(&screen),
            concat!(
                "sb docker\n",
                "╰── ✓ Docker containers stopped\n",
                "    ├── ✓ Docker controller service ready\n",
                "    ├── ✓ Requesting Docker stop job\n",
                "    ╰── ✓ Waiting for Docker stop job\n",
            ),
            "{screen}"
        );
        assert!(result.is_ok(), "{screen}");
        let seen = fake.seen();
        assert_eq!(seen[0].path, "/stop");
        assert_eq!(
            seen[0].raw_query.as_deref(),
            Some("ignore=plex&ignore=sonarr")
        );
    }

    /// A restart: one service check, then the stop (with the ignored
    /// names) and the start (without), each under restart's own labels.
    #[tokio::test]
    async fn restart_stops_then_starts() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let fake = FakeController::start().expect("fake");
        fake.script("/stop", Answer::ok(JOB_S));
        fake.script("/job_status/job-s", status("completed"));
        fake.script("/start", Answer::ok(JOB_T));
        fake.script("/job_status/job-t", status("completed"));
        let c = controller(dir.path(), &fake, "echo active");
        let ignore = ["plex".to_string()];
        let (result, screen) = run(Command::Restart, &c, &ignore, &Shutdown::new()).await;
        assert_eq!(
            tree(&screen),
            concat!(
                "sb docker\n",
                "╰── ✓ Docker containers restarted\n",
                "    ├── ✓ Docker controller service ready\n",
                "    ├── ✓ Stopped Docker containers\n",
                "    │   ├── ✓ Requesting Docker stop job\n",
                "    │   ╰── ✓ Waiting for Docker stop job\n",
                "    ╰── ✓ Started Docker containers\n",
                "        ├── ✓ Requesting Docker start job\n",
                "        ╰── ✓ Waiting for Docker start job\n",
            ),
            "{screen}"
        );
        assert!(result.is_ok(), "{screen}");
        assert_eq!(screen.matches("Docker controller service ready").count(), 1);
        let seen = fake.seen();
        let asked: Vec<(&str, Option<&str>)> = seen
            .iter()
            .map(|s| (s.path.as_str(), s.raw_query.as_deref()))
            .collect();
        assert_eq!(
            asked,
            [
                ("/stop", Some("ignore=plex")),
                ("/job_status/job-s", None),
                ("/start", None),
                ("/job_status/job-t", None),
            ]
        );
    }

    /// A failed stop starts nothing: restart ends with its stop's failure.
    #[tokio::test]
    async fn a_failed_stop_starts_nothing() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let fake = FakeController::start().expect("fake");
        fake.script("/stop", Answer::ok(JOB_S));
        fake.script("/job_status/job-s", status("failed"));
        let c = controller(dir.path(), &fake, "echo active");
        let (result, screen) = run(Command::Restart, &c, &[], &Shutdown::new()).await;
        assert_eq!(
            tree(&screen),
            concat!(
                "sb docker\n",
                "╰── ✗ Restarting Docker containers          sub-step failed\n",
                "    ├── ✓ Docker controller service ready\n",
                "    ╰── ✗ Stopping Docker containers        sub-step failed\n",
                "        ├── ✓ Requesting Docker stop job\n",
                "        ╰── ✗ Waiting for Docker stop job\n",
                "            job failed\n",
            ),
            "{screen}"
        );
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert!(!screen.contains("Starting Docker containers"), "{screen}");
        assert!(fake.seen().iter().all(|s| s.path != "/start"));
    }

    /// A failure at each step: its ✗ label, sb-go's text in the block,
    /// exit 1, and nothing asked after it.
    #[tokio::test]
    async fn each_failure_shows_its_step_and_text() {
        let _spawns = SPAWNS.lock().await;
        // The service isn't running: nothing asked of the controller.
        let dir = tempfile::tempdir().expect("dir");
        let fake = FakeController::start().expect("fake");
        let c = controller(dir.path(), &fake, "echo inactive; exit 3");
        let (result, screen) = run(Command::Start, &c, &[], &Shutdown::new()).await;
        assert_eq!(
            tree(&screen),
            concat!(
                "sb docker\n",
                "╰── ✗ Starting Docker containers            sub-step failed\n",
                "    ╰── ✗ Docker controller service check\n",
                "        the Docker controller service is not running\n",
            ),
            "{screen}"
        );
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert!(!screen.contains("exit 3"), "sb-go's text alone\n{screen}");
        assert_eq!(fake.seen().len(), 0);
        // The request is refused: no wait.
        let dir = tempfile::tempdir().expect("dir");
        let fake = FakeController::start().expect("fake");
        fake.script("/start", Answer::with_status(503, r#"{"error":"blocked"}"#));
        let c = controller(dir.path(), &fake, "echo active");
        let (result, screen) = run(Command::Start, &c, &[], &Shutdown::new()).await;
        assert_eq!(
            tree(&screen),
            concat!(
                "sb docker\n",
                "╰── ✗ Starting Docker containers            sub-step failed\n",
                "    ├── ✓ Docker controller service ready\n",
                "    ╰── ✗ Requesting Docker start job\n",
                "        docker controller request failed with status code: 503\n",
            ),
            "{screen}"
        );
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert_eq!(fake.seen().len(), 1);
        // The job fails.
        let dir = tempfile::tempdir().expect("dir");
        let fake = FakeController::start().expect("fake");
        fake.script("/stop", Answer::ok(JOB_S));
        fake.script("/job_status/job-s", status("failed"));
        let c = controller(dir.path(), &fake, "echo active");
        let (result, screen) = run(Command::Stop, &c, &[], &Shutdown::new()).await;
        assert_eq!(
            tree(&screen),
            concat!(
                "sb docker\n",
                "╰── ✗ Stopping Docker containers            sub-step failed\n",
                "    ├── ✓ Docker controller service ready\n",
                "    ├── ✓ Requesting Docker stop job\n",
                "    ╰── ✗ Waiting for Docker stop job\n",
                "        job failed\n",
            ),
            "{screen}"
        );
        assert!(matches!(result, Err(Exit(1))), "{screen}");
    }

    /// Run `command` and signal SIGINT once `ready` says so: the result
    /// and the screen.
    async fn interrupted_run(
        command: Command,
        c: &Controller,
        ready: impl Fn() -> bool,
    ) -> (Result<(), Exit>, String) {
        let shutdown = Shutdown::new();
        let signal = async {
            while !ready() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
            shutdown.signalled(2);
        };
        let (ran, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(run(command, c, &[], &shutdown), signal)
        })
        .await
        .expect("the run ends");
        ran
    }

    /// Interrupted during the request and during the wait: the step's
    /// interrupted ending and decision 5's notice ("starting" or
    /// "stopping", restart's start and stop alike).
    #[tokio::test]
    async fn an_interrupted_job_says_the_controller_may_go_on() {
        let _spawns = SPAWNS.lock().await;
        let stopping =
            "the controller may still be stopping containers; sb docker ps shows where they are";
        // During the request.
        let dir = tempfile::tempdir().expect("dir");
        let fake = FakeController::start().expect("fake");
        fake.script("/start", Answer::ok(JOB_T).after(Duration::from_secs(30)));
        let c = controller(dir.path(), &fake, "echo active");
        let (result, screen) =
            interrupted_run(Command::Start, &c, || !fake.seen().is_empty()).await;
        assert_eq!(
            tree(&screen),
            concat!(
                "sb docker\n",
                "╰── ✗ Starting Docker containers                interrupted\n",
                "    ├── ✓ Docker controller service ready\n",
                "    ╰── ✗ Requesting Docker start job           interrupted\n",
                "        › the controller may still be starting containers; sb docker ps shows where they are\n",
            ),
            "{screen}"
        );
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        // During the wait.
        let dir = tempfile::tempdir().expect("dir");
        let fake = FakeController::start().expect("fake");
        fake.script("/stop", Answer::ok(JOB_S));
        fake.script("/job_status/job-s", status("running"));
        let c =
            controller(dir.path(), &fake, "echo active").with_polls(Duration::from_secs(60), 60);
        let (result, screen) = interrupted_run(Command::Stop, &c, || fake.seen().len() >= 2).await;
        assert_eq!(
            tree(&screen),
            concat!(
                "sb docker\n",
                "╰── ✗ Stopping Docker containers                interrupted\n",
                "    ├── ✓ Docker controller service ready\n",
                "    ├── ✓ Requesting Docker stop job\n",
                "    ╰── ✗ Waiting for Docker stop job           interrupted\n",
                "        › the controller may still be stopping containers; sb docker ps shows where they are\n",
            ),
            "{screen}"
        );
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert_eq!(fake.seen().len(), 2, "no poll after the interrupt");
        // Restart's start: "starting".
        let dir = tempfile::tempdir().expect("dir");
        let fake = FakeController::start().expect("fake");
        fake.script("/stop", Answer::ok(JOB_S));
        fake.script("/job_status/job-s", status("completed"));
        fake.script("/start", Answer::ok(JOB_T));
        fake.script("/job_status/job-t", status("running"));
        let c =
            controller(dir.path(), &fake, "echo active").with_polls(Duration::from_secs(60), 60);
        let (result, screen) =
            interrupted_run(Command::Restart, &c, || fake.seen().len() >= 4).await;
        assert_eq!(
            tree(&screen),
            concat!(
                "sb docker\n",
                "╰── ✗ Restarting Docker containers              interrupted\n",
                "    ├── ✓ Docker controller service ready\n",
                "    ├── ✓ Stopped Docker containers\n",
                "    │   ├── ✓ Requesting Docker stop job\n",
                "    │   ╰── ✓ Waiting for Docker stop job\n",
                "    ╰── ✗ Starting Docker containers            interrupted\n",
                "        ├── ✓ Requesting Docker start job\n",
                "        ╰── ✗ Waiting for Docker start job      interrupted\n",
                "            › the controller may still be starting containers; sb docker ps shows where they are\n",
            ),
            "{screen}"
        );
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert!(!screen.contains(stopping), "{screen}");
    }

    /// Restart interrupted during its stop: "stopping", and nothing
    /// started after it.
    #[tokio::test]
    async fn an_interrupted_restart_stop_starts_nothing() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let fake = FakeController::start().expect("fake");
        fake.script("/stop", Answer::ok(JOB_S));
        fake.script("/job_status/job-s", status("running"));
        let c =
            controller(dir.path(), &fake, "echo active").with_polls(Duration::from_secs(60), 60);
        let (result, screen) =
            interrupted_run(Command::Restart, &c, || fake.seen().len() >= 2).await;
        assert_eq!(
            tree(&screen),
            concat!(
                "sb docker\n",
                "╰── ✗ Restarting Docker containers              interrupted\n",
                "    ├── ✓ Docker controller service ready\n",
                "    ╰── ✗ Stopping Docker containers            interrupted\n",
                "        ├── ✓ Requesting Docker stop job\n",
                "        ╰── ✗ Waiting for Docker stop job       interrupted\n",
                "            › the controller may still be stopping containers; sb docker ps shows where they are\n",
            ),
            "{screen}"
        );
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert!(fake.seen().iter().all(|s| s.path != "/start"));
    }

    /// Interrupted during the service check: its interrupted ending, no
    /// notice (no job was asked for) and no "not running".
    #[tokio::test]
    async fn an_interrupted_check_has_no_notice() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let fake = FakeController::start().expect("fake");
        let c = controller(dir.path(), &fake, "exec sleep 30");
        let ran = dir.path().join("ran");
        let (result, screen) = interrupted_run(Command::Start, &c, || ran.exists()).await;
        assert_eq!(
            tree(&screen),
            concat!(
                "sb docker\n",
                "╰── ✗ Starting Docker containers                interrupted\n",
                "    ╰── ✗ Checking Docker controller service    interrupted\n",
            ),
            "{screen}"
        );
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert!(!screen.contains("may still be"), "{screen}");
        assert!(!screen.contains("not running"), "{screen}");
        assert_eq!(fake.seen().len(), 0);
    }
}
