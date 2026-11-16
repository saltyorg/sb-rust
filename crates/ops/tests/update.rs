//! `sb update`'s first half in process (ops spec, "`sb update`", steps 1 to
//! 5, 7 and 9; module build M84): sb-ops' `update::run` on the harness's
//! stand-in apt, Saltbox and Sandbox checkouts of the harness's stand-in
//! remotes (real git, `file://` only, never the network), release's fake
//! GitHub for self-update, and runners drawing to memory, as any user
//! (the Saltbox user is one no host has, so ownership is skipped). The
//! binary's run as root is crates/sb/tests/update.rs.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use sb_cli_core::build_info::BuildInfo;
use sb_cli_core::cancel::Shutdown;
use sb_host::disk::{Disk, Stats};
use sb_ops::testing::{Host, Remote, git};
use sb_ops::update::{self, Migrations, Update, UpdateArgs};
use sb_release::testing::{Answer, Fake, stand_in};
use sb_release::{GithubRepo, Releases, SelfUpdate};
use sb_repo::Answer as BranchAnswer;
use sb_term_ui::{Options, PromptError, Runner, Target};
use sha2::{Digest as _, Sha256};

/// Tests that write a stand-in program and run it hold this from writing
/// it until its run is over (ETXTBSY otherwise).
static SPAWNS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A user no host has: the ownership steps skip it.
const USER: &str = "sb-ops-test-no-such-user";

/// sb's releases, as the binary has them.
const REPO: GithubRepo = GithubRepo {
    owner: "saltyorg",
    name: "sb-go",
};

/// A stand-in host and the two checkouts, each a clone of its own remote
/// at a branch; accounts.yml in Saltbox's (ignored, as Saltbox has it)
/// naming [`USER`]; a fake GitHub with nothing scripted yet.
struct World {
    host: Host,
    saltbox_remote: Remote,
    sandbox_remote: Remote,
    dir: tempfile::TempDir,
    fake: Fake,
    releases: Releases,
}

impl World {
    fn new(saltbox: &str, sandbox: &str) -> Self {
        let host = Host::new();
        let saltbox_remote = Remote::saltbox(&[], USER);
        let sandbox_remote = Remote::saltbox(&[], USER);
        let dir = tempfile::tempdir().expect("tempdir");
        let world_fake = Fake::start().expect("fake GitHub");
        let releases = world_fake.releases("sb/test").expect("release client");
        let world = Self {
            host,
            saltbox_remote,
            sandbox_remote,
            dir,
            fake: world_fake,
            releases,
        };
        world.saltbox_remote.clone_into(saltbox, &world.saltbox());
        world.sandbox_remote.clone_into(sandbox, &world.sandbox());
        std::fs::write(
            world.saltbox().join("accounts.yml"),
            format!("user:\n  name: {USER}\n"),
        )
        .expect("accounts");
        world
    }

    fn saltbox(&self) -> PathBuf {
        self.dir.path().join("saltbox")
    }

    fn sandbox(&self) -> PathBuf {
        self.dir.path().join("sandbox")
    }

    /// A checkout's branch and `branch.txt`.
    fn branch(path: &Path) -> (String, String) {
        let name = git(path, &["rev-parse", "--abbrev-ref", "HEAD"]);
        let text = std::fs::read_to_string(path.join("branch.txt")).expect("branch.txt");
        (name, text.trim().to_owned())
    }

