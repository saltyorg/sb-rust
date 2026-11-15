//! The session's tests on temporary roots (tui spec, Testing Strategy,
//! "Session"; sb-go `facts/session_test.go` and the lock cases of
//! `facts/apply_test.go`). A role lock is held as Saltbox's module holds it:
//! an exclusive `flock` on `<role>.ini.lock`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rustix::fs::FlockOperation;

use super::{Drift, DriftChoice, EDITOR_LOCK, FileError, SaveHooks, Session, SessionError, Wait};
use crate::facts::apply::{Change, RoleOutcome, SaveError};
use crate::facts::document::Edit;
use crate::facts::{Fact, Instance};

/// A temporary facts directory, by its real path (no symlink in it).
fn temp_root() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = std::fs::canonicalize(dir.path()).expect("canonical path");
    (dir, path)
}

fn fixture(root: &Path, role: &str, text: &str) -> PathBuf {
    let path = root.join(format!("{role}.ini"));
    std::fs::write(&path, text).expect("write fixture");
    path
}

fn open(root: &Path) -> Session {
    Session::open(root).expect("the session opens")
}

/// A lock held as Saltbox's `saltbox_facts` module holds it.
fn saltbox_lock(path: &Path) -> std::fs::File {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .expect("lock file");
    rustix::fs::flock(&file, FlockOperation::NonBlockingLockExclusive).expect("lock taken");
    file
}

/// Whether Saltbox's module gets `path`'s lock within a second: another
/// test's child process can hold a copy of a closed lock's fd for a
/// moment, between its fork and exec.
fn lock_is_free(path: &Path) -> bool {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("lock file");
    (0..50).any(|_| {
        let free = rustix::fs::flock(&file, FlockOperation::NonBlockingLockExclusive).is_ok();
        if !free {
            std::thread::sleep(Duration::from_millis(20));
        }
        free
    })
}

fn set(role: &str, instance: &str, key: &str, value: &str) -> Change {
    Change::Edit {
        role: role.to_owned(),
        edit: Edit::Set {
            instance: instance.to_owned(),
            key: key.to_owned(),
            value: value.to_owned(),
        },
    }
}

/// The fact editor's answers, scripted: each wait is kept unless `cancel`,
/// each drift answered with `drift`, and `on_wait` run on each wait.
struct Hooks {
    waits: usize,
    cancel: bool,
    /// `stopped` says yes from its call after this many.
    stop_after: Option<usize>,
    stops_asked: usize,
    drift: DriftChoice,
    drifts: Vec<(String, Drift)>,
    on_wait: Box<dyn FnMut()>,
    on_drift: Box<dyn FnMut()>,
}

impl Default for Hooks {
    fn default() -> Self {
        Self {
            waits: 0,
            cancel: false,
            stop_after: None,
            stops_asked: 0,
            drift: DriftChoice::Cancel,
            drifts: Vec::new(),
            on_wait: Box::new(|| {}),
            on_drift: Box::new(|| {}),
        }
    }
}

impl SaveHooks for Hooks {
    fn waiting(&mut self, _role: &str, _waited: Duration) -> Wait {
        self.waits += 1;
        (self.on_wait)();
        if self.cancel {
            Wait::Cancel
        } else {
            Wait::Keep
        }
    }

    fn drift(&mut self, role: &str, drift: &Drift) -> DriftChoice {
        (self.on_drift)();
        self.drifts.push((role.to_owned(), drift.clone()));
        self.drift
    }

    fn stopped(&mut self) -> bool {
        self.stops_asked += 1;
        self.stop_after
            .is_some_and(|after| self.stops_asked > after)
    }
}

fn values(session: &Session, role: &str) -> Vec<(String, Vec<Fact>)> {
    session
        .catalog()
        .into_iter()
        .filter(|found| found.name == role)
        .flat_map(|found| found.instances)
        .map(|instance| (instance.name, instance.facts))
        .collect()
}

