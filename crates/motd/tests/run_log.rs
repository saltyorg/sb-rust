//! motd's run-log records (motd spec, success criterion 12): the sections
//! chosen, and each section's outcome and time. A test binary of its own:
//! tracing caches each callsite's interest process-wide
//! (<https://docs.rs/tracing-core/0.1.36/tracing_core/callsite/index.html>),
//! so a log test sharing a process with tests that run without a
//! subscriber misses events.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sb_cli_core::theme::Theme;
use sb_motd::{Colours, Options, Section, run};

/// A log writer the test can read back (as sb-host's log tests).
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

/// `body` on a paused current-thread runtime, its log captured.
fn logged<F: std::future::Future>(body: F) -> (F::Output, String) {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    let output = tracing::subscriber::with_default(subscriber, || {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .expect("runtime")
            .block_on(body)
    });
    (output, captured.text())
}

#[test]
fn the_sections_chosen_and_each_outcome_are_logged() {
    let theme = Theme::plain();
    let options = Options {
        all: false,
        sections: vec![Section::Kernel, Section::Distribution],
    };
    let colours = Colours {
        theme: &theme,
        on: false,
    };
    let (report, text) = logged(sb_motd::report(&options, colours));
    assert!(report.is_ok());
    assert!(
        text.contains("motd sections chosen sections=\"distro,kernel\""),
        "{text}"
    );
    for flag in ["distro", "kernel"] {
        assert!(
            text.lines().any(|line| line.contains("motd section ")
                && line.contains(&format!("section=\"{flag}\""))
                && line.contains("outcome=\"shown\"")
                && line.contains("elapsed_ms=")),
            "{flag}: {text}"
        );
    }
}

#[test]
fn a_timeout_a_panic_and_an_empty_value_are_logged_as_such() {
    let jobs: Vec<(Section, run::Job)> = vec![
        (
            Section::Distribution,
            Box::pin(async {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                String::new()
            }),
        ),
        (Section::Kernel, Box::pin(async { panic!("boom") })),
        (Section::Uptime, Box::pin(async { String::new() })),
    ];
    let (_, text) = logged(run::collect(jobs, run::CAP));
    let line = |flag: &str| {
        text.lines()
            .find(|line| line.contains(&format!("section=\"{flag}\"")))
            .unwrap_or_else(|| panic!("no {flag} line: {text}"))
            .to_string()
    };
    assert!(line("distro").contains("outcome=\"timed out\" elapsed_ms=30000"));
    assert!(line("kernel").contains("outcome=\"panicked\""));
    assert!(line("uptime").contains("outcome=\"empty\""));
}