    /// update on the world with `args`, `terminal` as term-ui's rule
    /// gives it, the questions answered by `replies` in order (none left:
    /// no terminal; each question's text recorded), self-update as
    /// `self_update` says (none: skipped): the error, the screen and the
    /// shutdown.
    async fn run(
        &self,
        args: &UpdateArgs,
        terminal: Result<(), String>,
        replies: &[bool],
        self_update: Option<SelfUpdate<'_>>,
        disk: &Disk,
    ) -> Ran {
        let apt = self.host.apt();
        let (saltbox, sandbox) = (self.saltbox(), self.sandbox());
        let paths = [self.dir.path().to_path_buf()];
        let update = Update {
            saltbox: &saltbox,
            sandbox: &sandbox,
            disk,
            disk_paths: &paths,
            apt: &apt,
            releases: &self.releases,
            self_update,
            args,
        };
        let term = indicatif::InMemoryTerm::new(100, 400);
        let shutdown = self_update.map_or_else(Shutdown::new, |update| update.shutdown.clone());
        let asked = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&asked);
        let mut replies = replies.iter().copied();
        let ask = move |shown: &str, question: &str| {
            record
                .lock()
                .expect("asked")
                .push(format!("{shown} | {question}"));
            replies.next().ok_or_else(|| PromptError::NoTerminal {
                question: question.to_owned(),
                reason: "no terminal".to_owned(),
                flag: update::BRANCH_FLAGS,
            })
        };
        let screen = term.clone();
        let runner = || {
            let options = Options {
                target: Target::InMemory {
                    term: screen.clone(),
                    colour: false,
                },
            };
            Runner::start("sb update", &shutdown, None, options)
        };
        let result = update::run(&update, terminal, ask, runner, &shutdown).await;
        let asked = asked.lock().expect("asked").clone();
        Ran {
            error: result.err().map(|err| format!("{err:#}")),
            screen: term.contents(),
            requested: shutdown.requested(),
            asked,
        }
    }
}

/// What a run left.
struct Ran {
    error: Option<String>,
    screen: String,
    requested: Option<u8>,
    asked: Vec<String>,
}

/// The flags as clap parses them from `args`.
fn args(args: &[&str]) -> UpdateArgs {
    use clap::Parser as _;
    #[derive(clap::Parser)]
    struct Cli {
        #[command(flatten)]
        args: UpdateArgs,
    }
    Cli::try_parse_from([&["sb"], args].concat())
        .expect("parses")
        .args
}

/// Every filesystem with plenty free.
const ROOMY: Stats = Stats {
    blocks: 1 << 30,
    bavail: 1 << 29,
    frsize: 4096,
    fsid: 1,
};

/// A filesystem with 1 GiB free.
const FULL: Stats = Stats {
    blocks: 1 << 22,
    bavail: (1 << 30) / 4096,
    frsize: 4096,
    fsid: 1,
};

/// host's disk checks, every filesystem with plenty free.
fn roomy() -> Disk {
    Disk::new().with_statvfs(|_| Ok(ROOMY))
}

/// host's disk checks, every filesystem with 1 GiB free.
fn full() -> Disk {
    Disk::new().with_statvfs(|_| Ok(FULL))
}

/// The labels' positions on the screen, which must rise.
fn in_order(screen: &str, labels: &[&str]) {
    let mut at = 0;
    for label in labels {
        let found = screen[at..]
            .find(label)
            .unwrap_or_else(|| panic!("{label} after {at}:\n{screen}"));
        at += found + label.len();
    }
}

#[test]
fn the_flags_are_sb_gos_and_migrations() {
    let parsed = args(&[]);
    assert_eq!(parsed.branch_flags.answer(), None);
    assert!(!parsed.skip_self_update);
    assert_eq!(parsed.migrations, None);
    let parsed = args(&[
        "--reset-branch",
        "--skip-self-update",
        "--migrations",
        "skip",
    ]);
    assert_eq!(parsed.branch_flags.answer(), Some(BranchAnswer::Reset));
    assert!(parsed.skip_self_update);
    assert_eq!(parsed.migrations, Some(Migrations::Skip));
    assert_eq!(
        args(&["--migrations", "run"]).migrations,
        Some(Migrations::Run)
    );
}

