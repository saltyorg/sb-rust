//! The run-log records of an attached child (exec spec, "Logging"; plan
//! Task 8b): a start record naming the way and an end record, its output
//! never logged; a refused one (stdin bytes) writes no start record. A
//! test binary of its own: tracing caches each callsite's interest
//! process-wide
//! (https://docs.rs/tracing-core/0.1.36/tracing_core/callsite/index.html),
//! as tests/capture_log.rs.

use std::io::Write;
use std::sync::{Arc, Mutex};

use sb_cli_core::cancel::Shutdown;
use sb_exec::{Cmd, Ending};

/// A log writer the test can read back (as tests/capture_log.rs).
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
async fn the_records_are_written_and_the_output_is_not() {
    let log = Captured::default();
    let writer = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("subscriber");
    let shutdown = Shutdown::new();
    let ran = Cmd::new("sh")
        .arg("-c")
        // The output is made at run time, so it isn't in the logged
        // command line (whose literals are logged by design).
        .arg("printf 'attached-%s\\n' output-not-logged; exit 2")
        .attached(&shutdown)
        .await
        .expect("ran");
    assert_eq!(ran.outcome.ending, Ending::Exited(2));
    let text = log.text();
    let started: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("child started"))
        .collect();
    assert_eq!(started.len(), 1, "{text}");
    assert!(started[0].contains("way=\"attached\""), "{text}");
    assert!(
        text.lines()
            .any(|l| l.contains("child ended") && l.contains("code 2")),
        "{text}"
    );
    assert!(!text.contains("attached-output-not-logged"), "{text}");

    // Refused before anything spawns: no start record, a "not started".
    let before = text.lines().count();
    let refused = Cmd::new("cat")
        .stdin(b"x".to_vec())
        .attached(&shutdown)
        .await;
    assert!(refused.is_err());
    let text = log.text();
    let new: Vec<&str> = text.lines().skip(before).collect();
    assert!(!new.iter().any(|l| l.contains("child started")), "{text}");
    assert!(
        new.iter().any(|l| l.contains("child not started")),
        "{text}"
    );
}