// sb-go TestCatalogPreservesLiteralAndEmptyInstances
#[test]
fn the_catalog_lists_roles_sorted_with_their_instances() {
    let (_dir, root) = temp_root();
    fixture(&root, "zeta", "[last]\nk = v\n");
    fixture(
        &root,
        "alpha",
        "; retained\nsecret = default-value\n[zulu]\nz = None\na =\n[empty]\n",
    );
    std::fs::write(root.join("notes.txt"), "not a role").expect("other file");
    let session = open(&root);
    let catalog = session.catalog();
    let names: Vec<&str> = catalog.iter().map(|role| role.name.as_str()).collect();
    assert_eq!(names, ["alpha", "zeta"]);
    assert_eq!(
        catalog[0].instances,
        vec![
            Instance {
                name: "empty".to_owned(),
                facts: vec![],
            },
            Instance {
                name: "zulu".to_owned(),
                facts: vec![
                    Fact {
                        key: "a".to_owned(),
                        value: String::new(),
                    },
                    Fact {
                        key: "z".to_owned(),
                        value: "None".to_owned(),
                    },
                ],
            },
        ]
    );
}

// sb-go TestOpenSessionRejectsUnsafeInputs
#[test]
fn unsafe_paths_and_files_are_refused() {
    for kind in [
        "relative root",
        "missing root",
        "symlink root",
        "symlink ancestor",
        "file root",
        "symlink role",
        "directory role",
        "fifo role",
        "malformed role",
        "too big role",
        "symlink editor lock",
        "directory editor lock",
    ] {
        let (_dir, base) = temp_root();
        let mut facts = base.join("facts");
        std::fs::create_dir(&facts).expect("facts dir");
        match kind {
            "relative root" => facts = PathBuf::from("facts"),
            "missing root" => facts = base.join("missing"),
            "symlink root" => {
                std::os::unix::fs::symlink(&facts, base.join("link")).expect("symlink");
                facts = base.join("link");
            }
            "symlink ancestor" => {
                std::os::unix::fs::symlink(&base, base.join("link")).expect("symlink");
                facts = base.join("link").join("facts");
            }
            "file root" => facts = fixture(&base, "not-dir", "x"),
            "symlink role" => {
                let target = fixture(&base, "target", "[x]\nk=v\n");
                std::os::unix::fs::symlink(target, facts.join("role.ini")).expect("symlink");
            }
            "directory role" => std::fs::create_dir(facts.join("role.ini")).expect("dir"),
            "fifo role" => rustix::fs::mknodat(
                rustix::fs::CWD,
                facts.join("role.ini"),
                rustix::fs::FileType::Fifo,
                rustix::fs::Mode::from_raw_mode(0o600),
                0,
            )
            .expect("fifo"),
            "malformed role" => {
                fixture(&facts, "role", "[broken\nk=v\n");
            }
            "too big role" => {
                let text = format!("[x]\nk = {}\n", "v".repeat(1 << 20));
                fixture(&facts, "role", &text);
            }
            "symlink editor lock" => {
                let target = fixture(&base, "target", "unchanged");
                std::os::unix::fs::symlink(target, facts.join(EDITOR_LOCK)).expect("symlink");
            }
            "directory editor lock" => std::fs::create_dir(facts.join(EDITOR_LOCK)).expect("dir"),
            _ => unreachable!(),
        }
        assert!(Session::open(&facts).is_err(), "{kind} accepted");
    }
}

#[test]
fn open_errors_name_the_file() {
    let (_dir, root) = temp_root();
    fixture(&root, "sonarr", "[broken\n");
    let err = Session::open(&root).unwrap_err();
    assert_eq!(
        err.to_string(),
        "catalog sonarr: malformed section at line 1"
    );

    let (_dir, root) = temp_root();
    std::fs::create_dir(root.join("radarr.ini")).expect("dir");
    let err = Session::open(&root).unwrap_err();
    assert_eq!(
        err.to_string(),
        "catalog radarr: facts file radarr.ini must be a regular file"
    );

    let (_dir, root) = temp_root();
    std::os::unix::fs::symlink("/dev/null", root.join("plex.ini")).expect("symlink");
    let err = Session::open(&root).unwrap_err();
    assert_eq!(
        err.to_string(),
        "catalog plex: facts file plex.ini must not be a symlink"
    );

    let (_dir, root) = temp_root();
    // Exactly the cap is fine.
    let at_cap = format!("[x]\nk = {}\n", "v".repeat((1 << 20) - 9));
    assert_eq!(at_cap.len(), 1 << 20);
    fixture(&root, "big", &at_cap);
    drop(open(&root));
    fixture(&root, "big", &format!("[x]\nk = {}\n", "v".repeat(1 << 20)));
    let err = Session::open(&root).unwrap_err();
    assert_eq!(
        err.to_string(),
        "catalog big: facts file big.ini is bigger than 1048576 bytes"
    );

    let (_dir, root) = temp_root();
    let name: &std::ffi::OsStr = std::os::unix::ffi::OsStrExt::from_bytes(b"bad\xff.ini");
    std::fs::write(root.join(name), "").expect("non-UTF-8 name");
    let err = Session::open(&root).unwrap_err();
    assert_eq!(
        err.to_string(),
        "catalog bad\u{fffd}.ini: facts file name bad\u{fffd}.ini is not UTF-8"
    );

    let (_dir, root) = temp_root();
    std::fs::write(root.join(".ini"), "").expect("nameless role");
    let err = Session::open(&root).unwrap_err();
    assert_eq!(
        err.to_string(),
        "catalog .ini: facts file .ini names no app"
    );

    let (_dir, base) = temp_root();
    let err = Session::open(&base.join("missing")).unwrap_err();
    assert!(matches!(err, SessionError::Directory(_)), "{err}");
    assert!(
        err.to_string()
            .starts_with("facts directory must be an existing directory without symlinks: "),
        "{err}"
    );
}

