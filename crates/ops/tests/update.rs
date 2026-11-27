//! `sb update` in process (ops spec, "`sb update`"; module build M84 and
//! M85): sb-ops' `update::run` on the harness's Saltbox host (stand-in
//! apt, which update never runs, and `ansible-playbook`, python-env's
//! stand-in tree, Saltbox and Sandbox checkouts of stand-in remotes with real git, `file://` only,
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
use sb_cli_core::build_info::{BuildInfo, Channel};
use sb_cli_core::cancel::Shutdown;
use sb_completion::install::Paths;
use sb_config::http;
use sb_config::run::Settings;
use sb_host::disk::{Disk, Stats};
use sb_ops::Checkout;
use sb_ops::completions::Refresh;
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
    name: "sb-rust",
};

/// The harness's Saltbox host, each checkout on a branch, and a fake
/// GitHub for self-update with nothing scripted yet.
struct World {
    sb: Saltbox,
    host: Host,
    fake: Fake,
    releases: Releases,
    backups: tempfile::TempDir,
    /// The run is interrupted (a SIGTERM recorded) as the migrations
    /// question is answered.
    interrupt_at_confirm: std::cell::Cell<bool>,
    /// The completion refresh's root (never the host's `/usr` or `/etc`)
    /// and sb's stand-in executable in it.
    completion: tempfile::TempDir,
    /// The refresh is given an executable that isn't there, so it fails.
    broken_completion: std::cell::Cell<bool>,
    /// The physical cores the tag listings see (none: the host's).
    cores: std::cell::Cell<Option<usize>>,
    /// The run is interrupted (Ctrl-C) once every one of these files is
    /// there, or after 30 s.
    ctrl_c_once: std::cell::RefCell<Vec<PathBuf>>,
    /// Each question asked appends `ask <question>` to this file.
    events: std::cell::RefCell<Option<PathBuf>>,
    /// How long the Ctrl-C waits once its files are there.
    ctrl_c_delay: std::cell::Cell<Duration>,
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
            interrupt_at_confirm: std::cell::Cell::new(false),
            completion: completion_root(),
            broken_completion: std::cell::Cell::new(false),
            cores: std::cell::Cell::new(None),
            ctrl_c_once: std::cell::RefCell::new(Vec::new()),
            events: std::cell::RefCell::new(None),
            ctrl_c_delay: std::cell::Cell::new(Duration::ZERO),
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
            // No zone files, none of the host's: the world's timezone is
            // `auto`, which reads none.
            zoneinfo: Vec::new(),
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

    /// sb's stand-in executable for the completion refresh, or one that
    /// isn't there when [`World::broken_completion`] is set.
    fn executable(&self) -> PathBuf {
        self.completion
            .path()
            .join(if self.broken_completion.get() {
                "missing"
            } else {
                "opt/sb/sb"
            })
    }

    /// The stand-in `ansible-playbook`, on [`World::cores`] physical cores.
    fn ansible(&self) -> Ansible {
        let ansible = Ansible::with_program(self.host.stand_ins().join("ansible-playbook"));
        match self.cores.get() {
            Some(cores) => {
                ansible.with_listings_at_once(std::num::NonZeroUsize::new(cores).expect("cores"))
            }
            None => ansible,
        }
    }

    /// The Ctrl-C [`World::ctrl_c_once`] asks for, on `shutdown`: pressed
    /// once its files are there (30 s at most), by a task of its own.
    fn ctrl_c(&self, shutdown: &Shutdown) {
        let waited = self.ctrl_c_once.take();
        let delay = self.ctrl_c_delay.get();
        if !waited.is_empty() {
            let shutdown = shutdown.clone();
            drop(tokio::spawn(async move {
                let deadline = std::time::Instant::now() + Duration::from_secs(30);
                while !waited.iter().all(|file| file.exists())
                    && std::time::Instant::now() < deadline
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                tokio::time::sleep(delay).await;
                shutdown.signalled(2);
            }));
        }
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
        self.run_migrating(args, terminal, replies, self_update, disk, None)
            .await
    }

    /// [`World::run`], the migrations question answered `migrate` (none:
    /// as with no terminal).
    async fn run_migrating(
        &self,
        args: &UpdateArgs,
        terminal: Result<(), String>,
        replies: &[bool],
        self_update: Option<SelfUpdate<'_>>,
        disk: &Disk,
        migrate: Option<bool>,
    ) -> Ran {
        let root = self.saltbox();
        let paths = [self.sb.tree.root.path().to_path_buf()];
        let python = self.sb.tree.env();
        let ansible = self.ansible();
        let settings = self.settings();
        let completion_paths = Paths::under(self.completion.path());
        let executable = Ok(self.executable());
        let update = Update {
            root: &root,
            disk,
            disk_paths: &paths,
            releases: &self.releases,
            self_update,
            args,
            python: &python,
            ansible: &ansible,
            git_at_once: self.cores.get().and_then(std::num::NonZeroUsize::new),
            settings: &settings,
            backups: self.backups.path(),
            // Piped, as the harness's runs are.
            stdout_terminal: false,
            completions: Refresh {
                paths: &completion_paths,
                executable: &executable,
            },
        };
        let term = indicatif::InMemoryTerm::new(100, 400);
        let shutdown = self_update.map_or_else(Shutdown::new, |update| update.shutdown.clone());
        self.ctrl_c(&shutdown);
        let asked = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&asked);
        let mut replies = replies.iter().copied();
        let events = self.events.borrow().clone();
        let ask = move |shown: &str, question: &str, flag: &'static str| {
            record
                .lock()
                .expect("asked")
                .push(format!("{shown} | {question}"));
            if let Some(events) = &events {
                append(events, &format!("ask {question}"));
            }
            replies.next().ok_or_else(|| PromptError::NoTerminal {
                question: question.to_owned(),
                reason: "no terminal".to_owned(),
                flag,
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
        let confirmed = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&confirmed);
        let interrupt = self.interrupt_at_confirm.get().then(|| shutdown.clone());
        let confirm = move |question: &str, _table: &str| {
            record.lock().expect("confirmed").push(question.to_owned());
            if let Some(shutdown) = &interrupt {
                shutdown.signalled(15);
            }
            migrate.ok_or_else(|| PromptError::NoTerminal {
                question: question.to_owned(),
                reason: "no terminal".to_owned(),
                flag: "--migrations",
            })
        };
        let mut lines = Vec::new();
        let result = update::run(
            &update, terminal, ask, confirm, runner, &shutdown, &mut lines,
        )
        .await;
        let asked = asked.lock().expect("asked").clone();
        let confirmed = confirmed.lock().expect("confirmed").clone();
        Ran {
            error: result.as_ref().err().map(|err| format!("{err:#}")),
            block: result.err().map(|err| {
                sb_cli_core::exit::error_block(
                    err.as_ref(),
                    Some("POINTER"),
                    &sb_cli_core::theme::Theme::plain(),
                )
            }),
            screen: term.contents(),
            requested: shutdown.requested(),
            asked,
            confirmed,
            lines: String::from_utf8_lossy(&lines).into_owned(),
        }
    }
}

/// What a run left: the migrations question asked (`confirmed`) and the
/// lines the migrations wrote besides.
struct Ran {
    error: Option<String>,
    /// cli-core's error block for the error, with `POINTER` as the log's.
    block: Option<String>,
    screen: String,
    requested: Option<u8>,
    asked: Vec<String>,
    confirmed: Vec<String>,
    lines: String,
}

/// A completion root holding sb's stand-in executable and nothing else
/// (no shell, so the refresh checks each and writes nothing).
fn completion_root() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("completion root");
    let sb = dir.path().join("opt/sb/sb");
    std::fs::create_dir_all(sb.parent().expect("parent")).expect("dirs");
    sb_cli_core::testing::write_program(&sb, "#!/bin/sh\n");
    dir
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
    assert_eq!(update::missing_answers(None, &off, None, None, &[]), Ok(()));
    assert_eq!(
        update::missing_answers(none, &[], None, Some(Migrations::Skip), &[]),
        Ok(())
    );
    assert_eq!(
        update::missing_answers(
            none,
            &off,
            Some(BranchAnswer::Keep),
            Some(Migrations::Run),
            &[]
        ),
        Ok(())
    );
    // A branch flag given, `--migrations` missing: only it is named.
    let err = update::missing_answers(none, &off, Some(BranchAnswer::Reset), None, &[])
        .expect_err("migrations only");
    assert!(err.contains("--migrations"), "{err}");
    assert!(!err.contains("--keep-branch"), "{err}");
    let err = update::missing_answers(none, &[], None, None, &[]).expect_err("migrations");
    assert!(err.contains("no terminal (os error 6)"), "{err}");
    assert!(
        err.contains("--migrations run or --migrations skip"),
        "{err}"
    );
    assert!(!err.contains("--keep-branch"), "{err}");
    let err =
        update::missing_answers(none, &off, None, Some(Migrations::Skip), &[]).expect_err("branch");
    assert!(
        err.contains(
            "--keep-branch or --reset-branch (Saltbox is on branch 'develop', not 'master')"
        ),
        "{err}"
    );
    assert!(!err.contains("--migrations"), "{err}");
    let both = [("Saltbox", "develop"), ("Sandbox", "feature")];
    let err = update::missing_answers(none, &both, None, None, &[]).expect_err("both");
    assert!(err.contains("Saltbox is on branch 'develop'"), "{err}");
    assert!(err.contains("Sandbox is on branch 'feature'"), "{err}");
    assert!(err.contains("--migrations"), "{err}");
    // Local changes (decision 40): the checkout and its counts, and both
    // flags that answer them, after the others.
    let dirty = [("Saltbox", "1 tracked change".to_owned())];
    let err = update::missing_answers(none, &[], None, Some(Migrations::Skip), &dirty)
        .expect_err("dirty");
    assert_eq!(
        err,
        "can't ask sb update's questions: no terminal (os error 6); answer them with \
         --discard-local or --skip-git (Saltbox has 1 tracked change that the update would \
         discard)"
    );
    assert_eq!(
        update::missing_answers(None, &[], None, None, &dirty),
        Ok(())
    );
}

