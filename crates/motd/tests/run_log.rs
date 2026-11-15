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
    let (report, text) = logged(sb_motd::report(
        &options,
        colours,
        sb_motd::config::Loaded::default,
    ));
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

/// motd.yml is opened once per run, its issues logged with their
/// positions in sb-config's wording, and no credential's value with them
/// (criteria 5 and 12).
#[test]
fn motd_yml_is_checked_once_and_its_issues_logged() {
    let root = tempfile::tempdir().expect("root");
    std::fs::create_dir(root.path().join("schema")).expect("schema dir");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../config/tests/fixtures/saltbox-schemas/motd.schema.yml"),
        root.path().join("schema/motd.schema.yml"),
    )
    .expect("schema");
    std::fs::write(
        root.path().join("motd.yml"),
        "colors:\n  text:\n    label: pink\nsonarr:\n  instances:\n    - name: Sonarr\n      url: not a url\n      apikey: canary-apikey-zz\n",
    )
    .expect("motd.yml");
    let settings = sb_config::run::Settings {
        root: root.path().to_path_buf(),
        zoneinfo: Vec::new(),
        http: sb_config::http::Http::new(&sb_config::http::Proxy::Off, Duration::from_secs(1))
            .expect("client"),
        cloudflare_url: String::new(),
        dockerhub_url: String::new(),
        backoff: sb_config::cloudflare::backoff,
    };
    let (loaded, text) = logged(async { sb_motd::config::load(&settings) });
    assert_eq!(
        loaded.line.as_deref(),
        Some("motd.yml has 2 issues; run sb validate-config")
    );
    assert_eq!(
        text.matches("config file checked").count(),
        1,
        "opened once: {text}"
    );
    assert!(text.contains("file=\"motd.yml\" issues=2"), "{text}");
    assert!(
        text.contains("motd.yml issue issue=motd.yml:3:12: colors.text.label: "),
        "{text}"
    );
    assert!(
        text.contains("motd.yml:7:12: sonarr.instances[0].url: "),
        "{text}"
    );
    assert!(!text.contains("canary-apikey-zz"), "{text}");
}
