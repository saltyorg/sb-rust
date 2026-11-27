//! `sb docker start`, `stop` and `restart` as term-ui steps (sb-go's
//! `cmd/dockerStart.go`, `dockerStop.go` and `dockerRestart.go`): the
//! spec's labels, the service check, then each job asked for and waited
//! on; an interrupted request or wait says the controller may still be
//! at work (decision 5).

use std::cell::Cell;

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
            job(step, controller, Action::Start, &[], &Tracked::default()).await?;
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
            job(step, controller, Action::Stop, ignore, &Tracked::default()).await?;
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
            let stop = Tracked::default();
            let stopped = step
                .step(stopping, async |step| {
                    job(step, controller, Action::Stop, ignore, &stop).await
                })
                .await;
            // The floor (docker-M2): once the stop is asked for, a restart
            // ends with the start requested, or a line saying it wasn't
            // and why: a stop never asked for (its step skipped, the run
            // interrupted) or one that didn't end done.
            if let Err(exit) = stopped {
                let ended = stop.ended.get().unwrap_or(Ended::Interrupted);
                step.info(format!(
                    "the start wasn't requested: {}",
                    ended.why_no_start()
                ));
                return Err(exit.into());
            }
            start_after_stop(step, controller).await
        })
        .await
}

/// Restart's start, once its stop ended done. An interrupt as the stop's
/// last answer came (`run_until_cancelled` prefers the answer when both
/// are ready) or just after it skips the start's request undrawn: then a
/// line says so, as does a refused start, whose containers stay stopped.
async fn start_after_stop(step: &mut Step, controller: &Controller) -> anyhow::Result<()> {
    let starting = Labels::new("Starting Docker containers", "Started Docker containers")
        .failure("Start Docker containers");
    let start = Tracked::default();
    let started = step
        .step(starting, async |step| {
            job(step, controller, Action::Start, &[], &start).await
        })
        .await;
    if let Err(exit) = started {
        if !start.requested.get() {
            step.info("the start wasn't requested: the run was interrupted");
        } else if start.ended.get() == Some(Ended::Refused) {
            step.info(
                "the containers were stopped and stay stopped: the controller refused the start",
            );
        }
        return Err(exit.into());
    }
    Ok(())
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
/// Whether it was requested and how it ended short of done go in
/// `tracked`, for a restart's lines (the steps' own errors stay inside
/// the runner).
async fn job(
    step: &mut Step,
    controller: &Controller,
    action: Action,
    ignore: &[String],
    tracked: &Tracked,
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
            tracked.requested.set(true);
            tracked.noted(stoppable(step, action, controller.request(action, ignore)).await)
        })
        .await?;
    // A signal just as the job was accepted: the request ended done and
    // the wait would be skipped unseen (its token already cancelled), so
    // the notice goes here.
    if step.token().is_cancelled() {
        return tracked.noted(Err(interrupted(step, action).into()));
    }
    step.step(Labels::new(waiting, waiting), async |step| {
        tracked.noted(stoppable(step, action, controller.wait(&asked)).await)
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
        Some(Ok(done)) => Ok(done),
        Some(Err(err)) => {
            if no_final_status(&err) {
                notice(step, action);
            }
            Err(err.into())
        }
        None => Err(interrupted(step, action).into()),
    }
}

/// Whether `err` ended a request or a wait without a final job status, so
/// a job may have been accepted and go on without sb (docker spec
/// decision 10; docker-M2): a request that wasn't answered or whose
/// answer couldn't be read (it may have been accepted), a wait that gave
/// up (sb's timeout, failed polls, a status sb doesn't know). Not a refused request (its
/// status code is the controller's answer), nor a failed job (final), nor
/// a check or setup failure before any request.
fn no_final_status(err: &ControlError) -> bool {
    matches!(
        err,
        ControlError::Send(_)
            | ControlError::DecodeJob(_)
            | ControlError::MissingJobId
            | ControlError::Poll(_)
            | ControlError::StatusCheck(_)
            | ControlError::DecodeStatus(_)
            | ControlError::MissingStatus
            | ControlError::UnknownStatus(_)
            | ControlError::Timeout
    )
}

