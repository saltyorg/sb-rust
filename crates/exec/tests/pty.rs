//! `pty` with real children on a pseudo-terminal, through term-ui's
//! in-memory screen (exec spec, `pty`; plan Task 7). A step whose child
//! fails keeps its last lines in the failure block, which is how a test
//! sees what the child printed.

use std::time::{Duration, Instant};

use indicatif::InMemoryTerm;
use sb_cli_core::cancel::Shutdown;
use sb_exec::{Cmd, Ending, Ran, SpawnCause};
use sb_term_ui::{Labels, Options, Runner, Target};

/// Run `cmd` on a PTY in one step, the step failing when the child does
/// (so its lines stay on screen). Returns the result and the screen.
async fn on_pty(cmd: Cmd, colour: bool) -> (Result<Ran, sb_exec::SpawnError>, String) {
    let term = InMemoryTerm::new(40, 140);
    let options = Options {
        target: Target::InMemory {
            term: term.clone(),
            colour,
        },
    };
    let shutdown = Shutdown::new();
    let mut run = Runner::start("sb test", &shutdown, None, options).await;
    let mut result = None;
    let _ = run
        .step(Labels::new("Running", "ran"), async |step| {
            let done = cmd.pty(step).await;
            let failed = done
                .as_ref()
                .ok()
                .map(|d| d.outcome.ending != Ending::Exited(0));
            result = Some(done);
            if failed == Some(true) {
                anyhow::bail!("the child failed");
            }
            Ok(())
        })
        .await;
    run.finish();
    // With colour, the screen with its escape codes (`contents` renders
    // them away).
    let screen = if colour {
        String::from_utf8_lossy(&term.contents_formatted()).into_owned()
    } else {
        term.contents()
    };
    (result.expect("the step ran"), screen)
}

async fn within(cmd: Cmd) -> (Result<Ran, sb_exec::SpawnError>, String) {
    tokio::time::timeout(Duration::from_secs(20), on_pty(cmd, false))
        .await
        .expect("ended")
}

fn sh(script: &'static str) -> Cmd {
    Cmd::new("sh").arg("-c").arg(script)
}

fn has_line(screen: &str, line: &str) -> bool {
    screen.lines().any(|l| l.trim() == line)
}

/// The child sees a terminal, of the fallback size when stderr isn't one
/// (a test's stderr isn't).
#[tokio::test]
async fn the_child_sees_a_terminal_of_sbs_size() {
    let (done, screen) = within(sh("test -t 0 && test -t 1 && echo tty; stty size; exit 1")).await;
    assert_eq!(done.expect("ran").outcome.ending, Ending::Exited(1));
    assert!(has_line(&screen, "tty"), "{screen}");
    assert!(has_line(&screen, "24 120"), "{screen}");
}

#[tokio::test]
async fn codes_signals_and_spawn_failures() {
    let (done, _) = within(sh("exit 3")).await;
    let err = done.expect("ran").check().expect_err("exit 3");
    assert_eq!(err.reason(), "exit 3");
    assert!(
        err.last_lines().is_empty(),
        "the step's own lines show them"
    );
    let (done, _) = within(sh("kill -TERM $$")).await;
    assert_eq!(done.expect("ran").outcome.ending, Ending::Signalled(15));
    let (again, _) = within(sh("kill -TERM $$")).await;
    let err = again.expect("ran").check().expect_err("signalled");
    assert_eq!(err.reason(), "SIGTERM");
    let (missing, _) = within(Cmd::new("sb-exec-no-such-program")).await;
    let err = missing.expect_err("not found");
    assert!(
        matches!(err.cause(), SpawnCause::Io(io) if io.kind() == std::io::ErrorKind::NotFound),
        "{err:?}"
    );
    assert!(!format!("{err:?}").contains("PATH"), "{err:?}");
    let dir = tempfile::tempdir().expect("temp dir");
    let script = dir.path().join("script");
    std::fs::write(&script, "#!/bin/sh\n").expect("write");
    let (denied, _) = within(Cmd::new(&script)).await;
    let err = denied.expect_err("not executable");
    assert!(
        matches!(err.cause(), SpawnCause::Io(io) if io.kind() == std::io::ErrorKind::PermissionDenied),
        "{err:?}"
    );
}

