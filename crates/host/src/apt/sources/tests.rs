//! `add_repositories`' rules on temporary roots (host spec, Testing
//! Strategy: sb-go's four `apt_test.go` cases, unchanged on refusal, a
//! symlinked `sources.list`, mode and owner kept), and the fenced cleanup
//! (ops spec, decisions 5 and 28). Nothing here touches `/etc/apt` or
//! `/var/backups`: the roots are test-only fields.

use std::collections::BTreeMap;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

use super::*;

const RUN: &str = "0123456789abcdef";

/// `etc/apt/sources.list.d` and `var/backups` in a temp dir, owned by the
/// test's own user.
struct Fixture {
    dir: tempfile::TempDir,
    roots: Roots,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let etc_apt = dir.path().join("etc/apt");
        let var_backups = dir.path().join("var/backups");
        std::fs::create_dir_all(etc_apt.join("sources.list.d")).expect("mkdir");
        std::fs::create_dir_all(&var_backups).expect("mkdir");
        for path in [&etc_apt.join("sources.list.d"), &var_backups] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let roots = Roots {
            etc_apt,
            var_backups,
            owner: rustix::process::geteuid().as_raw(),
            ..Roots::default()
        };
        Self { dir, roots }
    }

    fn sources_d(&self) -> PathBuf {
        self.roots.etc_apt.join("sources.list.d")
    }

    fn sources_list(&self) -> PathBuf {
        self.roots.etc_apt.join("sources.list")
    }

    fn backup(&self) -> PathBuf {
        self.roots.var_backups.join("sb/apt-sources").join(RUN)
    }

    /// Every entry under the temp dir: a file's content, a link's target,
    /// or `<dir>`, with its mode.
    fn tree(&self) -> BTreeMap<PathBuf, String> {
        walkdir::WalkDir::new(self.dir.path())
            .into_iter()
            .map(|entry| {
                let entry = entry.expect("walk");
                let meta = entry.path().symlink_metadata().expect("lstat");
                let what = if meta.file_type().is_symlink() {
                    format!(
                        "-> {}",
                        std::fs::read_link(entry.path())
                            .expect("readlink")
                            .display()
                    )
                } else if meta.is_dir() {
                    "<dir>".to_string()
                } else if meta.is_file() {
                    String::from_utf8_lossy(&std::fs::read(entry.path()).expect("read"))
                        .into_owned()
                } else {
                    "<other>".to_string()
                };
                (
                    entry.path().to_path_buf(),
                    format!("{:o} {what}", meta.mode() & 0o7777),
                )
            })
            .collect()
    }
}

/// `text` at `path`, 0644.
fn write(path: &Path, text: &str) {
    std::fs::write(path, text).expect("write");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).expect("read")
}

fn mode(path: &Path) -> u32 {
    path.symlink_metadata().expect("lstat").mode() & 0o7777
}

/// [`apply`] with a fresh report, returned on success.
fn run(roots: &Roots, codename: &str, run_id: &str) -> Result<Report, SourcesError> {
    let mut report = Report::default();
    apply(roots, codename, run_id, &mut report)?;
    Ok(report)
}

/// A FIFO named `name` in `dir`.
fn fifo(dir: &Path, name: &str) {
    let dir =
        rustix::fs::open(dir, OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty()).expect("open dir");
    rustix::fs::mknodat(&dir, name, FileType::Fifo, Mode::RUSR | Mode::WUSR, 0).expect("mkfifo");
}

#[test]
fn the_release_rule_is_sb_gos() {
    assert_eq!(release("jammy").expect("jammy"), Release::Legacy);
    assert_eq!(release("noble").expect("noble"), Release::Deb822);
    assert_eq!(release("resolute").expect("resolute"), Release::Deb822);
    for codename in ["focal", "plucky", "", "Jammy"] {
        let error = release(codename).expect_err("unsupported");
        assert_eq!(
            error.to_string(),
            format!("unsupported Ubuntu release: {codename}")
        );
    }
}

#[test]
fn an_unsupported_release_changes_nothing() {
    let fixture = Fixture::new();
    write(
        &fixture.sources_list(),
        "deb http://mirror.example.invalid/ubuntu focal main\n",
    );
    write(&fixture.sources_d().join("ubuntu.sources"), "official\n");
    write(
        &fixture.sources_d().join("third-party.list"),
        "third party\n",
    );
    let before = fixture.tree();
    let error = run(&fixture.roots, "focal", RUN).expect_err("unsupported");
    assert_eq!(error.to_string(), "unsupported Ubuntu release: focal");
    assert_eq!(fixture.tree(), before);
}

