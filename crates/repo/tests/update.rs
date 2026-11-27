//! sb-repo against real git on local repositories (repo spec, Testing
//! Strategy): a bare "remote" in a temp dir with `master` and `develop`,
//! and a single-branch clone of it, the shape setup makes. Nothing here
//! touches a Saltbox checkout or the network (`file://` remotes only).

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use sb_cli_core::cancel::Shutdown;
use sb_repo::{Answer, Choice, Repo, RepoError, branch_choice, branch_notices};
use sb_term_ui::{Labels, Options, Runner, Step, Target};
use tokio_util::sync::CancellationToken;

/// `git <args>` in `dir`, which must succeed; its stdout. None of the
/// caller's repository variables (`common::isolate`).
fn git(dir: &Path, args: &[&str]) -> String {
    let out = common::isolate(&mut Command::new("git"))
        // None of the host's git settings: its global and system config
        // files (commit signing, credential helpers, rewrites) and its
        // per-user ignore and attributes files stay out, so a commit here
        // never asks for a key (the gate's mutation sandbox has no agent
        // to sign with) and `git add` takes every fixture file.
        // GIT_CONFIG_GLOBAL and GIT_CONFIG_SYSTEM set to /dev/null skip
        // those config files (git 2.32+):
        // https://git-scm.com/docs/git#Documentation/git.txt-GITCONFIGGLOBAL
        // The ignore and attributes files are read without any config,
        // from $XDG_CONFIG_HOME/git, unless these name others:
        // https://git-scm.com/docs/git-config#Documentation/git-config.txt-coreexcludesFile
        .args(["-c", "core.excludesFile=/dev/null"])
        .args(["-c", "core.attributesFile=/dev/null"])
        .args(["-c", "user.name=sb", "-c", "user.email=sb@example.invalid"])
        .args([
            "-c",
            "init.defaultBranch=master",
            "-c",
            "advice.detachedHead=false",
        ])
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
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A remote with `master` and `develop`, its working copy to push from,
/// and a `--depth 1 -b master` clone of it.
struct Fixture {
    dir: tempfile::TempDir,
    work: PathBuf,
    checkout: PathBuf,
    /// The git `Repo` runs here (`common::isolated_git`).
    git: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let remote = dir.path().join("remote.git");
        let work = dir.path().join("work");
        let checkout = dir.path().join("checkout");
        git(
            dir.path(),
            &["init", "--quiet", "--bare", remote.to_str().expect("utf-8")],
        );
        git(
            dir.path(),
            &["init", "--quiet", work.to_str().expect("utf-8")],
        );
        std::fs::write(work.join("tracked.txt"), "one\n").expect("write");
        std::fs::write(work.join(".gitignore"), "accounts.yml\n").expect("write");
        git(&work, &["add", "."]);
        git(&work, &["commit", "--quiet", "-m", "first"]);
        git(
            &work,
            &[
                "remote",
                "add",
                "origin",
                &format!("file://{}", remote.display()),
            ],
        );
        git(&work, &["push", "--quiet", "origin", "master"]);
        git(&work, &["checkout", "--quiet", "-b", "develop"]);
        std::fs::write(work.join("develop.txt"), "develop\n").expect("write");
        git(&work, &["add", "."]);
        git(&work, &["commit", "--quiet", "-m", "develop"]);
        git(&work, &["push", "--quiet", "origin", "develop"]);
        git(&work, &["checkout", "--quiet", "master"]);
        git(
            dir.path(),
            &[
                "clone",
                "--quiet",
                "--depth",
                "1",
                "-b",
                "master",
                &format!("file://{}", remote.display()),
                checkout.to_str().expect("utf-8"),
            ],
        );
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).expect("bin");
        let git = common::isolated_git(&bin);
        Self {
            dir,
            work,
            checkout,
            git,
        }
    }

    fn repo(&self) -> Repo {
        Repo::open("Sandbox", &self.checkout)
            .expect("open")
            .with_git(&self.git)
    }

    /// A new commit on `branch` in the remote.
    fn advance(&self, branch: &str, file: &str) -> String {
        git(&self.work, &["checkout", "--quiet", branch]);
        std::fs::write(self.work.join(file), file).expect("write");
        git(&self.work, &["add", "."]);
        git(&self.work, &["commit", "--quiet", "-m", file]);
        git(&self.work, &["push", "--quiet", "origin", branch]);
        git(&self.work, &["rev-parse", branch])
    }

    fn head(&self) -> String {
        git(&self.checkout, &["rev-parse", "HEAD"])
    }

    /// The remote's `file://` URL.
    fn url(&self) -> String {
        format!("file://{}", self.dir.path().join("remote.git").display())
    }

    /// A submodule at `roles/module` on the remote's master, its
    /// repository at the returned path.
    fn add_submodule(&self) -> PathBuf {
        let module = self.dir.path().join("module");
        git(
            self.dir.path(),
            &["init", "--quiet", module.to_str().expect("utf-8")],
        );
        std::fs::write(module.join("inside.txt"), "inside\n").expect("write");
        git(&module, &["add", "."]);
        git(&module, &["commit", "--quiet", "-m", "module"]);
        git(
            &self.work,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "--quiet",
                &format!("file://{}", module.display()),
                "roles/module",
            ],
        );
        git(&self.work, &["commit", "--quiet", "-m", "submodule"]);
        git(&self.work, &["push", "--quiet", "origin", "master"]);
        module
    }

    fn fetch_values(&self) -> Vec<String> {
        git(
            &self.checkout,
            &["config", "--get-all", "remote.origin.fetch"],
        )
        .lines()
        .map(str::to_string)
        .collect()
    }
}

/// What a run of `fetch_and_reset` left: the error it returned, if any,
/// and the screen, which shows a sub-step's own error.
struct Ran {
    error: Option<String>,
    screen: String,
}

/// `fetch_and_reset` under a runner drawing to memory.
async fn update(repo: &Repo, branch: &str) -> Ran {
    in_step(async |step| repo.fetch_and_reset(branch, "", step).await).await
}

/// `body` in a step of a runner drawing to memory, wide enough that a
/// temp dir's paths in an error stay on one line.
async fn in_step(body: impl AsyncFnOnce(&mut Step) -> anyhow::Result<()>) -> Ran {
    let term = indicatif::InMemoryTerm::new(60, 400);
    let options = Options {
        target: Target::InMemory {
            term: term.clone(),
            colour: false,
        },
    };
    let shutdown = Shutdown::new();
    let mut runner = Runner::start("sb test", &shutdown, None, options);
    let mut error = None;
    let labels = Labels::new("Updating Sandbox repository", "Sandbox repository updated");
    let _ = runner
        .step(labels, async |step| {
            let result = body(step).await;
            error = result.as_ref().err().map(ToString::to_string);
            result
        })
        .await;
    runner.finish();
    Ran {
        error,
        screen: term.contents(),
    }
}