// sb-go TestEditorLockSingletonAndFailedStartupCleanup
#[test]
fn a_second_editor_is_refused_and_a_failed_open_releases_the_lock() {
    let (_dir, root) = temp_root();
    fixture(&root, "role", "[x]\nk=v\n");
    let first = open(&root);
    let err = Session::open(&root).unwrap_err();
    assert!(matches!(err, SessionError::EditorActive));
    assert_eq!(err.to_string(), "another fact editor is active");
    let mode = std::fs::metadata(root.join(EDITOR_LOCK))
        .expect("lock")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o640);
    drop(first);
    fixture(&root, "role", "[broken");
    assert!(Session::open(&root).is_err());
    fixture(&root, "role", "[x]\nk=v\n");
    drop(open(&root));
}

/// Criterion 24: staging takes no lock, so Saltbox runs beside an open
/// editor.
#[test]
fn an_open_session_holds_no_role_lock() {
    let (_dir, root) = temp_root();
    fixture(&root, "role", "[x]\nk = v\n");
    let _session = open(&root);
    assert!(!root.join("role.ini.lock").exists());
    let _saltbox = saltbox_lock(&root.join("role.ini.lock"));
}

#[test]
fn a_held_role_lock_is_waited_for_then_released_after_the_commit() {
    let (_dir, root) = temp_root();
    fixture(&root, "role", "[x]\nk = v\n");
    let lock = root.join("role.ini.lock");
    let saltbox = saltbox_lock(&lock);
    let mut session = open(&root);
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        drop(saltbox);
    });
    let mut hooks = Hooks::default();
    let outcomes = session.save(&[set("role", "x", "k", "w")], &mut hooks);
    release.join().expect("release thread");
    assert!(
        matches!(outcomes["role"], RoleOutcome::Applied),
        "{outcomes:?}"
    );
    assert!(hooks.waits >= 1);
    assert!(
        std::fs::read_to_string(root.join("role.ini"))
            .unwrap()
            .contains("k = w")
    );
    assert!(lock_is_free(&lock), "the lock is released after the commit");
}

#[test]
fn a_lock_held_past_the_timeout_fails_the_role_and_stops_the_save() {
    let (_dir, root) = temp_root();
    fixture(&root, "a", "[x]\nk = v\n");
    fixture(&root, "b", "[x]\nk = v\n");
    let _saltbox = saltbox_lock(&root.join("a.ini.lock"));
    let mut session = open(&root);
    session.lock_timeout = Duration::from_millis(120);
    let mut hooks = Hooks::default();
    let started = std::time::Instant::now();
    let outcomes = session.save(
        &[set("a", "x", "k", "w"), set("b", "x", "k", "w")],
        &mut hooks,
    );
    assert!(started.elapsed() >= Duration::from_millis(120));
    assert!(
        matches!(outcomes["a"], RoleOutcome::Failed(SaveError::LockTimeout)),
        "{outcomes:?}"
    );
    assert!(
        matches!(outcomes["b"], RoleOutcome::Unattempted),
        "{outcomes:?}"
    );
    assert!(hooks.waits >= 2);
    assert_eq!(
        SaveError::LockTimeout.to_string(),
        "timed out waiting for Saltbox's lock"
    );
    for role in ["a", "b"] {
        assert_eq!(
            std::fs::read_to_string(root.join(format!("{role}.ini"))).unwrap(),
            "[x]\nk = v\n"
        );
    }
    assert!(!root.join("b.ini.lock").exists(), "b was never locked");
}