/// The flags every piped run gives but `--skip-self-update`'s choice.
const PIPED: [&str; 4] = [
    "--reset-branch",
    "--migrations",
    "skip",
    "--skip-self-update",
];

/// Without a terminal and with every flag, sb-go's order but the tags
/// and apt (no refresh of its own, decision 49): Saltbox reset to
/// master and its runtime (deadsnakes, venv, fact); the config
/// permissions; Sandbox reset; then both tags caches rebuilt, top-level
/// steps after Sandbox's (decision 46); the announcements;
/// validate-config's checks, each file validated.
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
            "├── ✓ Saltbox and Sandbox updated",
            "Saltbox: Currently on branch 'develop'",
            "Sandbox: Currently on branch 'develop'",
            "│   ├── ✓ Saltbox repository updated (master)",
            "│   ├── ✓ Sandbox repository updated (master)",
            "Checking for old deadsnakes Python packages",
            "Ansible virtual environment ready",
            "saltbox.fact is ready",
            "├── ✓ Saltbox configuration permissions secured",
            "├── ✓ Updating Saltbox tags cache",
            "├── ✓ Updating Sandbox tags cache",
            "Checked for new announcements",
            "Saltbox configuration validated",
            "Validated accounts.yml",
            "Validated settings.yml",
            // The completion refresh last (ops spec, step 12; M87).
            "Shell completions checked",
            "Shell completions refreshed",
            "Bash completion checked",
            "Zsh completion checked",
            "Fish completion checked",
        ],
    );
    assert!(!ran.screen.contains("refresh failed"), "{}", ran.screen);
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
        ["ansible-playbook", "ansible-playbook"],
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

/// A run refreshes no apt lists (ops decision 49, the maintainer
/// 2026-11-23): no "Updating apt package cache" step, and neither the
/// host's apt-get nor the runtime's runs; the runtime installs nothing
/// with apt (libpq-dev dropped, 2026-11-21).
#[tokio::test]
async fn a_run_refreshes_no_apt_lists() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("master", "master");
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
    for absent in [
        "native Python build prerequisites",
        "Updating apt package cache",
        "Refreshing apt package lists",
    ] {
        assert!(!ran.screen.contains(absent), "{absent}:\n{}", ran.screen);
    }
    let host_apt: Vec<String> = world
        .host
        .calls()
        .into_iter()
        .filter(|call| call.starts_with("apt-get"))
        .collect();
    assert_eq!(host_apt, Vec::<String>::new());
    let tree_apt: Vec<String> = world
        .sb
        .tree
        .calls_made()
        .into_iter()
        .filter(|call| call.contains("apt-get"))
        .collect();
    assert_eq!(tree_apt, Vec::<String>::new());
}

/// On 22.04 with a deadsnakes source, the runtime's cleanup still moves
/// it aside and refreshes the lists, the one `apt-get update` an update
/// runs (ops decision 49; python-env, "Deadsnakes cleanup").
#[tokio::test]
async fn the_deadsnakes_cleanup_still_refreshes_the_lists() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("master", "master");
    world.sb.tree.release("22.04");
    let ppa = "deb https://ppa.launchpadcontent.net/deadsnakes/ppa/ubuntu jammy main\n";
    std::fs::write(world.sb.tree.sources_d().join("ppa.list"), ppa).expect("write");
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
    assert!(
        ran.screen
            .contains("Moved 1 deadsnakes repository files to "),
        "{}",
        ran.screen
    );
    assert!(
        !ran.screen.contains("Updating apt package cache"),
        "{}",
        ran.screen
    );
    let updates: Vec<String> = world
        .sb
        .tree
        .calls_made()
        .into_iter()
        .filter(|call| call.contains("/stand-ins/apt-get ") && call.ends_with(" update"))
        .collect();
    assert_eq!(updates.len(), 1, "{updates:?}");
    assert!(
        !world
            .host
            .calls()
            .iter()
            .any(|call| call.starts_with("apt-get")),
        "{:?}",
        world.host.calls()
    );
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
        // The spaces before each time collapsed: update's time column is
        // its whole tree's (term-ui decision 14), validate-config's its own.
        screen[start..]
            .lines()
            .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
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
    // A failed validation ends the run before the completion refresh, as
    // sb-go's update returns before it.
    assert!(!ran.screen.contains("Shell completion"), "{}", ran.screen);
}

/// A completion refresh that fails is a warning, never update's failure
/// (ops spec, step 12; sb-go's `runner.Warning`).
#[tokio::test]
async fn a_failed_completion_refresh_only_warns() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("master", "master");
    world.broken_completion.set(true);
    let ran = world
        .run(&args(&PIPED), Err("piped".to_owned()), &[], None, &roomy())
        .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    in_order(
        &ran.screen,
        &[
            "Saltbox configuration validated",
            "Shell completions checked",
            "! Shell completion refresh failed: resolve completion executable: ",
        ],
    );
}

/// With a shell there, the refresh writes its file under the completion
/// root (the test's, never the host's).
#[tokio::test]
async fn the_completion_refresh_writes_each_installed_shells_file() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("master", "master");
    let root = world.completion.path();
    for (path, text) in [
        ("usr/bin/bash", "#!/bin/sh\nexit 0\n"),
        ("usr/share/bash-completion/bash_completion", ""),
    ] {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        sb_cli_core::testing::write_program(&path, text);
    }
    let ran = world
        .run(&args(&PIPED), Err("piped".to_owned()), &[], None, &roomy())
        .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert!(
        root.join("usr/local/share/bash-completion/completions/sb.bash")
            .is_file(),
        "{}",
        ran.screen
    );
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

/// A checkout that isn't there fails its own step, after the other
/// checkout, as sb-go's `requireDirectory` does; it isn't asked
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
            "✗ Saltbox and Sandbox update",
            "error updating Sandbox",
            "inspect required directory",
            "Saltbox repository updated (master)",
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
    // Each answer is a notice under its step (ops decision 39).
    in_order(
        &ran.screen,
        &[
            "Saltbox and Sandbox updated",
            "› Saltbox: Reset from branch 'develop' to 'master' (your answer)",
            "› Sandbox: Kept branch 'develop' (your answer)",
        ],
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

/// A failing `apt-get update` no longer fails the update (ops decision
/// 48): neither update nor its runtime runs one where no deadsnakes
/// source is left, so a broken apt source stops nothing. The binary's
/// run with every apt-get on `PATH` failing is crates/sb/tests/update.rs.
#[tokio::test]
async fn a_failing_apt_get_does_not_stop_the_update() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("develop", "develop");
    world.sb.tree.fail("apt-get-update");
    let flags = args(&[
        "--reset-branch",
        "--migrations",
        "skip",
        "--skip-self-update",
    ]);
    let ran = world.run(&flags, Ok(()), &[], None, &roomy()).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert!(
        !ran.screen.contains("error updating apt cache"),
        "{}",
        ran.screen
    );
    assert!(
        ran.screen.contains("Saltbox and Sandbox updated"),
        "{}",
        ran.screen
    );
    assert_eq!(
        World::branch(&world.saltbox()),
        ("master".into(), "master".into())
    );
}

/// Self-update inside update: a replaced sb ends the run with exit 0 (a
/// requested 0) and release's "Re-run" warning, before either
/// checkout; a build with self-update off warns and goes on.
#[tokio::test]
async fn self_update_replaced_ends_the_run_with_exit_0() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("master", "master");
    let body = stand_in("99.0.0");
    let asset = sb_release::asset_name(Channel::Release).expect("an architecture with an asset");
    let path = format!("/saltyorg/sb-rust/releases/download/v99.0.0/{asset}");
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
    let released = BuildInfo::from_env("0.1.0", Some("1.0.0"), Some("abc1234"), Channel::Release);
    let shutdown = Shutdown::new();
    let replace = SelfUpdate {
        repo: REPO,
        build: &released,
        target: &target,
        yes: true,
        force: false,
        switch_to: None,
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
    assert!(
        !ran.screen.contains("Saltbox and Sandbox"),
        "{}",
        ran.screen
    );

    let world = World::new("master", "master");
    let raw = BuildInfo::from_env("0.1.0", None, None, Channel::Release);
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
            "Saltbox and Sandbox updated",
            "Saltbox repository updated",
            "Sandbox repository updated",
        ],
    );
}

