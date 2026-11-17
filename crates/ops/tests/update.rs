//! `sb update` in process (ops spec, "`sb update`"; module build M84 and
//! M85): sb-ops' `update::run` on the harness's Saltbox host (stand-in
//! apt and `ansible-playbook`, python-env's stand-in tree, Saltbox and
//! Sandbox checkouts of stand-in remotes with real git, `file://` only,
//! sb-config's fixtures and its fake for the credential checks), release's
//! fake GitHub for self-update, and runners drawing to memory, as any user
//! (the fixtures' Saltbox user is one no host has, so ownership is
//! skipped). The binary's run as root is crates/sb/tests/update.rs.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sb_ansible::Ansible;
use sb_ansible::TagCache;
use sb_cli_core::build_info::BuildInfo;
use sb_cli_core::cancel::Shutdown;
use sb_config::run::Settings;
use sb_config::{http, validators};
use sb_host::disk::{Disk, Stats};
use sb_ops::Checkout;
use sb_ops::testing::{Host, Saltbox};
use sb_ops::update::{self, Migrations, Update, UpdateArgs};
use sb_release::testing::{Answer, Fake, stand_in};
use sb_release::{GithubRepo, Releases, SelfUpdate};
use sb_repo::Answer as BranchAnswer;
use sb_term_ui::{Options, PromptError, Runner, Target};
use sha2::{Digest as _, Sha256};

/// Tests that write a stand-in program and run it hold this from writing
/// it until its run is over (ETXTBSY otherwise).
static SPAWNS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// sb's releases, as the binary has them.
const REPO: GithubRepo = GithubRepo {
    owner: "saltyorg",
    name: "sb-go",
};

/// The harness's Saltbox host, each checkout on a branch, and a fake
/// GitHub for self-update with nothing scripted yet.
struct World {
    sb: Saltbox,
    host: Host,
    fake: Fake,
    releases: Releases,
    backups: tempfile::TempDir,
}

impl World {
    fn new(saltbox: &str, sandbox: &str) -> Self {
        let mut sb = Saltbox::new(saltbox, sandbox);
        // The harness's host is the world's: it holds the calls.
        let host = std::mem::replace(&mut sb.host, Host::new());
        let fake = Fake::start().expect("fake GitHub");
        let releases = fake.releases("sb/test").expect("release client");
        Self {
            sb,
            host,
            fake,
            releases,
            backups: tempfile::tempdir().expect("backups"),
        }
    }

    fn saltbox(&self) -> PathBuf {
        self.sb.root().to_path_buf()
    }

    fn sandbox(&self) -> PathBuf {
        self.sb.sandbox()
    }

    /// A checkout's branch and `branch.txt`.
    fn branch(path: &Path) -> (String, String) {
        Saltbox::branch(path)
    }

    /// validate-config's settings on the world: its root, the fake's URLs.
    fn settings(&self) -> Settings {
        let (cloudflare_url, dockerhub_url) = self.sb.check_urls();
        Settings {
            root: self.saltbox(),
            zoneinfo: validators::Saltbox::zoneinfo_places(None),
            http: http::Http::new(&http::Proxy::Off, Duration::from_secs(10)).expect("http"),
            cloudflare_url,
            dockerhub_url,
            backoff: |_| Duration::ZERO,
        }
    }

