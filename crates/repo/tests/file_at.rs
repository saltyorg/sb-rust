//! `Repo::file_at` (repo spec, amended 2026-11-18): a file's text at a
//! git ref, in a temporary repository made with the host's git.

mod common;

use std::path::Path;
use std::process::Command;

use sb_repo::{Repo, RepoError};
use tokio_util::sync::CancellationToken;

/// `git <args>` in `dir`, which must succeed, with none of the host's git
/// settings and none of the caller's repository variables (as update.rs's
/// helper).
fn git(dir: &Path, args: &[&str]) {
    let out = common::isolate(&mut Command::new("git"))
        .args(["-c", "core.excludesFile=/dev/null"])
        .args(["-c", "core.attributesFile=/dev/null"])
        .args(["-c", "user.name=sb", "-c", "user.email=sb@example.invalid"])
        .args(["-c", "init.defaultBranch=master"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("LC_ALL", "C")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository whose first commit holds `announcements.yml` as `one`,
/// a `develop` branch where it is `two`, and `master` checked out with
/// an uncommitted edit.
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let work = dir.path();
    git(work, &["init", "--quiet"]);
    std::fs::write(work.join("other.txt"), "x\n").expect("write");
    git(work, &["add", "."]);
    git(work, &["commit", "--quiet", "-m", "without"]);
    git(work, &["tag", "without"]);
    std::fs::write(work.join("announcements.yml"), "one\n").expect("write");
    git(work, &["add", "."]);
    git(work, &["commit", "--quiet", "-m", "one"]);
    git(work, &["checkout", "--quiet", "-b", "develop"]);
    std::fs::write(work.join("announcements.yml"), "two\n").expect("write");
    git(work, &["commit", "--quiet", "-am", "two"]);
    git(work, &["checkout", "--quiet", "master"]);
    std::fs::write(work.join("announcements.yml"), "edited\n").expect("write");
    dir
}

fn token() -> CancellationToken {
    CancellationToken::new()
}

/// The repository at `path`, its git the isolating stand-in in `bin`
/// (`common::isolated_git`).
fn open(path: &Path, bin: &Path) -> Repo {
    Repo::open("Saltbox", path)
        .expect("open")
        .with_git(common::isolated_git(bin))
}

/// The committed text at `HEAD`, not the working tree's edit; a branch's
/// own text; a commit without the file reads as none.
#[tokio::test]
async fn the_text_at_a_ref_or_none() {
    let dir = fixture();
    let bin = tempfile::tempdir().expect("bin");
    let repo = open(dir.path(), bin.path());
    let at = async |reference: &str| {
        repo.file_at(reference, "announcements.yml", &token())
            .await
            .expect("read")
    };
    assert_eq!(at("HEAD").await.as_deref(), Some("one\n"));
    assert_eq!(at("develop").await.as_deref(), Some("two\n"));
    assert_eq!(at("without").await, None);
    assert_eq!(
        repo.file_at("HEAD", "missing.yml", &token())
            .await
            .expect("read"),
        None
    );
}

/// A ref git doesn't know, a path given as a ref, and one starting with
/// `-` are the ref's error, naming it and the checkout.
#[tokio::test]
async fn an_unknown_ref_is_named() {
    let dir = fixture();
    let bin = tempfile::tempdir().expect("bin");
    let repo = open(dir.path(), bin.path());
    for reference in ["nope", "-p", "--all", "HEAD:announcements.yml"] {
        let err = repo
            .file_at(reference, "announcements.yml", &token())
            .await
            .expect_err(reference);
        assert!(matches!(err, RepoError::Ref { .. }), "{reference}: {err:?}");
        assert_eq!(
            err.to_string(),
            format!(
                "'{}' isn't a commit in {}",
                reference.escape_debug(),
                dir.path().display()
            )
        );
    }
}

/// A file over 1 MiB, and one that isn't UTF-8, are refused as cli-core's
/// small files are.
#[tokio::test]
async fn a_big_or_binary_file_is_refused() {
    let dir = fixture();
    let work = dir.path();
    std::fs::write(work.join("big.yml"), vec![b'a'; (1 << 20) + 1]).expect("write");
    std::fs::write(work.join("binary.yml"), b"ok \xff\xfe\n").expect("write");
    std::fs::write(work.join("cut.yml"), b"ok \xf0\x9f\x98\n").expect("write");
    git(work, &["add", "big.yml", "binary.yml", "cut.yml"]);
    git(work, &["commit", "--quiet", "-m", "files"]);
    let bin = tempfile::tempdir().expect("bin");
    let repo = open(work, bin.path());
    let err = repo
        .file_at("HEAD", "big.yml", &token())
        .await
        .expect_err("big");
    assert_eq!(err.to_string(), "HEAD:big.yml is bigger than 1048576 bytes");
    for name in ["binary.yml", "cut.yml"] {
        let err = repo.file_at("HEAD", name, &token()).await.expect_err(name);
        assert!(matches!(err, RepoError::Blob(_)), "{err:?}");
        assert_eq!(err.to_string(), format!("HEAD:{name} isn't UTF-8"));
    }
}

/// Outside a repository, git's own failure, not "isn't a commit": its
/// words kept.
#[tokio::test]
async fn outside_a_repository_is_gits_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = tempfile::tempdir().expect("bin");
    let repo = open(dir.path(), bin.path());
    let err = repo
        .file_at("HEAD", "announcements.yml", &token())
        .await
        .expect_err("no repository");
    assert!(matches!(err, RepoError::Git(_)), "{err:?}");
    // git's words: "not a git repository", or, on a tmpfs as the gate's
    // sandbox has, "Stopping at filesystem boundary".
    assert!(err.to_string().contains("exited with code 128"), "{err}");
}

/// A directory at the path is no file.
#[tokio::test]
async fn a_directory_at_the_path_is_no_file() {
    let dir = fixture();
    let work = dir.path();
    std::fs::create_dir(work.join("folder")).expect("dir");
    std::fs::write(work.join("folder/inside"), "x\n").expect("write");
    git(work, &["add", "folder"]);
    git(work, &["commit", "--quiet", "-m", "folder"]);
    let bin = tempfile::tempdir().expect("bin");
    let repo = open(work, bin.path());
    assert_eq!(
        repo.file_at("HEAD", "folder", &token())
            .await
            .expect("read"),
        None
    );
}

/// A git at `path` that runs `then` (shell) when its arguments match the
/// shell pattern `when`, and the real git otherwise, isolated as
/// `common::stand_in` runs it; each stand-in has its own path.
fn git_that(path: &Path, when: &str, then: &str) -> std::path::PathBuf {
    common::stand_in(
        path,
        &format!("case \"$*\" in\n  {when}) {then} ;;\nesac\nexec git \"$@\"\n"),
    )
}

/// git failing at any step after the ref is known is git's failure, its
/// words kept, never "no file"; a size git gives that isn't a number is
/// named with the object.
#[tokio::test]
async fn a_failing_step_is_gits_error_never_no_file() {
    let dir = fixture();
    let bin = tempfile::tempdir().expect("bin");
    let refused = "echo 'fatal: refused' >&2; exit 1";
    let steps = ["ls-tree*", "\"cat-file -s\"*", "\"cat-file blob\"*"];
    for (n, when) in steps.into_iter().enumerate() {
        let git = git_that(&bin.path().join(format!("git-{n}")), when, refused);
        let repo = Repo::open("Saltbox", dir.path())
            .expect("open")
            .with_git(git);
        let err = repo
            .file_at("HEAD", "announcements.yml", &token())
            .await
            .expect_err(when);
        assert!(matches!(err, RepoError::Git(_)), "{when}: {err:?}");
        assert!(err.to_string().contains("fatal: refused"), "{when}: {err}");
    }
    let repo = Repo::open("Saltbox", dir.path())
        .expect("open")
        .with_git(git_that(
            &bin.path().join("git-size"),
            "\"cat-file -s\"*",
            "echo many; exit 0",
        ));
    let err = repo
        .file_at("HEAD", "announcements.yml", &token())
        .await
        .expect_err("not a size");
    assert_eq!(
        err.to_string(),
        "git cat-file -s HEAD:announcements.yml: \"many\\n\""
    );
}
