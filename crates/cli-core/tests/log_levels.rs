//! The run log's default levels (term-ui decision 9: verbose by default,
//! no flag): sb's own crates at DEBUG, dependencies at WARN, and
//! dependencies that log through the `log` crate (bollard) reach the run
//! log at all. A test binary of its own: the `log` bridge and the global
//! subscriber are process-wide, and tracing caches each callsite's
//! interest (https://docs.rs/tracing-core/0.1.36/tracing_core/callsite/index.html).

use sb_cli_core::log::RunLog;

#[test]
fn own_crates_log_at_debug_and_dependencies_at_warn() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (log, subscriber) = RunLog::open(dir.path()).expect("open");
    tracing::subscriber::set_global_default(subscriber).expect("global subscriber");
    // The bridge turns no `log` record below WARN into an event at all
    // (bollard's DEBUG request detail never reaches the subscriber).
    assert_eq!(
        tracing_log::log::max_level(),
        tracing_log::log::LevelFilter::Warn
    );
    let _run = log.enter();
    // A dependency logging through `log`, as bollard does.
    tracing_log::log::warn!(target: "bollard::docker", "bollard-warn-marker");
    tracing_log::log::info!(target: "bollard::docker", "bollard-info-marker");
    tracing_log::log::debug!(target: "bollard::docker", "bollard-debug-marker");
    // A dependency logging through tracing.
    tracing::info!(target: "hyper::proto", "hyper-info-marker");
    tracing::debug!(target: "hyper::proto", "hyper-debug-marker");
    // sb's own crates.
    tracing::debug!(target: "sb", "sb-main-debug-marker");
    tracing::debug!(target: "sb_docker::ps", "sb-docker-debug-marker");
    tracing::trace!(target: "sb_docker::ps", "sb-docker-trace-marker");
    tracing::debug!(target: "sb_cli_core::log", "cli-core-debug-marker");
    let text = std::fs::read_to_string(dir.path().join("sb.log")).expect("log");
    assert!(text.contains("bollard-warn-marker"), "{text}");
    for dropped in [
        "bollard-info-marker",
        "bollard-debug-marker",
        "hyper-info-marker",
        "hyper-debug-marker",
        "sb-docker-trace-marker",
    ] {
        assert!(!text.contains(dropped), "{dropped}: {text}");
    }
    assert!(text.contains("sb-main-debug-marker"), "{text}");
    assert!(text.contains("sb-docker-debug-marker"), "{text}");
    assert!(text.contains("cli-core-debug-marker"), "{text}");
    // Bridged records are normalised: the real target, not tracing-log's
    // `log.*` fields, on the record's line.
    assert!(!text.contains("log.target"), "{text}");
    // Every line names its source (maintainer, Checkpoint A), the bridged
    // dependency's included.
    let line = |marker: &str| {
        text.lines()
            .find(|l| l.contains(marker))
            .unwrap_or_default()
    };
    assert!(
        line("bollard-warn-marker").contains("bollard::docker: bollard-warn-marker"),
        "{text}"
    );
    assert!(
        line("sb-docker-debug-marker").contains("sb_docker::ps: sb-docker-debug-marker"),
        "{text}"
    );
}
