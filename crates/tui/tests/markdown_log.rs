//! Rendering an announcement writes nothing to the run log: tui-markdown
//! warns, through tracing, of a fenced code block whose language syntect
//! doesn't know (```` ```text ````, a common fence), which isn't a problem
//! of the run (the code review of M74, 2026-11-15). A test binary of its
//! own, as `refresh_log.rs`: tracing caches each callsite's interest
//! process-wide
//! (<https://docs.rs/tracing-core/0.1.36/tracing_core/callsite/index.html>).

use std::io::Write;
use std::sync::{Arc, Mutex};

use sb_cli_core::theme::{Palette, Profile, Theme};
use sb_tui::markdown::{Announcement, write_inline};

/// A log writer the test can read back (sb-exec's `tests/capture_log.rs`).
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn take(&self) -> String {
        let mut bytes = self.0.lock().expect("lock");
        String::from_utf8_lossy(&std::mem::take(&mut *bytes)).into_owned()
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

/// One test: a test binary has one global subscriber.
#[test]
fn an_unknown_fence_language_leaves_no_record() {
    let log = Captured::default();
    let writer = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("subscriber");
    // The test's own records reach the capture: it is listening.
    tracing::warn!("listening");
    assert!(log.take().contains("listening"));
    let message = "Before:\n\n```text\nplain\n```\n\n```nosuchlang\nx\n```\n";
    for theme in [
        Theme::plain(),
        Theme::resolve(&Palette::default(), Profile::TrueColor),
    ] {
        let mut out = Vec::new();
        write_inline(
            &mut out,
            &[Announcement {
                heading: "h",
                message,
            }],
            &theme,
            None,
        )
        .expect("written");
        assert!(String::from_utf8_lossy(&out).contains("plain"));
        assert_eq!(log.take(), "", "{:?}", theme.profile());
    }
    // Records after rendering still reach the log.
    tracing::warn!("still listening");
    assert!(log.take().contains("still listening"));
}
