//! The ownership run-log records (host spec, "Ownership"): one count of
//! changed entries per root, never each path. A test binary of its own:
//! tracing caches each callsite's interest process-wide
//! (<https://docs.rs/tracing-core/0.1.36/tracing_core/callsite/index.html>),
//! so a log test sharing a process with tests that run without a
//! subscriber misses events.

use std::fs;
use std::io::Write;
use std::sync::{Arc, Mutex};

use nix::unistd::{User, geteuid};

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

/// A user no host has.
const UNKNOWN: &str = "sb-rust-user-that-does-not-exist-7fcd24d1";

#[test]
fn each_root_gets_one_count_and_no_entry_is_logged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("root");
    fs::create_dir(&root).expect("root");
    fs::write(root.join("inner-file"), "x").expect("file");
    let single = dir.path().join("single");
    fs::write(&single, "x").expect("single");
    // The current user owns the fresh files already, so the counts are 0:
    // a test can't change ownership without root. Its name comes from its
    // passwd entry, on purpose (the name sb would be given); a user with
    // none fails the lookup, saying so.
    let me = User::from_uid(geteuid())
        .expect("lookup")
        .expect("current user");

    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        sb_host::ensure_owner_recursive(&me.name, &[&root]).expect("recursive");
        sb_host::ensure_owner(&me.name, &[&single]).expect("owner");
        sb_host::ensure_owner_recursive(UNKNOWN, &[&root]).expect("skipped");
    });

    let text = captured.text();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert!(
        lines[0].contains("recursive ownership")
            && lines[0].contains(&format!("root={}", root.display()))
            && lines[0].contains("changed=0"),
        "{text}"
    );
    assert!(
        lines[1].contains(&format!("path={}", single.display()))
            && lines[1].contains("changed=false"),
        "{text}"
    );
    assert!(
        lines[2].contains("ownership skipped: no such user")
            && lines[2].contains(&format!("user=\"{UNKNOWN}\"")),
        "{text}"
    );
    assert!(!text.contains("inner-file"), "{text}");
}