/// Tests that write a stand-in git and run it hold this from writing it
/// until its run is over, so no other test's fork holds its write
/// descriptor when it runs (exec fails with ETXTBSY otherwise).
static SPAWNS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A git in `dir` that records `LC_ALL` and `GIT_TERMINAL_PROMPT` in
/// `env.log`, then runs the real git allowing `file://` submodules (git
/// 2.38.1 and later refuse them by default; `-c` reaches git's own
/// children through `GIT_CONFIG_PARAMETERS`).
fn recording_git(dir: &Path) -> PathBuf {
    let path = dir.join("git");
    common::stand_in(
        &path,
        &format!(
            "printf '%s %s\\n' \"$LC_ALL\" \"$GIT_TERMINAL_PROMPT\" >> '{}'\n\
             exec git -c protocol.file.allow=always \"$@\"\n",
            dir.join("env.log").display()
        ),
    );
    path
}

/// A git in `dir` that appends its arguments to `args.log`, then runs
/// the real git allowing `file://` submodules, as [`recording_git`].
fn arguments_git(dir: &Path) -> PathBuf {
    let path = dir.join("git");
    common::stand_in(
        &path,
        &format!(
            "printf '%s\\n' \"$*\" >> '{}'\n\
             exec git -c protocol.file.allow=always \"$@\"\n",
            dir.join("args.log").display()
        ),
    );
    path
}

/// The `submodule` runs [`arguments_git`] in `dir` recorded.
fn submodule_runs(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("args.log"))
        .expect("args")
        .lines()
        .filter(|line| line.starts_with("submodule"))
        .map(str::to_string)
        .collect()
}

fn token() -> CancellationToken {
    CancellationToken::new()
}

#[test]
fn open_refuses_a_missing_path_and_a_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("missing");
    let error = Repo::open("Sandbox", &missing).expect_err("missing");
    assert_eq!(
        error.to_string(),
        format!(
            "inspect required directory {}: No such file or directory (os error 2)",
            missing.display()
        )
    );
    let file = dir.path().join("file");
    std::fs::write(&file, "x").expect("write");
    let error = Repo::open("Sandbox", &file).expect_err("file");
    assert_eq!(
        error.to_string(),
        format!("{} exists but is not a directory", file.display())
    );
    let repo = Repo::open("Sandbox", dir.path()).expect("dir");
    assert_eq!((repo.name(), repo.path()), ("Sandbox", dir.path()));
}

#[tokio::test]
async fn the_current_branch_and_a_detached_head() {
    let fixture = Fixture::new();
    let repo = fixture.repo();
    assert_eq!(
        repo.current_branch(&token()).await.expect("branch"),
        "master"
    );
    git(&fixture.checkout, &["checkout", "--quiet", "--detach"]);
    assert_eq!(repo.current_branch(&token()).await.expect("branch"), "HEAD");
}

#[tokio::test]
async fn the_current_branch_outside_a_repository_is_sb_gos_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = tempfile::tempdir().expect("bin");
    let repo = Repo::open("Sandbox", dir.path())
        .expect("open")
        .with_git(common::isolated_git(bin.path()));
    let error = repo.current_branch(&token()).await.expect_err("not a repo");
    let text = error.to_string();
    assert!(text.starts_with("failed to get current branch: "), "{text}");
    assert!(text.contains("exited with code 128"), "{text}");
}

/// A git that can't be started is each read's own error: the branch's
/// and the refspec's, naming the program.
#[tokio::test]
async fn a_git_that_cant_start_is_named_by_each_read() {
    let fixture = Fixture::new();
    let repo = fixture.repo().with_git("/nonexistent/git");
    let branch = repo.current_branch(&token()).await.expect_err("no git");
    assert!(matches!(branch, RepoError::CurrentBranch(_)), "{branch:?}");
    assert!(branch.to_string().contains("/nonexistent/git"), "{branch}");
    let fetch = repo
        .ensure_fetch_all_branches(&token())
        .await
        .expect_err("no git");
    assert!(matches!(fetch, RepoError::ReadFetch(_)), "{fetch:?}");
    assert!(
        fetch
            .to_string()
            .starts_with("failed to read remote.origin.fetch: "),
        "{fetch}"
    );
    assert!(fetch.to_string().contains("/nonexistent/git"), "{fetch}");
}

/// `git config --get-all` failing other than "unset" (exit 1), as a
/// config file git can't read does (exit 3), is the read's error with
/// git's words, and nothing is added.
#[tokio::test]
async fn a_failed_refspec_read_adds_nothing() {
    let _spawns = SPAWNS.lock().await;
    let fixture = Fixture::new();
    let bin = tempfile::tempdir().expect("bin");
    let path = bin.path().join("git-bad-config");
    common::stand_in(
        &path,
        "if [ \"$1 $2\" = 'config --get-all' ]; then echo 'fatal: bad config line 1' >&2; exit 3; fi\n\
         exec git \"$@\"\n",
    );
    let repo = fixture.repo().with_git(path);
    let error = repo
        .ensure_fetch_all_branches(&token())
        .await
        .expect_err("refused");
    assert!(matches!(error, RepoError::ReadFetch(_)), "{error:?}");
    assert!(
        error.to_string().contains("fatal: bad config line 1"),
        "{error}"
    );
    assert_eq!(
        fixture.fetch_values(),
        ["+refs/heads/master:refs/remotes/origin/master"]
    );
}

#[tokio::test]
async fn branch_names_are_checked_before_use() {
    let fixture = Fixture::new();
    let repo = fixture.repo();
    for good in ["develop", "feature/x", "café"] {
        repo.check_branch_name(good, &token()).await.expect(good);
    }
    for bad in [
        "-x",
        "--force",
        "a..b",
        "@{-1}",
        "foo.lock",
        "",
        "HEAD",
        "a b",
        "x\u{1b}[1m",
    ] {
        let error = repo.check_branch_name(bad, &token()).await.expect_err(bad);
        assert!(
            matches!(error, RepoError::BranchName(_)),
            "{bad:?}: {error}"
        );
        assert_eq!(
            error.to_string(),
            format!("'{}' is not a valid branch name", bad.escape_debug())
        );
    }
}

#[tokio::test]
async fn the_refspec_is_widened_once() {
    let fixture = Fixture::new();
    let repo = fixture.repo();
    assert_eq!(
        fixture.fetch_values(),
        ["+refs/heads/master:refs/remotes/origin/master"]
    );
    repo.ensure_fetch_all_branches(&token())
        .await
        .expect("widen");
    repo.ensure_fetch_all_branches(&token())
        .await
        .expect("again");
    assert_eq!(
        fixture.fetch_values(),
        [
            "+refs/heads/master:refs/remotes/origin/master",
            "+refs/heads/*:refs/remotes/origin/*"
        ]
    );
}

#[tokio::test]
async fn an_unset_refspec_is_added() {
    let fixture = Fixture::new();
    git(
        &fixture.checkout,
        &["config", "--unset-all", "remote.origin.fetch"],
    );
    fixture
        .repo()
        .ensure_fetch_all_branches(&token())
        .await
        .expect("widen");
    assert_eq!(
        fixture.fetch_values(),
        ["+refs/heads/*:refs/remotes/origin/*"]
    );
}

#[tokio::test]
async fn the_refspec_outside_a_repository_is_sb_gos_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = tempfile::tempdir().expect("bin");
    let repo = Repo::open("Sandbox", dir.path())
        .expect("open")
        .with_git(common::isolated_git(bin.path()));
    let error = repo
        .ensure_fetch_all_branches(&token())
        .await
        .expect_err("not a repo");
    assert!(
        error
            .to_string()
            .starts_with("failed to update remote.origin.fetch: "),
        "{error}"
    );
}

