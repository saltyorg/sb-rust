//! A panic reaches the run log (sb-M2), in a test binary of its own: the
//! panic hook is process-wide, and cli-core's unit tests catch panics of
//! their own. Once the log is open and the run entered, a panic leaves a
//! record with its message, its location and the run id, one line even
//! with a newline in the message (the log's escaping), and the hook that
//! was there before still runs (std's message on stderr, as before).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use sb_cli_core::log::RunLog;

#[test]
fn a_panic_is_recorded_then_the_previous_hook_runs() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (log, subscriber) = RunLog::open(dir.path()).expect("open");
    // The hook in place before: std's, here a stand-in that counts.
    let before = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&before);
    std::panic::set_hook(Box::new(move |_| {
        counted.fetch_add(1, Ordering::SeqCst);
    }));
    let line = line!() + 7;
    tracing::subscriber::with_default(subscriber, || {
        // Installed before the run is entered: the hook holds the run's
        // span itself.
        log.record_panics();
        let _run = log.enter();
        let caught = std::panic::catch_unwind(|| {
            panic!("boom\n2026-11-12T00:00:00Z  INFO run{{run=0}}: sb: run finished");
        });
        assert!(caught.is_err());
    });
    assert_eq!(before.load(Ordering::SeqCst), 1, "the previous hook ran");
    let text = std::fs::read_to_string(dir.path().join("sb.log")).expect("log");
    let records: Vec<_> = text.lines().filter(|l| l.contains("panicked")).collect();
    assert_eq!(records.len(), 1, "{text}");
    let record = records[0];
    assert!(
        record.contains(&format!("run{{run={}}}", log.id())),
        "{record}"
    );
    // The run's own span, once, though the panic came inside it.
    assert_eq!(
        record.matches(&format!("run{{run={}}}", log.id())).count(),
        1,
        "{record}"
    );
    assert!(
        record.contains(r"boom\n2026-11-12T00:00:00Z  INFO run{run=0}: sb: run finished"),
        "{record}"
    );
    assert!(record.contains(&format!("{}:{line}:", file!())), "{record}");
    assert!(
        !text.lines().any(|l| l.starts_with("2026-11-12T00:00:00Z")),
        "{text}"
    );
}