#[test]
fn deb822_keeps_official_sources_and_moves_the_rest_aside() {
    for codename in ["noble", "resolute"] {
        let fixture = Fixture::new();
        let official = format!(
            "Types: deb\nURIs: http://archive.ubuntu.com/ubuntu/\nSuites: {codename} {codename}-updates\n"
        );
        write(&fixture.sources_d().join("ubuntu.sources"), &official);
        write(
            &fixture.sources_d().join("third-party.list"),
            "deb https://packages.example.invalid stable main\n",
        );
        let report = run(&fixture.roots, codename, RUN).expect("apply");
        assert_eq!(report.moved, ["third-party.list"]);
        assert_eq!(report.backup, Some(fixture.backup()));
        assert_eq!(read(&fixture.sources_d().join("ubuntu.sources")), official);
        assert!(!fixture.sources_d().join("third-party.list").exists());
        assert!(!fixture.sources_d().join("ubuntu-archive.sources").exists());
        assert_eq!(
            read(&fixture.backup().join("third-party.list")),
            "deb https://packages.example.invalid stable main\n"
        );
    }
}

#[test]
fn deb822_adds_the_official_archive_beside_a_custom_mirror() {
    for codename in ["noble", "resolute"] {
        let fixture = Fixture::new();
        let custom = format!(
            "Types: deb\nURIs: https://mirror.example.invalid/ubuntu/\nSuites: {codename}\n"
        );
        write(&fixture.sources_d().join("ubuntu.sources"), &custom);
        let report = run(&fixture.roots, codename, RUN).expect("apply");
        assert_eq!(report, Report::default(), "nothing to move");
        assert_eq!(read(&fixture.sources_d().join("ubuntu.sources")), custom);
        let archive = fixture.sources_d().join("ubuntu-archive.sources");
        assert_eq!(
            read(&archive),
            format!(
                "Types: deb\n\
                 URIs: http://archive.ubuntu.com/ubuntu/\n\
                 Suites: {codename} {codename}-updates {codename}-backports\n\
                 Components: main restricted universe multiverse\n\
                 Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n\
                 \n\
                 Types: deb\n\
                 URIs: http://security.ubuntu.com/ubuntu/\n\
                 Suites: {codename}-security\n\
                 Components: main restricted universe multiverse\n\
                 Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n"
            )
        );
        assert_eq!(mode(&archive), 0o644);
        assert_eq!(
            std::fs::read_dir(fixture.sources_d())
                .expect("list")
                .count(),
            2,
            "no temporary file left"
        );
    }
}

#[test]
fn deb822_without_ubuntu_sources_adds_the_archive() {
    let fixture = Fixture::new();
    run(&fixture.roots, "noble", RUN).expect("apply");
    assert!(fixture.sources_d().join("ubuntu-archive.sources").exists());
}

#[test]
fn jammy_appends_the_archive_and_moves_every_entry() {
    let fixture = Fixture::new();
    write(&fixture.sources_list(), "# existing configuration\n");
    write(
        &fixture.sources_d().join("ubuntu.sources"),
        "DEB822 data not used on Jammy\n",
    );
    write(
        &fixture.sources_d().join("third-party.list"),
        "third party\n",
    );
    let report = run(&fixture.roots, "jammy", RUN).expect("apply");
    assert_eq!(report.moved, ["third-party.list", "ubuntu.sources"]);
    assert_eq!(
        read(&fixture.sources_list()),
        "# existing configuration\n\
         deb http://archive.ubuntu.com/ubuntu/ jammy main\n\
         deb http://archive.ubuntu.com/ubuntu/ jammy universe\n\
         deb http://archive.ubuntu.com/ubuntu/ jammy restricted\n\
         deb http://archive.ubuntu.com/ubuntu/ jammy multiverse\n"
    );
    assert_eq!(
        std::fs::read_dir(fixture.sources_d())
            .expect("list")
            .count(),
        0
    );
}

#[test]
fn jammy_drops_blank_and_duplicate_lines() {
    let fixture = Fixture::new();
    write(
        &fixture.sources_list(),
        "deb http://archive.ubuntu.com/ubuntu/ jammy universe\n\n  # note\n# note\n\
         deb http://archive.ubuntu.com/ubuntu/ jammy universe\n",
    );
    run(&fixture.roots, "jammy", RUN).expect("apply");
    assert_eq!(
        read(&fixture.sources_list()),
        "deb http://archive.ubuntu.com/ubuntu/ jammy universe\n\
         # note\n\
         deb http://archive.ubuntu.com/ubuntu/ jammy main\n\
         deb http://archive.ubuntu.com/ubuntu/ jammy restricted\n\
         deb http://archive.ubuntu.com/ubuntu/ jammy multiverse\n"
    );
}

#[test]
fn jammy_creates_a_missing_sources_list_0644() {
    let fixture = Fixture::new();
    run(&fixture.roots, "jammy", RUN).expect("apply");
    assert_eq!(read(&fixture.sources_list()).lines().count(), 4);
    assert_eq!(mode(&fixture.sources_list()), 0o644);
}

