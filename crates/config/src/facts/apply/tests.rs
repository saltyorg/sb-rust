//! The save's tests on temporary roots (tui spec, Testing Strategy,
//! "Apply"; sb-go `facts/apply_test.go`). Owner tests run as root
//! (`SB_TEST_ROOT=1`, as `scripts/check.sh` sets it).

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{Change, RoleOutcome, SaveError};
use crate::facts::document::{DocumentError, Edit};
use crate::facts::session::{Drift, DriftChoice, SaveHooks, Session, Wait};

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

fn read(root: &Path, role: &str) -> String {
    std::fs::read_to_string(root.join(format!("{role}.ini"))).expect("read role")
}

/// sb-go `apply_test.go`'s `openFixture` file.
const FIXTURE: &str = "; header\nhidden = secret\n[default]\n; token comment\ntoken = original\nempty =\nnone = None\n[empty]\n[other]\nk = untouched\n";

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

fn edit(role: &str, edit: Edit) -> Change {
    Change::Edit {
        role: role.to_owned(),
        edit,
    }
}

/// Answers that keep waiting (another test's child process can hold a
/// copy of a lock's fd between its fork and exec) and never reload;
/// `stop_after` stops the save after that many roles.
#[derive(Default)]
struct Hooks {
    stop_after: Option<usize>,
    asked: usize,
}

impl SaveHooks for Hooks {
    fn waiting(&mut self, _role: &str, _waited: Duration) -> Wait {
        Wait::Keep
    }

    fn drift(&mut self, _role: &str, _drift: &Drift) -> DriftChoice {
        DriftChoice::Cancel
    }

    fn stopped(&mut self) -> bool {
        self.asked += 1;
        self.stop_after.is_some_and(|after| self.asked > after)
    }
}

