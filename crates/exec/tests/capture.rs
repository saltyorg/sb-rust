//! `capture` with real children (exec spec, "Ways to run", "Outcome and
//! errors", "Environment rules"; plan Task 3): `sh -c` scripts stand in for
//! tools.

use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, getpgid, kill_process_group};
use sb_exec::{Captured, Cmd, Ending, SpawnCause, SpawnError};
use tokio_util::sync::CancellationToken;

/// Set in a re-run of this binary that runs one test's child side.
const CHILD: &str = "SB_EXEC_CHILD";

const MIB: usize = 1 << 20;

/// `cmd` captured, failing the test rather than hanging it.
async fn capture(cmd: &Cmd) -> Result<Captured, SpawnError> {
    tokio::time::timeout(
        Duration::from_secs(20),
        cmd.capture(&CancellationToken::new()),
    )
    .await
    .expect("capture ended")
}

fn sh(script: &'static str) -> Cmd {
    Cmd::new("sh").arg("-c").arg(script)
}

#[tokio::test]
async fn exit_codes_are_the_childs() {
    let done = capture(&sh("echo out; echo err >&2")).await.expect("ran");
    assert_eq!(done.outcome.ending, Ending::Exited(0));
    assert_eq!(done.stdout, "out\n");
    assert_eq!(done.stderr, "err\n");
    assert!(!done.truncated);
    assert!(done.check().is_ok());
    let failed = capture(&sh("exit 3")).await.expect("ran");
    assert_eq!(failed.outcome.ending, Ending::Exited(3));
    let err = failed.check().expect_err("exit 3 fails");
    assert_eq!(err.to_string(), "sh -c exit 3 exited with code 3");
    assert_eq!(err.reason(), "exit 3");
}

#[tokio::test]
async fn a_signal_is_named() {
    let killed = capture(&sh("kill -TERM $$")).await.expect("ran");
    assert_eq!(killed.outcome.ending, Ending::Signalled(15));
    let err = killed.check().expect_err("a signal fails");
    assert_eq!(err.to_string(), "sh -c kill -TERM $$ was killed by SIGTERM");
}

/// The last lines of stderr, at most ten, come with the failure.
#[tokio::test]
async fn a_failure_carries_stderrs_last_lines() {
    let failed = capture(&sh("seq 1 12 >&2; echo only-out; exit 1"))
        .await
        .expect("ran");
    let err = failed.check().expect_err("fails");
    let expected: Vec<String> = (3..=12).map(|n| n.to_string()).collect();
    assert_eq!(err.last_lines(), expected);
}

/// A line past 4 KiB is cut there in the failure (term-ui's cut).
#[tokio::test]
async fn a_failures_long_line_is_cut() {
    let failed = capture(&sh("head -c 5000 /dev/zero | tr '\\0' e >&2; exit 1"))
        .await
        .expect("ran");
    let err = failed.check().expect_err("fails");
    assert_eq!(err.last_lines(), ["e".repeat(4096)]);
}