#[test]
fn jammy_keeps_the_mode_and_writes_through_a_symlink() {
    let fixture = Fixture::new();
    let target = fixture.dir.path().join("real.list");
    write(&target, "# real\n");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).expect("chmod");
    std::os::unix::fs::symlink(&target, fixture.sources_list()).expect("symlink");
    run(&fixture.roots, "jammy", RUN).expect("apply");
    assert!(
        fixture
            .sources_list()
            .symlink_metadata()
            .expect("lstat")
            .file_type()
            .is_symlink()
    );
    assert!(
        read(&target).starts_with("# real\ndeb http://archive.ubuntu.com/ubuntu/ jammy main\n")
    );
    assert_eq!(mode(&target), 0o640);
    let owner = target.metadata().expect("stat").uid();
    assert_eq!(owner, fixture.roots.owner);
}

#[test]
fn a_missing_sources_list_d_has_nothing_to_move() {
    let fixture = Fixture::new();
    std::fs::remove_dir(fixture.sources_d()).expect("rmdir");
    let report = run(&fixture.roots, "jammy", RUN).expect("apply");
    assert_eq!(report, Report::default());
}

#[test]
fn nothing_to_move_makes_no_backup_directory() {
    let fixture = Fixture::new();
    write(
        &fixture.sources_d().join("ubuntu.sources"),
        "URIs: http://archive.ubuntu.com/ubuntu/\n",
    );
    run(&fixture.roots, "noble", RUN).expect("apply");
    assert!(!fixture.roots.var_backups.join("sb").exists());
}

#[test]
fn the_backup_directories_are_root_only() {
    let fixture = Fixture::new();
    write(&fixture.sources_d().join("docker.list"), "docker\n");
    run(&fixture.roots, "jammy", RUN).expect("apply");
    for dir in [
        fixture.roots.var_backups.join("sb"),
        fixture.roots.var_backups.join("sb/apt-sources"),
        fixture.backup(),
    ] {
        assert_eq!(mode(&dir), 0o700, "{}", dir.display());
    }
    assert_eq!(mode(&fixture.backup().join("docker.list")), 0o644);
}

/// `apply` refused with `refusing to clean <dir>: <reason>` and changed
/// nothing.
fn assert_refused(fixture: &Fixture, dir: &Path, reason: &str) {
    let before = fixture.tree();
    let error = run(&fixture.roots, "jammy", RUN).expect_err("refused");
    assert_eq!(
        error.to_string(),
        format!("refusing to clean {}: {reason}", dir.display())
    );
    assert_eq!(fixture.tree(), before);
}

#[test]
fn a_symlinked_sources_list_d_is_refused() {
    let fixture = Fixture::new();
    let elsewhere = fixture.dir.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).expect("mkdir");
    write(&elsewhere.join("keep.list"), "keep\n");
    std::fs::remove_dir(fixture.sources_d()).expect("rmdir");
    std::os::unix::fs::symlink(&elsewhere, fixture.sources_d()).expect("symlink");
    assert_refused(&fixture, &fixture.sources_d(), "it is a symlink");
}