/// Two announcements upstream asking for migrations, Sandbox's the older;
/// the stand-in ansible-playbook lists tags as before, and for a run by
/// tag records its working directory in `cwd`, failing when `fail` is
/// there.
fn migrations_world() -> (World, PathBuf, PathBuf) {
    let world = World::new("master", "master");
    for (remote, tag, date) in [
        (&world.sb.saltbox_remote, "migrate-a", "2026-11-02"),
        (&world.sb.sandbox_remote, "migrate-b", "2026-11-01"),
    ] {
        let _ = sb_ops::testing::git(&remote.work(), &["checkout", "--quiet", "master"]);
        remote.commit(
            "announcements.yml",
            &format!(
                "announcements:\n- date: '{date}'\n  title: Moved\n  message: Run it\n  migration:\n    required: true\n    tag: {tag}\n"
            ),
        );
    }
    let cwd = world.sb.tree.root.path().join("migration-cwd");
    let fail = world.sb.tree.root.path().join("migration-fail");
    let _ = world.host.stand_in(
        "ansible-playbook",
        &format!(
            "case \"$*\" in\n*--list-tags*) cat <<'LIST'\n{}LIST\n;;\n*) pwd >> '{}'\n[ -e '{}' ] && exit 2\nexit 0 ;;\nesac",
            sb_ops::testing::LIST_TAGS,
            cwd.display(),
            fail.display()
        ),
    );
    (world, cwd, fail)
}

/// The runs by tag the stand-in saw.
fn tag_runs(world: &World) -> Vec<String> {
    world
        .host
        .calls()
        .into_iter()
        .filter(|call| call.contains("--tags"))
        .collect()
}

/// `--migrations run`: every migration, by date (Sandbox's first), each
/// its tag in its own checkout, between sb-go's lines; validation after.
#[tokio::test]
async fn migrations_run_by_date_each_in_its_checkout() {
    let _spawns = SPAWNS.lock().await;
    let (world, cwd, _) = migrations_world();
    let flags = args(&[
        "--reset-branch",
        "--migrations",
        "run",
        "--skip-self-update",
    ]);
    let ran = world
        .run(&flags, Err("piped".to_owned()), &[], None, &roomy())
        .await;
    assert_eq!(ran.error, None, "{}{}", ran.screen, ran.lines);
    assert_eq!(
        tag_runs(&world),
        [
            "ansible-playbook sandbox.yml --become --tags migrate-b",
            "ansible-playbook saltbox.yml --become --tags migrate-a"
        ]
    );
    let dirs = std::fs::read_to_string(&cwd).expect("cwd");
    assert_eq!(
        dirs.lines().collect::<Vec<_>>(),
        [
            world.sandbox().display().to_string(),
            world.saltbox().display().to_string()
        ]
    );
    in_order(
        &ran.lines,
        &[
            "Starting migration execution",
            "Running migration 'migrate-b' for Sandbox repository",
            "Running migration 'migrate-a' for Saltbox repository",
            "All migrations completed successfully",
        ],
    );
    assert!(ran.confirmed.is_empty(), "{:?}", ran.confirmed);
    assert!(
        ran.screen.contains("Saltbox configuration validated"),
        "{}",
        ran.screen
    );
}

/// `--migrations skip`, or no on the terminal: none run, each named in a
/// warning (decision 22); yes on the terminal runs them; no migrations,
/// no question.
#[tokio::test]
async fn migrations_skipped_are_named_and_the_question_decides() {
    let _spawns = SPAWNS.lock().await;
    let (world, _, _) = migrations_world();
    let flags = args(&[
        "--reset-branch",
        "--migrations",
        "skip",
        "--skip-self-update",
    ]);
    let ran = world
        .run(&flags, Err("piped".to_owned()), &[], None, &roomy())
        .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    // A warning notice in the tree, under the step that found them (ops
    // decision 39).
    in_order(
        &ran.screen,
        &[
            "Checked for new announcements",
            "! Skipped migrations: Sandbox migrate-b, Saltbox migrate-a",
            "Saltbox configuration validated",
        ],
    );
    assert!(tag_runs(&world).is_empty(), "{:?}", world.host.calls());

    let flags = args(&["--skip-self-update"]);
    let (world, _, _) = migrations_world();
    let ran = world
        .run_migrating(&flags, Ok(()), &[], None, &roomy(), Some(false))
        .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(ran.confirmed, [update::MIGRATIONS_QUESTION]);
    assert!(
        ran.screen.contains("! Skipped migrations:"),
        "{}",
        ran.screen
    );
    assert!(tag_runs(&world).is_empty(), "{:?}", world.host.calls());

    let (world, _, _) = migrations_world();
    let ran = world
        .run_migrating(&flags, Ok(()), &[], None, &roomy(), Some(true))
        .await;
    assert_eq!(ran.error, None, "{}{}", ran.screen, ran.lines);
    assert_eq!(tag_runs(&world).len(), 2, "{:?}", world.host.calls());
    assert!(
        ran.screen
            .contains("› Migrations run: Sandbox migrate-b, Saltbox migrate-a"),
        "{}",
        ran.screen
    );

    // No migration among the new announcements: nothing asked.
    let world = World::new("master", "master");
    let ran = world
        .run_migrating(&flags, Ok(()), &[], None, &roomy(), None)
        .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert!(ran.confirmed.is_empty(), "{:?}", ran.confirmed);
}

/// sb interrupted before a migration's playbook starts (here a SIGTERM as
/// the question is answered): exec's attached run doesn't start it and
/// is interrupted, and the warning names it and the one after it, each
/// with its by-hand line, before the run ends (ops difference 43).
#[tokio::test]
async fn an_interrupt_names_what_is_left_in_a_warning() {
    let _spawns = SPAWNS.lock().await;
    let (world, _, _) = migrations_world();
    world.interrupt_at_confirm.set(true);
    let flags = args(&["--reset-branch", "--skip-self-update"]);
    let ran = world
        .run_migrating(&flags, Ok(()), &[], None, &roomy(), Some(true))
        .await;
    assert!(ran.error.is_some(), "{}", ran.lines);
    assert!(
        ran.lines.contains(
            "Migrations interrupted; to run by hand: sb install sandbox-migrate-b, sb install \
             migrate-a"
        ),
        "{}",
        ran.lines
    );
    assert!(tag_runs(&world).is_empty(), "{:?}", world.host.calls());
    assert!(
        !ran.lines.contains("All migrations completed"),
        "{}",
        ran.lines
    );
}

/// Ctrl-C during a migration reaches Ansible, which exits 99
/// (`KEYBOARD_INTERRUPT`): the failure's text names the interrupted
/// migration and the one after it, nothing else runs.
#[tokio::test]
async fn ansibles_own_ctrl_c_exit_names_what_is_left() {
    let _spawns = SPAWNS.lock().await;
    let (world, _, _) = migrations_world();
    let _ = world.host.stand_in(
        "ansible-playbook",
        &format!(
            "case \"$*\" in\n*--list-tags*) cat <<'LIST'\n{}LIST\n;;\n*) exit 99 ;;\nesac",
            sb_ops::testing::LIST_TAGS
        ),
    );
    let flags = args(&[
        "--reset-branch",
        "--migrations",
        "run",
        "--skip-self-update",
    ]);
    let ran = world
        .run(&flags, Err("piped".to_owned()), &[], None, &roomy())
        .await;
    let error = ran.error.expect("an interrupted migration");
    assert!(
        error.starts_with(
            "failed to execute migration 'migrate-b' for Sandbox repository (to run by hand: \
             sb install sandbox-migrate-b, sb install migrate-a): "
        ),
        "{error}"
    );
    assert!(error.contains("exited with code 99"), "{error}");
    assert_eq!(tag_runs(&world).len(), 1, "{:?}", world.host.calls());
    assert!(
        !ran.screen.contains("Validating Saltbox configuration"),
        "{}",
        ran.screen
    );
}

/// A failing migration stops the run with the spec's text (exit 1): the
/// later migration and validation don't run.
#[tokio::test]
async fn a_failing_migration_stops_with_its_words() {
    let _spawns = SPAWNS.lock().await;
    let (world, _, fail) = migrations_world();
    std::fs::write(&fail, "").expect("fail marker");
    let flags = args(&[
        "--reset-branch",
        "--migrations",
        "run",
        "--skip-self-update",
    ]);
    let ran = world
        .run(&flags, Err("piped".to_owned()), &[], None, &roomy())
        .await;
    let error = ran.error.expect("a failed migration");
    assert!(
        // The failed one and the one after it, each with the line that
        // runs it by hand (the maintainer, 2026-11-17: ops difference 43).
        error.starts_with(
            "failed to execute migration 'migrate-b' for Sandbox repository (to run by hand: \
             sb install sandbox-migrate-b, sb install migrate-a): "
        ),
        "{error}"
    );
    assert!(error.contains("exited with code 2"), "{error}");
    // The playbook ran: its output above says why, so the block has no log
    // pointer (the maintainer, 2026-11-18).
    let block = ran.block.expect("a block");
    assert!(block.contains("exited with code 2"), "{block}");
    assert!(!block.contains("POINTER"), "{block}");
    assert_eq!(tag_runs(&world).len(), 1, "{:?}", world.host.calls());
    assert!(
        !ran.lines.contains("All migrations completed"),
        "{}",
        ran.lines
    );
    assert!(
        !ran.screen.contains("Validating Saltbox configuration"),
        "{}",
        ran.screen
    );
}

/// Saltbox's closing hint (ops spec, "What moved, and the closing hint":
/// hint H2), as the block's plain text.
const SALTBOX_HINT: &str = "\
Saltbox's files are updated; your server and apps are not yet. To apply it:
  sb install saltbox       your main tag (or mediabox, feederbox, core)
  sb install core,<apps>   the minimum: core plus the apps you run