#[tokio::test]
async fn the_same_branch_is_reset_to_the_remote_keeping_ignored_files() {
    let fixture = Fixture::new();
    let remote_head = fixture.advance("master", "second.txt");
    std::fs::write(fixture.checkout.join("tracked.txt"), "changed\n").expect("write");
    std::fs::write(fixture.checkout.join("untracked.txt"), "x").expect("write");
    std::fs::write(fixture.checkout.join("accounts.yml"), "user: {}\n").expect("write");
    assert_eq!(update(&fixture.repo(), "master").await.error, None);
    assert_eq!(fixture.head(), remote_head);
    assert_eq!(
        std::fs::read_to_string(fixture.checkout.join("tracked.txt")).expect("read"),
        "one\n"
    );
    assert!(!fixture.checkout.join("untracked.txt").exists());
    assert!(fixture.checkout.join("accounts.yml").exists());
}

#[tokio::test]
async fn another_branch_on_a_single_branch_clone() {
    let fixture = Fixture::new();
    let develop = fixture.advance("develop", "more.txt");
    let before = fixture.head();
    assert_eq!(update(&fixture.repo(), "develop").await.error, None);
    assert_eq!(fixture.head(), develop);
    assert_ne!(fixture.head(), before);
    assert_eq!(
        fixture
            .repo()
            .current_branch(&token())
            .await
            .expect("branch"),
        "develop"
    );
    assert!(fixture.checkout.join("develop.txt").exists());
}

#[tokio::test]
async fn a_missing_branch_fails_before_the_worktree_changes() {
    let fixture = Fixture::new();
    std::fs::write(fixture.checkout.join("marker.txt"), "x").expect("write");
    std::fs::write(fixture.checkout.join("tracked.txt"), "changed\n").expect("write");
    let ran = update(&fixture.repo(), "nowhere").await;
    assert_eq!(
        ran.error.as_deref(),
        Some("branch 'nowhere' doesn't exist on Sandbox's remote")
    );
    assert!(
        !ran.screen.contains("Resetting repository"),
        "{}",
        ran.screen
    );
    assert!(fixture.checkout.join("marker.txt").exists());
    assert_eq!(
        std::fs::read_to_string(fixture.checkout.join("tracked.txt")).expect("read"),
        "changed\n"
    );
}

#[tokio::test]
async fn a_detached_head_is_reset_to_the_branch() {
    let fixture = Fixture::new();
    git(&fixture.checkout, &["checkout", "--quiet", "--detach"]);
    let master = fixture.advance("master", "next.txt");
    assert_eq!(update(&fixture.repo(), "master").await.error, None);
    assert_eq!(fixture.head(), master);
}

#[tokio::test]
async fn a_branch_without_an_upstream_is_refused() {
    let fixture = Fixture::new();
    git(&fixture.checkout, &["checkout", "--quiet", "-b", "local"]);
    let ran = update(&fixture.repo(), "local").await;
    assert!(ran.error.is_some());
    assert!(
        ran.screen
            .contains("Sandbox: branch 'local' has no upstream to update from"),
        "{}",
        ran.screen
    );
}

/// The worktree as it stands: every entry outside `.git` (walkdir,
/// directories too, so an empty one counts) with each file's bytes, the
/// index's and the worktree's state (`git status`), HEAD and the local
/// branches, to compare a refused run's checkout with the one before it.
/// [`snapshot`]'s: each entry and a file's bytes, `git status`, and HEAD
/// with the local branches.
type Snapshot = (Vec<(PathBuf, Option<Vec<u8>>)>, String, String);

fn snapshot(checkout: &Path) -> Snapshot {
    let entries = walkdir::WalkDir::new(checkout)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| entry.file_name() != ".git")
        .map(|entry| {
            let entry = entry.expect("entry");
            let bytes = entry
                .file_type()
                .is_file()
                .then(|| std::fs::read(entry.path()).expect("read"));
            let path = entry.path().strip_prefix(checkout).expect("inside");
            (path.to_path_buf(), bytes)
        })
        .collect();
    let status = git(
        checkout,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );
    let refs = git(checkout, &["rev-parse", "--abbrev-ref", "HEAD"])
        + " "
        + &git(checkout, &["rev-parse", "HEAD"])
        + "\n"
        + &git(
            checkout,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname) %(upstream)",
                "refs/heads",
            ],
        );
    (entries, status, refs)
}

/// A tracked edit, a staged edit, an untracked file, an untracked
/// directory with a file and an empty one: what a refused run must leave
/// as it found them.
fn local_changes(checkout: &Path) {
    std::fs::write(checkout.join(".gitignore"), "accounts.yml\nstaged\n").expect("write");
    git(checkout, &["add", ".gitignore"]);
    std::fs::write(checkout.join("tracked.txt"), "edited\n").expect("write");
    std::fs::write(checkout.join("notes.txt"), "mine\n").expect("write");
    std::fs::create_dir(checkout.join("drafts")).expect("mkdir");
    std::fs::write(checkout.join("drafts/role.yml"), "draft\n").expect("write");
    std::fs::create_dir(checkout.join("empty")).expect("mkdir");
}

/// `--keep-branch` on a branch with no upstream (the test agent's d2):
/// refused before the worktree changes, with today's message, the
/// checkout byte for byte as it was (repo spec, decision 8, "Order").
#[tokio::test]
async fn keeping_a_branch_without_an_upstream_changes_nothing() {
    let fixture = Fixture::new();
    git(
        &fixture.checkout,
        &["checkout", "--quiet", "-b", "dev-local"],
    );
    std::fs::write(fixture.checkout.join("local.txt"), "commit\n").expect("write");
    git(&fixture.checkout, &["add", "local.txt"]);
    git(&fixture.checkout, &["commit", "--quiet", "-m", "local"]);
    local_changes(&fixture.checkout);
    let before = snapshot(&fixture.checkout);
    let ran = update(&fixture.repo(), "dev-local").await;
    assert_eq!(
        ran.error.as_deref(),
        Some("Sandbox: branch 'dev-local' has no upstream to update from")
    );
    assert!(
        !ran.screen.contains("Resetting repository"),
        "{}",
        ran.screen
    );
    assert_eq!(snapshot(&fixture.checkout), before);
}

/// `--reset-branch` (or the default branch chosen) to a local branch
/// with no upstream, from another branch: refused up front too, the
/// checkout unchanged.
#[tokio::test]
async fn resetting_to_a_local_branch_without_an_upstream_changes_nothing() {
    let fixture = Fixture::new();
    git(&fixture.checkout, &["branch", "--unset-upstream", "master"]);
    git(&fixture.checkout, &["checkout", "--quiet", "-b", "feature"]);
    local_changes(&fixture.checkout);
    let before = snapshot(&fixture.checkout);
    let ran = update(&fixture.repo(), "master").await;
    assert_eq!(
        ran.error.as_deref(),
        Some("Sandbox: branch 'master' has no upstream to update from")
    );
    assert!(
        !ran.screen.contains("Resetting repository"),
        "{}",
        ran.screen
    );
    assert_eq!(snapshot(&fixture.checkout), before);
}

