//! `stream` with real children, through term-ui's in-memory screen (exec
//! spec, "Ways to run", "Lines"; plan Task 6): each line of stdout and
//! stderr goes to the step, secrets taken out; stderr's tail is kept for
//! the caller; the step's token stops the child's group.

use std::time::{Duration, Instant};

use indicatif::InMemoryTerm;
use sb_cli_core::cancel::Shutdown;
use sb_exec::{Cmd, Ending, Streamed};
use sb_term_ui::{Labels, Options, Runner, Target};

/// Run `cmd` streamed in one step on an in-memory screen; returns the
/// result and the screen.
async fn streamed(cmd: Cmd) -> (Result<Streamed, sb_exec::SpawnError>, String) {
    let term = InMemoryTerm::new(40, 120);
    let options = Options {
        target: Target::InMemory {
            term: term.clone(),
            colour: false,
        },
    };
    let shutdown = Shutdown::new();
    let mut run = Runner::start("sb test", &shutdown, None, options);
    let mut result = None;
    let _ = run
        .step(Labels::new("Running", "ran"), async |step| {
            result = Some(cmd.stream(step).await);
            Ok(())
        })
        .await;
    run.finish();
    (result.expect("the step ran"), term.contents())
}

fn sh(script: &'static str) -> Cmd {
    Cmd::new("sh").arg("-c").arg(script)
}

async fn within(cmd: Cmd) -> (Result<Streamed, sb_exec::SpawnError>, String) {
    tokio::time::timeout(Duration::from_secs(20), streamed(cmd))
        .await
        .expect("ended")
}

#[tokio::test]
async fn a_child_that_succeeds_ends_its_step_done() {
    let (done, screen) = within(sh("echo out; echo err >&2; printf 'last'")).await;
    let done = done.expect("ran");
    assert_eq!(done.outcome.ending, Ending::Exited(0));
    // The step ends ✓ and keeps no output; the lines were in its tail
    // while live, and in the log. A failure keeps them (below).
    assert!(screen.contains("✓ ran"), "{screen}");
    assert_eq!(done.stderr_tail, "err\n");
    assert!(done.check().is_ok());
}

/// A failing child's step shows its last lines: the step's own tail.
#[tokio::test]
async fn a_failed_childs_lines_show_as_the_steps_output() {
    let (done, _) = within(sh("echo one; echo two >&2; printf 'three'; exit 3")).await;
    let done = done.expect("ran");
    assert_eq!(done.outcome.ending, Ending::Exited(3));
    let err = done.check().expect_err("exit 3");
    assert_eq!(err.reason(), "exit 3");
    assert!(
        err.last_lines().is_empty(),
        "the step's own lines show them"
    );
    let term = InMemoryTerm::new(40, 120);
    let options = Options {
        target: Target::InMemory {
            term: term.clone(),
            colour: false,
        },
    };
    let shutdown = Shutdown::new();
    let mut run = Runner::start("sb test", &shutdown, None, options);
    let _ = run
        .step(Labels::new("Running", "ran"), async |step| {
            let _ = sh("echo one; echo two >&2; printf 'three'; exit 3")
                .stream(step)
                .await?
                .check()?;
            Ok(())
        })
        .await;
    run.finish();
    let screen = term.contents();
    assert!(screen.contains("exit 3 · 0s"), "{screen}");
    for line in ["one", "two", "three"] {
        assert!(screen.lines().any(|l| l.trim() == line), "{line}: {screen}");
    }
}

#[tokio::test]
async fn a_signal_and_a_spawn_failure() {
    let (done, _) = within(sh("kill -TERM $$")).await;
    assert_eq!(done.expect("ran").outcome.ending, Ending::Signalled(15));
    let (missing, _) = within(Cmd::new("sb-exec-no-such-program")).await;
    assert!(missing.is_err());
}

#[tokio::test]
async fn stdin_is_the_bytes_given_or_null() {
    let (done, _) = within(sh("cat >&2").stdin("fed\n")).await;
    assert_eq!(done.expect("ran").stderr_tail, "fed\n");
    let (done, _) = within(Cmd::new("cat")).await;
    assert_eq!(done.expect("ran").outcome.ending, Ending::Exited(0));
}