/// The up-front check: on a terminal nothing is needed; without one,
/// `--migrations` always, and a branch flag when a checkout is off
/// master, each named with why, in one error.
#[test]
fn without_a_terminal_each_missing_flag_is_named() {
    let none = Some("no terminal (os error 6)");
    let off = [("Saltbox", "develop")];
    assert_eq!(update::missing_answers(None, &off, None, None), Ok(()));
    assert_eq!(
        update::missing_answers(none, &[], None, Some(Migrations::Skip)),
        Ok(())
    );
    assert_eq!(
        update::missing_answers(none, &off, Some(BranchAnswer::Keep), Some(Migrations::Run)),
        Ok(())
    );
    // A branch flag given, `--migrations` missing: only it is named.
    let err = update::missing_answers(none, &off, Some(BranchAnswer::Reset), None)
        .expect_err("migrations only");
    assert!(err.contains("--migrations"), "{err}");
    assert!(!err.contains("--keep-branch"), "{err}");
    let err = update::missing_answers(none, &[], None, None).expect_err("migrations");
    assert!(err.contains("no terminal (os error 6)"), "{err}");
    assert!(
        err.contains("--migrations run or --migrations skip"),
        "{err}"
    );
    assert!(!err.contains("--keep-branch"), "{err}");
    let err =
        update::missing_answers(none, &off, None, Some(Migrations::Skip)).expect_err("branch");
    assert!(
        err.contains(
            "--keep-branch or --reset-branch (Saltbox is on branch 'develop', not 'master')"
        ),
        "{err}"
    );
    assert!(!err.contains("--migrations"), "{err}");
    let both = [("Saltbox", "develop"), ("Sandbox", "feature")];
    let err = update::missing_answers(none, &both, None, None).expect_err("both");
    assert!(err.contains("Saltbox is on branch 'develop'"), "{err}");
    assert!(err.contains("Sandbox is on branch 'feature'"), "{err}");
    assert!(err.contains("--migrations"), "{err}");
}

/// Without a terminal and with every flag: apt, then Saltbox, then
/// Sandbox, each reset to master and given to the Saltbox user.
#[tokio::test]
async fn piped_with_every_flag_both_checkouts_update() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("develop", "develop");
    let flags = args(&[
        "--reset-branch",
        "--migrations",
        "skip",
        "--skip-self-update",
    ]);
    let disk = roomy();
    let ran = world
        .run(&flags, Err("no terminal".to_owned()), &[], None, &disk)
        .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    in_order(
        &ran.screen,
        &[
            "Updating apt package cache",
            "Refreshing apt package lists",
            "Saltbox updated",
            "Saltbox: Currently on branch 'develop'",
            "Saltbox repository updated (master)",
            "Sandbox updated",
            "Sandbox: Currently on branch 'develop'",
            "Sandbox repository updated (master)",
        ],
    );
    assert!(ran.asked.is_empty(), "{:?}", ran.asked);
    assert_eq!(ran.requested, None);
    assert_eq!(
        World::branch(&world.saltbox()),
        ("master".into(), "master".into())
    );
    assert_eq!(
        World::branch(&world.sandbox()),
        ("master".into(), "master".into())
    );
    let programs: Vec<String> = world
        .host
        .calls()
        .iter()
        .map(|call| call.split(' ').next().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(programs, ["apt-get"], "{:?}", world.host.calls());
}

/// `--keep-branch`: each checkout updates its own branch, with repo's
/// notices; on a terminal with no flag, each question is asked, Saltbox's
/// first, after its current branch is shown.
#[tokio::test]
async fn the_branch_questions_follow_the_flags_or_are_asked() {
    let _spawns = SPAWNS.lock().await;
    let disk = roomy();
    let world = World::new("develop", "develop");
    let flags = args(&[
        "--keep-branch",
        "--migrations",
        "skip",
        "--skip-self-update",
    ]);
    let ran = world
        .run(&flags, Err("piped".to_owned()), &[], None, &disk)
        .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert!(
        ran.screen
            .contains("Saltbox: Updating the current branch 'develop'"),
        "{}",
        ran.screen
    );
    assert!(
        ran.screen.contains("Sandbox repository updated (develop)"),
        "{}",
        ran.screen
    );
    assert_eq!(
        World::branch(&world.saltbox()),
        ("develop".into(), "develop".into())
    );

    let world = World::new("develop", "master");
    let flags = args(&["--skip-self-update"]);
    let ran = world.run(&flags, Ok(()), &[true], None, &disk).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(
        ran.asked,
        [
            "Saltbox: Currently on branch 'develop' | Saltbox: Do you want to reset to the 'master' branch?"
        ]
    );
    assert_eq!(
        World::branch(&world.saltbox()),
        ("master".into(), "master".into())
    );
    // Shown with the question, the current branch isn't noted again.
    assert!(
        !ran.screen.contains("Currently on branch"),
        "{}",
        ran.screen
    );
}

