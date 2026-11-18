//! `Repo::file_at` (repo spec, amended 2026-11-18): a file's text at a
//! git ref, in a temporary repository made with the host's git.

use std::path::Path;
use std::process::Command;

use sb_repo::{Repo, RepoError};
use tokio_util::sync::CancellationToken;

/// `git <args>` in `dir`, which must succeed, with none of the host's git
/// settings (as update.rs's helper).
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
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

/// The committed text at `HEAD`, not the working tree's edit; a branch's
/// own text; a commit without the file reads as none.
#[tokio::test]
async fn the_text_at_a_ref_or_none() {
    let dir = fixture();
    let repo = Repo::open("Saltbox", dir.path()).expect("open");
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
    let repo = Repo::open("Saltbox", dir.path()).expect("open");
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
    let repo = Repo::open("Saltbox", work).expect("open");
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
    let repo = Repo::open("Saltbox", dir.path()).expect("open");
    let err = repo
        .file_at("HEAD", "announcements.yml", &token())
        .await
        .expect_err("no repository");
    assert!(matches!(err, RepoError::Git(_)), "{err:?}");
    assert!(err.to_string().contains("not a git repository"), "{err}");
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
    let repo = Repo::open("Saltbox", work).expect("open");
    assert_eq!(
        repo.file_at("HEAD", "folder", &token())
            .await
            .expect("read"),
        None
    );
}
