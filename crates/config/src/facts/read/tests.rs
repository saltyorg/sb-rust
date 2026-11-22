//! The reader's tests on temporary facts directories (tui spec, "`sb fact
//! get` and `sb fact list`"): every lookup case and its exact text, the
//! JSON, the listing, the store's refusals, and that neither the editor's
//! lock nor Saltbox's role lock stops a read.

use std::path::{Path, PathBuf};

use rustix::fs::FlockOperation;

use super::{Found, LookupError, Name, Store, get, list, names_json};
use crate::facts::session::{EDITOR_LOCK, Session};

/// A known secret in the fixtures: never in an error or a `Debug`.
const SECRET: &str = "not-a-real-token";

/// A temporary facts directory, by its real path (no symlink in it), with
/// plex (two instances), teslamate (one) and homepage (a multi-line
/// value, an empty one and a `DEFAULT` section).
fn store_dir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = std::fs::canonicalize(dir.path()).expect("canonical path");
    write(
        &path,
        "plex",
        &format!("[plex]\ntoken = {SECRET}\nclient_identifier = abc\n\n[plex-4k]\ntoken = other\n"),
    );
    write(
        &path,
        "teslamate",
        "[teslamate]\npostgres_password = pw\nsecret_key = sk\n",
    );
    write(
        &path,
        "homepage",
        "[DEFAULT]\nshared = d\n\n[homepage]\nmotd = first line\n\tsecond line\nempty =\n",
    );
    (dir, path)
}

fn write(root: &Path, app: &str, text: &str) {
    std::fs::write(root.join(format!("{app}.ini")), text).expect("write fixture");
}

fn open(root: &Path) -> Store {
    Store::open(root).expect("the store opens")
}

fn value(root: &Path, app: &str, instance: Option<&str>, key: &str) -> String {
    get(&open(root), app, instance, key).expect("found").value
}

fn miss(root: &Path, app: &str, instance: Option<&str>, key: &str) -> String {
    let err = get(&open(root), app, instance, key).expect_err("a miss");
    let text = err.to_string();
    assert!(!text.contains(SECRET), "{text}");
    assert!(!format!("{err:?}").contains(SECRET), "{err:?}");
    text
}

#[test]
fn the_three_word_form_reads_the_value() {
    let (_dir, root) = store_dir();
    assert_eq!(value(&root, "plex", Some("plex"), "token"), SECRET);
    assert_eq!(value(&root, "plex", Some("plex-4k"), "token"), "other");
}

#[test]
fn the_found_fact_names_itself_and_hides_its_value_in_debug() {
    let (_dir, root) = store_dir();
    let found = get(&open(&root), "plex", Some("plex"), "token").expect("found");
    assert_eq!(
        (
            found.app.as_str(),
            found.instance.as_str(),
            found.key.as_str()
        ),
        ("plex", "plex", "token")
    );
    let debug = format!("{found:?}");
    assert!(debug.contains("token"), "{debug}");
    assert!(!debug.contains(SECRET), "{debug}");
}

#[test]
fn the_two_word_form_reads_an_app_with_one_instance() {
    let (_dir, root) = store_dir();
    assert_eq!(value(&root, "teslamate", None, "postgres_password"), "pw");
    let found = get(&open(&root), "teslamate", None, "secret_key").expect("found");
    assert_eq!(found.instance, "teslamate");
}

#[test]
fn the_two_word_form_with_several_instances_names_them() {
    let (_dir, root) = store_dir();
    assert_eq!(
        miss(&root, "plex", None, "token"),
        "app 'plex' has 2 instances; name one: plex, plex-4k"
    );
}

#[test]
fn a_multi_line_value_is_joined_by_newlines() {
    let (_dir, root) = store_dir();
    assert_eq!(
        value(&root, "homepage", Some("homepage"), "motd"),
        "first line\nsecond line"
    );
}

#[test]
fn an_unknown_app_points_at_the_list() {
    let (_dir, root) = store_dir();
    for app in ["plexx", "", "../plex", "pl/ex", "plex\0", "plex.ini"] {
        assert_eq!(
            miss(&root, app, Some("plex"), "token"),
            format!(
                "no facts for app '{}'; run sb fact list to see them",
                sb_cli_core::exit::escaped(app)
            ),
            "{app:?}"
        );
    }
}

#[test]
fn an_unknown_instance_lists_the_instances() {
    let (_dir, root) = store_dir();
    assert_eq!(
        miss(&root, "plex", Some("plex2"), "token"),
        "app 'plex' has no instance 'plex2'; instances: plex, plex-4k"
    );
}

#[test]
fn default_is_never_an_instance() {
    let (_dir, root) = store_dir();
    assert_eq!(
        miss(&root, "homepage", Some("DEFAULT"), "shared"),
        "app 'homepage' has no instance 'DEFAULT'; instances: homepage"
    );
    // homepage's one instance is homepage: DEFAULT doesn't make it two.
    assert_eq!(
        value(&root, "homepage", None, "motd"),
        "first line\nsecond line"
    );
}