/// Refused before anything: no terminal and a missing flag, too little
/// disk space, a checkout that isn't there; nothing ran, nothing changed.
#[tokio::test]
async fn refusals_come_before_any_step() {
    let _spawns = SPAWNS.lock().await;
    let roomy_disk = roomy();
    let world = World::new("develop", "master");
    let ran = world
        .run(
            &args(&["--skip-self-update"]),
            Err("no terminal".to_owned()),
            &[],
            None,
            &roomy_disk,
        )
        .await;
    let error = ran.error.expect("missing flags");
    assert!(error.contains("--keep-branch or --reset-branch"), "{error}");
    assert!(error.contains("--migrations"), "{error}");
    assert!(world.host.calls().is_empty(), "{:?}", world.host.calls());
    assert_eq!(
        World::branch(&world.saltbox()),
        ("develop".into(), "develop".into())
    );
    assert!(!ran.screen.contains("Updating"), "{}", ran.screen);

    let full_disk = full();
    let flags = args(&[
        "--reset-branch",
        "--migrations",
        "skip",
        "--skip-self-update",
    ]);
    let ran = world.run(&flags, Ok(()), &[], None, &full_disk).await;
    let error = ran.error.expect("disk");
    assert!(error.contains("INSUFFICIENT DISK SPACE"), "{error}");
    assert!(world.host.calls().is_empty(), "{:?}", world.host.calls());
}

/// A checkout that isn't there fails its own step, after apt and the
/// other checkout, as sb-go's `requireDirectory` does; it isn't asked
/// about up front.
#[tokio::test]
async fn a_missing_checkout_fails_its_own_step() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("develop", "develop");
    std::fs::remove_dir_all(world.sandbox()).expect("remove sandbox");
    let flags = args(&[
        "--reset-branch",
        "--migrations",
        "skip",
        "--skip-self-update",
    ]);
    let ran = world.run(&flags, Ok(()), &[], None, &roomy()).await;
    assert!(ran.error.is_some(), "{}", ran.screen);
    in_order(
        &ran.screen,
        &[
            "Updating apt package cache",
            "Saltbox updated",
            "Sandbox update",
            "error updating Sandbox",
            "inspect required directory",
        ],
    );
    assert_eq!(
        World::branch(&world.saltbox()),
        ("master".into(), "master".into())
    );
}

/// On a terminal with both checkouts off master: Saltbox's question, then
/// Sandbox's, each after its current branch, before any step; a no keeps
/// the branch, noted under its step; a question that can't be answered
/// (Ctrl-C, no terminal) ends the run before any step, nothing changed.
#[tokio::test]
async fn both_questions_are_asked_in_order_before_any_step() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("develop", "develop");
    let flags = args(&["--skip-self-update"]);
    let ran = world
        .run(&flags, Ok(()), &[true, false], None, &roomy())
        .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(
        ran.asked,
        [
            "Saltbox: Currently on branch 'develop' | Saltbox: Do you want to reset to the 'master' branch?",
            "Sandbox: Currently on branch 'develop' | Sandbox: Do you want to reset to the 'master' branch?",
        ]
    );
    assert_eq!(
        World::branch(&world.saltbox()),
        ("master".into(), "master".into())
    );
    assert_eq!(
        World::branch(&world.sandbox()),
        ("develop".into(), "develop".into())
    );
    assert!(
        ran.screen
            .contains("Sandbox: Updating the current branch 'develop'"),
        "{}",
        ran.screen
    );

    let world = World::new("develop", "develop");
    let ran = world.run(&flags, Ok(()), &[true], None, &roomy()).await;
    let error = ran.error.expect("Sandbox's question unanswered");
    assert!(error.starts_with("error updating Sandbox"), "{error}");
    assert_eq!(ran.asked.len(), 2, "{:?}", ran.asked);
    assert!(world.host.calls().is_empty(), "{:?}", world.host.calls());
    assert_eq!(
        World::branch(&world.saltbox()),
        ("develop".into(), "develop".into())
    );
    assert!(!ran.screen.contains("Updating"), "{}", ran.screen);
}

