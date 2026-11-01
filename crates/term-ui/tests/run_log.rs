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
    // The run's span, as cli-core's `RunLog::enter` opens it.
    let _span = tracing::info_span!("run", run = "t").entered();
    let secret = "hunter2-not-a-real-token";
    // Colour on, so the screen keeps colour codes and the log must strip
    // them itself (safe: this binary holds one test).
    anstream::ColorChoice::Always.write_global();
    let shutdown = Shutdown::new();
    // Plain (verbose), so the test draws no live area wherever it runs.
    let options = Options {
        verbose: true,
        ..Options::default()
    };
    let mut run = Runner::start("sb demo", &shutdown, None, options).await;
    let labels = Labels::new("Checking credentials", "credentials valid");
    run.step(labels, async |step| {
        step.info("3 accounts");
        step.warn("unknown key: foo");
        step.output("checking item 1");
        step.output("\x1b[32mgreen\x1b[0m");
        step.output("w".repeat(300));
        step.output(Line::sensitive(format!("ok\ntoken={secret}")));
        step.warn(Line::sensitive(format!("bad token {secret}")));
        // Shown to the user on purpose, still never logged.
        step.info(Line::sensitive(format!("your token: {secret}")).shown());
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
    shutdown.request(0);
    let _ = run
        .step(Labels::new("Late", "late"), async |_| Ok(()))
        .await;
    run.finish();
    let text = log.text();
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
    assert!(line_with("notice withheld (sensitive)").contains(" WARN "));
    assert!(line_with("text=\"checking item 1\"").contains(" INFO "));
    // The log gets plain text, whole, whatever the screen shows.
    line_with("text=\"green\"");
    assert!(
        !text.contains("\\u{1b}") && !text.contains('\x1b'),
        "{text}"
    );
    line_with(&format!("text=\"{}\"", "w".repeat(300)));
    // Sensitive output is one withheld record, whatever its line count.
    assert_eq!(
        text.matches("output withheld (sensitive)").count(),
        1,
        "{text}"
    );
    // The shown notice is withheld from the log too, at its own level.
    assert_eq!(
        text.matches("notice withheld (sensitive)").count(),
        2,
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l.contains(" INFO ") && l.contains("notice withheld"))
    );
    assert!(line_with("outcome=\"done\"").contains(" INFO "));
    assert!(line_with("step=\"Pulling\" outcome=\"failed\"").contains(" WARN "));
    line_with("elapsed_ms=");
    let refused = line_with("step=\"Checking token\" outcome=\"failed\"");
    assert!(
        refused.contains("error=\"the token was refused: (sensitive, withheld)\""),
        "{refused}"
    );
    line_with("step skipped (cancelled) step=\"Late\"");
}