#[tokio::test]
async fn stdin_bytes_are_refused_and_a_reader_gets_eof() {
    let (refused, _) = within(Cmd::new("cat").stdin("x")).await;
    let err = refused.expect_err("refused");
    assert!(
        matches!(err.cause(), SpawnCause::StdinRefused("pty")),
        "{err:?}"
    );
    let start = Instant::now();
    let (done, _) = within(Cmd::new("cat")).await;
    assert_eq!(done.expect("ran").outcome.ending, Ending::Exited(0));
    let (again, _) = within(Cmd::new("true")).await;
    assert!(again.expect("ran").check().is_ok(), "exit 0 checks out");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
}

/// A missing working directory is an error, not portable-pty's `$HOME`.
#[tokio::test]
async fn a_missing_working_directory_is_an_error() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (err, _) = within(Cmd::new("pwd").cwd(dir.path().join("gone"))).await;
    let err = err.expect_err("no directory");
    assert!(matches!(err.cause(), SpawnCause::NoDirectory(_)), "{err:?}");
    let (done, screen) = within(sh("pwd -P; exit 1").cwd(dir.path())).await;
    assert!(done.is_ok());
    let real = dir.path().canonicalize().expect("canonical");
    assert!(has_line(&screen, real.to_str().expect("utf-8")), "{screen}");
}

/// Lines split from bytes (a character split across writes stays whole),
/// `\r` progress at the next line, a line past 4 KiB cut, a last line
/// without `\n`.
#[tokio::test]
async fn the_ptys_lines_are_split_and_bounded() {
    let script = "printf '\\303'; sleep 0.1; printf '\\251\\n'; \
                  printf '10%%\\r50%%\\r100%%\\n'; \
                  head -c 5000 /dev/zero | tr '\\0' x; echo; printf after; exit 1";
    let (_, screen) = within(sh(script)).await;
    assert!(has_line(&screen, "é"), "{screen}");
    assert!(has_line(&screen, "100%"), "{screen}");
    assert!(
        !screen.lines().any(|l| l.trim().starts_with("10%")),
        "{screen}"
    );
    assert!(has_line(&screen, "after"), "{screen}");
    assert!(screen.contains("xxxx"), "{screen}");
}

#[tokio::test]
async fn secrets_are_taken_out_of_the_lines() {
    let cmd = sh("echo \"$1 $TOKEN\"; exit 1")
        .arg("sh")
        .secret("arg-secret-not-real-1")
        .secret_env("TOKEN", "env-secret-not-real-2");
    let (_, screen) = within(cmd).await;
    assert!(!screen.contains("not-real"), "{screen}");
    assert!(has_line(&screen, "[REDACTED] [REDACTED]"), "{screen}");
}

/// A background process holding the PTY: the run returns after the 2 s
/// read past the child's exit. It ignores SIGHUP (the kernel sends it to
/// the terminal's foreground group when the session's leader exits), set
/// before the leader exits.
#[tokio::test]
async fn a_grandchild_holding_the_pty_is_left_after_the_drain() {
    let start = Instant::now();
    // The pause lets the background subshell set its trap first.
    let script = "(trap '' HUP; exec sleep 3) & sleep 0.3; echo ok";
    let (done, _) = within(sh(script)).await;
    assert_eq!(done.expect("ran").outcome.ending, Ending::Exited(0));
    let waited = start.elapsed();
    assert!(
        waited >= Duration::from_secs(2) && waited < Duration::from_secs(3),
        "{waited:?}"
    );
}

/// `GIT_TERMINAL_PROMPT=0`; `COLORTERM` follows the profile, removed for
/// the plain theme a test runs with.
#[tokio::test]
async fn the_environment_follows_the_rules() {
    let (_, screen) = within(
        sh("env | grep -E '^(GIT_TERMINAL_PROMPT|COLORTERM)='; exit 1")
            .env("COLORTERM", "truecolor"),
    )
    .await;
    assert!(has_line(&screen, "GIT_TERMINAL_PROMPT=0"), "{screen}");
    assert!(!screen.contains("COLORTERM"), "{screen}");
}