/// A branch on the remote with no local branch yet is no refusal: the
/// checkout creates it tracking the remote's (git's checkout guess), as
/// before.
#[tokio::test]
async fn a_remote_branch_not_yet_local_is_still_checked_out() {
    let fixture = Fixture::new();
    git(
        &fixture.checkout,
        &["remote", "set-branches", "--add", "origin", "develop"],
    );
    let ran = update(&fixture.repo(), "develop").await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(
        git(
            &fixture.checkout,
            &["rev-parse", "--abbrev-ref", "develop@{u}"]
        ),
        "origin/develop"
    );
}

/// A branch only on the remote, checked out where git sets no upstream
/// (root's `branch.autoSetupMerge=false`): the up-front check can't see
/// it, so the check before the last reset refuses it, after the reset
/// (repo spec, Known limits).
#[tokio::test]
async fn a_checkout_git_leaves_without_an_upstream_is_refused_after_the_reset() {
    let fixture = Fixture::new();
    git(
        &fixture.checkout,
        &["remote", "set-branches", "--add", "origin", "develop"],
    );
    git(
        &fixture.checkout,
        &["config", "branch.autoSetupMerge", "false"],
    );
    let ran = update(&fixture.repo(), "develop").await;
    assert!(
        ran.screen.contains("Resetting repository to develop"),
        "{}",
        ran.screen
    );
    assert!(
        ran.screen
            .contains("Sandbox: branch 'develop' has no upstream to update from"),
        "{}",
        ran.screen
    );
}

#[tokio::test]
async fn a_failed_fetch_is_exec_s_error() {
    let fixture = Fixture::new();
    git(
        &fixture.checkout,
        &[
            "remote",
            "set-url",
            "origin",
            "file:///nonexistent/sb-rust-7fcd24d1.git",
        ],
    );
    let ran = update(&fixture.repo(), "master").await;
    assert!(ran.error.is_some());
    assert!(
        ran.screen.contains("exited with code 128"),
        "{}",
        ran.screen
    );
}

#[tokio::test]
async fn git_runs_with_lc_all_c_and_no_terminal_prompt() {
    let _spawns = SPAWNS.lock().await;
    let fixture = Fixture::new();
    let bin = tempfile::tempdir().expect("bin");
    let repo = fixture.repo().with_git(recording_git(bin.path()));
    assert_eq!(update(&repo, "master").await.error, None);
    let log = std::fs::read_to_string(bin.path().join("env.log")).expect("log");
    assert!(log.lines().count() >= 8, "{log}");
    assert!(log.lines().all(|line| line == "C 0"), "{log}");
}

#[tokio::test]
async fn a_submodule_added_upstream_is_initialised() {
    let _spawns = SPAWNS.lock().await;
    let fixture = Fixture::new();
    // A repository to use as a submodule, added to master upstream.
    let module = fixture.dir.path().join("module");
    git(
        fixture.dir.path(),
        &["init", "--quiet", module.to_str().expect("utf-8")],
    );
    std::fs::write(module.join("inside.txt"), "inside\n").expect("write");
    git(&module, &["add", "."]);
    git(&module, &["commit", "--quiet", "-m", "module"]);
    git(
        &fixture.work,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--quiet",
            &format!("file://{}", module.display()),
            "roles/module",
        ],
    );
    git(&fixture.work, &["commit", "--quiet", "-m", "submodule"]);
    git(&fixture.work, &["push", "--quiet", "origin", "master"]);
    let bin = tempfile::tempdir().expect("bin");
    let repo = fixture.repo().with_git(arguments_git(bin.path()));
    let ran = update(&repo, "master").await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert!(fixture.checkout.join("roles/module/inside.txt").exists());
    // With a `.gitmodules`, git's submodule update runs as it always did.
    assert_eq!(
        submodule_runs(bin.path()),
        ["submodule update --progress --init --recursive"]
    );
}

#[tokio::test]
async fn without_a_gitmodules_the_submodule_update_is_not_run() {
    let _spawns = SPAWNS.lock().await;
    let fixture = Fixture::new();
    let moved = fixture.advance("master", "next.txt");
    let bin = tempfile::tempdir().expect("bin");
    let repo = fixture.repo().with_git(arguments_git(bin.path()));
    let ran = update(&repo, "master").await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert!(!fixture.checkout.join(".gitmodules").exists());
    assert_eq!(fixture.head(), moved);
    // The step is still drawn; git's submodule never ran.
    assert!(
        ran.screen.contains("Updating git submodules"),
        "{}",
        ran.screen
    );
    assert_eq!(submodule_runs(bin.path()), Vec::<String>::new());
}

#[tokio::test]
async fn a_branch_starting_with_a_dash_never_reaches_git() {
    let fixture = Fixture::new();
    let ran = update(&fixture.repo(), "-x").await;
    assert_eq!(
        ran.error.as_deref(),
        Some("'-x' is not a valid branch name")
    );
}

#[tokio::test]
async fn a_name_git_rewrites_is_refused() {
    let fixture = Fixture::new();
    // With a branch switch in the reflog, `@{-1}` is valid to git and
    // prints the previous branch, not itself.
    git(&fixture.checkout, &["checkout", "--quiet", "-b", "other"]);
    git(&fixture.checkout, &["checkout", "--quiet", "master"]);
    let error = fixture
        .repo()
        .check_branch_name("@{-1}", &token())
        .await
        .expect_err("rewritten");
    assert_eq!(error.to_string(), "'@{-1}' is not a valid branch name");
}

/// What GitHub's https remote makes git print when it may not prompt.
const ASKED: &str =
    "fatal: could not read Username for 'https://github.com': terminal prompts disabled";

/// A git in `dir` whose plain `fetch` fails with `first` (stderr, exit
/// 128), whose `-c http.version=HTTP/1.1 fetch` does `retry`, and which
/// runs the real git otherwise; each call's arguments go to `calls.log`.
fn flaky_git(dir: &Path, first: &str, retry: &str) -> PathBuf {
    let path = dir.join("git");
    let log = dir.join("calls.log");
    common::stand_in(
        &path,
        &format!(
            "printf '%s\\n' \"$*\" >> '{log}'\n\
             if [ \"$1\" = fetch ]; then echo \"{first}\" >&2; exit 128; fi\n\
             if [ \"$1 $2 $3\" = '-c http.version=HTTP/1.1 fetch' ]; then {retry}; fi\n\
             exec git \"$@\"\n",
            log = log.display(),
        ),
    );
    path
}

fn calls(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("calls.log"))
        .expect("calls")
        .lines()
        .filter(|line| line.contains("fetch") && !line.starts_with("config"))
        .map(str::to_string)
        .collect()
}

#[tokio::test]
async fn a_credentials_prompt_is_retried_once_over_http_1_1() {
    let _spawns = SPAWNS.lock().await;
    let fixture = Fixture::new();
    let bin = tempfile::tempdir().expect("bin");
    let program = flaky_git(bin.path(), ASKED, "shift 2; exec git \"$@\"");
    let ran = update(&fixture.repo().with_git(program), "master").await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(
        calls(bin.path()),
        [
            "fetch --progress",
            "-c http.version=HTTP/1.1 fetch --progress"
        ]
    );
    assert!(
        ran.screen
            .contains("Retried Sandbox Git fetch over HTTP/1.1"),
        "{}",
        ran.screen
    );
}