/// A line is split from bytes, so a character split across reads stays
/// whole; `\r` progress shows what follows it; a line past 4 KiB is cut
/// there and its rest dropped up to its end.
#[tokio::test]
async fn lines_are_split_from_bytes_and_bounded() {
    let script = "printf '\\303'; sleep 0.1; printf '\\251\\n'; \
                  printf '10%%\\r50%%\\r100%%\\n'; \
                  head -c 5000 /dev/zero | tr '\\0' x; echo; echo after; exit 1";
    // Tall enough for the finished tree, which wraps the 4 KiB line over
    // rows (term-ui spec, "Long lines").
    let term = InMemoryTerm::new(200, 120);
    let options = Options {
        target: Target::InMemory {
            term: term.clone(),
            colour: false,
        },
    };
    let shutdown = Shutdown::new();
    let mut run = Runner::start("sb test", &shutdown, None, options);
    let _ = run
        .step(Labels::new("Running", "ran"), async |step| {
            let _ = sh(script).stream(step).await?.check()?;
            Ok(())
        })
        .await;
    run.finish();
    let screen = term.contents();
    assert!(screen.lines().any(|l| l.trim() == "é"), "{screen}");
    assert!(screen.lines().any(|l| l.trim() == "100%"), "{screen}");
    // Only what follows the last `\r` (the headline quotes the script).
    assert!(
        !screen.lines().any(|l| l.trim().starts_with("10%")),
        "{screen}"
    );
    assert!(screen.lines().any(|l| l.trim() == "after"), "{screen}");
    // The 4 KiB line, cut to the screen's width there; its length is
    // checked through the tail below.
    let (long, _) = within(sh(
        "head -c 5000 /dev/zero | tr '\\0' y >&2; echo >&2; echo ok >&2",
    ))
    .await;
    let tail = long.expect("ran").stderr_tail;
    let lines: Vec<&str> = tail.lines().collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0].len(), 4096);
    assert_eq!(lines[1], "ok");
}

/// stderr's last 64 KiB are kept for the caller (apt's retry match).
#[tokio::test]
async fn stderrs_tail_is_kept() {
    let script =
        "i=0; while [ $i -lt 4000 ]; do echo \"line $i of stderr's tail\" >&2; i=$((i+1)); done";
    let (done, _) = within(sh(script)).await;
    let tail = done.expect("ran").stderr_tail;
    assert!(tail.len() <= 64 * 1024, "{}", tail.len());
    assert!(
        tail.ends_with("line 3999 of stderr's tail\n"),
        "{}",
        &tail[tail.len() - 40..]
    );
    assert!(
        !tail.contains("\nline 0 of") && !tail.starts_with("line 0 of"),
        "the front is dropped"
    );
    assert!(tail.len() > 60 * 1024, "{}", tail.len());
    assert!(tail.starts_with("line "), "whole lines: {}", &tail[..20]);
}

#[tokio::test]
async fn secrets_are_taken_out_of_the_lines() {
    let cmd = sh("echo \"$1 $TOKEN\"; echo \"$TOKEN\" >&2; exit 1")
        .arg("sh")
        .secret("arg-secret-not-real-1")
        .secret_env("TOKEN", "env-secret-not-real-2");
    let (done, _) = within(cmd).await;
    let done = done.expect("ran");
    assert_eq!(done.stderr_tail, "[REDACTED]\n");
    let shown = format!("{done:?}");
    assert!(!shown.contains("not-real"), "{shown}");
}

/// A background grandchild holding stdout: the run returns after the 2 s
/// read past the child's exit.
#[tokio::test]
async fn a_grandchild_holding_stdout_is_left_after_the_drain() {
    let start = Instant::now();
    let (done, _) = within(sh("sleep 3 & echo ok")).await;
    assert_eq!(done.expect("ran").outcome.ending, Ending::Exited(0));
    let waited = start.elapsed();
    assert!(
        waited >= Duration::from_secs(2) && waited < Duration::from_secs(3),
        "{waited:?}"
    );
}

/// A final line without `\n` still arrives when the drain ends the read
/// because a grandchild holds the pipe (exec-L2), on stdout and stderr.
#[tokio::test]
async fn a_last_unterminated_line_arrives_after_the_drain() {
    const SCRIPT: &str =
        "sleep 30 & echo first >&2; printf 'last-of-stderr' >&2; printf 'last-of-stdout'; exit 1";
    let (done, _) = within(sh(SCRIPT)).await;
    let done = done.expect("ran");
    assert_eq!(done.outcome.ending, Ending::Exited(1));
    assert_eq!(done.stderr_tail, "first\nlast-of-stderr\n");
    // A failed step keeps its lines on screen: stdout's last one too.
    let term = InMemoryTerm::new(40, 120);
    let options = Options {
        target: Target::InMemory {
            term: term.clone(),
            colour: false,
        },
    };
    let shutdown = Shutdown::new();
    let mut run = Runner::start("sb test", &shutdown, None, options);
    let _ = run
        .step(Labels::new("Running", "ran"), async |step| {
            let _ = sh(SCRIPT).stream(step).await?.check()?;
            Ok(())
        })
        .await;
    run.finish();
    let screen = term.contents();
    for line in ["first", "last-of-stderr", "last-of-stdout"] {
        assert!(screen.lines().any(|l| l.trim() == line), "{line}: {screen}");
    }
}