#[test]
fn a_writable_or_foreign_sources_list_d_is_refused() {
    let fixture = Fixture::new();
    write(&fixture.sources_d().join("keep.list"), "keep\n");
    for mode in [0o775, 0o757] {
        std::fs::set_permissions(fixture.sources_d(), std::fs::Permissions::from_mode(mode))
            .expect("chmod");
        assert_refused(
            &fixture,
            &fixture.sources_d(),
            "writable by group or others",
        );
    }
    std::fs::set_permissions(fixture.sources_d(), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    let mut foreign = Fixture {
        dir: fixture.dir,
        roots: fixture.roots.clone(),
    };
    foreign.roots.owner += 1;
    let dir = foreign.sources_d();
    assert_refused(&foreign, &dir, "not owned by root");
}

#[test]
fn a_file_in_place_of_sources_list_d_is_refused() {
    let fixture = Fixture::new();
    std::fs::remove_dir(fixture.sources_d()).expect("rmdir");
    write(&fixture.sources_d(), "not a directory\n");
    assert_refused(&fixture, &fixture.sources_d(), "not a directory");
}

#[test]
fn a_symlinked_or_writable_backup_directory_is_refused() {
    let fixture = Fixture::new();
    write(&fixture.sources_d().join("keep.list"), "keep\n");
    let elsewhere = fixture.dir.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).expect("mkdir");
    let sb = fixture.roots.var_backups.join("sb");
    std::os::unix::fs::symlink(&elsewhere, &sb).expect("symlink");
    assert_refused(&fixture, &sb, "it is a symlink");
    std::fs::remove_file(&sb).expect("unlink");
    std::fs::create_dir_all(sb.join("apt-sources")).expect("mkdir");
    std::fs::set_permissions(&sb, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    std::fs::set_permissions(
        sb.join("apt-sources"),
        std::fs::Permissions::from_mode(0o770),
    )
    .expect("chmod");
    assert_refused(
        &fixture,
        &sb.join("apt-sources"),
        "writable by group or others",
    );
}

#[test]
fn an_existing_run_directory_is_refused() {
    let fixture = Fixture::new();
    write(&fixture.sources_d().join("keep.list"), "keep\n");
    std::fs::create_dir_all(fixture.backup()).expect("mkdir");
    for dir in ["sb", "sb/apt-sources"] {
        let path = fixture.roots.var_backups.join(dir);
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    }
    assert_refused(&fixture, &fixture.backup(), "it already exists");
}

#[test]
fn a_run_id_that_is_not_a_name_is_refused() {
    let fixture = Fixture::new();
    write(&fixture.sources_d().join("keep.list"), "keep\n");
    let before = fixture.tree();
    for run_id in ["", "..", "a/b", "x y"] {
        let error = run(&fixture.roots, "jammy", run_id).expect_err("refused");
        assert!(error.to_string().contains("not a run id"), "{error}");
    }
    assert_eq!(fixture.tree(), before);
}

#[test]
fn a_subdirectory_fails_before_anything_moves() {
    let fixture = Fixture::new();
    write(&fixture.sources_d().join("a.list"), "a\n");
    std::fs::create_dir(fixture.sources_d().join("nested")).expect("mkdir");
    let before = fixture.tree();
    let error = run(&fixture.roots, "jammy", RUN).expect_err("subdirectory");
    assert_eq!(
        error.to_string(),
        format!(
            "error removing {}: Is a directory (os error 21)",
            fixture.sources_d().join("nested").display()
        )
    );
    assert_eq!(fixture.tree(), before);
}

#[test]
fn a_symlink_entry_moves_as_a_link() {
    let fixture = Fixture::new();
    let target = fixture.dir.path().join("target.list");
    write(&target, "target\n");
    std::os::unix::fs::symlink(&target, fixture.sources_d().join("link.list")).expect("symlink");
    run(&fixture.roots, "jammy", RUN).expect("apply");
    let moved = fixture.backup().join("link.list");
    assert_eq!(std::fs::read_link(&moved).expect("readlink"), target);
    assert_eq!(read(&target), "target\n");
}

#[test]
fn across_filesystems_entries_are_copied_then_removed() {
    let mut fixture = Fixture::new();
    fixture.roots.rename = |_, _, _, _| Err(rustix::io::Errno::XDEV);
    let target = fixture.dir.path().join("target.list");
    write(&target, "target\n");
    write(&fixture.sources_d().join("a.list"), "a\n");
    std::fs::set_permissions(
        fixture.sources_d().join("a.list"),
        std::fs::Permissions::from_mode(0o600),
    )
    .expect("chmod");
    std::os::unix::fs::symlink(&target, fixture.sources_d().join("link.list")).expect("symlink");
    let report = run(&fixture.roots, "jammy", RUN).expect("apply");
    assert_eq!(report.moved, ["a.list", "link.list"]);
    assert_eq!(read(&fixture.backup().join("a.list")), "a\n");
    assert_eq!(mode(&fixture.backup().join("a.list")), 0o600);
    assert_eq!(
        std::fs::read_link(fixture.backup().join("link.list")).expect("readlink"),
        target
    );
    assert_eq!(
        std::fs::read_dir(fixture.sources_d())
            .expect("list")
            .count(),
        0
    );
}

#[test]
fn another_move_failure_is_named() {
    let mut fixture = Fixture::new();
    fixture.roots.rename = |_, _, _, _| Err(rustix::io::Errno::PERM);
    write(&fixture.sources_d().join("a.list"), "a\n");
    let error = run(&fixture.roots, "jammy", RUN).expect_err("failed");
    assert_eq!(
        error.to_string(),
        format!(
            "error moving {} to {}: Operation not permitted (os error 1)",
            fixture.sources_d().join("a.list").display(),
            fixture.backup().display()
        )
    );
    assert!(fixture.sources_d().join("a.list").exists());
}

std::thread_local! {
    /// This thread's captured log text while [`capture`] runs, else `None`.
    static CAPTURE: std::cell::RefCell<Option<Vec<u8>>> = const { std::cell::RefCell::new(None) };
}

/// The global subscriber's writer: appends to the calling thread's
/// [`CAPTURE`] buffer when one is on, and drops the text otherwise. The
/// fmt layer writes on the thread that emits the event.
struct ThreadCapture;

impl io::Write for ThreadCapture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        CAPTURE.with_borrow_mut(|buffer| {
            if let Some(buffer) = buffer {
                buffer.extend_from_slice(bytes);
            }
        });
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Runs `body` with this thread's log records captured, and returns its
/// result with their text.
///
/// One global subscriber for the whole test binary, installed once
/// (<https://docs.rs/tracing/0.1.44/tracing/subscriber/fn.set_global_default.html>).
/// A scoped one (`with_default`) is not enough: callsite interest is
/// process-wide, and a test on another thread with no subscriber can
/// cache a callsite as "never"
/// (<https://docs.rs/tracing-core/0.1.36/tracing_core/callsite/index.html>),
/// so records went missing in the full suite. With a global dispatcher
/// always present, no callsite is cached as "never". No other test in
/// this binary installs a subscriber.
fn capture<T>(body: impl FnOnce() -> T) -> (T, String) {
    static GLOBAL: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    GLOBAL.get_or_init(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_writer(|| ThreadCapture)
            .with_ansi(false)
            .finish();
        tracing::subscriber::set_global_default(subscriber).expect("the only global subscriber");
    });
    CAPTURE.set(Some(Vec::new()));
    let result = body();
    let bytes = CAPTURE.take().unwrap_or_default();
    (result, String::from_utf8_lossy(&bytes).into_owned())
}

