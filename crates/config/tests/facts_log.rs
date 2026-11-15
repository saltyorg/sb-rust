//! The fact editor's log records (CONSTRAINTS.md, Logging; tui spec,
//! "Secrets": never a fact value, masked or not): open and save a role
//! holding a known secret, and change it to another, and neither value is
//! in the log, while the role, key and outcome are. A test binary of its
//! own: tracing caches each callsite's interest process-wide
//! (<https://docs.rs/tracing-core/0.1.36/tracing_core/callsite/index.html>).

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sb_config::facts::apply::{Change, RoleOutcome};
use sb_config::facts::document::Edit;
use sb_config::facts::session::{Drift, DriftChoice, SaveHooks, Session, Wait};

const OLD_SECRET: &str = "claim-not-a-real-secret-1";
const NEW_SECRET: &str = "claim-not-a-real-secret-2";
const DISK_SECRET: &str = "claim-not-a-real-secret-3";
const REFUSED_SECRET: &str = "claim-not-a-real-secret-4";

/// A log writer the test can read back (as `run_log.rs`'s).
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

struct Hooks;

impl SaveHooks for Hooks {
    fn waiting(&mut self, _role: &str, _waited: Duration) -> Wait {
        Wait::Keep
    }
    fn drift(&mut self, _role: &str, _drift: &Drift) -> DriftChoice {
        DriftChoice::Reload
    }
    fn stopped(&mut self) -> bool {
        false
    }
}

#[test]
fn no_fact_value_reaches_the_log() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical path");
    std::fs::write(
        root.join("plex.ini"),
        format!("[plex]\nclaim_token = {OLD_SECRET}\nurl = keep\n"),
    )
    .expect("fixture");
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    // Saltbox holds the role's lock for a moment: the wait is logged once.
    let lock = std::fs::File::create(root.join("plex.ini.lock")).expect("lock file");
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        .expect("Saltbox's lock");
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        drop(lock);
    });
    tracing::subscriber::with_default(subscriber, || {
        let mut session = Session::open(&root).expect("open");
        let outcomes = session.save(
            &[Change::Edit {
                role: "plex".to_owned(),
                edit: Edit::Set {
                    instance: "plex".to_owned(),
                    key: "claim_token".to_owned(),
                    value: NEW_SECRET.to_owned(),
                },
            }],
            &mut Hooks,
        );
        assert!(
            matches!(outcomes["plex"], RoleOutcome::Applied),
            "{outcomes:?}"
        );
        // Saltbox writes the role meanwhile: drift, reloaded.
        std::fs::write(
            root.join("plex.ini"),
            format!("[plex]\nclaim_token = {DISK_SECRET}\nurl = keep\n"),
        )
        .expect("Saltbox's write");
        let set = |value: &str| Change::Edit {
            role: "plex".to_owned(),
            edit: Edit::Set {
                instance: "plex".to_owned(),
                key: "url".to_owned(),
                value: value.to_owned(),
            },
        };
        let outcomes = session.save(&[set("changed")], &mut Hooks);
        assert!(
            matches!(outcomes["plex"], RoleOutcome::Drifted { reloaded: true }),
            "{outcomes:?}"
        );
        // A value the file can't hold is refused, its text unlogged.
        let outcomes = session.save(&[set(&format!("a\n# {REFUSED_SECRET}"))], &mut Hooks);
        assert!(
            matches!(outcomes["plex"], RoleOutcome::Failed(_)),
            "{outcomes:?}"
        );
    });
    let log = captured.text();
    for secret in [OLD_SECRET, NEW_SECRET, DISK_SECRET, REFUSED_SECRET] {
        assert!(!log.contains(secret), "{log}");
    }
    release.join().expect("release");
    assert!(log.contains("role changed on disk"), "{log}");
    assert_eq!(
        log.matches("waiting for Saltbox's role lock").count(),
        1,
        "{log}"
    );
    assert!(log.contains("refused"), "{log}");
    assert!(log.contains("set plex/claim_token"), "{log}");
    assert!(log.contains("role lock taken"), "{log}");
    assert!(log.contains("applied"), "{log}");
}
