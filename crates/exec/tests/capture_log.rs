//! The run-log records of a captured child (exec spec, "Logging"; plan
//! Tasks 2 and 3): the start record (the command with literals in place,
//! the count of masked arguments, changed variable names, the working
//! directory, the way) and the end record (outcome, elapsed), with no
//! secret and no pass-through value in either. A test binary of its own:
//! tracing caches each callsite's interest process-wide
//! (https://docs.rs/tracing-core/0.1.36/tracing_core/callsite/index.html),
//! so a log test sharing a process with tests that run without a
//! subscriber misses events (as sb-config's tests/run_log.rs).

use std::io::Write;
use std::sync::{Arc, Mutex};

use sb_exec::Cmd;
use tokio_util::sync::CancellationToken;

/// A log writer the test can read back (sb-config's tests/run_log.rs).
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("lock")).into_owned()
    }
}

impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("lock").extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn the_records_name_the_child_and_keep_its_secrets() {
    let log = Captured::default();
    let writer = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("subscriber");
    let dir = tempfile::tempdir().expect("temp dir");
    let cmd = Cmd::new("sh")
        .arg("-c")
        .arg("echo \"$1 $TOKEN $2\"; exit 4")
        .arg("sh")
        .secret("arg-secret-not-real-1")
        .value("pass-through-2")
        .secret_env("TOKEN", "env-secret-not-real-3")
        .env("SB_EXEC_PLAIN", "plain-value-4")
        .cwd(dir.path());
    let done = cmd.capture(&CancellationToken::new()).await.expect("ran");
    assert_eq!(done.outcome.ending, sb_exec::Ending::Exited(4));
    let text = log.text();
    let start = text
        .lines()
        .find(|l| l.contains("child started"))
        .unwrap_or_else(|| panic!("no start record: {text}"));
    for field in [
        r#"command="sh -c echo \"$1 $TOKEN $2\"; exit 4 sh [REDACTED] [ARG]""#,
        "masked=2",
        r#"env="GIT_TERMINAL_PROMPT,SB_EXEC_PLAIN,TOKEN""#,
        "env_cleared=false",
        &format!("cwd=\"{}\"", dir.path().display()),
        "way=\"capture\"",
    ] {
        assert!(start.contains(field), "{field}: {start}");
    }
    let end = text
        .lines()
        .find(|l| l.contains("child ended"))
        .unwrap_or_else(|| panic!("no end record: {text}"));
    assert!(end.contains(r"outcome=exited with code 4"), "{end}");
    assert!(end.contains("elapsed_ms="), "{end}");
    // A child that never starts says so, and claims no start.
    let missing = Cmd::new("sb-exec-no-such-program").secret("missing-secret-5");
    missing
        .capture(&CancellationToken::new())
        .await
        .expect_err("not found");
    let text = log.text();
    let not_started = text
        .lines()
        .find(|l| l.contains("child not started"))
        .unwrap_or_else(|| panic!("no record of the failure: {text}"));
    assert!(
        not_started.contains(r#"command="sb-exec-no-such-program [REDACTED]""#),
        "{not_started}"
    );
    assert_eq!(text.matches("child started").count(), 1, "{text}");
    // A working directory that can't be looked at: the reason reaches the
    // log too, not only its headline.
    let through = dir.path().join("file");
    std::fs::write(&through, "").expect("write");
    Cmd::new("true")
        .cwd(through.join("below"))
        .capture(&CancellationToken::new())
        .await
        .expect_err("not a directory");
    let text = log.text();
    let unreachable = text
        .lines()
        .filter(|l| l.contains("child not started"))
        .nth(1)
        .unwrap_or_else(|| panic!("no record of the second failure: {text}"));
    assert!(unreachable.contains("Not a directory"), "{unreachable}");
    // A streamed child's lines reach the log through term-ui's step, the
    // secrets already out of them.
    let term = indicatif::InMemoryTerm::new(20, 100);
    let options = sb_term_ui::Options {
        target: sb_term_ui::Target::InMemory {
            term,
            colour: false,
        },
    };
    let shutdown = sb_cli_core::cancel::Shutdown::new();
    let mut run = sb_term_ui::Runner::start("sb test", &shutdown, None, options).await;
    let _ = run
        .step(sb_term_ui::Labels::new("Running", "ran"), async |step| {
            // Only the log matters here, not the outcome.
            let _ = Cmd::new("sh")
                .arg("-c")
                .arg("echo \"streamed $TOKEN\"")
                .secret_env("TOKEN", "stream-secret-not-real-6")
                .stream(step)
                .await?;
            Ok(())
        })
        .await;
    run.finish();
    let text = log.text();
    assert!(text.contains("streamed [REDACTED]"), "{text}");
    assert!(text.contains(r#"way="stream""#), "{text}");
    for hidden in [
        "stream-secret-not-real-6",
        "missing-secret-5",
        "arg-secret-not-real-1",
        "pass-through-2",
        "env-secret-not-real-3",
        "plain-value-4",
    ] {
        assert!(!text.contains(hidden), "{hidden} in the log: {text}");
    }
}