#[test]
fn an_app_without_instances_says_so() {
    let (_dir, root) = store_dir();
    write(&root, "bare", "[DEFAULT]\nx = 1\n");
    for instance in [None, Some("bare")] {
        assert_eq!(
            miss(&root, "bare", instance, "x"),
            "app 'bare' has no instances",
            "{instance:?}"
        );
    }
}

#[test]
fn an_unknown_key_lists_the_facts() {
    let (_dir, root) = store_dir();
    assert_eq!(
        miss(&root, "plex", Some("plex"), "tokn"),
        "plex/plex has no fact 'tokn'; facts: client_identifier, token"
    );
}

#[test]
fn an_empty_value_is_a_miss() {
    let (_dir, root) = store_dir();
    assert_eq!(
        miss(&root, "homepage", Some("homepage"), "empty"),
        "homepage/homepage empty is empty"
    );
}

#[test]
fn names_in_errors_are_escaped() {
    let (_dir, root) = store_dir();
    assert_eq!(
        miss(&root, "plex", Some("a\x1b]0;x\x07"), "token"),
        "app 'plex' has no instance 'a\\u{1b}]0;x\\u{7}'; instances: plex, plex-4k"
    );
    assert_eq!(
        miss(&root, "plex", Some("plex"), "k\nnew"),
        "plex/plex has no fact 'k\\nnew'; facts: client_identifier, token"
    );
}

#[test]
fn a_read_works_while_the_editor_is_open() {
    let (_dir, root) = store_dir();
    let session = Session::open(&root).expect("the editor's session");
    assert_eq!(value(&root, "plex", Some("plex"), "token"), SECRET);
    assert_eq!(list(&open(&root), None, None).expect("listed").len(), 7);
    drop(session);
}

#[test]
fn a_read_works_while_saltbox_holds_the_role_lock() {
    let (_dir, root) = store_dir();
    let lock = std::fs::File::create(root.join("plex.ini.lock")).expect("lock");
    rustix::fs::flock(&lock, FlockOperation::NonBlockingLockExclusive).expect("locked");
    assert_eq!(value(&root, "plex", Some("plex"), "token"), SECRET);
}

#[test]
fn a_read_takes_no_lock_and_writes_nothing() {
    let (_dir, root) = store_dir();
    let before: Vec<_> = names_in(&root);
    let _ = value(&root, "plex", Some("plex"), "token");
    let _ = list(&open(&root), None, None).expect("listed");
    assert_eq!(names_in(&root), before);
    assert!(!root.join(EDITOR_LOCK).exists());
}

fn names_in(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root)
        .expect("list")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

#[test]
fn a_symlinked_directory_is_refused() {
    let (_dir, root) = store_dir();
    let link = root.join("link");
    std::os::unix::fs::symlink(&root, &link).expect("symlink");
    let err = Store::open(&link).expect_err("refused");
    assert!(
        err.to_string()
            .starts_with("facts directory must be an existing directory without symlinks"),
        "{err}"
    );
    let err = Store::open(&root.join("missing")).expect_err("refused");
    assert!(err.to_string().starts_with("facts directory"), "{err}");
    let err = Store::open(Path::new("relative")).expect_err("refused");
    assert!(err.to_string().starts_with("facts directory"), "{err}");
}

#[test]
fn a_symlinked_file_is_refused() {
    let (_dir, root) = store_dir();
    std::os::unix::fs::symlink(root.join("plex.ini"), root.join("linked.ini")).expect("symlink");
    assert_eq!(
        miss(&root, "linked", Some("plex"), "token"),
        "catalog linked: facts file linked.ini must not be a symlink"
    );
    let err = list(&open(&root), None, None).expect_err("refused");
    assert_eq!(
        err.to_string(),
        "catalog linked: facts file linked.ini must not be a symlink"
    );
}

#[test]
fn a_fifo_is_refused_without_hanging() {
    let (_dir, root) = store_dir();
    rustix::fs::mknodat(
        rustix::fs::CWD,
        root.join("fifo.ini"),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o600),
        0,
    )
    .expect("fifo");
    assert_eq!(
        miss(&root, "fifo", Some("x"), "y"),
        "catalog fifo: facts file fifo.ini must be a regular file"
    );
}

#[test]
fn a_file_over_one_mib_is_refused() {
    let (_dir, root) = store_dir();
    let big = format!(
        "[big]\nk = {}\n",
        "x".repeat(sb_cli_core::config::MAX_FILE_BYTES)
    );
    write(&root, "big", &big);
    assert_eq!(
        miss(&root, "big", Some("big"), "k"),
        format!(
            "catalog big: facts file big.ini is bigger than {} bytes",
            sb_cli_core::config::MAX_FILE_BYTES
        )
    );
}