/// Decision 5's notice under `step`: the controller has no cancel, so a
/// job asked for may go on without sb.
fn interrupted(step: &mut Step, action: Action) -> ControlError {
    notice(step, action);
    ControlError::Interrupted
}

/// The notice that the controller may still be at work (decisions 5 and
/// 10), under `step`.
fn notice(step: &mut Step, action: Action) {
    let doing = match action {
        Action::Start => "starting",
        Action::Stop => "stopping",
    };
    step.info(format!(
        "the controller may still be {doing} containers; sb docker ps shows where they are"
    ));
}

/// What a restart needs to know of one job (the floor, docker-M2):
/// whether its request was made, and how it ended short of done.
#[derive(Default)]
struct Tracked {
    requested: Cell<bool>,
    ended: Cell<Option<Ended>>,
}

impl Tracked {
    /// `done` as it is, how it ended noted first.
    fn noted<T>(&self, done: anyhow::Result<T>) -> anyhow::Result<T> {
        if let Err(err) = &done {
            self.ended.set(Some(Ended::of(err)));
        }
        done
    }
}

/// How a job ended short of done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ended {
    Interrupted,
    JobFailed,
    /// The controller refused the request (its status code).
    Refused,
    /// No final job status ([`no_final_status`]).
    Unknown,
    /// Anything else (none reaches a job today: the URL and the client
    /// are checked before any request).
    Other,
}

impl Ended {
    fn of(err: &anyhow::Error) -> Self {
        match err.downcast_ref::<ControlError>() {
            Some(ControlError::Interrupted) => Ended::Interrupted,
            Some(ControlError::JobFailed(_)) => Ended::JobFailed,
            Some(ControlError::RequestStatus(_)) => Ended::Refused,
            Some(err) if no_final_status(err) => Ended::Unknown,
            _ => Ended::Other,
        }
    }