/// A failing apt ends the run with its words, before either checkout.
#[tokio::test]
async fn a_failing_apt_stops_before_the_checkouts() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("develop", "develop");
    world.host.fail("apt-get");
    let flags = args(&[
        "--reset-branch",
        "--migrations",
        "skip",
        "--skip-self-update",
    ]);
    let ran = world.run(&flags, Ok(()), &[], None, &roomy()).await;
    assert!(ran.error.is_some());
    assert!(
        ran.screen.contains("error updating apt cache"),
        "{}",
        ran.screen
    );
    assert!(!ran.screen.contains("Updating Saltbox"), "{}", ran.screen);
    assert_eq!(
        World::branch(&world.saltbox()),
        ("develop".into(), "develop".into())
    );
}

/// Self-update inside update: a replaced sb ends the run with exit 0 (a
/// requested 0) and release's "Re-run" warning, before apt or either
/// checkout; a build with self-update off warns and goes on.
#[tokio::test]
async fn self_update_replaced_ends_the_run_with_exit_0() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("master", "master");
    let body = stand_in("99.0.0");
    let asset = sb_release::asset_name().expect("an architecture with an asset");
    let path = format!("/saltyorg/sb-go/releases/download/v99.0.0/{asset}");
    world.fake.script(
        "/version",
        Answer::json(format!(
            r#"[{{"tag_name":"v99.0.0","draft":false,"prerelease":false,"assets":[{{"name":"{asset}","size":{},"digest":"sha256:{}","browser_download_url":"{}{path}"}}]}}]"#,
            body.len(),
            hex::encode(Sha256::digest(&body)),
            world.fake.url()
        )),
    );
    world.fake.script(&path, Answer::new(200, body.clone()));
    let target = world.dir.path().join("sb");
    std::fs::write(&target, b"old sb").expect("target");
    let released = BuildInfo::from_env("0.1.0", Some("1.0.0"), Some("abc1234"), None);
    let shutdown = Shutdown::new();
    let replace = SelfUpdate {
        repo: REPO,
        build: &released,
        target: &target,
        yes: true,
        force: false,
        message: Some(update::RERUN),
        shutdown: &shutdown,
    };
    let flags = args(&["--reset-branch", "--migrations", "skip"]);
    let ran = world
        .run(
            &flags,
            Err("piped".to_owned()),
            &[],
            Some(replace),
            &roomy(),
        )
        .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(ran.requested, Some(0));
    assert!(
        ran.screen
            .contains("Re-run the update command to update Saltbox"),
        "{}",
        ran.screen
    );
    assert_eq!(std::fs::read(&target).expect("target"), body);
    assert!(world.host.calls().is_empty(), "{:?}", world.host.calls());
    assert!(!ran.screen.contains("Updating Saltbox"), "{}", ran.screen);

    let world = World::new("master", "master");
    let raw = BuildInfo::from_env("0.1.0", None, None, None);
    let shutdown = Shutdown::new();
    let off = SelfUpdate {
        build: &raw,
        shutdown: &shutdown,
        ..replace
    };
    let ran = world
        .run(&flags, Err("piped".to_owned()), &[], Some(off), &roomy())
        .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(ran.requested, None);
    in_order(
        &ran.screen,
        &[
            "Self-update is disabled in this build",
            "Saltbox updated",
            "Sandbox updated",
        ],
    );
}