#[test]
fn a_cancelled_wait_leaves_the_roles_unattempted_and_the_lock_unheld() {
    let (_dir, root) = temp_root();
    fixture(&root, "a", "[x]\nk = v\n");
    fixture(&root, "b", "[x]\nk = v\n");
    let lock = root.join("a.ini.lock");
    let saltbox = saltbox_lock(&lock);
    let mut session = open(&root);
    let mut hooks = Hooks {
        cancel: true,
        ..Hooks::default()
    };
    let outcomes = session.save(
        &[set("a", "x", "k", "w"), set("b", "x", "k", "w")],
        &mut hooks,
    );
    assert!(
        matches!(outcomes["a"], RoleOutcome::Unattempted),
        "{outcomes:?}"
    );
    assert!(
        matches!(outcomes["b"], RoleOutcome::Unattempted),
        "{outcomes:?}"
    );
    assert_eq!(hooks.waits, 1);
    assert!(!root.join("b.ini.lock").exists(), "b was never locked");
    drop(saltbox);
    assert!(lock_is_free(&lock));
    for role in ["a", "b"] {
        assert_eq!(
            std::fs::read_to_string(root.join(format!("{role}.ini"))).unwrap(),
            "[x]\nk = v\n"
        );
    }
}

/// A stop (Ctrl+C, SIGTERM) ends a wait at once, well before the timeout.
#[test]
fn a_stop_ends_the_wait_for_a_held_lock() {
    let (_dir, root) = temp_root();
    fixture(&root, "a", "[x]\nk = v\n");
    let _saltbox = saltbox_lock(&root.join("a.ini.lock"));
    let mut session = open(&root);
    let mut hooks = Hooks {
        stop_after: Some(1),
        ..Hooks::default()
    };
    let started = std::time::Instant::now();
    let outcomes = session.save(&[set("a", "x", "k", "w")], &mut hooks);
    assert!(
        matches!(outcomes["a"], RoleOutcome::Unattempted),
        "{outcomes:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(hooks.waits, 0, "stopped before the dialog");
}

// sb-go TestRoleLockTimeoutAndSidecarReplacement
#[test]
fn a_lock_file_replaced_while_waiting_fails_the_role() {
    let (_dir, root) = temp_root();
    fixture(&root, "a", "[x]\nk = v\n");
    let lock = root.join("a.ini.lock");
    let saltbox = std::cell::RefCell::new(Some(saltbox_lock(&lock)));
    let replacement = std::rc::Rc::new(std::cell::RefCell::new(None));
    let mut session = open(&root);
    let swap_root = root.clone();
    let swapped = replacement.clone();
    let mut hooks = Hooks {
        on_wait: Box::new(move || {
            if let Some(held) = saltbox.borrow_mut().take() {
                std::fs::rename(swap_root.join("a.ini.lock"), swap_root.join("old.lock"))
                    .expect("move lock");
                *swapped.borrow_mut() = Some(saltbox_lock(&swap_root.join("a.ini.lock")));
                drop(held);
            }
        }),
        ..Hooks::default()
    };
    let outcomes = session.save(&[set("a", "x", "k", "w")], &mut hooks);
    assert!(
        matches!(&outcomes["a"], RoleOutcome::Failed(SaveError::File(FileError::Changed(name))) if name == "a.ini.lock"),
        "{outcomes:?}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("a.ini")).unwrap(),
        "[x]\nk = v\n"
    );
}

#[test]
fn changed_bytes_are_drift_asked_with_the_lock_released() {
    let (_dir, root) = temp_root();
    fixture(&root, "role", "[default]\ntoken = original\n");
    let mut session = open(&root);
    fixture(&root, "role", "[default]\ntoken = external\n");
    let lock = root.join("role.ini.lock");
    let checked = std::rc::Rc::new(std::cell::Cell::new(false));
    let seen = checked.clone();
    let lock_path = lock.clone();
    let mut hooks = Hooks {
        on_drift: Box::new(move || seen.set(lock_is_free(&lock_path))),
        ..Hooks::default()
    };
    let change = set("role", "default", "token", "edited");
    let outcomes = session.save(std::slice::from_ref(&change), &mut hooks);
    assert!(
        matches!(outcomes["role"], RoleOutcome::Drifted { reloaded: false }),
        "{outcomes:?}"
    );
    assert!(checked.get(), "the lock is free while the dialog is up");
    let (role, drift) = &hooks.drifts[0];
    assert_eq!(role, "role");
    assert_eq!(drift.before.instances[0].facts[0].value, "original");
    assert_eq!(
        drift.after.as_ref().unwrap().instances[0].facts[0].value,
        "external"
    );
    assert_eq!(
        values(&session, "role")[0].1[0].value,
        "original",
        "nothing adopted"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("role.ini")).unwrap(),
        "[default]\ntoken = external\n"
    );

    // Cancel kept the change staged: the next save asks again; Reload adopts.
    let mut hooks = Hooks {
        drift: DriftChoice::Reload,
        ..Hooks::default()
    };
    let outcomes = session.save(std::slice::from_ref(&change), &mut hooks);
    assert!(
        matches!(outcomes["role"], RoleOutcome::Drifted { reloaded: true }),
        "{outcomes:?}"
    );
    assert_eq!(values(&session, "role")[0].1[0].value, "external");
    assert_eq!(
        std::fs::read_to_string(root.join("role.ini")).unwrap(),
        "[default]\ntoken = external\n"
    );
    // The change made again after the reload saves.
    let outcomes = session.save(&[change], &mut Hooks::default());
    assert!(
        matches!(outcomes["role"], RoleOutcome::Applied),
        "{outcomes:?}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("role.ini")).unwrap(),
        "[default]\ntoken = edited\n"
    );
    assert!(lock_is_free(&lock));
}