    /// Why a restart's start wasn't requested after a stop that ended so.
    fn why_no_start(self) -> &'static str {
        match self {
            Ended::Interrupted => "the run was interrupted",
            Ended::JobFailed => "the stop job failed",
            Ended::Refused => "the controller refused the stop",
            Ended::Unknown => "sb couldn't tell whether the stop finished",
            Ended::Other => "the stop wasn't done",
        }
    }
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
            .with_wait(Duration::from_millis(20), Duration::from_secs(60))
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

    /// A failed job names its containers under its step (decision 11),
    /// one line each, after sb-go's "job failed" and the controller's
    /// error; a failed job is final, so no notice that the controller may
    /// still be at work.
    #[tokio::test]
    async fn a_failed_job_names_its_containers_under_its_step() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let fake = FakeController::start().expect("fake");
        fake.script("/start", Answer::ok(JOB_T));
        fake.script(
            "/job_status/job-t",
            Answer::ok(
                r#"{"status":"failed","failed":["plex","radarr"],"outcomes":[{"container":"sonarr","outcome":"changed"},{"container":"plex","outcome":"failed","reason":"operation_failed","error":"port is already allocated"},{"container":"radarr","outcome":"blocked","reason":"dependency_failed","error":"plex"}],"error":"one or more containers did not reach the requested state"}"#,
            ),
        );
        let c = controller(dir.path(), &fake, "echo active");
        let (result, screen) = run(Command::Start, &c, &[], &Shutdown::new()).await;
        assert_eq!(
            tree(&screen),
            concat!(
                "sb docker\n",
                "╰── ✗ Starting Docker containers            sub-step failed\n",
                "    ├── ✓ Docker controller service ready\n",
                "    ├── ✓ Requesting Docker start job\n",
                "    ╰── ✗ Waiting for Docker start job\n",
                "        job failed: one or more containers did not reach the requested state\n",
                "          plex: failed (operation_failed): port is already allocated\n",
                "          radarr: blocked (dependency_failed): plex\n",
            ),
            "{screen}"
        );
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert!(!screen.contains("may still be"), "{screen}");
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
                "    › the start wasn't requested: the stop job failed\n",
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
        interrupted_run_noting(command, c, ready, || ()).await
    }

    /// [`interrupted_run`], with `at_signal` run just before the SIGINT,
    /// to note what the controller had seen by then.
    async fn interrupted_run_noting(
        command: Command,
        c: &Controller,
        ready: impl Fn() -> bool,
        at_signal: impl FnOnce(),
    ) -> (Result<(), Exit>, String) {
        let shutdown = Shutdown::new();
        let signal = async {
            while !ready() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
            at_signal();
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
        // A short interval, so a poll after the interrupt would come
        // within the wait below; on the real clock, as client.rs's wait
        // tests are (tokio's paused clock runs ahead while a real socket
        // waits).
        let interval = Duration::from_millis(20);
        let c = controller(dir.path(), &fake, "echo active")
            .with_wait(interval, Duration::from_secs(3600));
        let polls_at_signal = std::cell::Cell::new(0);
        let (result, screen) = interrupted_run_noting(
            Command::Stop,
            &c,
            || fake.seen().len() >= 2,
            || polls_at_signal.set(fake.seen().len()),
        )
        .await;
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
        // Ten intervals on: at most the one poll the interrupt can meet
        // already started (sent, not yet seen by the fake), none after it.
        tokio::time::sleep(interval * 10).await;
        let polls = fake.seen().len();
        assert!(
            polls <= polls_at_signal.get() + 1,
            "{polls} requests, {} at the interrupt: a poll after it",
            polls_at_signal.get()
        );
        // Restart's start: "starting".
        let dir = tempfile::tempdir().expect("dir");
        let fake = FakeController::start().expect("fake");
        fake.script("/stop", Answer::ok(JOB_S));
        fake.script("/job_status/job-s", status("completed"));
        fake.script("/start", Answer::ok(JOB_T));
        fake.script("/job_status/job-t", status("running"));
        let c = controller(dir.path(), &fake, "echo active")
            .with_wait(Duration::from_secs(60), Duration::from_secs(3600));
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
        let c = controller(dir.path(), &fake, "echo active")
            .with_wait(Duration::from_secs(60), Duration::from_secs(3600));
        let (result, screen) =
            interrupted_run(Command::Restart, &c, || fake.seen().len() >= 2).await;
        assert_eq!(
            tree(&screen),
            concat!(
                "sb docker\n",
                "╰── ✗ Restarting Docker containers              interrupted\n",
                "    › the start wasn't requested: the run was interrupted\n",
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

    const STOPPING: &str =
        "› the controller may still be stopping containers; sb docker ps shows where they are";
    const STARTING: &str =
        "› the controller may still be starting containers; sb docker ps shows where they are";

    /// The endings without a final job status (docker-M2): sb's own
    /// timeout, failed polls, a timed-out request. Each is scripted on
    /// `fake` for `path`'s job (`stop` or `start`); the controller to run
    /// it with.
    fn non_final(dir: &Path, fake: &FakeController, path: &str, how: &str) -> Controller {
        let (job, answer) = if path == "stop" {
            ("job-s", JOB_S)
        } else {
            ("job-t", JOB_T)
        };
        let c = controller(dir, fake, "echo active");
        match how {
            "timeout" => {
                fake.script(&format!("/{path}"), Answer::ok(answer));
                fake.script(&format!("/job_status/{job}"), status("running"));
                c.with_wait(Duration::from_millis(20), Duration::from_millis(50))
            }
            "failed polls" => {
                fake.script(&format!("/{path}"), Answer::ok(answer));
                fake.script(&format!("/job_status/{job}"), Answer::with_status(502, ""));
                c
            }
            "timed-out request" => {
                fake.script(
                    &format!("/{path}"),
                    Answer::ok(answer).after(Duration::from_secs(5)),
                );
                c.with_request_timeout(Duration::from_millis(100))
            }
            _ => unreachable!("{how}"),
        }
    }

    /// The floor (docker-M2): a restart whose stop ends without a final
    /// status requests no start, and says so and why, with decision 5's
    /// notice that the controller may still be stopping containers.
    #[tokio::test]
    async fn restart_says_why_the_start_wasnt_requested() {
        let _spawns = SPAWNS.lock().await;
        for how in ["timeout", "failed polls", "timed-out request"] {
            let dir = tempfile::tempdir().expect("dir");
            let fake = FakeController::start().expect("fake");
            let c = non_final(dir.path(), &fake, "stop", how);
            let (result, screen) = run(Command::Restart, &c, &[], &Shutdown::new()).await;
            assert!(matches!(result, Err(Exit(1))), "{how}\n{screen}");
            assert!(fake.seen().iter().all(|s| s.path != "/start"), "{how}");
            assert!(screen.contains(STOPPING), "{how}\n{screen}");
            assert!(
                screen.contains(
                    "› the start wasn't requested: sb couldn't tell whether the stop finished"
                ),
                "{how}\n{screen}"
            );
        }
    }

    /// The same floor for a stop that ended (a failed job) or was
    /// interrupted: the line says why, and a failed job has no notice (its
    /// status is final).
    #[tokio::test]
    async fn restart_says_why_after_a_final_stop() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let fake = FakeController::start().expect("fake");
        fake.script("/stop", Answer::ok(JOB_S));
        fake.script("/job_status/job-s", status("failed"));
        let c = controller(dir.path(), &fake, "echo active");
        let (_, screen) = run(Command::Restart, &c, &[], &Shutdown::new()).await;
        assert!(
            screen.contains("› the start wasn't requested: the stop job failed"),
            "{screen}"
        );
        assert!(!screen.contains("may still be"), "{screen}");
        // A refused stop: its own reason, no notice.
        let dir = tempfile::tempdir().expect("dir");
        let fake = FakeController::start().expect("fake");
        fake.script("/stop", Answer::with_status(503, r#"{"error":"blocked"}"#));
        let c = controller(dir.path(), &fake, "echo active");
        let (_, screen) = run(Command::Restart, &c, &[], &Shutdown::new()).await;
        assert!(
            screen.contains("› the start wasn't requested: the controller refused the stop"),
            "{screen}"
        );
        assert!(!screen.contains("may still be"), "{screen}");
    }

    /// An interrupt landing as the stop's `completed` arrives: the wait may
    /// end done (`run_until_cancelled` prefers its future when both are
    /// ready) and the start step is then skipped undrawn, or the wait ends
    /// interrupted. Either way no start is requested and the restart says
    /// so. Which way it goes is a race, so several runs.
    #[tokio::test]
    async fn an_interrupt_as_the_stop_completes_says_the_start_wasnt_requested() {
        let _spawns = SPAWNS.lock().await;
        for run_n in 0..10 {
            let dir = tempfile::tempdir().expect("dir");
            let fake = FakeController::start().expect("fake");
            let shutdown = Shutdown::new();
            let signal = shutdown.clone();
            fake.script("/stop", Answer::ok(JOB_S));
            fake.script(
                "/job_status/job-s",
                status("completed").then(move || signal.signalled(2)),
            );
            fake.script("/start", Answer::ok(JOB_T));
            fake.script("/job_status/job-t", status("completed"));
            let c = controller(dir.path(), &fake, "echo active");
            let (result, screen) = run(Command::Restart, &c, &[], &shutdown).await;
            assert!(matches!(result, Err(Exit(1))), "run {run_n}\n{screen}");
            assert!(
                fake.seen().iter().all(|s| s.path != "/start"),
                "run {run_n}"
            );
            assert!(
                screen.contains("› the start wasn't requested: the run was interrupted"),
                "run {run_n}\n{screen}"
            );
        }
    }

    /// The gap itself, made certain: the stop is done, then the run is
    /// interrupted before the start's step begins. The start step is
    /// skipped undrawn and nothing is asked, so the restart's step says
    /// the start wasn't requested.
    #[tokio::test]
    async fn an_interrupt_before_the_start_begins_says_so() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let fake = FakeController::start().expect("fake");
        fake.script("/start", Answer::ok(JOB_T));
        let c = controller(dir.path(), &fake, "echo active");
        let shutdown = Shutdown::new();
        let term = indicatif::InMemoryTerm::new(40, 140);
        let options = Options {
            target: Target::InMemory {
                term: term.clone(),
                colour: false,
            },
        };
        let mut runner = Runner::start("sb docker", &shutdown, None, options);
        let restarting = Labels::new(
            "Restarting Docker containers",
            "Docker containers restarted",
        );
        let result = runner
            .step(restarting, async |step| {
                shutdown.signalled(2);
                start_after_stop(step, &c).await
            })
            .await;
        runner.finish();
        let screen = term.contents();
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert_eq!(fake.seen().len(), 0, "{screen}");
        assert!(!screen.contains("Starting Docker containers"), "{screen}");
        assert!(
            screen.contains("› the start wasn't requested: the run was interrupted"),
            "{screen}"
        );
    }

    /// After a completed stop, a refused start: the restart says the
    /// containers were stopped and stay stopped.
    #[tokio::test]
    async fn a_refused_start_after_the_stop_says_they_stay_stopped() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let fake = FakeController::start().expect("fake");
        fake.script("/stop", Answer::ok(JOB_S));
        fake.script("/job_status/job-s", status("completed"));
        fake.script("/start", Answer::with_status(503, r#"{"error":"blocked"}"#));
        let c = controller(dir.path(), &fake, "echo active");
        let (result, screen) = run(Command::Restart, &c, &[], &Shutdown::new()).await;
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert!(
            screen.contains(
                "› the containers were stopped and stay stopped: the controller refused the start"
            ),
            "{screen}"
        );
        assert!(!screen.contains("may still be"), "{screen}");
    }

    /// Decision 10: the notice on every ending without a final job status,
    /// a start's and a stop's alike; none for a refused request or a failed
    /// job, whose answers are final.
    #[tokio::test]
    async fn every_ending_without_a_final_status_has_the_notice() {
        let _spawns = SPAWNS.lock().await;
        for (command, path, notice) in [
            (Command::Start, "start", STARTING),
            (Command::Stop, "stop", STOPPING),
        ] {
            for how in ["timeout", "failed polls", "timed-out request"] {
                let dir = tempfile::tempdir().expect("dir");
                let fake = FakeController::start().expect("fake");
                let c = non_final(dir.path(), &fake, path, how);
                let (result, screen) = run(command, &c, &[], &Shutdown::new()).await;
                assert!(matches!(result, Err(Exit(1))), "{path} {how}\n{screen}");
                assert!(screen.contains(notice), "{path} {how}\n{screen}");
            }
        }
        // An unknown status, a missing job ID: no final status either.
        let dir = tempfile::tempdir().expect("dir");
        let fake = FakeController::start().expect("fake");
        fake.script("/start", Answer::ok(JOB_T));
        fake.script("/job_status/job-t", status("queued"));
        let c = controller(dir.path(), &fake, "echo active");
        let (_, screen) = run(Command::Start, &c, &[], &Shutdown::new()).await;
        assert!(screen.contains(STARTING), "{screen}");
        let dir = tempfile::tempdir().expect("dir");
        let fake = FakeController::start().expect("fake");
        fake.script("/start", Answer::ok(r#"{"status":"pending"}"#));
        let c = controller(dir.path(), &fake, "echo active");
        let (_, screen) = run(Command::Start, &c, &[], &Shutdown::new()).await;
        assert!(screen.contains(STARTING), "{screen}");
        // Final answers: a refused request, a failed job.
        let dir = tempfile::tempdir().expect("dir");
        let fake = FakeController::start().expect("fake");
        fake.script("/start", Answer::with_status(503, r#"{"error":"blocked"}"#));
        let c = controller(dir.path(), &fake, "echo active");
        let (_, screen) = run(Command::Start, &c, &[], &Shutdown::new()).await;
        assert!(!screen.contains("may still be"), "{screen}");
        let dir = tempfile::tempdir().expect("dir");
        let fake = FakeController::start().expect("fake");
        fake.script("/start", Answer::ok(JOB_T));
        fake.script("/job_status/job-t", status("failed"));
        let c = controller(dir.path(), &fake, "echo active");
        let (_, screen) = run(Command::Start, &c, &[], &Shutdown::new()).await;
        assert!(!screen.contains("may still be"), "{screen}");
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