/// Every name logged, escaped, before the first one moves (ops spec,
/// "Evidence before acting"), captured through [`capture`]. Only the
/// records before the first move are asserted. The renameat stand-in logs
/// each call, to order the moves among the records.
#[test]
fn every_name_is_logged_before_anything_moves() {
    let mut fixture = Fixture::new();
    fixture.roots.rename = |from, name, to, new| {
        tracing::info!("renameat called");
        rustix::fs::renameat(from, name, to, new)
    };
    for name in ["a.list", "b.list", "evil\nname.list"] {
        write(&fixture.sources_d().join(name), "x\n");
    }
    let (result, text) = capture(|| run(&fixture.roots, "jammy", RUN));
    result.expect("apply");
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.contains("apt source moved aside"))
        .collect();
    assert_eq!(lines.len(), 6, "{text}");
    for (line, name) in lines[..3]
        .iter()
        .zip(["a.list", "b.list", "evil\\nname.list"])
    {
        assert!(
            line.contains("apt source to move aside") && line.contains(&format!("name={name}")),
            "{text}"
        );
    }
    assert!(lines[3].contains("renameat called"), "{text}");
}

#[test]
fn a_sources_list_that_is_not_utf8_is_kept_byte_for_byte() {
    let fixture = Fixture::new();
    std::fs::write(fixture.sources_list(), b"# caf\xe9\n").expect("write");
    write(&fixture.sources_d().join("docker.list"), "docker\n");
    run(&fixture.roots, "jammy", RUN).expect("apply");
    let saved = std::fs::read(fixture.sources_list()).expect("read");
    assert!(saved.starts_with(b"# caf\xe9\ndeb http://archive.ubuntu.com/ubuntu/ jammy main\n"));
}

#[test]
fn a_sources_list_that_cannot_be_read_fails_before_anything_moves() {
    let fixture = Fixture::new();
    write(&fixture.sources_d().join("docker.list"), "docker\n");
    fifo(&fixture.roots.etc_apt, "sources.list");
    let before = fixture.tree();
    let error = run(&fixture.roots, "jammy", RUN).expect_err("fifo");
    assert_eq!(
        error.to_string(),
        format!(
            "error reading {}: not a regular file",
            fixture.sources_list().display()
        )
    );
    assert_eq!(fixture.tree(), before);
}

#[test]
fn a_dangling_sources_list_link_fails_before_anything_moves() {
    let fixture = Fixture::new();
    write(&fixture.sources_d().join("docker.list"), "docker\n");
    std::os::unix::fs::symlink(fixture.dir.path().join("gone"), fixture.sources_list())
        .expect("symlink");
    let before = fixture.tree();
    let error = run(&fixture.roots, "jammy", RUN).expect_err("dangling");
    assert!(
        error.to_string().starts_with(&format!(
            "error reading {}: ",
            fixture.sources_list().display()
        )),
        "{error}"
    );
    assert_eq!(fixture.tree(), before);
}

#[test]
fn an_unreadable_ubuntu_sources_fails_before_anything_moves() {
    let fixture = Fixture::new();
    write(&fixture.sources_d().join("docker.list"), "docker\n");
    // A FIFO with no writer: read without waiting, and refused.
    fifo(&fixture.sources_d(), "ubuntu.sources");
    let before = fixture.tree();
    let error = run(&fixture.roots, "noble", RUN).expect_err("fifo");
    assert_eq!(
        error.to_string(),
        format!(
            "error checking ubuntu.sources mirror configuration: error reading {}: not a regular file",
            fixture.sources_d().join("ubuntu.sources").display()
        )
    );
    assert_eq!(fixture.tree(), before);
}

#[test]
fn a_symlinked_ubuntu_sources_is_not_followed() {
    let fixture = Fixture::new();
    let target = fixture.dir.path().join("official.sources");
    write(&target, "URIs: http://archive.ubuntu.com/ubuntu/\n");
    std::os::unix::fs::symlink(&target, fixture.sources_d().join("ubuntu.sources"))
        .expect("symlink");
    write(&fixture.sources_d().join("docker.list"), "docker\n");
    let before = fixture.tree();
    let error = run(&fixture.roots, "noble", RUN).expect_err("symlink");
    assert!(
        error
            .to_string()
            .starts_with("error checking ubuntu.sources mirror configuration: error reading "),
        "{error}"
    );
    assert_eq!(fixture.tree(), before);
}