";

/// Sandbox's closing hint, when only Sandbox moved.
const SANDBOX_HINT: &str = "\
Sandbox's files are updated; your Sandbox apps are not yet. To apply it:
  sb install sandbox-<app>   each Sandbox app you run
  sb install sandbox-roles   the apps you list in sandbox_roles
";

/// The tags the stand-in lists in a checkout without `tags.txt`
/// (sb-ops' `LIST_TAGS`).
const DEFAULT_TAGS: &str = "always, authelia, docker, plex, sanity-check, sonarr, traefik";

/// A checkout's HEAD as the block shows it: its first 7 characters.
fn short(path: &Path) -> String {
    sb_ops::testing::git(path, &["rev-parse", "HEAD"])[..7].to_owned()
}

/// `remote`'s master gets `tags.txt` holding `tags` (`a, b`), so the next
/// pull moves the checkout and its listing gives those tags.
fn retag(remote: &sb_ops::testing::Remote, tags: &str) {
    let _ = sb_ops::testing::git(&remote.work(), &["checkout", "--quiet", "master"]);
    remote.commit("tags.txt", tags);
}

/// What moved (ops spec, "What moved, and the closing hint"; decisions 36
/// and 37), on `lines` after the last tree: a first update with nothing
/// to pull, both up to date and no hint; Sandbox's pull adding two tags
/// and removing one, its range, the tags with `sandbox-`, and Sandbox's
/// hint; Saltbox's pull with the same tags, `, no tag changes` and H2;
/// both moving, Saltbox's tags as typed and H2 alone.
#[tokio::test]
async fn what_moved_shows_each_range_the_tags_a_pull_changed_and_the_hint() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("master", "master");
    let (flags, disk) = (args(&PIPED), roomy());
    let run = || world.run(&flags, Err("piped".to_owned()), &[], None, &disk);
    let ran = run().await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    let (saltbox, sandbox) = (short(&world.saltbox()), short(&world.sandbox()));
    assert_eq!(
        ran.lines,
        format!(
            "\nSaltbox  already up to date ({saltbox})\nSandbox  already up to date ({sandbox})\n"
        )
    );

    retag(
        &world.sb.sandbox_remote,
        "always, authelia, docker, karakeep, plex, sanity-check, sonarr, tracearr",
    );
    let ran = run().await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    let moved = short(&world.sandbox());
    assert_eq!(
        ran.lines,
        format!(
            "\nSaltbox  already up to date ({saltbox})\nSandbox  {sandbox} → {moved}\n  \
             New tags:      sandbox-karakeep, sandbox-tracearr\n  \
             Removed tags:  sandbox-traefik\n\n{SANDBOX_HINT}"
        )
    );
    let sandbox = moved;

    retag(&world.sb.saltbox_remote, DEFAULT_TAGS);
    let ran = run().await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    let moved = short(&world.saltbox());
    assert_eq!(
        ran.lines,
        format!(
            "\nSaltbox  {saltbox} → {moved}, no tag changes\n\
             Sandbox  already up to date ({sandbox})\n\n{SALTBOX_HINT}"
        )
    );
    let saltbox = moved;

    retag(
        &world.sb.saltbox_remote,
        "always, docker, plex, sanity-check, sonarr, traefik",
    );
    retag(&world.sb.sandbox_remote, "always, karakeep");
    let ran = run().await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    let (saltbox_now, sandbox_now) = (short(&world.saltbox()), short(&world.sandbox()));
    assert_eq!(
        ran.lines,
        format!(
            "\nSaltbox  {saltbox} → {saltbox_now}\n  Removed tags:  authelia\n\
             Sandbox  {sandbox} → {sandbox_now}\n  Removed tags:  sandbox-authelia, \
             sandbox-docker, sandbox-plex,\n                 sandbox-sanity-check, \
             sandbox-sonarr, sandbox-tracearr\n\n{SALTBOX_HINT}"
        )
    );
}

/// The first update on a host with no cache (decision 37): the pull's
/// range, never every tag as new.
#[tokio::test]
async fn the_first_update_on_a_host_with_no_cache_shows_only_the_range() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("master", "master");
    assert!(!world.saltbox().join("cache.json").exists());
    let saltbox = short(&world.saltbox());
    retag(&world.sb.saltbox_remote, "plex, sonarr");
    let ran = world
        .run(&args(&PIPED), Err("piped".to_owned()), &[], None, &roomy())
        .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(
        ran.lines,
        format!(
            "\nSaltbox  {saltbox} → {}\nSandbox  already up to date ({})\n\n{SALTBOX_HINT}",
            short(&world.saltbox()),
            short(&world.sandbox())
        )
    );
}