fn temp_files(root: &Path) -> Vec<String> {
    std::fs::read_dir(root)
        .expect("read dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| Path::new(name).extension().is_some_and(|ext| ext == "tmp"))
        .collect()
}

// sb-go TestApplyTypedChangesPreservesMetadataAndUnrelatedData
#[test]
fn a_save_replaces_the_file_keeping_its_mode_and_unrelated_bytes() {
    let (_dir, root) = temp_root();
    let name = fixture(&root, "role", FIXTURE);
    std::fs::set_permissions(&name, std::fs::Permissions::from_mode(0o670)).expect("chmod");
    let before = std::fs::metadata(&name).expect("stat");
    let mut session = Session::open(&root).expect("open");
    let outcomes = session.save(
        &[
            set("role", "default", "token", "changed"),
            set("role", "empty", "added", "new"),
            edit(
                "role",
                Edit::DeleteFact {
                    instance: "default".to_owned(),
                    key: "empty".to_owned(),
                },
            ),
            edit(
                "role",
                Edit::DeleteInstance {
                    instance: "other".to_owned(),
                },
            ),
        ],
        &mut Hooks::default(),
    );
    assert!(
        matches!(outcomes["role"], RoleOutcome::Applied),
        "{outcomes:?}"
    );
    let after = std::fs::metadata(&name).expect("stat");
    assert_eq!(after.permissions().mode() & 0o7777, 0o670);
    assert_ne!(before.ino(), after.ino(), "a new file was renamed in");
    assert_eq!((before.uid(), before.gid()), (after.uid(), after.gid()));
    assert_eq!(
        read(&root, "role"),
        "; header\nhidden = secret\n[default]\n; token comment\ntoken = changed\nnone = None\n[empty]\nadded = new\n"
    );
    let catalog = session.catalog();
    assert_eq!(catalog[0].instances.len(), 2);
    assert_eq!(catalog[0].instances[1].facts[0].value, "new");
    assert_eq!(temp_files(&root), Vec::<String>::new());

    let outcomes = session.save(
        &[Change::DeleteRole {
            role: "role".to_owned(),
        }],
        &mut Hooks::default(),
    );
    assert!(
        matches!(outcomes["role"], RoleOutcome::Applied),
        "{outcomes:?}"
    );
    assert!(!name.exists());
    assert_eq!(session.catalog(), vec![]);
}

/// Criterion 25, as root: a file owned by another user keeps its owner,
/// and its mode, setuid included (chown before chmod).
#[test]
fn owner_and_mode_are_kept_as_root() {
    if std::env::var_os("SB_TEST_ROOT").is_none_or(|v| v != "1") {
        return;
    }
    for mode in [0o640, 0o4750] {
        let (_dir, root) = temp_root();
        let name = fixture(&root, "role", "[x]\nk = v\n");
        std::os::unix::fs::chown(&name, Some(65534), Some(65534)).expect("chown to nobody");
        std::fs::set_permissions(&name, std::fs::Permissions::from_mode(mode)).expect("chmod");
        let mut session = Session::open(&root).expect("open");
        let outcomes = session.save(&[set("role", "x", "k", "w")], &mut Hooks::default());
        assert!(
            matches!(outcomes["role"], RoleOutcome::Applied),
            "{outcomes:?}"
        );
        let after = std::fs::metadata(&name).expect("stat");
        assert_eq!((after.uid(), after.gid()), (65534, 65534));
        assert_eq!(after.permissions().mode() & 0o7777, mode, "{mode:o}");
        assert_eq!(read(&root, "role"), "[x]\nk = w\n");
    }
}

// sb-go TestApplyKeepsEmptySectionsAndNoopBytes
#[test]
fn a_change_to_the_same_value_writes_nothing() {
    let (_dir, root) = temp_root();
    let name = fixture(&root, "role", FIXTURE);
    let before = std::fs::metadata(&name).expect("stat").ino();
    let mut session = Session::open(&root).expect("open");
    let outcomes = session.save(
        &[set("role", "default", "token", "original")],
        &mut Hooks::default(),
    );
    assert!(
        matches!(outcomes["role"], RoleOutcome::Applied),
        "{outcomes:?}"
    );
    assert_eq!(std::fs::metadata(&name).expect("stat").ino(), before);
    assert_eq!(read(&root, "role"), FIXTURE);
    assert_eq!(session.seam.events, ["commit role", "released role"]);
}

/// The call order: the temp file is ready before the commit's checks, the
/// directory is synced after the rename, and the lock goes last.
#[test]
fn the_directory_is_synced_after_the_rename_and_before_the_release() {
    let (_dir, root) = temp_root();
    fixture(&root, "a", "[x]\nk = v\n");
    fixture(&root, "b", "[x]\nk = v\n");
    let mut session = Session::open(&root).expect("open");
    let _outcomes = session.save(
        &[set("b", "x", "k", "w"), set("a", "x", "k", "w")],
        &mut Hooks::default(),
    );
    assert_eq!(
        session.seam.events,
        [
            "prepared a",
            "commit a",
            "renamed a",
            "synced a",
            "released a",
            "prepared b",
            "commit b",
            "renamed b",
            "synced b",
            "released b",
        ]
    );
}

#[test]
fn the_temp_file_has_the_files_mode_before_the_rename() {
    let (_dir, root) = temp_root();
    let name = fixture(&root, "role", "[x]\nk = v\n");
    std::fs::set_permissions(&name, std::fs::Permissions::from_mode(0o604)).expect("chmod");
    let mut session = Session::open(&root).expect("open");
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let record = seen.clone();
    let dir = root.clone();
    session.seam.before_commit = Some(Box::new(move |_| {
        for temp in temp_files(&dir) {
            let mode = std::fs::metadata(dir.join(&temp))
                .expect("temp")
                .permissions()
                .mode();
            record.borrow_mut().push((temp, mode & 0o7777));
        }
    }));
    let _outcomes = session.save(&[set("role", "x", "k", "w")], &mut Hooks::default());
    let seen = seen.borrow();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(
        seen[0].0.starts_with(".role-") && seen[0].0.len() == ".role-".len() + 16 + ".tmp".len()
    );
    assert_eq!(seen[0].1, 0o604);
    assert_eq!(temp_files(&root), Vec::<String>::new());
}

/// The baseline changed between prepare and commit: Failed, then
/// Unattempted, the temp file gone and the disk version kept.
#[test]
fn a_file_changed_before_the_commit_fails_the_role() {
    let (_dir, root) = temp_root();
    fixture(&root, "a", "[x]\nk = v\n");
    fixture(&root, "b", "[x]\nk = v\n");
    let mut session = Session::open(&root).expect("open");
    let dir = root.clone();
    session.seam.before_commit = Some(Box::new(move |role| {
        if role == "a" {
            std::fs::write(dir.join("a.ini"), "[x]\nk = external\n").expect("external write");
        }
    }));
    let outcomes = session.save(
        &[set("a", "x", "k", "w"), set("b", "x", "k", "w")],
        &mut Hooks::default(),
    );
    assert!(
        matches!(outcomes["a"], RoleOutcome::Failed(SaveError::Changed)),
        "{outcomes:?}"
    );
    assert!(
        matches!(outcomes["b"], RoleOutcome::Unattempted),
        "{outcomes:?}"
    );
    assert_eq!(read(&root, "a"), "[x]\nk = external\n");
    assert_eq!(read(&root, "b"), "[x]\nk = v\n");
    assert_eq!(temp_files(&root), Vec::<String>::new());
    assert_eq!(
        SaveError::Changed.to_string(),
        "the file changed on disk during the save; reload required"
    );
}

#[test]
fn a_file_replaced_or_deleted_before_the_commit_fails_the_role() {
    for replace in [true, false] {
        let (_dir, root) = temp_root();
        let name = fixture(&root, "a", "[x]\nk = v\n");
        let mut session = Session::open(&root).expect("open");
        let old = root.join("a.ini.old");
        session.seam.before_commit = Some(Box::new(move |_| {
            // Moved, not removed, so a new file can't reuse the inode.
            std::fs::rename(&name, &old).expect("move");
            if replace {
                std::fs::write(&name, "[x]\nk = v\n").expect("same bytes, new file");
            }
        }));
        let outcomes = session.save(&[set("a", "x", "k", "w")], &mut Hooks::default());
        assert!(
            matches!(outcomes["a"], RoleOutcome::Failed(SaveError::Changed)),
            "{outcomes:?}"
        );
        assert_eq!(temp_files(&root), Vec::<String>::new());
    }
}

#[test]
fn a_lock_file_replaced_before_the_commit_fails_the_role() {
    let (_dir, root) = temp_root();
    fixture(&root, "a", "[x]\nk = v\n");
    let mut session = Session::open(&root).expect("open");
    let dir = root.clone();
    session.seam.before_commit = Some(Box::new(move |_| {
        std::fs::rename(dir.join("a.ini.lock"), dir.join("old.lock")).expect("move lock");
        std::fs::write(dir.join("a.ini.lock"), "").expect("new lock");
    }));
    let outcomes = session.save(&[set("a", "x", "k", "w")], &mut Hooks::default());
    assert!(
        matches!(outcomes["a"], RoleOutcome::Failed(SaveError::Changed)),
        "{outcomes:?}"
    );
    assert_eq!(read(&root, "a"), "[x]\nk = v\n");
}

// sb-go TestApplyPreflightsAllRolesAndReportsPartialCommit (per role now)
#[test]
fn a_failed_rename_stops_the_save_with_the_earlier_roles_applied() {
    let (_dir, root) = temp_root();
    for role in ["a", "b", "c"] {
        fixture(&root, role, "[default]\nk = old\n");
    }
    let mut session = Session::open(&root).expect("open");
    session.seam.fail = Some(("rename", "b".to_owned()));
    let changes: Vec<Change> = ["c", "a", "b"]
        .iter()
        .map(|role| set(role, "default", "k", "new"))
        .collect();
    let outcomes = session.save(&changes, &mut Hooks::default());
    assert!(
        matches!(outcomes["a"], RoleOutcome::Applied),
        "{outcomes:?}"
    );
    let RoleOutcome::Failed(cause) = &outcomes["b"] else {
        panic!("{outcomes:?}");
    };
    assert!(
        cause.to_string().starts_with("replace facts file: "),
        "{cause}"
    );
    assert!(
        matches!(outcomes["c"], RoleOutcome::Unattempted),
        "{outcomes:?}"
    );
    assert_eq!(read(&root, "a"), "[default]\nk = new\n");
    assert_eq!(read(&root, "b"), "[default]\nk = old\n");
    assert_eq!(read(&root, "c"), "[default]\nk = old\n");
    assert_eq!(temp_files(&root), Vec::<String>::new());
    // The applied role saves as a no-op; the others go on.
    session.seam.fail = None;
    let outcomes = session.save(&changes, &mut Hooks::default());
    assert!(
        outcomes
            .values()
            .all(|outcome| matches!(outcome, RoleOutcome::Applied)),
        "{outcomes:?}"
    );
    assert_eq!(read(&root, "c"), "[default]\nk = new\n");
}

#[test]
fn a_stop_leaves_the_remaining_roles_unattempted() {
    let (_dir, root) = temp_root();
    fixture(&root, "a", "[x]\nk = v\n");
    fixture(&root, "b", "[x]\nk = v\n");
    let mut session = Session::open(&root).expect("open");
    let mut hooks = Hooks {
        stop_after: Some(1),
        ..Hooks::default()
    };
    let outcomes = session.save(
        &[set("a", "x", "k", "w"), set("b", "x", "k", "w")],
        &mut hooks,
    );
    assert!(
        matches!(outcomes["a"], RoleOutcome::Applied),
        "{outcomes:?}"
    );
    assert!(
        matches!(outcomes["b"], RoleOutcome::Unattempted),
        "{outcomes:?}"
    );
    assert_eq!(read(&root, "b"), "[x]\nk = v\n");
}

// sb-go TestApplyRejectsInvalidAndConflictingChanges, the role's cases
#[test]
fn invalid_changes_fail_the_role_and_leave_the_disk_alone() {
    let (_dir, root) = temp_root();
    fixture(&root, "role", FIXTURE);
    let mut session = Session::open(&root).expect("open");
    let outcomes = session.save(
        &[set("missing", "default", "k", "v")],
        &mut Hooks::default(),
    );
    assert!(
        matches!(&outcomes["missing"], RoleOutcome::Failed(SaveError::UnknownRole(role)) if role == "missing"),
        "{outcomes:?}"
    );
    assert!(
        !root.join("missing.ini.lock").exists(),
        "no lock for an unknown role"
    );
    let outcomes = session.save(
        &[
            Change::DeleteRole {
                role: "role".to_owned(),
            },
            set("role", "default", "k", "v"),
        ],
        &mut Hooks::default(),
    );
    assert!(
        matches!(
            outcomes["role"],
            RoleOutcome::Failed(SaveError::DeleteConflict)
        ),
        "{outcomes:?}"
    );
    let outcomes = session.save(
        &[set("role", "default", "k", "v\0injected")],
        &mut Hooks::default(),
    );
    assert!(
        matches!(
            outcomes["role"],
            RoleOutcome::Failed(SaveError::Document(DocumentError::ValueCharacters))
        ),
        "{outcomes:?}"
    );
    assert_eq!(read(&root, "role"), FIXTURE);
    assert_eq!(temp_files(&root), Vec::<String>::new());
    assert_eq!(
        SaveError::DeleteConflict.to_string(),
        "deleting an app conflicts with its other changes"
    );
    assert_eq!(
        SaveError::UnknownRole("a\nb".to_owned()).to_string(),
        "unknown app a\\nb"
    );
}

#[test]
fn a_saved_value_is_in_the_catalog_and_the_next_save_starts_from_it() {
    let (_dir, root) = temp_root();
    fixture(&root, "role", "[x]\nk = v\n");
    let mut session = Session::open(&root).expect("open");
    let _outcomes = session.save(&[set("role", "x", "k", "one")], &mut Hooks::default());
    assert_eq!(session.catalog()[0].instances[0].facts[0].value, "one");
    let outcomes = session.save(
        &[set("role", "x", "k", "two\nlines")],
        &mut Hooks::default(),
    );
    assert!(
        matches!(outcomes["role"], RoleOutcome::Applied),
        "{outcomes:?}"
    );
    assert_eq!(read(&root, "role"), "[x]\nk = two\n\tlines\n");
}

/// A refusal found before the save: no role is locked or written, the
/// refused one is Failed and the others Unattempted, as sb-go preflights.
#[test]
fn changes_one_role_refuses_are_found_before_any_role_is_written() {
    let (_dir, root) = temp_root();
    fixture(&root, "a", "[x]\nk = v\n");
    fixture(&root, "b", "[x]\nk = v\n");
    let mut session = Session::open(&root).expect("open");
    let changes = [set("a", "x", "k", "w"), set("b", "x", "k", "w\0")];
    let refused = session.check(&changes).unwrap_err();
    assert_eq!(refused.role, "b");
    assert!(matches!(
        refused.cause,
        SaveError::Document(DocumentError::ValueCharacters)
    ));
    assert_eq!(
        refused.to_string(),
        "b: fact values must not contain carriage returns or NUL"
    );
    let outcomes = session.save(&changes, &mut Hooks::default());
    assert!(
        matches!(outcomes["a"], RoleOutcome::Unattempted),
        "{outcomes:?}"
    );
    assert!(
        matches!(outcomes["b"], RoleOutcome::Failed(SaveError::Document(_))),
        "{outcomes:?}"
    );
    assert_eq!(read(&root, "a"), "[x]\nk = v\n");
    assert!(!root.join("a.ini.lock").exists(), "nothing was locked");
    assert!(session.check(&changes[..1]).is_ok());
    assert!(session.check(&[]).is_ok());
}

/// The rename done and the directory sync failed: on disk, reported so,
/// the save stopped, and saving the same changes again is harmless.
#[test]
fn a_failed_directory_sync_after_the_rename_is_applied_unsynced() {
    let (_dir, root) = temp_root();
    fixture(&root, "a", "[x]\nk = v\nj = 1\n");
    fixture(&root, "b", "[x]\nk = v\n");
    let mut session = Session::open(&root).expect("open");
    session.seam.fail = Some(("sync", "a".to_owned()));
    let changes = [
        edit(
            "a",
            Edit::DeleteFact {
                instance: "x".to_owned(),
                key: "j".to_owned(),
            },
        ),
        set("b", "x", "k", "w"),
    ];
    let outcomes = session.save(&changes, &mut Hooks::default());
    let RoleOutcome::Unsynced(cause) = &outcomes["a"] else {
        panic!("{outcomes:?}");
    };
    assert!(
        cause.to_string().starts_with("sync facts directory: "),
        "{cause}"
    );
    assert!(
        matches!(outcomes["b"], RoleOutcome::Unattempted),
        "{outcomes:?}"
    );
    assert_eq!(read(&root, "a"), "[x]\nk = v\n");
    assert_eq!(read(&root, "b"), "[x]\nk = v\n");
    // The screen drops a's change as applied and saves b's.
    session.seam.fail = None;
    let outcomes = session.save(&changes[1..], &mut Hooks::default());
    assert!(
        matches!(outcomes["b"], RoleOutcome::Applied),
        "{outcomes:?}"
    );
}

#[test]
fn a_failed_temp_write_leaves_no_temp_file() {
    let (_dir, root) = temp_root();
    fixture(&root, "a", "[x]\nk = v\n");
    let mut session = Session::open(&root).expect("open");
    session.seam.fail = Some(("write", "a".to_owned()));
    let outcomes = session.save(&[set("a", "x", "k", "w")], &mut Hooks::default());
    let RoleOutcome::Failed(cause) = &outcomes["a"] else {
        panic!("{outcomes:?}");
    };
    assert!(
        cause.to_string().starts_with("write temp file: "),
        "{cause}"
    );
    assert_eq!(temp_files(&root), Vec::<String>::new());
    assert_eq!(read(&root, "a"), "[x]\nk = v\n");
}

/// No `Debug` form shows a fact's value (M-PUBLIC-DEBUG; tui spec
/// decision 32).
#[test]
fn debug_forms_of_the_session_and_outcomes_leave_values_out() {
    const OLD: &str = "old-not-a-real-secret";
    const NEW: &str = "new-not-a-real-secret";
    let (_dir, root) = temp_root();
    fixture(&root, "plex", &format!("[plex]\ntoken = {OLD}\n"));
    let mut session = Session::open(&root).expect("open");
    let shown_session = format!("{session:?} {:?}", session.roles["plex"]);
    let outcomes = session.save(&[set("plex", "plex", "token", NEW)], &mut Hooks::default());
    let drift = Drift {
        before: session.catalog()[0].clone(),
        after: Some(session.catalog()[0].clone()),
    };
    let shown = format!("{shown_session} {session:?} {outcomes:?} {drift:?}");
    assert!(!shown.contains(OLD) && !shown.contains(NEW), "{shown}");
    let state = format!("{:?}", session.roles["plex"]);
    assert!(
        state.starts_with("RoleState {") && state.contains("token"),
        "{state}"
    );
}

/// The temp file's name, planted beforehand as a symlink out of the
/// directory: the create refuses it (`O_EXCL`, `O_NOFOLLOW`), nothing is
/// written through it, and the role fails.
#[test]
fn a_planted_temp_name_fails_the_save_and_writes_nothing_through_it() {
    let (_dir, root) = temp_root();
    fixture(&root, "role", "[x]\nk = v\n");
    let outside = root.join("outside");
    std::fs::create_dir(&outside).expect("outside dir");
    let target = outside.join("target");
    std::fs::write(&target, "untouched").expect("target");
    let mut session = Session::open(&root).expect("open");
    // The save draws its name from the thread's generator: seed it twice
    // the same way to know the name.
    fastrand::seed(7);
    let random: String = std::iter::repeat_with(fastrand::alphanumeric)
        .take(16)
        .collect();
    let planted = root.join(format!(".role-{random}.tmp"));
    std::os::unix::fs::symlink(&target, &planted).expect("planted symlink");
    fastrand::seed(7);
    let outcomes = session.save(&[set("role", "x", "k", "w")], &mut Hooks::default());
    let RoleOutcome::Failed(cause) = &outcomes["role"] else {
        panic!("{outcomes:?}");
    };
    assert!(
        cause.to_string().starts_with("create temp file: "),
        "{cause}"
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "untouched");
    assert!(std::fs::symlink_metadata(&planted).unwrap().is_symlink());
    assert_eq!(read(&root, "role"), "[x]\nk = v\n");
}