#[tokio::test]
async fn a_retry_asked_again_is_sb_gos_public_repository_error() {
    let _spawns = SPAWNS.lock().await;
    let fixture = Fixture::new();
    let bin = tempfile::tempdir().expect("bin");
    let program = flaky_git(
        bin.path(),
        ASKED,
        &format!("echo \"{ASKED}\" >&2; exit 128"),
    );
    let ran = update(&fixture.repo().with_git(program), "master").await;
    assert!(ran.error.is_some());
    assert_eq!(calls(bin.path()).len(), 2);
    for text in [
        "Sandbox Git fetch retry over HTTP/1.1",
        "GitHub unexpectedly requested authentication for the public Sandbox repository.",
        "Git details:",
    ] {
        assert!(ran.screen.contains(text), "{text}\n{}", ran.screen);
    }
}

#[tokio::test]
async fn a_retry_failing_otherwise_is_exec_s_error() {
    let _spawns = SPAWNS.lock().await;
    let fixture = Fixture::new();
    let bin = tempfile::tempdir().expect("bin");
    let program = flaky_git(bin.path(), ASKED, "echo 'fatal: other' >&2; exit 2");
    let ran = update(&fixture.repo().with_git(program), "master").await;
    assert!(ran.error.is_some());
    assert!(ran.screen.contains("exited with code 2"), "{}", ran.screen);
    assert!(
        !ran.screen.contains("GitHub unexpectedly"),
        "{}",
        ran.screen
    );
}

#[tokio::test]
async fn another_failure_is_not_retried() {
    let _spawns = SPAWNS.lock().await;
    let fixture = Fixture::new();
    let bin = tempfile::tempdir().expect("bin");
    let program = flaky_git(bin.path(), "fatal: repository not found", "exit 0");
    let ran = update(&fixture.repo().with_git(program), "master").await;
    assert!(ran.error.is_some());
    assert_eq!(calls(bin.path()), ["fetch --progress"]);
    assert!(
        ran.screen.contains("exited with code 128"),
        "{}",
        ran.screen
    );
    assert!(!ran.screen.contains("HTTP/1.1"), "{}", ran.screen);
}

#[tokio::test]
async fn ssh_never_waits_on_a_terminal() {
    let _spawns = SPAWNS.lock().await;
    let fixture = Fixture::new();
    let bin = tempfile::tempdir().expect("bin");
    // A stand-in ssh that records what decides whether ssh may prompt,
    // then fails as ssh does (255).
    let ssh = bin.path().join("ssh");
    sb_cli_core::testing::write_program(
        &ssh,
        format!(
            "#!/bin/sh\nprintf '%s %s\\n' \"$SSH_ASKPASS\" \"$SSH_ASKPASS_REQUIRE\" > '{}'\nexit 255\n",
            bin.path().join("ssh.log").display()
        ),
    );
    git(
        &fixture.checkout,
        &[
            "remote",
            "set-url",
            "origin",
            "ssh://git@example.invalid/sandbox.git",
        ],
    );
    git(
        &fixture.checkout,
        &["config", "core.sshCommand", ssh.to_str().expect("utf-8")],
    );
    let ran = update(&fixture.repo(), "master").await;
    assert!(ran.error.is_some());
    assert_eq!(
        std::fs::read_to_string(bin.path().join("ssh.log")).expect("ssh ran"),
        "/bin/false force\n"
    );
}

#[tokio::test]
async fn a_credentials_line_in_a_run_that_succeeds_is_not_retried() {
    let _spawns = SPAWNS.lock().await;
    let fixture = Fixture::new();
    let bin = tempfile::tempdir().expect("bin");
    // git's own retries (a submodule clone) can print the line and still
    // exit 0.
    let program = flaky_git(bin.path(), ASKED, "exit 0");
    let path = bin.path().join("git");
    let script = std::fs::read_to_string(&path).expect("read").replace(
        &format!("echo \"{ASKED}\" >&2; exit 128"),
        &format!("echo \"{ASKED}\" >&2; exec git \"$@\""),
    );
    std::fs::write(&path, script).expect("write");
    let ran = update(&fixture.repo().with_git(program), "master").await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(calls(bin.path()), ["fetch --progress"]);
}

#[tokio::test]
async fn a_retry_that_succeeds_with_the_line_is_done() {
    let _spawns = SPAWNS.lock().await;
    let fixture = Fixture::new();
    let bin = tempfile::tempdir().expect("bin");
    let program = flaky_git(
        bin.path(),
        ASKED,
        &format!("echo \"{ASKED}\" >&2; shift 2; exec git \"$@\""),
    );
    let ran = update(&fixture.repo().with_git(program), "master").await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(calls(bin.path()).len(), 2);
}

#[test]
fn the_same_branch_is_used_whatever_the_answer() {
    for answer in [None, Some(Answer::Reset), Some(Answer::Keep)] {
        assert_eq!(
            branch_choice("master", "master", answer),
            Choice::Use("master".to_string()),
            "{answer:?}"
        );
    }
}

#[test]
fn another_branch_follows_the_answer() {
    assert_eq!(
        branch_choice("develop", "master", Some(Answer::Reset)),
        Choice::Use("master".to_string())
    );
    assert_eq!(
        branch_choice("develop", "master", Some(Answer::Keep)),
        Choice::Keep("develop".to_string())
    );
    assert_eq!(branch_choice("develop", "master", None), Choice::Ask);
}

#[test]
fn a_detached_head_is_another_branch() {
    assert_eq!(branch_choice("HEAD", "master", None), Choice::Ask);
    assert_eq!(
        branch_choice("HEAD", "master", Some(Answer::Reset)),
        Choice::Use("master".to_string())
    );
    assert_eq!(
        branch_choice("HEAD", "master", Some(Answer::Keep)),
        Choice::Keep("HEAD".to_string())
    );
}

#[test]
fn the_notices_are_sb_gos_info_lines() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = Repo::open("Sandbox", dir.path()).expect("open");
    let same = branch_choice("master", "master", None);
    assert_eq!(
        branch_notices(&repo, "master", "master", &same),
        Vec::<String>::new()
    );
    let reset = branch_choice("develop", "master", Some(Answer::Reset));
    assert_eq!(
        branch_notices(&repo, "develop", "master", &reset),
        ["Sandbox: Currently on branch 'develop'"]
    );
    let ask = branch_choice("develop", "master", None);
    assert_eq!(
        branch_notices(&repo, "develop", "master", &ask),
        ["Sandbox: Currently on branch 'develop'"]
    );
    let keep = branch_choice("develop", "master", Some(Answer::Keep));
    assert_eq!(
        branch_notices(&repo, "develop", "master", &keep),
        [
            "Sandbox: Currently on branch 'develop'",
            "Sandbox: Updating the current branch 'develop'"
        ]
    );
}

#[tokio::test]
async fn head_is_the_checkout_s_commit_and_moves_with_an_update() {
    let fixture = Fixture::new();
    let repo = fixture.repo();
    let before = repo.head(&token()).await.expect("head");
    assert_eq!(before, fixture.head());
    assert_eq!(before.len(), 40, "{before}");
    let master = fixture.advance("master", "next.txt");
    assert_eq!(update(&repo, "master").await.error, None);
    let after = repo.head(&token()).await.expect("head");
    assert_eq!(after, master);
    assert_ne!(after, before);
}