/// A grandchild still writing when the drain ends the read: a reader cut
/// while it waits to hand a line over keeps no line twice in the tail.
#[tokio::test]
async fn a_line_cut_off_by_the_drain_is_kept_once() {
    let (done, _) = within(sh(
        "(i=0; while :; do i=$((i+1)); echo \"n$i\"; done) >&2 & exit 1",
    ))
    .await;
    let tail = done.expect("ran").stderr_tail;
    let lines: Vec<&str> = tail.lines().collect();
    assert!(lines.len() > 100, "{}", lines.len());
    for pair in lines.windows(2) {
        assert_ne!(pair[0], pair[1], "kept twice");
    }
}

/// The step's token (Ctrl-C, a failing sibling) stops the child's group.
#[tokio::test]
async fn the_steps_token_stops_the_child() {
    let term = InMemoryTerm::new(40, 120);
    let options = Options {
        target: Target::InMemory {
            term: term.clone(),
            colour: false,
        },
    };
    let shutdown = Shutdown::new();
    let mut run = Runner::start("sb test", &shutdown, None, options);
    let start = Instant::now();
    let mut ending = None;
    let _ = run
        .step(Labels::new("Running", "ran"), async |step| {
            let cmd = Cmd::new("sleep")
                .arg("30")
                .grace(Duration::from_millis(500));
            let (done, ()) = tokio::join!(cmd.stream(step), async {
                tokio::time::sleep(Duration::from_millis(200)).await;
                shutdown.signalled(2);
            });
            ending = Some(done.expect("ran").outcome.ending);
            Err::<(), _>(anyhow::anyhow!("interrupted"))
        })
        .await;
    run.finish();
    assert_eq!(ending, Some(Ending::Interrupted));
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );
    assert!(
        term.contents().contains("interrupted"),
        "{}",
        term.contents()
    );
}

#[tokio::test]
async fn the_environment_follows_the_rules() {
    let (done, _) = within(sh("env >&2").env("SB_EXEC_A", "1")).await;
    let tail = done.expect("ran").stderr_tail;
    assert!(tail.lines().any(|l| l == "SB_EXEC_A=1"), "{tail}");
    assert!(tail.lines().any(|l| l == "GIT_TERMINAL_PROMPT=0"), "{tail}");
    assert!(
        tail.lines().any(|l| l == "SSH_ASKPASS=/bin/false"),
        "{tail}"
    );
    assert!(
        tail.lines().any(|l| l == "SSH_ASKPASS_REQUIRE=force"),
        "{tail}"
    );
}

/// The step's lines, kept in its failure block, show the placeholder in
/// the secrets' place.
#[tokio::test]
async fn secrets_are_taken_out_of_the_steps_lines() {
    let term = InMemoryTerm::new(40, 120);
    let options = Options {
        target: Target::InMemory {
            term: term.clone(),
            colour: false,
        },
    };
    let shutdown = Shutdown::new();
    let mut run = Runner::start("sb test", &shutdown, None, options);
    let _ = run
        .step(Labels::new("Running", "ran"), async |step| {
            let _ = sh("echo \"$1 $TOKEN\"; exit 1")
                .arg("sh")
                .secret("arg-secret-not-real-1")
                .secret_env("TOKEN", "env-secret-not-real-2")
                .stream(step)
                .await?
                .check()?;
            Ok(())
        })
        .await;
    run.finish();
    let screen = term.contents();
    assert!(!screen.contains("not-real"), "{screen}");
    assert!(
        screen.lines().any(|l| l.trim() == "[REDACTED] [REDACTED]"),
        "{screen}"
    );
}

/// The tail's lines, as kept for the caller.
async fn tail_of(script: &'static str) -> String {
    within(sh(script)).await.0.expect("ran").stderr_tail
}