#[test]
fn an_unreadable_file_is_refused_naming_it() {
    let (_dir, root) = store_dir();
    write(&root, "broken", "[broken]\nport = 1\rx\n");
    let text = miss(&root, "broken", Some("broken"), "port");
    assert!(text.starts_with("catalog broken: "), "{text}");
    // Another app reads as ever: get opens only its own file.
    assert_eq!(value(&root, "plex", Some("plex"), "token"), SECRET);
    let err = list(&open(&root), None, None).expect_err("refused");
    assert!(err.to_string().starts_with("catalog broken: "), "{err}");
}

fn name(app: &str, instance: &str, key: &str) -> Name {
    Name {
        app: app.to_owned(),
        instance: instance.to_owned(),
        key: key.to_owned(),
    }
}

#[test]
fn list_gives_every_name_sorted_and_no_value() {
    let (_dir, root) = store_dir();
    assert_eq!(
        list(&open(&root), None, None).expect("listed"),
        [
            name("homepage", "homepage", "empty"),
            name("homepage", "homepage", "motd"),
            name("plex", "plex", "client_identifier"),
            name("plex", "plex", "token"),
            name("plex", "plex-4k", "token"),
            name("teslamate", "teslamate", "postgres_password"),
            name("teslamate", "teslamate", "secret_key"),
        ]
    );
}

#[test]
fn list_narrows_by_app_and_instance() {
    let (_dir, root) = store_dir();
    let store = open(&root);
    assert_eq!(
        list(&store, Some("plex"), None).expect("listed"),
        [
            name("plex", "plex", "client_identifier"),
            name("plex", "plex", "token"),
            name("plex", "plex-4k", "token"),
        ]
    );
    assert_eq!(
        list(&store, Some("plex"), Some("plex-4k")).expect("listed"),
        [name("plex", "plex-4k", "token")]
    );
    // Narrowed to one app, a broken other file doesn't matter.
    write(&root, "broken", "[broken]\nport = 1\rx\n");
    assert_eq!(
        list(&store, Some("teslamate"), None).expect("listed").len(),
        2
    );
}

#[test]
fn list_misses_say_which() {
    let (_dir, root) = store_dir();
    let store = open(&root);
    let text = |app, instance| {
        list(&store, Some(app), instance)
            .expect_err("a miss")
            .to_string()
    };
    assert_eq!(
        text("plexx", None),
        "no facts for app 'plexx'; run sb fact list to see them"
    );
    assert_eq!(
        text("plex", Some("x")),
        "app 'plex' has no instance 'x'; instances: plex, plex-4k"
    );
}

#[test]
fn an_empty_store_lists_nothing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical path");
    std::fs::write(root.join("notes.txt"), "not a fact file").expect("write");
    assert_eq!(list(&open(&root), None, None).expect("listed"), []);
    assert_eq!(names_json(&[]), "[]\n");
}

#[test]
fn the_value_json_is_one_lossless_line() {
    let found = Found {
        app: "plex".to_owned(),
        instance: "plex".to_owned(),
        key: "token".to_owned(),
        value: "a\nb\t\"c\"\\".to_owned(),
    };
    assert_eq!(
        found.json(),
        "{\"app\":\"plex\",\"instance\":\"plex\",\"key\":\"token\",\"value\":\"a\\nb\\t\\\"c\\\"\\\\\"}\n"
    );
}

#[test]
fn the_json_escapes_every_control_character() {
    let found = Found {
        app: "a\u{85}".to_owned(),
        instance: "i".to_owned(),
        key: "k".to_owned(),
        value: "\x1b]0;t\x07\u{7f}\u{9b}2J é\u{a0}".to_owned(),
    };
    let json = found.json();
    assert_eq!(
        json,
        "{\"app\":\"a\\u0085\",\"instance\":\"i\",\"key\":\"k\",\"value\":\"\\u001b]0;t\\u0007\\u007f\\u009b2J é\u{a0}\"}\n"
    );
    assert!(!json.trim_end().chars().any(char::is_control), "{json:?}");
}

#[test]
fn the_names_json_is_an_array_of_names() {
    assert_eq!(
        names_json(&[name("plex", "plex", "token"), name("t", "t\u{9b}", "k")]),
        "[{\"app\":\"plex\",\"instance\":\"plex\",\"key\":\"token\"},{\"app\":\"t\",\"instance\":\"t\\u009b\",\"key\":\"k\"}]\n"
    );
}

#[test]
fn lookup_errors_from_the_store_keep_its_text() {
    let err = LookupError::from(crate::facts::session::SessionError::EditorActive);
    assert_eq!(err.to_string(), "another fact editor is active");
}

#[test]
fn the_store_lists_apps_and_reads_one_role() {
    let (_dir, root) = store_dir();
    let store = open(&root);
    assert_eq!(
        store.apps().expect("apps"),
        ["homepage", "plex", "teslamate"]
    );
    let role = store.role("plex").expect("read").expect("there");
    let instances: Vec<&str> = role.instances.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(instances, ["plex", "plex-4k"]);
    assert!(store.role("missing").expect("read").is_none());
    assert!(store.role("a/b").expect("read").is_none());
}