/// The tags show only when the old entry was valid for the commit before
/// the pull (decision 37): a valid entry written by hand, as sb-go writes
/// one, shows them; each other case shows the range only: no file, no
/// entry, an unreadable file (over cli-core's 1 MiB), a corrupt file, an
/// entry of another shape, an entry for another commit, empty tags.
#[tokio::test]
async fn the_tags_show_only_from_an_old_entry_valid_for_the_commit_before_the_pull() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("master", "master");
    let flags = args(&PIPED);
    let ran = world
        .run(&flags, Err("piped".to_owned()), &[], None, &roomy())
        .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    let file = world.saltbox().join("cache.json");
    let key = serde_json::to_string(&world.saltbox().display().to_string()).expect("key");
    let entry = |value: &str| format!("{{{key}:{value}}}");
    let cases = [
        "a valid entry",
        "no file",
        "no entry",
        "an unreadable file",
        "a corrupt file",
        "another shape",
        "another commit",
        "empty tags",
    ];
    for (at, case) in cases.into_iter().enumerate() {
        let old = sb_ops::testing::git(&world.saltbox(), &["rev-parse", "HEAD"]);
        // The file's text, or none.
        let text = match case {
            "a valid entry" => Some(entry(&format!(r#"{{"commit":"{old}","tags":["plex"]}}"#))),
            "no file" => None,
            "no entry" => Some("{}".to_owned()),
            "an unreadable file" => Some(" ".repeat(1024 * 1024 + 1)),
            "a corrupt file" => Some("{not json".to_owned()),
            "another shape" => Some(entry(&format!(r#"{{"commit":"{old}","tags":"plex"}}"#))),
            "another commit" => Some(entry(&format!(
                r#"{{"commit":"{}","tags":["plex"]}}"#,
                "0".repeat(40)
            ))),
            "empty tags" => Some(entry(&format!(r#"{{"commit":"{old}","tags":[]}}"#))),
            _ => unreachable!("{case}"),
        };
        match text {
            Some(text) => std::fs::write(&file, text).expect("cache"),
            None => {
                let _ = std::fs::remove_file(&file);
            }
        }
        retag(&world.sb.saltbox_remote, &format!("plex, case-{at}"));
        let ran = world
            .run(&flags, Err("piped".to_owned()), &[], None, &roomy())
            .await;
        assert_eq!(ran.error, None, "{case}: {}", ran.screen);
        let range = format!("Saltbox  {} → {}\n", &old[..7], short(&world.saltbox()));
        if case == "a valid entry" {
            assert!(
                ran.lines
                    .contains(&format!("\n{range}  New tags:      case-{at}\n")),
                "{case}: {}",
                ran.lines
            );
            assert!(
                !ran.lines.contains("Removed tags:"),
                "{case}: {}",
                ran.lines
            );
        } else {
            assert!(
                ran.lines.contains(&format!("\n{range}Sandbox  ")),
                "{case}: {}",
                ran.lines
            );
            assert!(!ran.lines.contains("tags:"), "{case}: {}", ran.lines);
        }
        assert!(ran.lines.ends_with(SALTBOX_HINT), "{case}: {}", ran.lines);
    }
}

/// A rebuild that fails after the pull (decisions 37 and 38): the run's
/// error, the range only, no hint. Since decision 46 the rebuilds come
/// after Sandbox's step, so Sandbox is pulled and named too.
#[tokio::test]
async fn a_failed_rebuild_shows_the_range_only_and_no_hint() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("master", "master");
    let flags = args(&PIPED);
    let ran = world
        .run(&flags, Err("piped".to_owned()), &[], None, &roomy())
        .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    let saltbox = short(&world.saltbox());
    retag(&world.sb.saltbox_remote, "plex, sonarr");
    world.host.fail("ansible-playbook");
    let ran = world
        .run(&flags, Err("piped".to_owned()), &[], None, &roomy())
        .await;
    assert!(ran.error.is_some(), "{}", ran.screen);
    in_order(
        &ran.screen,
        &[
            "├── ✓ Saltbox and Sandbox updated",
            "── ✗ Updating Saltbox tags cache",
        ],
    );
    // Sandbox didn't move and its entry is valid: no rebuild is due.
    assert!(
        !ran.screen.contains("Updating Sandbox tags cache"),
        "{}",
        ran.screen
    );
    assert_eq!(
        ran.lines,
        format!(
            "\nSaltbox  {saltbox} → {}\nSandbox  already up to date ({})\n",
            short(&world.saltbox()),
            short(&world.sandbox())
        )
    );
}

/// Saltbox's runtime failing after its pull (decision 38): the pull is
/// named (its HEAD is read before the runtime), the range only, no hint.
/// Since decision 46 the runtime comes after both git phases, so
/// Sandbox's pull is named too.
#[tokio::test]
async fn a_runtime_failure_after_the_pull_still_names_it() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("master", "master");
    let flags = args(&PIPED);
    let ran = world
        .run(&flags, Err("piped".to_owned()), &[], None, &roomy())
        .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    let saltbox = short(&world.saltbox());
    // The pull brings a new lock, so the venv is rebuilt, and its sync
    // fails (python-env's stand-in uv).
    retag(&world.sb.saltbox_remote, "plex, sonarr");
    let layout = &world.sb.tree.layout;
    let lock = layout
        .lock
        .strip_prefix(&layout.checkout)
        .expect("in the checkout");
    world.sb.saltbox_remote.commit(
        &lock.display().to_string(),
        "ansible==12.1.0 --hash=sha256:01\n",
    );
    world.sb.tree.fail("pip-sync");
    let ran = world
        .run(&flags, Err("piped".to_owned()), &[], None, &roomy())
        .await;
    assert!(ran.error.is_some(), "{}", ran.screen);
    assert!(
        !ran.screen.contains("Updating Saltbox tags cache"),
        "{}",
        ran.screen
    );
    assert_eq!(
        ran.lines,
        format!(
            "\nSaltbox  {saltbox} → {}\nSandbox  already up to date ({})\n",
            short(&world.saltbox()),
            short(&world.sandbox())
        )
    );
}

/// A failure after Saltbox's pull (decision 38): the Saltbox checkout
/// that moved is named, the Sandbox step that failed isn't, no hint.
#[tokio::test]
async fn a_failure_after_saltboxs_pull_shows_what_moved_without_the_hint() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("master", "master");
    let flags = args(&PIPED);
    let ran = world
        .run(&flags, Err("piped".to_owned()), &[], None, &roomy())
        .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    let saltbox = short(&world.saltbox());
    retag(&world.sb.saltbox_remote, DEFAULT_TAGS);
    std::fs::remove_dir_all(world.sandbox()).expect("remove sandbox");
    let ran = world
        .run(&flags, Err("piped".to_owned()), &[], None, &roomy())
        .await;
    assert!(ran.error.is_some(), "{}", ran.screen);
    assert!(
        ran.screen.contains("error updating Sandbox"),
        "{}",
        ran.screen
    );
    // No listing after a failed git phase (decision 46): Saltbox's range
    // only.
    assert!(!ran.screen.contains("tags cache"), "{}", ran.screen);
    assert_eq!(
        ran.lines,
        format!("\nSaltbox  {saltbox} → {}\n", short(&world.saltbox()))
    );
}

/// A path in a checkout and a file's bytes (none for a directory).
type Entry = (PathBuf, Option<Vec<u8>>);

/// [`snapshot`]'s: the entries, and git's state in one text.
type Snapshot = (Vec<Entry>, String);

/// A new commit on master in each remote, which an update would pull.
fn upstream(world: &World) {
    for remote in [&world.sb.saltbox_remote, &world.sb.sandbox_remote] {
        let _ = sb_ops::testing::git(&remote.work(), &["checkout", "--quiet", "master"]);
        remote.commit("upstream.txt", "new\n");
    }
}

/// sb's own files in the Saltbox checkout, which a run writes (ignored,
/// as Saltbox's .gitignore has them): the tags cache and the fact.
const OURS: [&str; 2] = ["cache.json", "ansible_facts.d"];

/// A checkout as it stands, byte for byte: every entry outside `.git`
/// with each file's bytes (std's `read_dir`, sorted), the index's and the
/// worktree's state (`git status`, ignored files too) and HEAD with the
/// local branches; sb's own files ([`OURS`]) aside.
fn snapshot(checkout: &Path) -> Snapshot {
    fn walk(dir: &Path, top: &Path, out: &mut Vec<Entry>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .expect("read dir")
            .map(|entry| entry.expect("entry").path())
            .collect();
        entries.sort();
        for path in entries {
            let rel = path.strip_prefix(top).expect("inside").to_path_buf();
            if rel == Path::new(".git") || OURS.iter().any(|ours| rel.starts_with(ours)) {
                continue;
            }
            if path.is_dir() {
                out.push((rel, None));
                walk(&path, top, out);
            } else {
                out.push((rel, Some(std::fs::read(&path).expect("read"))));
            }
        }
    }
    let mut entries = Vec::new();
    walk(checkout, checkout, &mut entries);
    let git = |args: &[&str]| sb_ops::testing::git(checkout, args);
    let status: Vec<String> = git(&[
        "status",
        "--porcelain=v1",
        "--untracked-files=all",
        "--ignored",
    ])
    .lines()
    .filter(|line| {
        !OURS
            .iter()
            .any(|ours| line.starts_with(&format!("!! {ours}")))
    })
    .map(str::to_owned)
    .collect();
    let state = [
        status.join("\n"),
        git(&["rev-parse", "--abbrev-ref", "HEAD"]),
        git(&["rev-parse", "HEAD"]),
        git(&[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/heads",
        ]),
        git(&["config", "--get-all", "remote.origin.fetch"]),
    ]
    .join("\n");
    (entries, state)
}

/// A local commit, a tracked edit and two untracked entries in
/// `checkout`, beside its ignored configs: what an update would discard
/// (decision 40).
fn dirty(checkout: &Path) {
    let git = |args: &[&str]| {
        let _ = sb_ops::testing::git(checkout, args);
    };
    std::fs::write(checkout.join("local.txt"), "mine\n").expect("write");
    git(&["add", "local.txt"]);
    git(&["commit", "--quiet", "-m", "local"]);
    std::fs::write(checkout.join("branch.txt"), "edited\n").expect("write");
    std::fs::write(checkout.join("notes.txt"), "mine\n").expect("write");
    std::fs::create_dir(checkout.join("drafts")).expect("mkdir");
    std::fs::write(checkout.join("drafts/role.yml"), "draft\n").expect("write");
}

/// What `dirty` leaves, as repo's summary words it.
const DIRTY: &str = "1 local commit on 'master', 1 tracked change and 2 untracked files";

/// On a terminal, a checkout with local changes gets its question after
/// its branch's, before any step, naming them (ops decision 40); a no
/// leaves it byte for byte as it was, the answer a notice under its
/// step, its tags rebuilt and `left as is` in what moved, while the
/// other checkout updates as before; a clean one gets no question.
#[tokio::test]
async fn a_declined_local_changes_question_leaves_the_checkout_as_it_is() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("master", "master");
    dirty(&world.saltbox());
    upstream(&world);
    let before = snapshot(&world.saltbox());
    let flags = args(&["--migrations", "skip", "--skip-self-update"]);
    let ran = world.run(&flags, Ok(()), &[false], None, &roomy()).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(ran.asked.len(), 1, "{:?}", ran.asked);
    let (shown, question) = ran.asked[0].split_once(" | ").expect("asked");
    assert_eq!(question, "Saltbox: Discard these local changes and update?");
    assert_eq!(
        shown,
        format!(
            "Saltbox: updating to 'master' would discard {DIRTY}:\n  branch.txt\n  drafts/\n  \
             notes.txt\nDeclining leaves Saltbox as it is; the rest of the update goes on."
        )
    );
    assert_eq!(snapshot(&world.saltbox()), before);
    in_order(
        &ran.screen,
        &[
            "Saltbox and Sandbox updated",
            &format!("› Saltbox: Left as is, keeping {DIRTY} (your answer)"),
            "Sandbox repository updated (master)",
            "├── ✓ Updating Saltbox tags cache",
            "├── ✓ Updating Sandbox tags cache",
        ],
    );
    assert!(
        !ran.screen.contains("Saltbox repository updated"),
        "{}",
        ran.screen
    );
    let head = sb_ops::testing::git(&world.saltbox(), &["rev-parse", "HEAD"]);
    assert!(
        ran.lines.contains(&format!(
            "Saltbox  left as is ({}, master, 3 local changes)",
            &head[..7]
        )),
        "{}",
        ran.lines
    );
    assert!(ran.lines.contains("Sandbox  "), "{}", ran.lines);
    // Only Sandbox moved: Sandbox's hint.
    assert!(
        ran.lines.contains("Sandbox's files are updated"),
        "{}",
        ran.lines
    );
    assert_eq!(World::branch(&world.sandbox()).1, "master");
    assert!(world.sandbox().join("upstream.txt").exists());
}

/// A yes discards them: the update runs as before, the answer a notice.
#[tokio::test]
async fn an_accepted_local_changes_question_discards_them() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("master", "master");
    dirty(&world.saltbox());
    let flags = args(&["--migrations", "skip", "--skip-self-update"]);
    let ran = world.run(&flags, Ok(()), &[true], None, &roomy()).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(ran.asked.len(), 1, "{:?}", ran.asked);
    in_order(
        &ran.screen,
        &[
            "Saltbox and Sandbox updated",
            &format!("› Saltbox: Discarding {DIRTY} (your answer)"),
            "Saltbox repository updated (master)",
        ],
    );
    for gone in ["local.txt", "notes.txt", "drafts"] {
        assert!(!world.saltbox().join(gone).exists(), "{gone}");
    }
    assert_eq!(World::branch(&world.saltbox()).1, "master");
    // The ignored configs stay.
    assert!(world.saltbox().join("accounts.yml").exists());
}

/// Without a terminal, local changes are refused before any step, naming
/// the checkout, its counts and both flags; nothing ran or changed. With
/// `--discard-local` they are discarded without a question, noted.
#[tokio::test]
async fn without_a_terminal_local_changes_need_a_flag() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("master", "master");
    dirty(&world.sandbox());
    let before = snapshot(&world.sandbox());
    let ran = world
        .run(
            &args(&PIPED),
            Err("no terminal".to_owned()),
            &[],
            None,
            &roomy(),
        )
        .await;
    let error = ran.error.expect("refused");
    assert!(
        error.contains(&format!(
            "--discard-local or --skip-git (Sandbox has {DIRTY} that the update would discard)"
        )),
        "{error}"
    );
    assert!(world.host.calls().is_empty(), "{:?}", world.host.calls());
    assert!(!ran.screen.contains("Updating"), "{}", ran.screen);
    assert_eq!(snapshot(&world.sandbox()), before);

    let mut flags = PIPED.to_vec();
    flags.push("--discard-local");
    let ran = world
        .run(
            &args(&flags),
            Err("no terminal".to_owned()),
            &[],
            None,
            &roomy(),
        )
        .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert!(ran.asked.is_empty(), "{:?}", ran.asked);
    assert!(
        ran.screen
            .contains(&format!("› Sandbox: Discarding {DIRTY} (--discard-local)")),
        "{}",
        ran.screen
    );
    assert!(!world.sandbox().join("notes.txt").exists());
}

/// Without a terminal, a checkout whose discards git can't read up front
/// (its branch has no upstream to count local commits against) is left
/// out of the refusal, dirty as it is: the refusal names only the other
/// one, and nothing ran or changed.
#[tokio::test]
async fn without_a_terminal_a_checkout_without_an_upstream_is_left_out_of_the_refusal() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("master", "master");
    dirty(&world.sandbox());
    dirty(&world.saltbox());
    let _ = sb_ops::testing::git(&world.saltbox(), &["branch", "--unset-upstream", "master"]);
    let before = snapshot(&world.saltbox());
    let ran = world
        .run(
            &args(&PIPED),
            Err("no terminal".to_owned()),
            &[],
            None,
            &roomy(),
        )
        .await;
    let error = ran.error.expect("refused");
    assert!(
        error.contains(&format!(
            "--discard-local or --skip-git (Sandbox has {DIRTY} that the update would discard)"
        )),
        "{error}"
    );
    assert!(!error.contains("Saltbox"), "{error}");
    assert!(world.host.calls().is_empty(), "{:?}", world.host.calls());
    assert_eq!(snapshot(&world.saltbox()), before);
}

/// A question that comes up after a terminal check passed but can't be
/// asked (sb went to the background) is the refusal with the counts,
/// ending the run before any step.
#[tokio::test]
async fn a_local_changes_question_that_cant_be_asked_is_refused() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("master", "master");
    dirty(&world.saltbox());
    let flags = args(&["--migrations", "skip", "--skip-self-update"]);
    let ran = world.run(&flags, Ok(()), &[], None, &roomy()).await;
    let error = ran.error.expect("refused");
    assert_eq!(
        error,
        format!(
            "error updating Saltbox: can't ask before discarding local changes: no terminal; \
             answer with --discard-local or --skip-git (Saltbox has {DIRTY} that the update \
             would discard)"
        )
    );
    assert!(world.host.calls().is_empty(), "{:?}", world.host.calls());
}