/// Colours stay for a coloured target and go otherwise (term-ui's).
#[tokio::test]
async fn colours_follow_the_target() {
    let cmd = || sh("printf '\\033[31mred\\033[0m\\n'; exit 1");
    let (_, coloured) = tokio::time::timeout(Duration::from_secs(20), on_pty(cmd(), true))
        .await
        .expect("ended");
    assert!(coloured.contains("\u{1b}[31m"), "{coloured:?}");
    let (_, plain) = within(cmd()).await;
    assert!(
        !plain.contains("\u{1b}[31m") && has_line(&plain, "red"),
        "{plain:?}"
    );
}

/// The step's token and a timeout stop the session's group; a dropped run
/// leaves nothing of it.
#[tokio::test]
async fn cancellation_and_timeouts_reach_the_group() {
    let start = Instant::now();
    let (done, _) = within(
        Cmd::new("sleep")
            .arg("30")
            .timeout(Duration::from_millis(200))
            .grace(Duration::from_millis(500)),
    )
    .await;
    assert_eq!(done.expect("ran").outcome.ending, Ending::TimedOut);
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "{:?}",
        start.elapsed()
    );

    let term = InMemoryTerm::new(40, 140);
    let options = Options {
        target: Target::InMemory {
            term: term.clone(),
            colour: false,
        },
    };
    let shutdown = Shutdown::new();
    let mut run = Runner::start("sb test", &shutdown, None, options).await;
    let mut ending = None;
    let started = Instant::now();
    let _ = run
        .step(Labels::new("Running", "ran"), async |step| {
            let cmd = Cmd::new("sleep")
                .arg("30")
                .grace(Duration::from_millis(500));
            let (done, ()) = tokio::join!(cmd.pty(step), async {
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
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
}

/// Dropping a `pty` run kills the session's group at once.
#[tokio::test]
async fn a_dropped_pty_run_leaves_no_member() {
    use rustix::process::{Pid, test_kill_process_group};
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("pid");
    let term = InMemoryTerm::new(40, 140);
    let options = Options {
        target: Target::InMemory {
            term,
            colour: false,
        },
    };
    let shutdown = Shutdown::new();
    let mut run = Runner::start("sb test", &shutdown, None, options).await;
    let mut leader = None;
    let _ = run
        .step(Labels::new("Running", "ran"), async |step| {
            let cmd = sh("echo $$ > \"$1\"; exec sleep 30")
                .arg("sh")
                .value(file.to_str().expect("utf-8"));
            let written = async {
                loop {
                    if let Ok(text) = std::fs::read_to_string(&file)
                        && text.ends_with('\n')
                    {
                        return text.trim().parse::<i32>().expect("pid");
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            };
            tokio::select! {
                _ = cmd.pty(step) => panic!("ended on its own"),
                pid = written => leader = Some(pid),
            }
            Ok(())
        })
        .await;
    run.finish();
    let leader = Pid::from_raw(leader.expect("started")).expect("pid");
    let start = Instant::now();
    while test_kill_process_group(leader).is_ok() {
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "the group is left"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A reading child's first read is end of file, not an empty line: a
/// `[Y/n]` prompt must not take its default.
#[tokio::test]
async fn a_reading_child_gets_end_of_file() {
    let (_, screen) = within(sh("read x; echo \"rc=$? x=[$x]\"; exit 1")).await;
    assert!(has_line(&screen, "rc=1 x=[]"), "{screen}");
}

/// The failure block holds exactly the child's lines: nothing echoed by
/// the PTY (the EOF sb sends would show as an empty line).
#[tokio::test]
async fn the_block_holds_only_the_childs_lines() {
    let (_, screen) = within(sh("printf 'only\\n'; exit 1")).await;
    let block: Vec<&str> = screen
        .lines()
        .skip_while(|l| !l.contains("the child failed"))
        .skip(1)
        .map(str::trim)
        .collect();
    assert_eq!(block, ["only"], "{screen}");
}

/// A child ignoring SIGTERM is SIGKILLed after the grace.
#[tokio::test]
async fn a_child_ignoring_sigterm_is_killed() {
    let start = Instant::now();
    let (done, _) = within(
        sh("trap '' TERM; exec sleep 30")
            .timeout(Duration::from_millis(200))
            .grace(Duration::from_millis(300)),
    )
    .await;
    assert_eq!(done.expect("ran").outcome.ending, Ending::TimedOut);
    let took = start.elapsed();
    assert!(
        took >= Duration::from_millis(500) && took < Duration::from_secs(3),
        "{took:?}"
    );
}

/// A cleared environment holds what was set and the rules; portable-pty
/// adds `SHELL` whatever sb asks (its `as_command`).
#[tokio::test]
async fn a_cleared_environment_on_a_pty() {
    let (_, screen) = within(
        Cmd::new("/bin/sh")
            .arg("-c")
            .arg("/usr/bin/env; exit 1")
            .env_clear()
            .env("SB_EXEC_A", "1"),
    )
    .await;
    assert!(has_line(&screen, "SB_EXEC_A=1"), "{screen}");
    assert!(has_line(&screen, "GIT_TERMINAL_PROMPT=0"), "{screen}");
    assert!(!screen.contains("HOME="), "{screen}");
    assert!(screen.contains("SHELL="), "portable-pty's own: {screen}");
}

/// A line far longer than one skipped chunk is cut once; a secret across
/// the cut is found whole.
#[tokio::test]
async fn long_lines_and_secrets_across_the_cut() {
    let (_, screen) = within(sh(
        "head -c 20000 /dev/zero | tr '\\0' w; echo; echo ok; exit 1",
    ))
    .await;
    assert_eq!(
        screen.lines().filter(|l| l.contains("www")).count(),
        1,
        "{screen}"
    );
    assert!(has_line(&screen, "ok"), "{screen}");
    let cmd = sh("head -c 4090 /dev/zero | tr '\\0' x; printf '%s\\n' \"$T\"; exit 1")
        .secret_env("T", "across-the-cut-secret");
    let term = InMemoryTerm::new(40, 5000);
    let options = Options {
        target: Target::InMemory {
            term: term.clone(),
            colour: false,
        },
    };
    let shutdown = Shutdown::new();
    let mut run = Runner::start("sb test", &shutdown, None, options).await;
    let _ = run
        .step(Labels::new("Running", "ran"), async |step| {
            let _ = cmd.pty(step).await?.check()?;
            Ok(())
        })
        .await;
    run.finish();
    let screen = term.contents();
    assert!(!screen.contains("across"), "a part of the secret shows");
    assert!(
        screen.contains("x[REDACTED]"),
        "{}",
        &screen[screen.len().saturating_sub(40)..]
    );
}

/// On a terminal, the PTY takes sb's size: run through util-linux
/// `script`, a re-run of this test with stderr a terminal of 30×100.
#[tokio::test]
async fn the_pty_takes_sbs_terminal_size() {
    if std::env::var_os("SB_EXEC_PTY_CHILD").is_some() {
        let (_, screen) = within(sh("stty size; exit 1")).await;
        println!(
            "{}",
            if has_line(&screen, "30 100") {
                "SIZE-OK"
            } else {
                "SIZE-WRONG"
            }
        );
        return;
    }
    let me = std::env::current_exe().expect("test binary");
    let out = std::process::Command::new("script")
        .args([
            "-qec",
            &format!(
                "stty cols 100 rows 30; exec {} --exact the_pty_takes_sbs_terminal_size --nocapture --test-threads=1",
                me.display()
            ),
            "/dev/null",
        ])
        .env("SB_EXEC_PTY_CHILD", "1")
        .output()
        .expect("run script");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("SIZE-OK"), "{text}");
}

/// With a background process holding the PTY after the child is reaped,
/// the newline sb's writer sends as it closes isn't echoed into the lines.
#[tokio::test]
async fn nothing_sb_writes_is_echoed() {
    let (_, screen) = within(sh(
        "(trap '' HUP; sleep 0.4; echo late) & sleep 0.1; echo first; exit 1",
    ))
    .await;
    let block: Vec<&str> = screen
        .lines()
        .skip_while(|l| !l.contains("the child failed"))
        .skip(1)
        .map(str::trim)
        .collect();
    assert_eq!(block, ["first", "late"], "{screen}");
}