/// `head`'s error starts with sb-go's `GetGitCommitHash` text.
fn assert_head_error(error: &RepoError) {
    assert!(matches!(error, RepoError::Head(_)), "{error:?}");
    let text = error.to_string();
    assert!(
        text.starts_with("error occurred while trying to get the git commit hash: "),
        "{text}"
    );
}

#[tokio::test]
async fn head_in_an_unborn_repository_is_sb_gos_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    git(dir.path(), &["init", "--quiet"]);
    let bin = tempfile::tempdir().expect("bin");
    let repo = Repo::open("Sandbox", dir.path())
        .expect("open")
        .with_git(common::isolated_git(bin.path()));
    let error = repo.head(&token()).await.expect_err("unborn");
    assert_head_error(&error);
    assert!(
        error.to_string().contains("exited with code 128"),
        "{error}"
    );
}

#[tokio::test]
async fn head_outside_a_repository_is_sb_gos_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = tempfile::tempdir().expect("bin");
    let repo = Repo::open("Sandbox", dir.path())
        .expect("open")
        .with_git(common::isolated_git(bin.path()));
    let error = repo.head(&token()).await.expect_err("not a repo");
    assert_head_error(&error);
    assert!(
        error.to_string().contains("exited with code 128"),
        "{error}"
    );
}

#[tokio::test]
async fn head_without_a_git_program_is_sb_gos_error() {
    let fixture = Fixture::new();
    let bin = tempfile::tempdir().expect("bin");
    let repo = fixture.repo().with_git(bin.path().join("missing-git"));
    let error = repo.head(&token()).await.expect_err("no git");
    assert_head_error(&error);
}

/// `Repo::clone_with_git` under a runner, with `program` as git; the
/// repository it returned, if any, and what the run left. The real git
/// goes through the fixture's isolating stand-in (`common::isolated_git`),
/// so `Repo::clone`, which runs the git on `PATH` with the caller's
/// environment, isn't run here.
async fn clone(program: &Path, path: &Path, url: &str, branch: &str) -> (Option<Repo>, Ran) {
    let mut cloned = None;
    let ran = in_step(async |step| {
        cloned = Some(Repo::clone_with_git(program, "Sandbox", path, url, branch, step).await?);
        Ok(())
    })
    .await;
    (cloned, ran)
}

#[tokio::test]
async fn clone_into_a_missing_directory_is_a_shallow_checkout_of_the_branch() {
    let fixture = Fixture::new();
    let path = fixture.dir.path().join("new/sandbox");
    let (repo, ran) = clone(&fixture.git, &path, &fixture.url(), "develop").await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    let repo = repo.expect("cloned");
    assert_eq!((repo.name(), repo.path()), ("Sandbox", path.as_path()));
    assert_eq!(
        repo.current_branch(&token()).await.expect("branch"),
        "develop"
    );
    assert_eq!(
        repo.head(&token()).await.expect("head"),
        git(&fixture.work, &["rev-parse", "develop"])
    );
    assert_eq!(
        git(&path, &["rev-parse", "--is-shallow-repository"]),
        "true"
    );
    let cloning = format!(
        "Cloning Sandbox repository to {} (branch: develop)",
        path.display()
    );
    assert!(ran.screen.contains(&cloning), "{}", ran.screen);
    assert!(
        ran.screen.contains("Updating git submodules"),
        "{}",
        ran.screen
    );
}

#[tokio::test]
async fn clone_initialises_the_submodules() {
    let _spawns = SPAWNS.lock().await;
    let fixture = Fixture::new();
    let _module = fixture.add_submodule();
    let bin = tempfile::tempdir().expect("bin");
    let path = fixture.dir.path().join("sandbox");
    let (_, ran) = clone(&recording_git(bin.path()), &path, &fixture.url(), "master").await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert!(path.join("roles/module/inside.txt").exists());
    let log = std::fs::read_to_string(bin.path().join("env.log")).expect("log");
    assert!(log.lines().all(|line| line == "C 0"), "{log}");
}

#[tokio::test]
async fn clone_refuses_a_destination_that_is_there() {
    let fixture = Fixture::new();
    let (repo, ran) = clone(&fixture.git, &fixture.checkout, &fixture.url(), "master").await;
    assert!(repo.is_none());
    let refused = format!(
        "destination path '{}' already exists",
        fixture.checkout.display()
    );
    assert!(ran.screen.contains(&refused), "{}", ran.screen);
    assert_eq!(
        git(&fixture.checkout, &["status", "--porcelain"]),
        "",
        "the checkout is untouched"
    );
}

#[tokio::test]
async fn a_failed_clone_leaves_no_directory() {
    let fixture = Fixture::new();
    let path = fixture.dir.path().join("sandbox");
    let (repo, ran) = clone(&fixture.git, &path, &fixture.url(), "missing").await;
    assert!(repo.is_none());
    let failed = format!(
        "failed to clone Sandbox repository '{}' (branch: 'missing') to '{}'",
        fixture.url(),
        path.display()
    );
    assert!(ran.screen.contains(&failed), "{}", ran.screen);
    assert!(
        ran.screen.contains("exited with code 128"),
        "{}",
        ran.screen
    );
    assert!(!path.exists(), "git removes the directory it made");
}

#[tokio::test]
async fn clone_checks_the_branch_name_before_git_runs() {
    let fixture = Fixture::new();
    let path = fixture.dir.path().join("sandbox");
    for name in ["-x", "a..b"] {
        let (repo, ran) = clone(&fixture.git, &path, &fixture.url(), name).await;
        assert!(repo.is_none());
        let refused = format!("'{name}' is not a valid branch name");
        assert!(ran.screen.contains(&refused), "{}", ran.screen);
        assert!(!path.exists());
    }
}

#[tokio::test]
async fn a_failed_submodule_update_after_a_clone_leaves_the_checkout() {
    let _spawns = SPAWNS.lock().await;
    let fixture = Fixture::new();
    let module = fixture.add_submodule();
    std::fs::remove_dir_all(&module).expect("remove the module's repository");
    let bin = tempfile::tempdir().expect("bin");
    let path = fixture.dir.path().join("sandbox");
    let (repo, ran) = clone(&recording_git(bin.path()), &path, &fixture.url(), "master").await;
    assert!(repo.is_none());
    assert!(
        ran.screen.contains("error running git submodule update"),
        "{}",
        ran.screen
    );
    assert_eq!(
        git(&path, &["rev-parse", "HEAD"]),
        git(&fixture.work, &["rev-parse", "master"]),
        "the clone stays"
    );
}

/// `init_from_remote` on `repo` under a runner.
async fn init(repo: &Repo, url: &str, branch: &str) -> Ran {
    in_step(async |step| repo.init_from_remote(url, branch, step).await).await
}