/// `--skip-git` (ops decision 41): both checkouts byte for byte as they
/// were, off master, dirty and behind their remotes alike, with no
/// terminal and neither a branch flag nor `--migrations`; no question;
/// the runtime, both tags caches rebuilt, the config permissions,
/// validation and completions as before; the announcements a notice;
/// what moved `left as is` for both, no hint; the run log's records.
#[tokio::test]
async fn skip_git_leaves_both_checkouts_as_they_are() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("develop", "master");
    dirty(&world.sandbox());
    upstream(&world);
    let before = (snapshot(&world.saltbox()), snapshot(&world.sandbox()));
    let ran = world
        .run(
            &args(&["--skip-git", "--skip-self-update"]),
            Err("no terminal".to_owned()),
            &[],
            None,
            &roomy(),
        )
        .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert!(ran.asked.is_empty(), "{:?}", ran.asked);
    assert!(ran.confirmed.is_empty(), "{:?}", ran.confirmed);
    assert_eq!(snapshot(&world.saltbox()), before.0);
    assert_eq!(snapshot(&world.sandbox()), before.1);
    in_order(
        &ran.screen,
        &[
            "Saltbox and Sandbox updated",
            "Ansible virtual environment ready",
            "Saltbox configuration permissions secured",
            "├── ✓ Updating Saltbox tags cache",
            "├── ✓ Updating Sandbox tags cache",
            update::NOT_COMPARED,
            &format!("› {}", update::SEE_THEM),
            "Saltbox configuration validated",
            "Shell completions checked",
        ],
    );
    for absent in [
        "Fetching repository changes",
        "repository updated",
        "new announcements",
    ] {
        assert!(!ran.screen.contains(absent), "{absent}:\n{}", ran.screen);
    }
    assert_eq!(
        world.programs(),
        ["ansible-playbook", "ansible-playbook"],
        "{:?}",
        world.host.calls()
    );
    let head = |path: &Path| sb_ops::testing::git(path, &["rev-parse", "--short=7", "HEAD"]);
    assert_eq!(
        ran.lines,
        format!(
            "\nSaltbox  left as is ({}, develop, no local changes)\n\
             Sandbox  left as is ({}, master, 3 local changes)\n",
            head(&world.saltbox()),
            head(&world.sandbox())
        )
    );
    // Both caches at each checkout's HEAD.
    let cache = TagCache::load(&world.saltbox());
    for (checkout, path) in [
        (Checkout::Saltbox, world.saltbox()),
        (Checkout::Sandbox, world.sandbox()),
    ] {
        let entry = cache
            .entry(&checkout.playbooks(&world.saltbox()))
            .expect("cached");
        assert_eq!(
            entry.commit,
            sb_ops::testing::git(&path, &["rev-parse", "HEAD"])
        );
    }
}

/// `--skip-git` conflicts with both branch flags and `--discard-local`
/// (clap's error, exit 2), and isn't in help; `--discard-local` is.
#[test]
fn skip_git_conflicts_with_the_flags_it_makes_meaningless() {
    use clap::{CommandFactory as _, Parser as _};
    #[derive(clap::Parser)]
    struct Cli {
        #[command(flatten)]
        args: UpdateArgs,
    }
    for other in ["--keep-branch", "--reset-branch", "--discard-local"] {
        let err = Cli::try_parse_from(["sb", "--skip-git", other])
            .err()
            .expect("conflict");
        assert_eq!(
            err.kind(),
            clap::error::ErrorKind::ArgumentConflict,
            "{other}"
        );
        assert_eq!(err.exit_code(), 2);
    }
    let parsed = Cli::try_parse_from(["sb", "--skip-git"]).expect("alone");
    assert!(parsed.args.skip_git);
    assert!(!parsed.args.discard.discard_local);
    let help = Cli::command().render_long_help().to_string();
    assert!(!help.contains("--skip-git"), "{help}");
    assert!(help.contains("--discard-local"), "{help}");
}

/// Declined after a branch answer, the checkout is left on its branch:
/// the branch's notice would say otherwise, so only the answer is noted.
#[tokio::test]
async fn a_checkout_left_as_is_isnt_noted_as_reset_or_updated() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("develop", "master");
    std::fs::write(world.saltbox().join("notes.txt"), "mine\n").expect("untracked");
    for flag in ["--keep-branch", "--reset-branch"] {
        let flags = args(&[flag, "--migrations", "skip", "--skip-self-update"]);
        let ran = world.run(&flags, Ok(()), &[false], None, &roomy()).await;
        assert_eq!(ran.error, None, "{flag}: {}", ran.screen);
        assert!(
            ran.screen
                .contains("› Saltbox: Left as is, keeping 1 untracked file (your answer)"),
            "{flag}: {}",
            ran.screen
        );
        for wrong in [
            "Updating the current branch",
            "Reset from branch",
            "Currently on",
        ] {
            assert!(
                !ran.screen.contains(wrong),
                "{flag}, {wrong}: {}",
                ran.screen
            );
        }
        assert_eq!(
            World::branch(&world.saltbox()),
            ("develop".into(), "develop".into())
        );
    }
}

/// A checkout git can't read for its local changes (a corrupt index)
/// fails its own step, untouched, after the other one updated: never
/// "nothing to discard".
#[tokio::test]
async fn a_checkout_git_cant_read_fails_its_own_step_untouched() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("master", "master");
    upstream(&world);
    std::fs::write(world.sandbox().join(".git/index"), "not an index").expect("corrupt");
    let head = sb_ops::testing::git(&world.sandbox(), &["rev-parse", "HEAD"]);
    let flags = args(&["--migrations", "skip", "--skip-self-update"]);
    let ran = world.run(&flags, Ok(()), &[], None, &roomy()).await;
    assert!(ran.error.is_some(), "{}", ran.screen);
    assert!(ran.asked.is_empty(), "{:?}", ran.asked);
    in_order(
        &ran.screen,
        &[
            "✗ Saltbox and Sandbox update",
            "error updating Sandbox",
            "Saltbox repository updated (master)",
        ],
    );
    assert!(
        !ran.screen.contains("Sandbox repository updated"),
        "{}",
        ran.screen
    );
    assert_eq!(
        sb_ops::testing::git(&world.sandbox(), &["rev-parse", "HEAD"]),
        head
    );
    assert!(world.saltbox().join("upstream.txt").exists());
}