#[test]
fn deb822_without_sources_list_d_cannot_add_the_archive() {
    let fixture = Fixture::new();
    std::fs::remove_dir(fixture.sources_d()).expect("rmdir");
    let error = run(&fixture.roots, "noble", RUN).expect_err("missing");
    assert_eq!(
        error.to_string(),
        format!(
            "error writing ubuntu-archive.sources: error writing to {}: entity not found",
            fixture.sources_d().join("ubuntu-archive.sources").display()
        )
    );
}

#[test]
fn a_fifo_entry_fails_before_anything_moves() {
    let fixture = Fixture::new();
    write(&fixture.sources_d().join("a.list"), "a\n");
    fifo(&fixture.sources_d(), "pipe.list");
    let before = fixture.tree();
    let error = run(&fixture.roots, "jammy", RUN).expect_err("fifo");
    assert_eq!(
        error.to_string(),
        format!(
            "error removing {}: not a regular file or symlink",
            fixture.sources_d().join("pipe.list").display()
        )
    );
    assert_eq!(fixture.tree(), before);
}

#[test]
fn a_missing_or_writable_var_backups_is_refused() {
    let fixture = Fixture::new();
    write(&fixture.sources_d().join("keep.list"), "keep\n");
    std::fs::set_permissions(
        &fixture.roots.var_backups,
        std::fs::Permissions::from_mode(0o777),
    )
    .expect("chmod");
    assert_refused(
        &fixture,
        &fixture.roots.var_backups,
        "writable by group or others",
    );
    std::fs::remove_dir(&fixture.roots.var_backups).expect("rmdir");
    assert_refused(&fixture, &fixture.roots.var_backups, "it is missing");
}

#[test]
fn a_failed_move_keeps_what_already_moved_in_the_report() {
    let mut fixture = Fixture::new();
    fixture.roots.rename = |from, name, to, new| {
        if name == c"b.list" {
            Err(rustix::io::Errno::PERM)
        } else {
            rustix::fs::renameat(from, name, to, new)
        }
    };
    write(&fixture.sources_d().join("a.list"), "a\n");
    write(&fixture.sources_d().join("b.list"), "b\n");
    let mut report = Report::default();
    let error = apply(&fixture.roots, "jammy", RUN, &mut report).expect_err("failed");
    assert!(error.to_string().starts_with("error moving "), "{error}");
    assert_eq!(report.moved, ["a.list"]);
    assert_eq!(report.backup, Some(fixture.backup()));
    assert_eq!(read(&fixture.backup().join("a.list")), "a\n");
    assert!(fixture.sources_d().join("b.list").exists());
}

#[test]
fn across_filesystems_setuid_and_setgid_are_dropped() {
    let mut fixture = Fixture::new();
    fixture.roots.rename = |_, _, _, _| Err(rustix::io::Errno::XDEV);
    let entry = fixture.sources_d().join("a.list");
    write(&entry, "a\n");
    std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o6755)).expect("chmod");
    run(&fixture.roots, "jammy", RUN).expect("apply");
    assert_eq!(mode(&fixture.backup().join("a.list")), 0o755);
    assert!(!entry.exists());
}

// `remove_mentioning` (host spec, "apt", amendment H3): python-env's
// deadsnakes cleanup on the same fenced handles.

const PPA: &str = "deb https://ppa.launchpadcontent.net/deadsnakes/ppa/ubuntu jammy main\n";

/// `/var/backups/sb/deadsnakes/<RUN>` under the fixture.
fn deadsnakes(fixture: &Fixture) -> PathBuf {
    fixture.roots.var_backups.join("sb/deadsnakes").join(RUN)
}

/// [`remove_mentioning`] of `deadsnakes`, into the `deadsnakes` folder.
fn remove(roots: &Roots, run_id: &str) -> Result<Vec<PathBuf>, SourcesError> {
    remove_mentioning(roots, "deadsnakes", "deadsnakes", run_id)
}