/// A line past 4 KiB with no newline at the end of the pipe is still
/// delivered, cut; one longer than a skipped chunk is dropped whole past
/// its cut, so its rest never comes back as lines of its own.
#[tokio::test]
async fn over_long_lines_are_cut_once() {
    let tail = tail_of("head -c 5000 /dev/zero | tr '\\0' z >&2").await;
    assert_eq!(tail.len(), 4096 + 1, "a cut last line and its newline");
    let tail = tail_of("head -c 20000 /dev/zero | tr '\\0' w >&2; echo >&2; echo ok >&2").await;
    let lines: Vec<usize> = tail.lines().map(str::len).collect();
    assert_eq!(lines, [4096, 2], "{lines:?}");
    assert!(tail.ends_with("\nok\n"));
}

/// A secret across the 4 KiB cut is found whole and shown as the
/// placeholder, whether it starts well before the cut or just before it.
#[tokio::test]
async fn a_secret_across_a_lines_cut_is_masked() {
    for (pad, script) in [
        (
            4090,
            "head -c 4090 /dev/zero | tr '\\0' x >&2; printf '%s\\n' \"$T\" >&2",
        ),
        (
            4095,
            "head -c 4095 /dev/zero | tr '\\0' x >&2; printf '%s\\n' \"$T\" >&2",
        ),
    ] {
        let cmd = sh(script).secret_env("T", "across-the-cut-secret");
        let tail = within(cmd).await.0.expect("ran").stderr_tail;
        assert!(
            !tail.contains("across"),
            "{pad}: a part of the secret shows"
        );
        assert!(
            tail.trim_end().ends_with("x[REDACTED]"),
            "{pad}: {}",
            &tail[tail.len() - 20..]
        );
    }
}

/// A secret ending in a newline (read from a file) is masked although the
/// line it is printed on loses that newline.
#[tokio::test]
async fn a_secret_with_a_newline_is_masked_in_lines() {
    let cmd = sh("printf '%s' \"$T\" >&2").secret_env("T", "tok-secret-not-real\n");
    let tail = within(cmd).await.0.expect("ran").stderr_tail;
    assert_eq!(tail, "[REDACTED]\n");
}

/// A child that writes many lines as it stops (it catches SIGTERM): none
/// of them is lost when the stop ends the run.
#[tokio::test]
async fn a_stopped_childs_last_lines_are_kept() {
    for run in 0..5 {
        let term = InMemoryTerm::new(40, 120);
        let options = Options {
            target: Target::InMemory {
                term: term.clone(),
                colour: false,
            },
        };
        let shutdown = Shutdown::new();
        let mut started = Runner::start("sb test", &shutdown, None, options);
        let mut tail = None;
        let _ = started
            .step(Labels::new("Running", "ran"), async |step| {
                let cmd = sh("trap 'i=0; while [ $i -lt 300 ]; do echo t$i >&2; i=$((i+1)); done; exit 0' TERM; \
                              echo ready >&2; while :; do sleep 0.05; done")
                    .grace(Duration::from_secs(5));
                let token = step.token();
                let (done, ()) = tokio::join!(cmd.stream(step), async {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    token.cancel();
                });
                tail = Some(done.expect("ran").stderr_tail);
                Ok(())
            })
            .await;
        started.finish();
        let tail = tail.expect("ran");
        assert!(
            tail.ends_with("t299\n"),
            "run {run}: {}",
            &tail[tail.len().saturating_sub(30)..]
        );
        assert_eq!(
            tail.lines().filter(|l| l.starts_with('t')).count(),
            300,
            "run {run}"
        );
    }
}

/// A character split by the 4 KiB cut is dropped, not shown as U+FFFD.
#[tokio::test]
async fn a_cut_never_splits_a_character() {
    let tail =
        tail_of("head -c 4095 /dev/zero | tr '\\0' x >&2; printf '\\303\\251more\\n' >&2").await;
    assert_eq!(tail.trim_end().len(), 4095, "{}", &tail[4090..]);
    assert!(!tail.contains('\u{fffd}'));
}

/// A line of nothing but UTF-8 continuation bytes (not text at all): the
/// cut backs up to its start and shows nothing, without failing.
#[tokio::test]
async fn a_cut_line_of_stray_bytes_shows_nothing() {
    let tail =
        tail_of("head -c 5000 /dev/zero | tr '\\0' '\\200' >&2; echo >&2; echo ok >&2").await;
    assert_eq!(tail, "\nok\n");
}