/// The stand-in `ansible-playbook` for the tag steps (decision 46): each
/// run appends `start <dir> <Saltbox HEAD> <Sandbox HEAD>` to
/// `<meet>/log` (<dir> the last part of its working directory), marks
/// itself started, waits until two runs have started or `ticks` times
/// 0.05 s have passed, appends `end <dir>`, then runs `then` (by default
/// the harness's listing).
fn meeting_stand_in(world: &World, meet: &Path, ticks: u32, then: &str) {
    let head = |path: PathBuf| {
        format!(
            "$(GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null git -c safe.directory='*' \
             -C '{}' rev-parse HEAD)",
            path.display()
        )
    };
    let meet = meet.display();
    let script = format!(
        "name=${{PWD##*/}}\n\
         echo \"start $name {saltbox} {sandbox}\" >> '{meet}/log'\n\
         : > '{meet}/'\"$name.started\"\n\
         i=0\n\
         while [ \"$i\" -lt {ticks} ]; do\n\
           set -- '{meet}'/*.started\n\
           [ $# -ge 2 ] && break\n\
           sleep 0.05\n\
           i=$((i + 1))\n\
         done\n\
         echo \"end $name\" >> '{meet}/log'\n\
         {then}",
        saltbox = head(world.saltbox()),
        sandbox = head(world.sandbox()),
    );
    let _ = world.host.stand_in("ansible-playbook", &script);
}

/// The last part of a checkout's path, as the stand-in names it.
fn dir_name(path: &Path) -> String {
    path.file_name()
        .expect("a name")
        .to_string_lossy()
        .into_owned()
}

/// Both tag listings start only once both checkouts' git steps have
/// ended (decision 46): each sees Saltbox's and Sandbox's new HEADs. On 2
/// physical cores they run at once, on 1 in turn; the tree and what
/// moved are the same either way.
#[tokio::test]
async fn the_tag_listings_start_after_both_pulls_at_once_on_two_cores() {
    let _spawns = SPAWNS.lock().await;
    for (cores, ticks) in [(2, 400), (1, 4)] {
        let world = World::new("master", "master");
        world.cores.set(Some(cores));
        let meet = tempfile::tempdir().expect("meet");
        meeting_stand_in(
            &world,
            meet.path(),
            ticks,
            &sb_ops::testing::list_tags_script(),
        );
        let (saltbox_before, sandbox_before) = (short(&world.saltbox()), short(&world.sandbox()));
        upstream(&world);
        let ran = world
            .run(&args(&PIPED), Err("piped".to_owned()), &[], None, &roomy())
            .await;
        assert_eq!(ran.error, None, "{cores}: {}", ran.screen);
        let git = |path: PathBuf| sb_ops::testing::git(&path, &["rev-parse", "HEAD"]);
        let heads = format!("{} {}", git(world.saltbox()), git(world.sandbox()));
        let (saltbox, sandbox) = (dir_name(&world.saltbox()), dir_name(&world.sandbox()));
        let log = std::fs::read_to_string(meet.path().join("log")).expect("log");
        let log: Vec<&str> = log.lines().collect();
        let start = |name: &str| format!("start {name} {heads}");
        let end = |name: &str| format!("end {name}");
        if cores == 2 {
            assert_eq!(log.len(), 4, "{log:?}");
            let mut starts = log[..2].to_vec();
            starts.sort_unstable();
            let mut want = vec![start(&saltbox), start(&sandbox)];
            want.sort_unstable();
            assert_eq!(starts, want, "both at once, after both pulls");
            assert!(
                log[2..].iter().all(|line| line.starts_with("end ")),
                "{log:?}"
            );
        } else {
            assert_eq!(
                log,
                [
                    start(&saltbox),
                    end(&saltbox),
                    start(&sandbox),
                    end(&sandbox)
                ],
                "in turn, after both pulls"
            );
        }
        in_order(
            &ran.screen,
            &[
                "├── ✓ Saltbox and Sandbox updated",
                "│   ├── ✓ Saltbox repository updated (master)",
                "│   ├── ✓ Sandbox repository updated (master)",
                "├── ✓ Updating Saltbox tags cache",
                "├── ✓ Updating Sandbox tags cache",
                "Checked for new announcements",
            ],
        );
        // Neither tag step is a sub-step any more.
        assert!(
            ran.screen
                .lines()
                .filter(|line| line.contains("tags cache"))
                .all(|line| line.starts_with("├── ")),
            "{}",
            ran.screen
        );
        assert_eq!(
            ran.lines,
            format!(
                "\nSaltbox  {saltbox_before} → {}\nSandbox  {sandbox_before} → {}\n\n{SALTBOX_HINT}",
                short(&world.saltbox()),
                short(&world.sandbox())
            ),
            "{cores}"
        );
    }
}

/// At once, a Saltbox listing that fails after Sandbox's has failed is
/// still shown first, and the run fails; both pulls finished, so both
/// ranges show, without tags or the hint (decisions 38 and 46).
#[tokio::test]
async fn at_once_saltboxs_tag_failure_comes_first() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("master", "master");
    world.cores.set(Some(2));
    let sandbox = dir_name(&world.sandbox());
    let meet = tempfile::tempdir().expect("meet");
    meeting_stand_in(
        &world,
        meet.path(),
        400,
        &format!(
            "if [ \"$name\" = {sandbox} ]; then echo 'sandbox broke' >&2; exit 5; fi\n\
             sleep 0.5; echo 'saltbox broke' >&2; exit 4"
        ),
    );
    upstream(&world);
    let ran = world
        .run(&args(&PIPED), Err("piped".to_owned()), &[], None, &roomy())
        .await;
    assert!(ran.error.is_some(), "{}", ran.screen);
    in_order(
        &ran.screen,
        &[
            "├── ✓ Saltbox and Sandbox updated",
            "✗ Updating Saltbox tags cache",
            "saltbox broke",
            "✗ Updating Sandbox tags cache",
            "sandbox broke",
        ],
    );
    assert!(ran.lines.contains("\nSaltbox  "), "{}", ran.lines);
    assert!(ran.lines.contains("\nSandbox  "), "{}", ran.lines);
    assert!(!ran.lines.contains("sb install"), "{}", ran.lines);
}

/// Ctrl-C while both listings run at once: both get SIGTERM and both are
/// waited for before the run ends; the run is interrupted.
#[tokio::test]
async fn ctrl_c_during_both_tag_listings_stops_and_reaps_both() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("master", "master");
    world.cores.set(Some(2));
    let meet = tempfile::tempdir().expect("meet");
    let dir = meet.path().display();
    meeting_stand_in(
        &world,
        meet.path(),
        0,
        &format!(
            "trap 'echo term > \"{dir}/$name.term\"; exit 143' TERM\n\
             echo $$ > \"{dir}/$name.pid.tmp\" && mv \"{dir}/$name.pid.tmp\" \"{dir}/$name.pid\"\n\
             while :; do sleep 0.05; done"
        ),
    );
    let names = [dir_name(&world.saltbox()), dir_name(&world.sandbox())];
    *world.ctrl_c_once.borrow_mut() = names
        .iter()
        .map(|name| meet.path().join(format!("{name}.pid")))
        .collect();
    upstream(&world);
    let ran = world
        .run(&args(&PIPED), Err("piped".to_owned()), &[], None, &roomy())
        .await;
    assert!(ran.error.is_some(), "{}", ran.screen);
    assert!(ran.requested.is_some(), "{}", ran.screen);
    for name in &names {
        assert!(
            meet.path().join(format!("{name}.term")).exists(),
            "{name}'s listing got SIGTERM"
        );
        let pid = std::fs::read_to_string(meet.path().join(format!("{name}.pid"))).expect("pid");
        assert!(
            !Path::new(&format!("/proc/{}", pid.trim())).exists(),
            "{name}'s listing is gone"
        );
    }
}

/// A line appended to `file`.
fn append(file: &Path, line: &str) {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .expect("events");
    writeln!(file, "{line}").expect("an event");
}