#[test]
fn only_regular_files_that_mention_the_needle_move() {
    let fixture = Fixture::new();
    let ppa = "deadsnakes-ubuntu-ppa-jammy.list";
    write(&fixture.sources_d().join(ppa), PPA);
    std::fs::set_permissions(
        fixture.sources_d().join(ppa),
        std::fs::Permissions::from_mode(0o640),
    )
    .expect("chmod");
    write(&fixture.sources_d().join("docker.list"), "docker\n");
    let target = fixture.dir.path().join("target.list");
    write(&target, PPA);
    std::os::unix::fs::symlink(&target, fixture.sources_d().join("link.list")).expect("symlink");
    std::fs::create_dir(fixture.sources_d().join("nested")).expect("mkdir");
    write(&fixture.sources_d().join("nested/inner.list"), PPA);
    fifo(&fixture.sources_d(), "pipe.list");
    let moved = remove(&fixture.roots, RUN).expect("remove");
    assert_eq!(moved, [deadsnakes(&fixture).join(ppa)]);
    assert_eq!(read(&deadsnakes(&fixture).join(ppa)), PPA);
    assert_eq!(mode(&deadsnakes(&fixture).join(ppa)), 0o640);
    assert!(!fixture.sources_d().join(ppa).exists());
    for kept in ["docker.list", "link.list", "nested/inner.list", "pipe.list"] {
        assert!(
            fixture.sources_d().join(kept).symlink_metadata().is_ok(),
            "{kept}"
        );
    }
    for dir in [
        fixture.roots.var_backups.join("sb"),
        fixture.roots.var_backups.join("sb/deadsnakes"),
        deadsnakes(&fixture),
    ] {
        assert_eq!(mode(&dir), 0o700, "{}", dir.display());
    }
}

#[test]
fn every_file_that_mentions_it_moves_in_name_order() {
    let fixture = Fixture::new();
    for name in ["b.list", "a.sources", "c.list"] {
        write(&fixture.sources_d().join(name), PPA);
    }
    let moved = remove(&fixture.roots, RUN).expect("remove");
    assert_eq!(
        moved,
        ["a.sources", "b.list", "c.list"].map(|name| deadsnakes(&fixture).join(name))
    );
    assert_eq!(
        std::fs::read_dir(fixture.sources_d())
            .expect("list")
            .count(),
        0
    );
}

#[test]
fn a_name_that_is_not_utf8_is_returned_as_it_is() {
    use std::os::unix::ffi::OsStrExt as _;
    let fixture = Fixture::new();
    let name = OsStr::from_bytes(b"caf\xe9.list");
    write(&fixture.sources_d().join(name), PPA);
    let moved = remove(&fixture.roots, RUN).expect("remove");
    assert_eq!(moved, [deadsnakes(&fixture).join(name)]);
    assert_eq!(read(&moved[0]), PPA);
}

#[test]
fn nothing_mentioning_it_moves_nothing_and_makes_no_backup() {
    let fixture = Fixture::new();
    write(&fixture.sources_d().join("docker.list"), "docker\n");
    let before = fixture.tree();
    assert_eq!(
        remove(&fixture.roots, RUN).expect("remove"),
        Vec::<PathBuf>::new()
    );
    assert_eq!(fixture.tree(), before);
    assert!(!fixture.roots.var_backups.join("sb").exists());
}

#[test]
fn an_empty_needle_moves_nothing() {
    let fixture = Fixture::new();
    write(&fixture.sources_d().join("docker.list"), "docker\n");
    let before = fixture.tree();
    let moved = remove_mentioning(&fixture.roots, "", "deadsnakes", RUN).expect("remove");
    assert_eq!(moved, Vec::<PathBuf>::new());
    assert_eq!(fixture.tree(), before);
}

#[test]
fn a_missing_sources_list_d_moves_nothing() {
    let fixture = Fixture::new();
    std::fs::remove_dir(fixture.sources_d()).expect("rmdir");
    assert_eq!(
        remove(&fixture.roots, RUN).expect("remove"),
        Vec::<PathBuf>::new()
    );
}

/// `remove` refused with `refusing to clean <dir>: <reason>`, nothing
/// changed.
fn assert_remove_refused(fixture: &Fixture, dir: &Path, reason: &str) {
    let before = fixture.tree();
    let error = remove(&fixture.roots, RUN).expect_err("refused");
    assert_eq!(
        error.to_string(),
        format!("refusing to clean {}: {reason}", dir.display())
    );
    assert_eq!(fixture.tree(), before);
}

