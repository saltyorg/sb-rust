//! The runner's run-log records (term-ui spec, "Run log"; CONSTRAINTS.md,
//! logging). A test binary of its own: tracing caches each callsite's
//! interest process-wide
//! (https://docs.rs/tracing-core/0.1.36/tracing_core/callsite/index.html),
//! so a log test sharing a process with tests that run without a
//! subscriber missed events.

use std::io::Write;
use std::sync::{Arc, Mutex};

use sb_cli_core::cancel::Shutdown;
use sb_term_ui::{Labels, Line, Options, Runner};
use tracing::Instrument;

/// A known secret, marked sensitive wherever the steps print it.
const SECRET: &str = "hunter2-not-a-real-token";

/// A log writer the test can read back.
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

/// Every step's start, notices, output and outcome go to the run log; a
/// known secret marked sensitive never does.
#[tokio::test]
async fn steps_are_logged_and_sensitive_text_is_withheld() {
    let log = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        // tracing-subscriber's `MakeWriter` for a closure returning a writer.
        .with_writer({
            let log = log.clone();
            move || log.clone()
        })
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    // The run's span, current only while the test's own future is polled
    // (`Instrument`, not `entered`): a sibling's task, polled on its own,
    // has the span only because the runner carries it there.
    let span = tracing::info_span!("run", run = "t");
    run_steps().instrument(span).await;
    check(&log.text());
}

/// The steps, run inside the test's span.
async fn run_steps() {
    let secret = SECRET;
    // Colour on, so the screen keeps colour codes and the log must strip
    // them itself (safe: this binary holds one test).
    anstream::ColorChoice::Always.write_global();
    let shutdown = Shutdown::new();
    // Live or plain, as the test's stderr is (a run from a terminal draws
    // live): only the log is checked, which both modes write the same.
    let options = Options::default();
    let mut run = Runner::start("sb demo", &shutdown, None, options).await;
    let labels = Labels::new("Checking credentials", "credentials valid");
    run.step(labels, async |step| {
        step.info("3 accounts");
        step.warn("unknown key: foo");
        step.output("checking item 1");
        step.output("\x1b[32mgreen\x1b[0m");
        step.output("w".repeat(300));
        step.output(Line::new("ok, token=").secret(secret));
        // A secret spanning lines is masked whole, before the split.
        step.output(Line::new("key: ").secret(format!("{secret}\nsecond-line")));
        step.warn(Line::new("bad token: ").secret(secret));
        // Shown to the user on purpose, its value still never logged.
        step.info(Line::new("your token: ").secret(secret).shown());
        Ok(())
    })
    .await
    .expect("step");
    let failing =
        async |_: &mut sb_term_ui::Step| -> anyhow::Result<()> { anyhow::bail!("timeout") };
    let _ = run.step(Labels::new("Pulling", "pulled"), failing).await;
    let sensitive = async |_: &mut sb_term_ui::Step| -> anyhow::Result<()> {
        let marked = sb_cli_core::log::Sensitive::new(std::io::Error::other(format!("t={secret}")));
        Err(anyhow::Error::new(marked).context("the token was refused"))
    };
    let _ = run
        .step(Labels::new("Checking token", "token valid"), sensitive)
        .await;
    // Concurrent siblings run as tasks; their records keep the run's span.
    run.step(Labels::new("Checking both", "both checked"), async |step| {
        let mut group = step.group();
        group.spawn(Labels::new("Sibling A", "a done"), |mut step| async move {
            step.output("from a task");
            Ok(())
        });
        group.join().await?;
        Ok(())
    })
    .await
    .expect("group");
    // A body cancelled by the command's own request: "stopped", and its
    // error still in the record (never shown).
    let stopping = async |_: &mut sb_term_ui::Step| -> anyhow::Result<()> {
        shutdown.request(0);
        anyhow::bail!("asked to stop")
    };
    let _ = run.step(Labels::new("Stopping", "stopped"), stopping).await;
    let _ = run
        .step(Labels::new("Late", "late"), async |_| Ok(()))
        .await;
    run.finish();
}

/// What the log must and mustn't hold.
fn check(text: &str) {
    let secret = SECRET;
    assert!(!text.contains(secret), "{text}");
    let line_with = |part: &str| {
        text.lines()
            .find(|l| l.contains(part))
            .unwrap_or_else(|| panic!("{part:?} missing from:\n{text}"))
            .to_string()
    };
    assert!(line_with("step started step=\"Checking credentials\"").contains("run{run=\"t\"}"));
    assert!(line_with("text=\"3 accounts\"").contains(" INFO "));
    assert!(line_with("text=\"unknown key: foo\"").contains(" WARN "));
    assert!(line_with("text=\"checking item 1\"").contains(" INFO "));
    // The log gets plain text, whole, whatever the screen shows.
    line_with("text=\"green\"");
    assert!(
        !text.contains("\\u{1b}") && !text.contains('\x1b'),
        "{text}"
    );
    line_with(&format!("text=\"{}\"", "w".repeat(300)));
    // Secret values are replaced in place; the rest of the line is logged.
    assert_eq!(
        text.matches("text=\"ok, token=[REDACTED]\"").count(),
        1,
        "{text}"
    );
    // The warning, and the shown notice too, at their own levels.
    assert_eq!(text.matches("token: [REDACTED]\"").count(), 2, "{text}");
    assert!(
        text.lines()
            .any(|l| l.contains(" INFO ") && l.contains("your token: [REDACTED]"))
    );
    assert!(line_with("text=\"bad token: [REDACTED]\"").contains(" WARN "));
    assert_eq!(
        text.matches("text=\"key: [REDACTED]\"").count(),
        1,
        "{text}"
    );
    assert!(!text.contains("second-line"), "{text}");
    assert!(line_with("outcome=\"done\"").contains(" INFO "));
    assert!(line_with("step=\"Pulling\" outcome=\"failed\"").contains(" WARN "));
    line_with("elapsed_ms=");
    assert!(line_with("text=\"from a task\"").contains("run{run=\"t\"}"));
    let stopping = line_with("step=\"Stopping\" outcome=\"stopped\"");
    assert!(stopping.contains("error=\"asked to stop\""), "{stopping}");
    let refused = line_with("step=\"Checking token\" outcome=\"failed\"");
    assert!(
        refused.contains("error=\"the token was refused: [REDACTED]\""),
        "{refused}"
    );
    line_with("step skipped (cancelled) step=\"Late\"");
}