/// Each checkout's fetch goes through a wrapper of `git-upload-pack` (its
/// `remote.origin.uploadpack`), which appends `start <repo>` to
/// `<meet>/log`, waits until both fetches have started or `ticks` times
/// 0.05 s have passed, then appends `end <repo>` and serves the fetch.
fn meeting_fetches(world: &World, meet: &Path, ticks: u32) {
    let saltbox_remote = world.sb.saltbox_remote.url.trim_start_matches("file://");
    let dir = meet.display();
    let script = meet.join("upload-pack");
    sb_cli_core::testing::write_program(
        &script,
        format!(
            "#!/bin/sh
             case \"$*\" in *'{saltbox_remote}'*) name=saltbox ;; *) name=sandbox ;; esac
             echo \"start $name\" >> '{dir}/log'
             : > '{dir}/'\"$name.started\"
             i=0
             while [ \"$i\" -lt {ticks} ]; do
               [ -e '{dir}/saltbox.started' ] && [ -e '{dir}/sandbox.started' ] && break
               sleep 0.05
               i=$((i + 1))
             done
             echo \"end $name\" >> '{dir}/log'
             exec git-upload-pack \"$@\"
"
        ),
    );
    for checkout in [world.saltbox(), world.sandbox()] {
        let _ = sb_ops::testing::git(
            &checkout,
            &[
                "config",
                "remote.origin.uploadpack",
                &script.display().to_string(),
            ],
        );
    }
}

/// Both checkouts' git phases (ops spec, step 7; decision 46): at once on
/// 2 physical cores (each fetch sees the other start), in turn on 1
/// (Saltbox's fetch ends before Sandbox's starts); the same tree either
/// way, Saltbox's repository step drawn first.
#[tokio::test]
async fn both_git_phases_run_at_once_on_two_cores_in_turn_on_one() {
    let _spawns = SPAWNS.lock().await;
    for (cores, ticks) in [(2, 400), (1, 4)] {
        let world = World::new("master", "master");
        world.cores.set(Some(cores));
        let meet = tempfile::tempdir().expect("meet");
        meeting_fetches(&world, meet.path(), ticks);
        upstream(&world);
        let ran = world
            .run(&args(&PIPED), Err("piped".to_owned()), &[], None, &roomy())
            .await;
        assert_eq!(ran.error, None, "{cores}: {}", ran.screen);
        let log = std::fs::read_to_string(meet.path().join("log")).expect("log");
        let log: Vec<&str> = log.lines().collect();
        if cores == 2 {
            assert_eq!(log.len(), 4, "{log:?}");
            assert!(
                log[..2].iter().all(|line| line.starts_with("start ")),
                "{log:?}"
            );
            assert!(
                log[2..].iter().all(|line| line.starts_with("end ")),
                "{log:?}"
            );
        } else {
            assert_eq!(
                log,
                [
                    "start saltbox",
                    "end saltbox",
                    "start sandbox",
                    "end sandbox"
                ]
            );
        }
        in_order(
            &ran.screen,
            &[
                "├── ✓ Saltbox and Sandbox updated",
                "│   ├── ✓ Saltbox repository updated (master)",
                "│   │   ├── ✓ Fetching repository changes",
                "│   ├── ✓ Sandbox repository updated (master)",
                "│   │   ├── ✓ Fetching repository changes",
                "Ansible virtual environment ready",
                "├── ✓ Saltbox configuration permissions secured",
                "├── ✓ Updating Saltbox tags cache",
            ],
        );
        assert_eq!(
            (
                World::branch(&world.saltbox()).0,
                World::branch(&world.sandbox()).0
            ),
            ("master".to_owned(), "master".to_owned())
        );
    }
}

/// Every question for both checkouts is asked before either checkout's
/// git phase starts a fetch (ops spec, step 7): no git child of one
/// checkout waits on the other's question, both branch questions and
/// Sandbox's local changes question included.
#[tokio::test]
async fn every_question_is_asked_before_any_fetch() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("develop", "develop");
    world.cores.set(Some(2));
    dirty(&world.sandbox());
    let meet = tempfile::tempdir().expect("meet");
    meeting_fetches(&world, meet.path(), 400);
    *world.events.borrow_mut() = Some(meet.path().join("log"));
    upstream(&world);
    let flags = args(&["--migrations", "skip", "--skip-self-update"]);
    // Reset both to master, then discard Sandbox's local changes.
    let ran = world
        .run(&flags, Ok(()), &[true, true, true], None, &roomy())
        .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    let log = std::fs::read_to_string(meet.path().join("log")).expect("log");
    let log: Vec<&str> = log.lines().collect();
    let asks = log.iter().filter(|line| line.starts_with("ask ")).count();
    assert_eq!(asks, 3, "{log:?}");
    assert!(
        log[..asks].iter().all(|line| line.starts_with("ask ")),
        "a fetch before a question: {log:?}"
    );
    assert_eq!(log.len(), asks + 4, "both fetches, at once: {log:?}");
}

/// At once, a failed git phase lets the other run to its end, is drawn in
/// its place (Saltbox's first) and stops the run before the runtime and
/// any listing; the pull that finished is named, without the hint
/// (decisions 38 and 46).
#[tokio::test]
async fn at_once_a_failed_git_phase_stops_before_any_listing() {
    let _spawns = SPAWNS.lock().await;
    for failing in [Checkout::Saltbox, Checkout::Sandbox] {
        let world = World::new("master", "master");
        world.cores.set(Some(2));
        upstream(&world);
        let (broken, other) = match failing {
            Checkout::Saltbox => (world.saltbox(), world.sandbox()),
            Checkout::Sandbox => (world.sandbox(), world.saltbox()),
        };
        let other_before = short(&other);
        let _ = sb_ops::testing::git(
            &broken,
            &[
                "remote",
                "set-url",
                "origin",
                "file:///nonexistent/remote.git",
            ],
        );
        let ran = world
            .run(&args(&PIPED), Err("piped".to_owned()), &[], None, &roomy())
            .await;
        assert!(ran.error.is_some(), "{failing:?}: {}", ran.screen);
        let (failed, done) = match failing {
            Checkout::Saltbox => (
                "✗ Updating Saltbox repository",
                "✓ Sandbox repository updated",
            ),
            Checkout::Sandbox => (
                "✓ Saltbox repository updated",
                "✗ Updating Sandbox repository",
            ),
        };
        in_order(
            &ran.screen,
            &["✗ Updating Saltbox and Sandbox", failed, done],
        );
        for absent in ["Ansible virtual environment", "tags cache", "permissions"] {
            assert!(!ran.screen.contains(absent), "{absent}: {}", ran.screen);
        }
        let name = match failing {
            Checkout::Saltbox => "Sandbox",
            Checkout::Sandbox => "Saltbox",
        };
        assert_eq!(
            ran.lines,
            format!("\n{name:<9}{other_before} → {}\n", short(&other)),
            "{failing:?}"
        );
    }
}

/// Ctrl-C while both checkouts fetch at once: both fetches are stopped
/// (their group gets SIGTERM), and the run ends interrupted before the
/// runtime and any listing.
#[tokio::test]
async fn ctrl_c_during_both_fetches_stops_both() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("master", "master");
    world.cores.set(Some(2));
    upstream(&world);
    let meet = tempfile::tempdir().expect("meet");
    let saltbox_remote = world.sb.saltbox_remote.url.trim_start_matches("file://");
    let dir = meet.path().display();
    let script = meet.path().join("upload-pack");
    sb_cli_core::testing::write_program(
        &script,
        format!(
            "#!/bin/sh\n\
             case \"$*\" in *'{saltbox_remote}'*) name=saltbox ;; *) name=sandbox ;; esac\n\
             trap 'echo term > \"{dir}/$name.term\"; exit 143' TERM\n\
             : > '{dir}/'\"$name.started\"\n\
             while :; do sleep 0.05; done\n"
        ),
    );
    for checkout in [world.saltbox(), world.sandbox()] {
        let _ = sb_ops::testing::git(
            &checkout,
            &[
                "config",
                "remote.origin.uploadpack",
                &script.display().to_string(),
            ],
        );
    }
    *world.ctrl_c_once.borrow_mut() = ["saltbox", "sandbox"]
        .iter()
        .map(|name| meet.path().join(format!("{name}.started")))
        .collect();
    let ran = world
        .run(&args(&PIPED), Err("piped".to_owned()), &[], None, &roomy())
        .await;
    assert!(ran.error.is_some(), "{}", ran.screen);
    assert!(ran.requested.is_some(), "{}", ran.screen);
    for name in ["saltbox", "sandbox"] {
        assert!(
            meet.path().join(format!("{name}.term")).exists(),
            "{name}'s fetch got SIGTERM: {}",
            ran.screen
        );
    }
    for absent in ["Ansible virtual environment", "tags cache"] {
        assert!(!ran.screen.contains(absent), "{absent}: {}", ran.screen);
    }
}

/// Ctrl-C while Sandbox's fetch still runs at once, Saltbox's pull already
/// finished: Saltbox's pull is still recorded, and what moved names it
/// (decision 38), though the run ends interrupted.
#[tokio::test]
async fn ctrl_c_after_saltboxs_pull_still_names_it() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new("master", "master");
    world.cores.set(Some(2));
    upstream(&world);
    let saltbox_before = short(&world.saltbox());
    let meet = tempfile::tempdir().expect("meet");
    let dir = meet.path().display();
    let script = meet.path().join("upload-pack");
    sb_cli_core::testing::write_program(
        &script,
        format!(
            "#!/bin/sh\n\
             : > '{dir}/sandbox.started'\n\
             while :; do sleep 0.05; done\n"
        ),
    );
    let _ = sb_ops::testing::git(
        &world.sandbox(),
        &[
            "config",
            "remote.origin.uploadpack",
            &script.display().to_string(),
        ],
    );
    // Once Sandbox's fetch hangs and Saltbox's reset has brought the new
    // file, with time for its ownership step and HEAD read to end.
    *world.ctrl_c_once.borrow_mut() = vec![
        meet.path().join("sandbox.started"),
        world.saltbox().join("upstream.txt"),
    ];
    world.ctrl_c_delay.set(Duration::from_millis(1500));
    let ran = world
        .run(&args(&PIPED), Err("piped".to_owned()), &[], None, &roomy())
        .await;
    assert!(ran.error.is_some(), "{}", ran.screen);
    assert!(ran.requested.is_some(), "{}", ran.screen);
    assert!(
        ran.screen.contains("✓ Saltbox repository updated (master)"),
        "{}",
        ran.screen
    );
    assert_eq!(
        ran.lines,
        format!(
            "\nSaltbox  {saltbox_before} → {}\n",
            short(&world.saltbox())
        ),
        "{}",
        ran.screen
    );
}
