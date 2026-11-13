//! sb-repo against real git on local repositories (repo spec, Testing
//! Strategy): a bare "remote" in a temp dir with `master` and `develop`,
//! and a single-branch clone of it, the shape setup makes. Nothing here
//! touches a Saltbox checkout or the network (`file://` remotes only).

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use sb_cli_core::cancel::Shutdown;
use sb_repo::{Answer, Choice, Repo, RepoError, branch_choice, branch_notices};
use sb_term_ui::{Labels, Options, Runner, Target};
use tokio_util::sync::CancellationToken;

/// `git <args>` in `dir`, which must succeed; its stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=sb", "-c", "user.email=sb@example.invalid"])
        .args([
            "-c",
            "init.defaultBranch=master",
            "-c",
            "advice.detachedHead=false",
        ])
        .args(args)
        .current_dir(dir)
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
        Self {
            dir,
            work,
            checkout,
        }
    }

    fn repo(&self) -> Repo {
        Repo::open("Sandbox", &self.checkout).expect("open")
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
    let term = indicatif::InMemoryTerm::new(40, 120);
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
            let result = repo.fetch_and_reset(branch, "", step).await;
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
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nprintf '%s %s\\n' \"$LC_ALL\" \"$GIT_TERMINAL_PROMPT\" >> '{}'\n\
             exec git -c protocol.file.allow=always \"$@\"\n",
            dir.join("env.log").display()
        ),
    )
    .expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
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
    let repo = Repo::open("Sandbox", dir.path()).expect("open");
    let error = repo.current_branch(&token()).await.expect_err("not a repo");
    let text = error.to_string();
    assert!(text.starts_with("failed to get current branch: "), "{text}");
    assert!(text.contains("exited with code 128"), "{text}");
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
    let repo = Repo::open("Sandbox", dir.path()).expect("open");
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
    let repo = fixture.repo().with_git(recording_git(bin.path()));
    let ran = update(&repo, "master").await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert!(fixture.checkout.join("roles/module/inside.txt").exists());
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
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{log}'\n\
             if [ \"$1\" = fetch ]; then echo \"{first}\" >&2; exit 128; fi\n\
             if [ \"$1 $2 $3\" = '-c http.version=HTTP/1.1 fetch' ]; then {retry}; fi\n\
             exec git \"$@\"\n",
            log = log.display(),
        ),
    )
    .expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
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
    std::fs::write(
        &ssh,
        format!(
            "#!/bin/sh\nprintf '%s %s\\n' \"$SSH_ASKPASS\" \"$SSH_ASKPASS_REQUIRE\" > '{}'\nexit 255\n",
            bin.path().join("ssh.log").display()
        ),
    )
    .expect("write");
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).expect("chmod");
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
    let repo = Repo::open("Sandbox", std::env::temp_dir()).expect("open");
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
    let repo = Repo::open("Sandbox", dir.path()).expect("open");
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
    let repo = Repo::open("Sandbox", dir.path()).expect("open");
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