#[tokio::test]
async fn init_from_remote_over_a_directory_keeps_its_ignored_files() {
    let fixture = Fixture::new();
    let root = fixture.dir.path().join("saltbox");
    std::fs::create_dir(&root).expect("mkdir");
    std::fs::write(root.join("accounts.yml"), "user: kept\n").expect("write");
    let repo = Repo::open("Saltbox", &root)
        .expect("open")
        .with_git(&fixture.git);
    let ran = init(&repo, &fixture.url(), "develop").await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    let develop = git(&fixture.work, &["rev-parse", "develop"]);
    assert_eq!(repo.head(&token()).await.expect("head"), develop);
    assert_eq!(
        repo.current_branch(&token()).await.expect("branch"),
        "develop",
        "HEAD is on the branch, not git's default"
    );
    assert_eq!(git(&root, &["rev-parse", "develop"]), develop);
    assert_eq!(
        git(&root, &["rev-parse", "--abbrev-ref", "develop@{u}"]),
        "origin/develop"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("develop.txt")).expect("tracked"),
        "develop\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("accounts.yml")).expect("kept"),
        "user: kept\n"
    );
    for label in [
        "Initializing Git repository",
        "Creating Git repository",
        "Configuring Git remote",
        "Fetching repository branches",
        "Creating branch develop",
        "Resetting to branch develop",
        "Updating git submodules",
    ] {
        assert!(ran.screen.contains(label), "{label}: {}", ran.screen);
    }
}

#[tokio::test]
async fn init_from_remote_initialises_the_submodules() {
    let _spawns = SPAWNS.lock().await;
    let fixture = Fixture::new();
    let _module = fixture.add_submodule();
    let root = fixture.dir.path().join("saltbox");
    std::fs::create_dir(&root).expect("mkdir");
    let bin = tempfile::tempdir().expect("bin");
    let repo = Repo::open("Saltbox", &root)
        .expect("open")
        .with_git(recording_git(bin.path()));
    let ran = init(&repo, &fixture.url(), "master").await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert!(root.join("roles/module/inside.txt").exists());
}

#[tokio::test]
async fn a_failed_init_fetch_leaves_the_git_directory_it_made() {
    let fixture = Fixture::new();
    let root = fixture.dir.path().join("saltbox");
    std::fs::create_dir(&root).expect("mkdir");
    let repo = Repo::open("Saltbox", &root)
        .expect("open")
        .with_git(&fixture.git);
    let url = "file:///nonexistent/sb-rust-7fcd24d1.git";
    let ran = init(&repo, url, "master").await;
    assert!(ran.error.is_some());
    assert!(
        ran.screen
            .contains("error running command [git fetch --progress --all --prune]"),
        "{}",
        ran.screen
    );
    // The known limit: a rerun of `setup --force` finds a checkout and
    // fetches and resets it.
    assert!(root.join(".git").is_dir());
    assert_eq!(git(&root, &["remote", "get-url", "origin"]), url);
}

#[tokio::test]
async fn init_from_remote_names_a_branch_missing_on_the_remote() {
    let fixture = Fixture::new();
    let root = fixture.dir.path().join("saltbox");
    std::fs::create_dir(&root).expect("mkdir");
    let repo = Repo::open("Saltbox", &root)
        .expect("open")
        .with_git(&fixture.git);
    let ran = init(&repo, &fixture.url(), "missing").await;
    assert!(ran.error.is_some());
    assert!(
        ran.screen
            .contains("error running command [git branch missing origin/missing]"),
        "{}",
        ran.screen
    );
    assert!(root.join(".git").is_dir());
}

#[tokio::test]
async fn init_from_remote_checks_the_branch_name_first() {
    let fixture = Fixture::new();
    let root = fixture.dir.path().join("saltbox");
    std::fs::create_dir(&root).expect("mkdir");
    let repo = Repo::open("Saltbox", &root)
        .expect("open")
        .with_git(&fixture.git);
    let ran = init(&repo, &fixture.url(), "-x").await;
    assert!(
        ran.screen.contains("'-x' is not a valid branch name"),
        "{}",
        ran.screen
    );
    assert!(!root.join(".git").exists());
}

#[tokio::test]
async fn clone_names_a_destination_it_cannot_use() {
    let fixture = Fixture::new();
    let (repo, ran) = clone(&fixture.git, Path::new(""), &fixture.url(), "master").await;
    assert!(repo.is_none());
    assert!(
        ran.screen.contains("inspect required directory : "),
        "{}",
        ran.screen
    );
    let under_a_file = fixture.checkout.join("tracked.txt/sandbox");
    let (repo, ran) = clone(&fixture.git, &under_a_file, &fixture.url(), "master").await;
    assert!(repo.is_none());
    let refused = format!(
        "inspect required directory {}: Not a directory (os error 20)",
        under_a_file.display()
    );
    assert!(ran.screen.contains(&refused), "{}", ran.screen);
    let not_utf8 = fixture
        .dir
        .path()
        .join(<std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(b"sb\xff"));
    let (repo, ran) = clone(&fixture.git, &not_utf8, &fixture.url(), "master").await;
    assert!(repo.is_none());
    assert!(
        ran.screen.contains("the path isn't UTF-8"),
        "{}",
        ran.screen
    );
    assert!(!not_utf8.exists());
}

/// A git in `dir` whose `<subcommand>` fails (`fatal: refused`, exit 1),
/// and which runs the real git otherwise.
fn failing_git(dir: &Path, subcommand: &str) -> PathBuf {
    let path = dir.join("git");
    common::stand_in(
        &path,
        &format!(
            "if [ \"$1\" = {subcommand} ]; then echo 'fatal: refused' >&2; exit 1; fi\n\
             exec git \"$@\"\n"
        ),
    );
    path
}

#[tokio::test]
async fn a_failed_git_init_leaves_no_git_directory() {
    let _spawns = SPAWNS.lock().await;
    let fixture = Fixture::new();
    let root = fixture.dir.path().join("saltbox");
    std::fs::create_dir(&root).expect("mkdir");
    let bin = tempfile::tempdir().expect("bin");
    let repo = Repo::open("Saltbox", &root)
        .expect("open")
        .with_git(failing_git(bin.path(), "init"));
    let ran = init(&repo, &fixture.url(), "master").await;
    assert!(ran.error.is_some());
    assert!(
        ran.screen
            .contains("error running command [git init -b master]"),
        "{}",
        ran.screen
    );
    assert!(!root.join(".git").exists());
}

#[tokio::test]
async fn a_failed_init_reset_leaves_the_branch_it_made() {
    let _spawns = SPAWNS.lock().await;
    let fixture = Fixture::new();
    let root = fixture.dir.path().join("saltbox");
    std::fs::create_dir(&root).expect("mkdir");
    let bin = tempfile::tempdir().expect("bin");
    let repo = Repo::open("Saltbox", &root)
        .expect("open")
        .with_git(failing_git(bin.path(), "reset"));
    let ran = init(&repo, &fixture.url(), "master").await;
    assert!(ran.error.is_some());
    assert!(
        ran.screen
            .contains("error running command [git reset --hard origin/master]"),
        "{}",
        ran.screen
    );
    assert_eq!(
        git(&root, &["rev-parse", "master"]),
        git(&fixture.work, &["rev-parse", "master"])
    );
}

#[tokio::test]
async fn a_clone_asked_for_credentials_is_retried_with_its_values() {
    let _spawns = SPAWNS.lock().await;
    let fixture = Fixture::new();
    let bin = tempfile::tempdir().expect("bin");
    let program = bin.path().join("git");
    let log = bin.path().join("calls.log");
    common::stand_in(
        &program,
        &format!(
            "printf '%s\\n' \"$*\" >> '{log}'\n\
             if [ \"$1\" = clone ]; then echo \"{ASKED}\" >&2; exit 128; fi\n\
             exec git \"$@\"\n",
            log = log.display(),
        ),
    );
    let path = fixture.dir.path().join("sandbox");
    let (repo, ran) = clone(&program, &path, &fixture.url(), "master").await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert!(repo.is_some());
    let calls = std::fs::read_to_string(&log).expect("calls");
    let clones: Vec<&str> = calls
        .lines()
        .filter(|line| line.contains("clone"))
        .collect();
    let args = format!(
        "--progress --depth 1 -b master {} {}",
        fixture.url(),
        path.display()
    );
    assert_eq!(
        clones,
        [
            format!("clone {args}"),
            format!("-c http.version=HTTP/1.1 clone {args}")
        ]
    );
    assert!(ran.screen.contains("Sandbox Git clone"), "{}", ran.screen);
}