#[test]
fn a_deleted_file_is_drift_and_a_reload_drops_the_role() {
    let (_dir, root) = temp_root();
    fixture(&root, "gone", "[x]\nk = v\n");
    fixture(&root, "kept", "[x]\nk = v\n");
    let mut session = open(&root);
    std::fs::remove_file(root.join("gone.ini")).expect("remove");
    let mut hooks = Hooks {
        drift: DriftChoice::Reload,
        ..Hooks::default()
    };
    let outcomes = session.save(
        &[set("gone", "x", "k", "w"), set("kept", "x", "k", "w")],
        &mut hooks,
    );
    assert!(
        matches!(outcomes["gone"], RoleOutcome::Drifted { reloaded: true }),
        "{outcomes:?}"
    );
    assert_eq!(hooks.drifts[0].1.after, None);
    // A drift doesn't stop the save.
    assert!(
        matches!(outcomes["kept"], RoleOutcome::Applied),
        "{outcomes:?}"
    );
    let names: Vec<String> = session
        .catalog()
        .into_iter()
        .map(|role| role.name)
        .collect();
    assert_eq!(names, ["kept"]);
    assert!(!root.join("gone.ini").exists());
}

// sb-go TestLockRoleAcceptsIdenticalAtomicReplacementBeforeAcquisition
#[test]
fn an_identical_replacement_is_no_drift() {
    let (_dir, root) = temp_root();
    let name = fixture(&root, "role", "[default]\ntoken = original\n");
    let mut session = open(&root);
    std::fs::rename(&name, root.join("role.ini.old")).expect("move");
    fixture(&root, "role", "[default]\ntoken = original\n");
    let mut hooks = Hooks::default();
    let outcomes = session.save(&[set("role", "default", "token", "updated")], &mut hooks);
    assert!(
        matches!(outcomes["role"], RoleOutcome::Applied),
        "{outcomes:?}"
    );
    assert_eq!(hooks.drifts, vec![]);
    assert_eq!(
        std::fs::read_to_string(&name).unwrap(),
        "[default]\ntoken = updated\n"
    );
    // sb created the lock file, 0640 as Saltbox's module creates it.
    let mode = std::fs::metadata(root.join("role.ini.lock"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o640);
}

/// A file sb can't read at Save (here no longer an INI file) fails the
/// role; it is no drift to reload.
#[test]
fn a_role_file_unreadable_at_save_fails_the_role() {
    let (_dir, root) = temp_root();
    fixture(&root, "role", "[x]\nk = v\n");
    let mut session = open(&root);
    fixture(&root, "role", "[broken\n");
    let mut hooks = Hooks::default();
    let outcomes = session.save(&[set("role", "x", "k", "w")], &mut hooks);
    assert!(
        matches!(
            outcomes["role"],
            RoleOutcome::Failed(SaveError::File(FileError::Document(_)))
        ),
        "{outcomes:?}"
    );
    assert_eq!(hooks.drifts, vec![]);
}
