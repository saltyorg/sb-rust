//! GPU's run-log records (motd spec, Testing: the run log names why
//! pci.ids wasn't read, and counts its malformed lines). A test binary of
//! its own: GPU reads on tokio's blocking pool, whose threads see only the
//! global subscriber, as sb's run log is
//! (<https://docs.rs/tracing/0.1.44/tracing/dispatcher/index.html#setting-the-default-subscriber>),
//! and a process takes one global subscriber.

use std::io::Write;
use std::sync::{Arc, Mutex, OnceLock};

/// A log writer the test can read back.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    /// What was written since the last call.
    fn take(&self) -> String {
        let bytes = std::mem::take(&mut *self.0.lock().expect("lock"));
        String::from_utf8_lossy(&bytes).into_owned()
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

/// `body` on a current-thread runtime, with what it logged.
fn logged<F: std::future::Future>(body: F) -> (F::Output, String) {
    static CAPTURED: OnceLock<Captured> = OnceLock::new();
    let captured = CAPTURED.get_or_init(|| {
        let captured = Captured::default();
        let writer = captured.clone();
        tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .init();
        captured
    });
    captured.take();
    let output = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime")
        .block_on(body);
    (output, captured.take())
}

/// A display device in a fixture sysfs tree under `dir`.
fn gpu_device(dir: &std::path::Path) {
    let device = dir.join("0000:01:00.0");
    std::fs::create_dir(&device).expect("device dir");
    for (name, value) in [
        ("class", "0x030000"),
        ("vendor", "0x10de"),
        ("device", "0x2204"),
        ("revision", "0x00"),
    ] {
        std::fs::write(device.join(name), format!("{value}\n")).expect("attribute");
    }
}

/// Without pci.ids the devices read by number, and the log says why
/// (motd spec, Testing: "the run log naming the cause").
#[test]
fn a_pci_ids_not_read_is_logged_with_its_cause() {
    let dir = tempfile::tempdir().expect("temp dir");
    gpu_device(dir.path());
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pci.ids");
    let sources = |ids: std::path::PathBuf, cap: u64| sb_motd::gpu::Sources {
        devices: dir.path().to_path_buf(),
        ids,
        cap,
    };
    let (value, text) = logged(sb_motd::gpu::gpu_at(sources(
        dir.path().join("missing"),
        1 << 20,
    )));
    assert_eq!(value, "Device 10de:2204");
    assert!(
        text.contains("pci.ids not read: No such file or directory"),
        "{text}"
    );
    // Nothing was parsed, so no line was skipped.
    assert!(!text.contains("lines skipped"), "{text}");
    let (value, text) = logged(sb_motd::gpu::gpu_at(sources(fixture.clone(), 10)));
    assert_eq!(value, "Device 10de:2204");
    assert!(text.contains("pci.ids not read: over 10 bytes"), "{text}");
    // The malformed line in NVIDIA's block is counted; the others are
    // in blocks sb doesn't read (the maintainer, 2026-11-21).
    let (value, text) = logged(sb_motd::gpu::gpu_at(sources(fixture, 1 << 20)));
    assert_eq!(value, "NVIDIA Corporation GA102 [GeForce RTX 3090]");
    assert!(
        text.contains("pci.ids lines skipped") && text.contains("malformed=1"),
        "{text}"
    );
}