#[test]
fn a_symlinked_writable_or_foreign_sources_list_d_is_refused_for_removal() {
    let fixture = Fixture::new();
    write(&fixture.sources_d().join("ppa.list"), PPA);
    for mode in [0o775, 0o757] {
        std::fs::set_permissions(fixture.sources_d(), std::fs::Permissions::from_mode(mode))
            .expect("chmod");
        assert_remove_refused(
            &fixture,
            &fixture.sources_d(),
            "writable by group or others",
        );
    }
    std::fs::set_permissions(fixture.sources_d(), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    let mut foreign = Fixture {
        dir: fixture.dir,
        roots: fixture.roots.clone(),
    };
    foreign.roots.owner += 1;
    let dir = foreign.sources_d();
    assert_remove_refused(&foreign, &dir, "not owned by root");
    foreign.roots.owner -= 1;
    let elsewhere = foreign.dir.path().join("elsewhere");
    std::fs::rename(foreign.sources_d(), &elsewhere).expect("rename");
    std::os::unix::fs::symlink(&elsewhere, foreign.sources_d()).expect("symlink");
    assert_remove_refused(&foreign, &dir, "it is a symlink");
}

#[test]
fn a_symlinked_or_writable_deadsnakes_folder_is_refused() {
    let fixture = Fixture::new();
    write(&fixture.sources_d().join("ppa.list"), PPA);
    let sb = fixture.roots.var_backups.join("sb");
    std::fs::create_dir(&sb).expect("mkdir");
    std::fs::set_permissions(&sb, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let elsewhere = fixture.dir.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).expect("mkdir");
    std::os::unix::fs::symlink(&elsewhere, sb.join("deadsnakes")).expect("symlink");
    assert_remove_refused(&fixture, &sb.join("deadsnakes"), "it is a symlink");
    std::fs::remove_file(sb.join("deadsnakes")).expect("unlink");
    std::fs::create_dir(sb.join("deadsnakes")).expect("mkdir");
    std::fs::set_permissions(
        sb.join("deadsnakes"),
        std::fs::Permissions::from_mode(0o770),
    )
    .expect("chmod");
    assert_remove_refused(
        &fixture,
        &sb.join("deadsnakes"),
        "writable by group or others",
    );
}

#[test]
fn an_existing_deadsnakes_run_directory_is_refused() {
    let fixture = Fixture::new();
    write(&fixture.sources_d().join("ppa.list"), PPA);
    std::fs::create_dir_all(deadsnakes(&fixture)).expect("mkdir");
    for dir in ["sb", "sb/deadsnakes"] {
        let path = fixture.roots.var_backups.join(dir);
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    }
    assert_remove_refused(&fixture, &deadsnakes(&fixture), "it already exists");
}

#[test]
fn a_bad_run_id_or_folder_is_refused_for_removal() {
    let fixture = Fixture::new();
    write(&fixture.sources_d().join("ppa.list"), PPA);
    let before = fixture.tree();
    for run_id in ["", "..", "a/b"] {
        let error = remove(&fixture.roots, run_id).expect_err("refused");
        assert!(error.to_string().contains("not a run id"), "{error}");
    }
    for folder in ["", ".", "..", "a/b"] {
        let error =
            remove_mentioning(&fixture.roots, "deadsnakes", folder, RUN).expect_err("refused");
        assert_eq!(
            error.to_string(),
            format!(
                "refusing to clean {}: {folder:?} is not a folder name",
                fixture.roots.var_backups.join("sb").display()
            )
        );
    }
    assert_eq!(fixture.tree(), before);
}

#[test]
fn across_filesystems_a_deadsnakes_file_is_copied_then_removed() {
    let mut fixture = Fixture::new();
    fixture.roots.rename = |_, _, _, _| Err(rustix::io::Errno::XDEV);
    write(&fixture.sources_d().join("ppa.list"), PPA);
    std::fs::set_permissions(
        fixture.sources_d().join("ppa.list"),
        std::fs::Permissions::from_mode(0o600),
    )
    .expect("chmod");
    let moved = remove(&fixture.roots, RUN).expect("remove");
    assert_eq!(moved, [deadsnakes(&fixture).join("ppa.list")]);
    assert_eq!(read(&moved[0]), PPA);
    assert_eq!(mode(&moved[0]), 0o600);
    assert!(!fixture.sources_d().join("ppa.list").exists());
}

#[test]
fn a_failed_removal_move_is_named() {
    let mut fixture = Fixture::new();
    fixture.roots.rename = |_, _, _, _| Err(rustix::io::Errno::PERM);
    write(&fixture.sources_d().join("ppa.list"), PPA);
    let error = remove(&fixture.roots, RUN).expect_err("failed");
    assert_eq!(
        error.to_string(),
        format!(
            "error moving {} to {}: Operation not permitted (os error 1)",
            fixture.sources_d().join("ppa.list").display(),
            deadsnakes(&fixture).display()
        )
    );
    assert!(fixture.sources_d().join("ppa.list").exists());
}

/// Each name logged, escaped, before the first move, as `apply` logs them.
#[test]
fn every_deadsnakes_name_is_logged_before_anything_moves() {
    let mut fixture = Fixture::new();
    fixture.roots.rename = |from, name, to, new| {
        tracing::info!("renameat called");
        rustix::fs::renameat(from, name, to, new)
    };
    for name in ["a.list", "evil\nname.list"] {
        write(&fixture.sources_d().join(name), PPA);
    }
    let (result, text) = capture(|| remove(&fixture.roots, RUN));
    result.expect("remove");
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.contains("apt source moved aside"))
        .collect();
    assert_eq!(lines.len(), 4, "{text}");
    for (line, name) in lines[..2].iter().zip(["a.list", "evil\\nname.list"]) {
        assert!(
            line.contains("apt source to move aside") && line.contains(&format!("name={name}")),
            "{text}"
        );
    }
    assert!(lines[2].contains("renameat called"), "{text}");
}