    /// The programs the stand-ins saw, in order.
    fn programs(&self) -> Vec<String> {
        self.host
            .calls()
            .iter()
            .map(|call| call.split(' ').next().unwrap_or_default().to_owned())
            .collect()
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
        let root = self.saltbox();
        let paths = [self.sb.tree.root.path().to_path_buf()];
        let python = self.sb.tree.env();
        let ansible = Ansible::with_program(self.host.stand_ins().join("ansible-playbook"));
        let settings = self.settings();
        let update = Update {
            root: &root,
            disk,
            disk_paths: &paths,
            apt: &apt,
            releases: &self.releases,
            self_update,
            args,
            python: &python,
            ansible: &ansible,
            settings: &settings,
            backups: self.backups.path(),
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

/// The flags every piped run gives but `--skip-self-update`'s choice.
const PIPED: [&str; 4] = [
    "--reset-branch",
    "--migrations",
    "skip",
    "--skip-self-update",
];

/// Without a terminal and with every flag, sb-go's order: apt; Saltbox
/// reset to master, its runtime (deadsnakes, venv, fact) and its tags
/// cache rebuilt; the config permissions; Sandbox reset and its tags; the
/// announcements; validate-config's checks, each file validated.
#[tokio::test]
async fn piped_with_every_flag_every_step_runs_in_order() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("develop", "develop");
    let ran = world
        .run(
            &args(&PIPED),
            Err("no terminal".to_owned()),
            &[],
            None,
            &roomy(),
        )
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
            "Checking for old deadsnakes Python packages",
            "Ansible virtual environment ready",
            "saltbox.fact is ready",
            "Updating Saltbox tags cache",
            "Saltbox configuration permissions secured",
            "Sandbox updated",
            "Sandbox: Currently on branch 'develop'",
            "Sandbox repository updated (master)",
            "Updating Sandbox tags cache",
            "Checked for new announcements",
            "Saltbox configuration validated",
            "Validated accounts.yml",
            "Validated settings.yml",
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
    assert_eq!(
        world.programs(),
        ["apt-get", "ansible-playbook", "ansible-playbook"],
        "{:?}",
        world.host.calls()
    );
    // Both entries cached, at each checkout's new HEAD.
    let cache = TagCache::load(&world.saltbox());
    for (checkout, path) in [
        (Checkout::Saltbox, world.saltbox()),
        (Checkout::Sandbox, world.sandbox()),
    ] {
        let head = sb_ops::testing::git(&path, &["rev-parse", "HEAD"]);
        let entry = cache
            .entry(&checkout.playbooks(&world.saltbox()))
            .expect("cached");
        assert_eq!(entry.commit, head, "{checkout:?}");
        assert!(entry.tags.contains(&"plex".to_owned()), "{checkout:?}");
    }
    // The config files are 0600 (secure_existing_configs).
    let mode = std::fs::metadata(world.saltbox().join("accounts.yml"))
        .expect("accounts")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

/// Nothing moved and both caches hold tags: no listing runs (sb-go's
/// rule, the commit unchanged and the entry there).
#[tokio::test]
async fn the_tags_are_kept_when_nothing_moved() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("master", "master");
    let flags = args(&PIPED);
    let ran = world
        .run(&flags, Err("piped".to_owned()), &[], None, &roomy())
        .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    let listed = |world: &World| {
        world
            .programs()
            .iter()
            .filter(|program| *program == "ansible-playbook")
            .count()
    };
    assert_eq!(listed(&world), 2);
    let ran = world
        .run(&flags, Err("piped".to_owned()), &[], None, &roomy())
        .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(listed(&world), 2, "{:?}", world.host.calls());
    assert!(!ran.screen.contains("tags cache"), "{}", ran.screen);
}

/// An announcement the pull brings is new (ops spec, "Announcements"); an
/// announcements file that can't be loaded is a warning naming it, that
/// checkout's announcements skipped, the update completing (decision 23).
#[tokio::test]
async fn new_announcements_are_found_and_a_broken_file_only_warns() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("master", "master");
    let entry = "- date: '2026-11-01'\n  title: Old\n  message: Seen before\n";
    for remote in [&world.sb.saltbox_remote, &world.sb.sandbox_remote] {
        let _ = sb_ops::testing::git(&remote.work(), &["checkout", "--quiet", "master"]);
        remote.commit("announcements.yml", &format!("announcements:\n{entry}"));
    }
    let flags = args(&PIPED);
    let ran = world
        .run(&flags, Err("piped".to_owned()), &[], None, &roomy())
        .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    // No file before the pull: the first one's entries are all new.
    assert!(
        ran.screen.contains("Displaying new announcements"),
        "{}",
        ran.screen
    );

    // Nothing new upstream: nothing displayed.
    let ran = world
        .run(&flags, Err("piped".to_owned()), &[], None, &roomy())
        .await;
    assert!(
        !ran.screen.contains("Displaying new announcements"),
        "{}",
        ran.screen
    );

    // A broken Sandbox file upstream: a warning naming it, Saltbox's new one
    // still found.
    world
        .sb
        .sandbox_remote
        .commit("announcements.yml", "announcements: [unclosed\n");
    world.sb.saltbox_remote.commit(
        "announcements.yml",
        &format!("announcements:\n{entry}- date: '2026-11-17'\n  title: New\n  message: Fresh\n"),
    );
    let ran = world
        .run(&flags, Err("piped".to_owned()), &[], None, &roomy())
        .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    let path = world.sandbox().join("announcements.yml");
    assert!(
        ran.screen.contains(&format!(
            "Sandbox announcements skipped: error loading after announcement file '{}'",
            path.display()
        )),
        "{}",
        ran.screen
    );
    assert!(
        ran.screen.contains("Displaying new announcements"),
        "{}",
        ran.screen
    );
    assert!(
        ran.screen.contains("Saltbox configuration validated"),
        "{}",
        ran.screen
    );

    // The broken file is now Sandbox's before: the same warning, for the
    // file before the pull.
    let ran = world
        .run(&flags, Err("piped".to_owned()), &[], None, &roomy())
        .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert!(
        ran.screen.contains(&format!(
            "Sandbox announcements skipped: error loading before announcement file '{}'",
            path.display()
        )),
        "{}",
        ran.screen
    );
}

/// Validation's issues show exactly as validate-config shows them: the
/// same lines as sb-config's own run on the same root.
#[tokio::test]
async fn validation_shows_issues_as_validate_config_does() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("master", "master");
    std::fs::write(
        world.saltbox().join("hetzner_vlan.yml"),
        "hetzner:\n  vlan_id: not-a-number\n  unknown: 1\n",
    )
    .expect("break hetzner_vlan.yml");
    let ran = world
        .run(&args(&PIPED), Err("piped".to_owned()), &[], None, &roomy())
        .await;
    assert!(ran.error.is_some(), "{}", ran.screen);
    let validation = |screen: &str| -> Vec<String> {
        let start = screen
            .find("Validating Saltbox configuration")
            .expect("the validation step");
        screen[start..]
            .lines()
            .map(|line| line.trim_end().to_owned())
            .filter(|line| !line.is_empty())
            .collect()
    };
    // validate-config's own run on the same root.
    let term = indicatif::InMemoryTerm::new(100, 400);
    let options = Options {
        target: Target::InMemory {
            term: term.clone(),
            colour: false,
        },
    };
    let shutdown = Shutdown::new();
    let mut runner = Runner::start("sb validate-config", &shutdown, None, options);
    let _ = sb_config::run::run(&mut runner, &world.settings(), world.backups.path()).await;
    runner.finish();
    // Both runners draw to memory, whose clock term-ui freezes: their
    // timings are the same.
    let ours = validation(&ran.screen);
    let theirs = validation(&term.contents());
    assert!(
        ours.iter().any(|line| line.contains("hetzner_vlan.yml")),
        "{ours:#?}"
    );
    assert_eq!(ours, theirs);
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
    let target = world.backups.path().join("sb");
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