/// A clean clone has nothing an update would discard: no question, no
/// output change (repo spec, `discards`; ops decision 40).
#[tokio::test]
async fn a_clean_checkout_has_nothing_to_discard() {
    let fixture = Fixture::new();
    // Saltbox's config is ignored: never counted (no `clean -x`).
    std::fs::write(fixture.checkout.join("accounts.yml"), "user: me\n").expect("write");
    let repo = fixture.repo();
    let discards = repo
        .discards("master", "master", &token())
        .await
        .expect("discards");
    assert!(discards.is_empty(), "{discards:?}");
    assert_eq!(discards.summary(), "");
    let changes = repo.local_changes(&token()).await.expect("changes");
    assert!(changes.is_empty(), "{changes:?}");
    assert_eq!(changes.count(), 0);
}

/// A local commit, a staged and an unstaged edit, an untracked file, an
/// untracked directory and an empty one: each counted as the reset and
/// the clean would drop them; an ignored file and a file touched but not
/// changed aren't; the reads change nothing.
#[tokio::test]
async fn local_commits_edits_and_untracked_files_are_counted() {
    let fixture = Fixture::new();
    std::fs::write(fixture.checkout.join("local.txt"), "commit\n").expect("write");
    git(&fixture.checkout, &["add", "local.txt"]);
    git(&fixture.checkout, &["commit", "--quiet", "-m", "local"]);
    local_changes(&fixture.checkout);
    std::fs::write(fixture.checkout.join("accounts.yml"), "user: me\n").expect("write");
    // Rewritten with the same bytes: a newer mtime, no change.
    std::fs::write(fixture.checkout.join("local.txt"), "commit\n").expect("write");
    let before = snapshot(&fixture.checkout);
    let repo = fixture.repo();
    let discards = repo
        .discards("master", "master", &token())
        .await
        .expect("discards");
    assert_eq!(discards.branch, "master");
    assert_eq!(discards.commits, 1);
    assert_eq!(discards.detached, 0);
    assert_eq!(discards.changes.tracked, [".gitignore", "tracked.txt"]);
    assert_eq!(
        discards.changes.untracked,
        ["drafts/", "empty/", "notes.txt"]
    );
    assert_eq!(
        discards.summary(),
        "1 local commit on 'master', 2 tracked changes and 3 untracked files"
    );
    assert_eq!(
        repo.local_changes(&token()).await.expect("changes"),
        discards.changes
    );
    assert_eq!(snapshot(&fixture.checkout), before);
    // And an update discards exactly that: the commit, the edits and the
    // untracked entries gone, the ignored file kept.
    let ran = update(&repo, "master").await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    let after = repo
        .discards("master", "master", &token())
        .await
        .expect("discards");
    assert!(after.is_empty(), "{after:?}");
    assert!(fixture.checkout.join("accounts.yml").exists());
}

/// A switch to another branch counts the target's local commits, which
/// its reset drops, never the current branch's, which stay on it.
#[tokio::test]
async fn a_switch_counts_the_targets_commits_not_the_current_ones() {
    let fixture = Fixture::new();
    std::fs::write(fixture.checkout.join("ahead.txt"), "ahead\n").expect("write");
    git(&fixture.checkout, &["add", "ahead.txt"]);
    git(&fixture.checkout, &["commit", "--quiet", "-m", "ahead"]);
    git(&fixture.checkout, &["checkout", "--quiet", "-b", "feature"]);
    for file in ["a.txt", "b.txt"] {
        std::fs::write(fixture.checkout.join(file), file).expect("write");
        git(&fixture.checkout, &["add", file]);
        git(&fixture.checkout, &["commit", "--quiet", "-m", file]);
    }
    let discards = fixture
        .repo()
        .discards("feature", "master", &token())
        .await
        .expect("discards");
    assert_eq!((discards.commits, discards.detached), (1, 0));
    assert_eq!(discards.summary(), "1 local commit on 'master'");
    // A branch with no upstream is the update's own refusal, before any
    // question: answering one would only lead to it (decision 12).
    let kept = fixture
        .repo()
        .discards("feature", "feature", &token())
        .await
        .expect_err("no upstream");
    assert_eq!(
        kept.to_string(),
        "Sandbox: branch 'feature' has no upstream to update from"
    );
    // A branch only on the remote has no local commits to lose.
    let remote = fixture
        .repo()
        .discards("feature", "develop", &token())
        .await
        .expect("discards");
    assert!(remote.is_empty(), "{remote:?}");
}

/// A detached HEAD's own commits are left behind by the checkout of the
/// branch: counted as such; one a tag holds isn't lost.
#[tokio::test]
async fn a_detached_heads_own_commits_are_counted() {
    let fixture = Fixture::new();
    git(&fixture.checkout, &["checkout", "--quiet", "--detach"]);
    for file in ["x.txt", "y.txt"] {
        std::fs::write(fixture.checkout.join(file), file).expect("write");
        git(&fixture.checkout, &["add", file]);
        git(&fixture.checkout, &["commit", "--quiet", "-m", file]);
    }
    let discards = fixture
        .repo()
        .discards("HEAD", "master", &token())
        .await
        .expect("discards");
    assert_eq!((discards.commits, discards.detached), (0, 2));
    assert_eq!(discards.summary(), "2 commits only on the detached HEAD");
    git(&fixture.checkout, &["tag", "kept"]);
    let tagged = fixture
        .repo()
        .discards("HEAD", "master", &token())
        .await
        .expect("discards");
    assert!(tagged.is_empty(), "{tagged:?}");
}

/// Outside a repository each read is git's error, never "nothing to
/// discard".
#[tokio::test]
async fn outside_a_repository_the_reads_are_gits_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = tempfile::tempdir().expect("bin");
    let repo = Repo::open("Sandbox", dir.path())
        .expect("open")
        .with_git(common::isolated_git(bin.path()));
    let err = repo
        .local_changes(&token())
        .await
        .expect_err("not a repository");
    assert!(matches!(err, RepoError::Git(_)), "{err:?}");
    let err = repo
        .discards("HEAD", "master", &token())
        .await
        .expect_err("not a repository");
    assert!(matches!(err, RepoError::Git(_)), "{err:?}");
}

/// An untracked name outside ASCII comes as it is, as the tracked ones
/// do (`core.quotePath=false`), not in git's octal escapes.
#[tokio::test]
async fn an_untracked_name_outside_ascii_is_as_it_is() {
    let fixture = Fixture::new();
    std::fs::write(fixture.checkout.join("café.txt"), "mine\n").expect("write");
    let changes = fixture
        .repo()
        .local_changes(&token())
        .await
        .expect("changes");
    assert_eq!(changes.untracked, ["café.txt"]);
}
