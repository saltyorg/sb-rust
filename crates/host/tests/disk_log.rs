//! The disk check's run-log records (host spec, "Disk space"): each
//! filesystem's total, free and used space, and each skipped duplicate. A
//! test binary of its own: tracing caches each callsite's interest
//! process-wide
//! (<https://docs.rs/tracing-core/0.1.36/tracing_core/callsite/index.html>),
//! so a log test sharing a process with tests that run without a
//! subscriber misses events.

use std::io::Write;
use std::sync::{Arc, Mutex};

/// A log writer the test can read back, through tracing-subscriber's
/// `MakeWriter` for a closure. Why not its `Mutex<W>` or `Arc<W>` writers:
/// the first owns the buffer, the second needs `&W: Write`.
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

#[test]
fn each_filesystem_s_figures_and_each_skipped_duplicate_are_logged() {
    // A directory and one inside it: two paths on one filesystem.
    let dir = tempfile::tempdir().expect("tempdir");
    let inner = dir.path().join("inner");
    std::fs::create_dir(&inner).expect("inner");

    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    let result = tracing::subscriber::with_default(subscriber, || {
        sb_host::check_disk_space(&[dir.path(), &inner])
    });

    let text = captured.text();
    let lines: Vec<&str> = text.lines().collect();
    let figures = lines[0];
    assert!(
        figures.contains("disk usage")
            && figures.contains(&format!("path={}", dir.path().display()))
            && figures.contains("total=")
            && figures.contains("free=")
            && figures.contains("used="),
        "{text}"
    );
    // The duplicate is reached only when the first filesystem passed.
    if result.is_ok() {
        assert_eq!(lines.len(), 2, "{text}");
        assert!(
            lines[1].contains("disk usage skipped: same filesystem")
                && lines[1].contains(&format!("path={}", inner.display()))
                && lines[1].contains(&format!("same_as={}", dir.path().display())),
            "{text}"
        );
    }
}