#[tokio::test]
async fn a_missing_program_is_a_spawn_error() {
    let err = capture(&Cmd::new("sb-exec-no-such-program"))
        .await
        .expect_err("not found");
    assert_eq!(err.to_string(), "could not run sb-exec-no-such-program");
    match err.cause() {
        SpawnCause::Io(io) => assert_eq!(io.kind(), std::io::ErrorKind::NotFound),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_file_that_isnt_executable_is_a_spawn_error() {
    let dir = tempfile::tempdir().expect("temp dir");
    let script = dir.path().join("script");
    std::fs::write(&script, "#!/bin/sh\necho no\n").expect("write");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    let err = capture(&Cmd::new(&script))
        .await
        .expect_err("not executable");
    match err.cause() {
        SpawnCause::Io(io) => assert_eq!(io.kind(), std::io::ErrorKind::PermissionDenied),
        other => panic!("{other:?}"),
    }
}

/// Checked before spawning: a missing directory would otherwise read as
/// the program not being found.
#[tokio::test]
async fn a_missing_working_directory_is_a_spawn_error() {
    let dir = tempfile::tempdir().expect("temp dir");
    let gone = dir.path().join("gone");
    let err = capture(&Cmd::new("true").cwd(&gone))
        .await
        .expect_err("no directory");
    assert_eq!(err.to_string(), "could not run true");
    assert_eq!(
        err.cause().to_string(),
        format!("no such working directory: {}", gone.display())
    );
    let file = dir.path().join("file");
    std::fs::write(&file, "").expect("write");
    let err = capture(&Cmd::new("true").cwd(&file))
        .await
        .expect_err("not a directory");
    assert!(matches!(err.cause(), SpawnCause::NoDirectory(_)), "{err:?}");
    // Not looked at at all (a path through a file): the reason is kept.
    let through = file.join("below");
    let err = capture(&Cmd::new("true").cwd(&through))
        .await
        .expect_err("can't look");
    let SpawnCause::Directory { dir, cause } = err.cause() else {
        panic!("{err:?}")
    };
    assert_eq!(dir, &through);
    assert_eq!(cause.kind(), std::io::ErrorKind::NotADirectory);
    assert_eq!(
        err.cause().to_string(),
        format!("working directory {}", through.display())
    );
}

#[tokio::test]
async fn the_child_runs_in_its_working_directory() {
    let dir = tempfile::tempdir().expect("temp dir");
    let done = capture(&Cmd::new("pwd").arg("-P").cwd(dir.path()))
        .await
        .expect("ran");
    let real = dir.path().canonicalize().expect("canonical");
    assert_eq!(done.stdout.trim_end(), real.to_str().expect("utf-8"));
}

#[tokio::test]
async fn stdin_is_the_bytes_given_or_null() {
    let done = capture(&Cmd::new("cat").stdin("hello\n"))
        .await
        .expect("ran");
    assert_eq!(done.stdout, "hello\n");
    // Null: `cat` reads EOF at once instead of waiting on a terminal.
    let done = capture(&Cmd::new("cat")).await.expect("ran");
    assert_eq!(done.stdout, "");
    // A child that doesn't read its stdin is fine, even past a pipe's
    // buffer.
    let done = capture(&Cmd::new("true").stdin(vec![b'x'; 4 * MIB]))
        .await
        .expect("ran");
    assert_eq!(done.outcome.ending, Ending::Exited(0));
}

/// stdout keeps its first 1 MiB, stderr its last; reading goes on past the
/// cap, so the child isn't blocked on a full pipe.
#[tokio::test]
async fn output_past_the_cap_is_cut_and_marked() {
    let done = capture(&sh("head -c 2097152 /dev/zero | tr '\\0' a; printf END; \
         printf START >&2; head -c 2097152 /dev/zero | tr '\\0' b >&2; printf END >&2"))
    .await
    .expect("ran");
    assert_eq!(done.outcome.ending, Ending::Exited(0));
    assert!(done.truncated);
    assert_eq!(done.stdout.len(), MIB);
    assert!(done.stdout.bytes().all(|b| b == b'a'));
    assert_eq!(done.stderr.len(), MIB);
    assert!(
        done.stderr.ends_with("bbbEND"),
        "{}",
        &done.stderr[MIB - 10..]
    );
    assert!(!done.stderr.contains("START"));
}

/// stdout alone one byte past the cap, no secret: still marked.
#[tokio::test]
async fn only_stdout_past_the_cap_marks_it() {
    let done = capture(&sh("head -c 1048577 /dev/zero | tr '\\0' a"))
        .await
        .expect("ran");
    assert!(done.truncated);
    assert_eq!(done.stdout.len(), MIB);
}

/// A secret across the start of stderr's kept tail leaves no part either.
#[tokio::test]
async fn a_secret_across_stderrs_cut_leaves_no_part() {
    let cmd = sh("printf '%s' \"$TOKEN\" >&2; head -c 1048570 /dev/zero | tr '\\0' b >&2")
        .secret_env("TOKEN", "across-the-cut-secret");
    let done = capture(&cmd).await.expect("ran");
    assert!(done.truncated);
    assert!(
        done.stderr.starts_with("[REDACTED]bbb"),
        "{}",
        &done.stderr[..20]
    );
    assert!(
        !done.stderr.contains("secret"),
        "a part of the secret shows"
    );
}

#[tokio::test]
async fn only_stderr_past_the_cap_still_marks_it() {
    let done = capture(&sh("head -c 1048577 /dev/zero | tr '\\0' b >&2"))
        .await
        .expect("ran");
    assert!(done.truncated);
    assert_eq!(done.stderr.len(), MIB);
    let exact = capture(&sh("head -c 1048576 /dev/zero | tr '\\0' a"))
        .await
        .expect("ran");
    assert!(!exact.truncated);
    assert_eq!(exact.stdout.len(), MIB);
}

/// A background grandchild holding stdout open: the output is read for 2 s
/// after the child exits, then capture returns.
#[tokio::test]
async fn a_grandchild_holding_stdout_is_left_after_the_drain() {
    let start = Instant::now();
    let done = capture(&sh("echo $$; sleep 30 & echo $!; exit 0"))
        .await
        .expect("ran");
    let waited = start.elapsed();
    let pids: Vec<i32> = done
        .stdout
        .lines()
        .map(|l| l.parse().expect("pid"))
        .collect();
    let [leader, sleeper] = pids[..] else {
        panic!("{pids:?}")
    };
    // The child had a process group of its own, which the grandchild is
    // still in; only then is the group killed, before the other checks,
    // so a failing one doesn't leave the sleep behind.
    let sleeper = Pid::from_raw(sleeper).expect("pid");
    let group = Pid::from_raw(leader).expect("pid");
    assert_eq!(getpgid(Some(sleeper)).expect("getpgid"), group);
    kill_process_group(group, Signal::KILL).expect("kill the group");
    assert!(
        waited >= Duration::from_secs(2) && waited < Duration::from_secs(10),
        "{waited:?}"
    );
    assert_eq!(done.outcome.ending, Ending::Exited(0));
}

/// Every `secret` argument and `secret_env` value is taken out of stdout,
/// stderr and the error; a shown value stays in both.
#[tokio::test]
async fn secrets_are_taken_out_of_output_and_errors() {
    let cmd = sh("echo \"$1 $TOKEN $2\"; echo \"$1 $TOKEN\" >&2; exit 1")
        .arg("sh")
        .secret("arg-secret-not-real-1")
        .shown_value("shown-value")
        .secret_env("TOKEN", "env-secret-not-real-3");
    let done = capture(&cmd).await.expect("ran");
    assert_eq!(done.stdout, "[REDACTED] [REDACTED] shown-value\n");
    assert_eq!(done.stderr, "[REDACTED] [REDACTED]\n");
    let err = done.check().expect_err("exit 1");
    let shown = format!("{err} {:?} {err:?}", err.last_lines());
    for hidden in ["arg-secret-not-real-1", "env-secret-not-real-3"] {
        assert!(!shown.contains(hidden), "{hidden}: {shown}");
    }
    assert!(
        err.to_string()
            .ends_with("sh [REDACTED] shown-value exited with code 1"),
        "{err}"
    );
    assert_eq!(err.last_lines(), ["[REDACTED] [REDACTED]"]);
}

/// A joined option (ansible's `--extra-vars=<value>`, one `shown_value`)
/// reaches the child as one element, byte for byte, JSON and a leading
/// `-` included; the error shows it as given.
#[tokio::test]
async fn a_joined_option_is_one_element_and_shown() {
    let cmd = sh("printf '%s\\n' \"$#\" \"$@\"; exit 2")
        .arg("sh")
        .shown_value(r#"--extra-vars={"a": [1, "b c"], "d": "-e"}"#)
        .shown_value("--x=-leading value");
    let done = capture(&cmd).await.expect("ran");
    assert_eq!(
        done.stdout,
        "2\n--extra-vars={\"a\": [1, \"b c\"], \"d\": \"-e\"}\n--x=-leading value\n"
    );
    let err = done.check().expect_err("exit 2");
    assert!(
        err.to_string().ends_with(
            r#"sh --extra-vars={"a": [1, "b c"], "d": "-e"} --x=-leading value exited with code 2"#
        ),
        "{err}"
    );
}

/// A secret crossing the cap is cut out whole, not left in part.
#[tokio::test]
async fn a_secret_across_the_cap_leaves_no_part() {
    let cmd = sh("head -c 1048570 /dev/zero | tr '\\0' a; echo \"$TOKEN tail\"")
        .secret_env("TOKEN", "across-the-cap-secret");
    let done = capture(&cmd).await.expect("ran");
    assert!(done.truncated);
    assert!(
        done.stdout.ends_with("aaa[REDACTED]"),
        "{}",
        &done.stdout[done.stdout.len() - 20..]
    );
    assert!(
        !done.stdout.contains("across"),
        "a part of the secret shows"
    );
}

#[tokio::test]
async fn the_environment_follows_the_rules() {
    let done = capture(&Cmd::new("env").env("SB_EXEC_A", "1").env_remove("HOME"))
        .await
        .expect("ran");
    let vars: Vec<&str> = done.stdout.lines().collect();
    assert!(vars.contains(&"SB_EXEC_A=1"), "{vars:?}");
    assert!(vars.contains(&"GIT_TERMINAL_PROMPT=0"), "{vars:?}");
    assert!(!vars.iter().any(|v| v.starts_with("HOME=")), "{vars:?}");
    assert!(
        vars.iter().any(|v| v.starts_with("PATH=")),
        "inherited: {vars:?}"
    );
    let cleared = capture(&Cmd::new("/usr/bin/env").env_clear().env("SB_EXEC_B", "2"))
        .await
        .expect("ran");
    let mut vars: Vec<&str> = cleared.stdout.lines().collect();
    vars.sort_unstable();
    assert_eq!(
        vars,
        [
            "GIT_TERMINAL_PROMPT=0",
            "SB_EXEC_B=2",
            "SSH_ASKPASS=/bin/false",
            "SSH_ASKPASS_REQUIRE=force"
        ]
    );
}

/// `COLORTERM` is set for pty and attached children only (decision 3): a
/// captured child keeps the inherited one as it is. sb's environment is
/// the re-run child's, set by the parent (the test process's own isn't
/// changed: `set_var` is unsafe).
#[tokio::test]
async fn an_inherited_colorterm_is_left_as_it_is() {
    if std::env::var_os(CHILD).is_some() {
        let done = capture(&Cmd::new("env")).await.expect("ran");
        print!("{}", done.stdout);
        return;
    }
    let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "an_inherited_colorterm_is_left_as_it_is",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .env("COLORTERM", "truecolor")
        .output()
        .expect("run child");
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.lines().any(|l| l == "COLORTERM=truecolor"),
        "{stdout}"
    );
}
