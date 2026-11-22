//! `sb setup` in process (ops spec, "`sb setup` (hidden)" and Testing
//! Strategy, "setup guard", "Sequences" and "setup's branch flags"):
//! sb-ops' guard, plan and run on the harness's stand-in host,
//! python-env's stand-in tree and fake GitHub, and real git on a local
//! bare repository standing in for Saltbox's (`file://` only, never the
//! network), as any user. The binary's run as root in the namespace,
//! the fetch over an existing checkout included, is
//! crates/sb/tests/setup.rs.

use std::path::Path;

use sb_cli_core::cancel::Shutdown;
use sb_completion::install::Paths;
use sb_ops::completions::Refresh;
use sb_ops::setup::{self, Found, GuardError, Plan, Setup};
use sb_ops::testing::{Host, Remote, git};
use sb_python_env::testing::Tree;
use sb_repo::Answer;
use sb_term_ui::{Options, PromptError, Runner, Target};

/// Tests that write a stand-in program and run it hold this from writing
/// it until its run is over (ETXTBSY otherwise).
static SPAWNS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A user no host has: the ownership steps skip it (host's
/// `ensure_owner`), so the run needs no root.
const USER: &str = "sb-ops-test-no-such-user";

/// A stand-in host, a python-env tree with no checkout yet (a fresh
/// server), and the harness's stand-in remote holding the tree's pins and
/// lock, accounts.yml's default naming [`USER`].
struct World {
    host: Host,
    tree: Tree,
    remote: Remote,
    /// The completion refresh's root (never the host's `/usr` or `/etc`)
    /// and sb's stand-in executable in it.
    completion: tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        let host = Host::new();
        let tree = Tree::new();
        let _ = tree.healthy();
        let layout = &tree.layout;
        let pins: Vec<(String, String)> = [&layout.python_pin, &layout.uv_pin, &layout.lock]
            .iter()
            .map(|pin| {
                let rel = pin.strip_prefix(&layout.checkout).expect("in the checkout");
                let text = std::fs::read_to_string(pin).expect("pin");
                (rel.display().to_string(), text)
            })
            .collect();
        let files: Vec<(&str, &str)> = pins.iter().map(|(r, t)| (r.as_str(), t.as_str())).collect();
        let remote = Remote::saltbox(&files, USER);
        // A fresh server: no checkout (the tree's own pins are in the
        // remote now).
        std::fs::remove_dir_all(&layout.checkout).expect("remove the tree's checkout");
        let completion = tempfile::tempdir().expect("completion root");
        let sb = completion.path().join("opt/sb/sb");
        std::fs::create_dir_all(sb.parent().expect("parent")).expect("dirs");
        sb_cli_core::testing::write_program(&sb, "#!/bin/sh\n");
        Self {
            host,
            tree,
            remote,
            completion,
        }
    }

    fn root(&self) -> &Path {
        &self.tree.layout.checkout
    }

    /// A clone of the remote at `branch`, where setup's root is.
    fn checkout(&self, branch: &str) {
        self.remote.clone_into(branch, self.root());
    }

    /// The root's branch and `branch.txt`.
    fn branch(&self) -> (String, String) {
        let name = git(self.root(), &["rev-parse", "--abbrev-ref", "HEAD"]);
        let text = std::fs::read_to_string(self.root().join("branch.txt")).expect("branch.txt");
        (name, text.trim().to_owned())
    }

    /// setup on the world as `plan` says: its result and the screen.
    async fn run(
        &self,
        plan: &Plan,
        support: fn() -> Result<(), sb_host::SetupUnsupported>,
    ) -> (bool, String) {
        self.run_refreshing(plan, support, "opt/sb/sb").await
    }

    /// [`World::run`], the completion refresh given `executable` under its
    /// root.
    async fn run_refreshing(
        &self,
        plan: &Plan,
        support: fn() -> Result<(), sb_host::SetupUnsupported>,
        executable: &str,
    ) -> (bool, String) {
        let completion_paths = Paths::under(self.completion.path());
        let executable = Ok(self.completion.path().join(executable));
        let (checks, apt, dirs, locale, bash) = (
            self.host.checks(),
            self.host.apt(),
            self.host.dirs(),
            self.host.locale(),
            self.host.bash(),
        );
        let python = self.tree.env();
        let setup = Setup {
            checks: &checks,
            support,
            apt: &apt,
            dirs: &dirs,
            locale: &locale,
            bash: &bash,
            root: self.root(),
            url: &self.remote.url,
            python: &python,
            run_id: "run1",
            completions: Refresh {
                paths: &completion_paths,
                executable: &executable,
            },
        };
        let term = indicatif::InMemoryTerm::new(100, 400);
        let options = Options {
            target: Target::InMemory {
                term: term.clone(),
                colour: false,
            },
        };
        let shutdown = Shutdown::new();
        let mut runner = Runner::start("sb setup", &shutdown, None, options);
        let ran = setup::run(&mut runner, &setup, plan).await;
        runner.finish();
        (ran.is_ok(), term.contents())
    }
}

/// A supported release, whatever the host runs.
fn supported() -> Result<(), sb_host::SetupUnsupported> {
    sb_host::setup_check("ubuntu", Some("24.04"))
}

/// The plan for `wanted` over what the guard (with `--force`) finds, the
/// question answered `reply` (or failing as with no terminal); the line
/// shown before the question and the question, when it was asked.
async fn plan_for(
    root: &Path,
    wanted: &str,
    answer: Option<Answer>,
    reply: Option<bool>,
) -> (anyhow::Result<Plan>, Option<(String, String)>) {
    let (plan, asked) = plan_asking(root, wanted, answer, false, &[reply]).await;
    (plan, asked.into_iter().next())
}

/// [`plan_for`] with `--discard-local` as `discard_local` says, each
/// question answered by the next of `replies` (none left, or `None`: as
/// with no terminal); every context and question asked, in order.
async fn plan_asking(
    root: &Path,
    wanted: &str,
    answer: Option<Answer>,
    discard_local: bool,
    replies: &[Option<bool>],
) -> (anyhow::Result<Plan>, Vec<(String, String)>) {
    let found = setup::guard(root, true).expect("guard");
    let mut asked = Vec::new();
    let mut replies = replies.iter().copied();
    let ask = |shown: &str, question: &str, flag: &'static str| {
        asked.push((shown.to_owned(), question.to_owned()));
        replies
            .next()
            .flatten()
            .ok_or_else(|| PromptError::NoTerminal {
                question: question.to_owned(),
                reason: "no terminal".to_owned(),
                flag,
            })
    };
    let plan = setup::plan(
        root,
        found,
        wanted,
        answer,
        discard_local,
        ask,
        &Shutdown::new(),
    )
    .await;
    (plan, asked)
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

/// sb-go's `setup_test.go` cases, with each refusal's text.
#[test]
fn the_guard_has_sb_gos_five_cases() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("missing");
    assert_eq!(
        setup::guard(&missing, false).expect("missing"),
        Found::Nothing
    );
    assert_eq!(
        setup::guard(&missing, true).expect("missing"),
        Found::Nothing
    );

    let existing = dir.path().join("existing");
    std::fs::create_dir(&existing).expect("dir");
    let err = setup::guard(&existing, false).expect_err("a directory without --force");
    assert!(matches!(err, GuardError::Exists(_)));
    assert_eq!(
        err.to_string(),
        format!(
            "setup already exists at {}; setup is intended for fresh installations only (use \
             --force to run it anyway)",
            existing.display()
        )
    );
    assert_eq!(
        setup::guard(&existing, true).expect("forced"),
        Found::Directory
    );

    let file = dir.path().join("file");
    std::fs::write(&file, "not a repository").expect("file");
    for force in [false, true] {
        let err = setup::guard(&file, force).expect_err("a file");
        assert_eq!(
            err.to_string(),
            format!("{} exists but is not a directory", file.display())
        );
    }

    // A symlink to a directory, and a dangling one: lstat sees the link.
    let link = dir.path().join("symlink");
    std::os::unix::fs::symlink(&existing, &link).expect("symlink");
    let dangling = dir.path().join("dangling");
    std::os::unix::fs::symlink(dir.path().join("nowhere"), &dangling).expect("symlink");
    for (path, force) in [(&link, false), (&link, true), (&dangling, true)] {
        let err = setup::guard(path, force).expect_err("a symlink");
        assert_eq!(
            err.to_string(),
            format!("{} exists but is a symlink", path.display())
        );
    }

    // Any other lstat error: a path through a file (ENOTDIR, root or not).
    let through = file.join("saltbox");
    let err = setup::guard(&through, true).expect_err("ENOTDIR");
    assert!(
        err.to_string().starts_with(&format!(
            "inspect existing Saltbox installation: lstat {}: ",
            through.display()
        )),
        "{err}"
    );
}

/// Without a checkout the plan is `-b`'s branch and nothing is asked.
#[tokio::test]
async fn without_a_checkout_the_plan_is_the_wanted_branch() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new();
    let (plan, asked) = plan_for(
        &world.tree.root.path().join("absent"),
        "develop",
        None,
        None,
    )
    .await;
    let plan = plan.expect("plan");
    assert_eq!(
        plan,
        Plan {
            found: Found::Nothing,
            branch: "develop".to_owned(),
            notices: Vec::new(),
            reset: true
        }
    );
    assert_eq!(asked, None);

    std::fs::create_dir_all(world.root()).expect("root");
    let (plan, asked) = plan_for(world.root(), "master", None, None).await;
    assert_eq!(plan.expect("plan").found, Found::Directory);
    assert_eq!(asked, None);

    // An empty `-b` is master, as sb-go's `SaltboxRepo` has it, rather
    // than a name git refuses after the prerequisites ran.
    let (plan, _) = plan_for(world.root(), "", None, None).await;
    assert_eq!(plan.expect("plan").branch, "master");
}

/// Over a checkout on another branch: the flags answer without asking,
/// with repo's notices; without them the question is asked, and with no
/// terminal its error names both flags.
#[tokio::test]
async fn over_a_checkout_on_another_branch_the_question_decides() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new();
    world.checkout("develop");
    let root = world.root();

    let (plan, asked) = plan_for(root, "master", Some(Answer::Reset), None).await;
    let plan = plan.expect("reset");
    assert_eq!(
        (plan.found, plan.branch.as_str()),
        (Found::Checkout, "master")
    );
    assert_eq!(plan.notices, ["Saltbox: Currently on branch 'develop'"]);
    assert_eq!(asked, None);

    let (plan, asked) = plan_for(root, "master", Some(Answer::Keep), None).await;
    let plan = plan.expect("keep");
    assert_eq!(plan.branch, "develop");
    assert_eq!(
        plan.notices,
        [
            "Saltbox: Currently on branch 'develop'",
            "Saltbox: Updating the current branch 'develop'"
        ]
    );
    assert_eq!(asked, None);

    // Asked: the current branch is shown with the question (sb-go's
    // order), and the answer is the notice (ops decision 39).
    let asking = Some((
        "Saltbox: Currently on branch 'develop'".to_owned(),
        "Saltbox: Do you want to reset to the 'master' branch?".to_owned(),
    ));
    let (plan, asked) = plan_for(root, "master", None, Some(true)).await;
    let plan = plan.expect("yes");
    assert_eq!(plan.branch, "master");
    assert_eq!(
        plan.notices,
        ["Saltbox: Reset from branch 'develop' to 'master' (your answer)"]
    );
    assert_eq!(asked, asking);
    let (plan, asked) = plan_for(root, "master", None, Some(false)).await;
    let plan = plan.expect("no");
    assert_eq!(plan.branch, "develop");
    assert_eq!(
        plan.notices,
        ["Saltbox: Kept branch 'develop' (your answer)"]
    );
    assert_eq!(asked, asking);

    let (plan, _) = plan_for(root, "master", None, None).await;
    let err = format!("{:#}", plan.expect_err("no terminal"));
    assert!(err.contains("--keep-branch or --reset-branch"), "{err}");

    // On the wanted branch nothing is asked, nothing noted.
    let (plan, asked) = plan_for(root, "develop", None, None).await;
    let plan = plan.expect("same");
    assert_eq!((plan.branch.as_str(), plan.notices.len()), ("develop", 0));
    assert_eq!(asked, None);
}

/// A kept detached HEAD, or a `-b` git refuses, fails the plan, before
/// any step; so does a `.git` that can't be inspected.
#[tokio::test]
async fn a_branch_git_refuses_fails_the_plan() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new();
    world.checkout("develop");
    let _ = git(world.root(), &["checkout", "--quiet", "--detach"]);
    let (plan, _) = plan_for(world.root(), "master", Some(Answer::Keep), None).await;
    let err = format!("{:#}", plan.expect_err("detached"));
    assert!(err.contains("'HEAD' is not a valid branch name"), "{err}");
    let (plan, _) = plan_for(world.root(), "-x", Some(Answer::Reset), None).await;
    assert!(plan.is_err());

    // `.git` through a file: ENOTDIR, not "missing".
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("root");
    std::fs::write(dir.path().join("file"), "").expect("file");
    std::os::unix::fs::symlink(dir.path().join("file"), &root).expect("link");
    let err = setup::plan(
        &root,
        Found::Directory,
        "master",
        None,
        false,
        |_, _, _| Ok(true),
        &Shutdown::new(),
    )
    .await
    .expect_err("ENOTDIR");
    assert!(
        format!("{err:#}").starts_with("inspect existing Saltbox Git repository: "),
        "{err:#}"
    );
}

/// A fresh install: every phase in sb-go's order and names, the clone at
/// `-b`'s branch, saltbox.fact, the defaults copied, the hooks run in the
/// root, the venv for accounts.yml's user.
#[tokio::test]
async fn a_fresh_install_runs_every_phase_in_sb_gos_order() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new();
    let plan = Plan {
        found: Found::Nothing,
        branch: "develop".to_owned(),
        notices: Vec::new(),
        reset: true,
    };
    let (ok, screen) = world.run(&plan, supported).await;
    assert!(ok, "{screen}");
    in_order(
        &screen,
        &[
            "Saltbox installation completed",
            "Checking system compatibility completed",
            "Checking Ubuntu version",
            "Checking CPU architecture",
            "Checking for LXC container",
            "Checking for desktop environment",
            "Installing system prerequisites completed",
            "Updating apt package cache",
            "Configuring system locale completed",
            "Preparing Saltbox repository completed",
            "Cloning Saltbox repository to",
            "(branch: develop)",
            "Updating git submodules",
            "saltbox.fact is ready",
            "Copying accounts.yml.default",
            "Copying settings.yml.default",
            "Activating Git hooks",
            "Installing Python runtime completed",
            "Ansible virtual environment",
            // The completion refresh after it (ops spec, setup step 4; M87).
            "Shell completions checked",
            "Shell completions refreshed",
        ],
    );
    assert!(!screen.contains("refresh failed"), "{screen}");
    assert_eq!(world.branch(), ("develop".to_owned(), "develop".to_owned()));
    assert!(world.tree.layout.fact.is_file());
    for name in ["accounts.yml", "settings.yml"] {
        assert!(world.root().join(name).is_file(), "{name}");
    }
    let programs: Vec<String> = world
        .host
        .calls()
        .iter()
        .map(|call| call.split(' ').next().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        programs,
        [
            "systemd-detect-virt",
            "dpkg-query",
            "apt-get",
            "apt-get",
            "apt-get",
            "lsb_release",
            "apt-get",
            "apt-get",
            "locale",
            "locale-gen",
            "update-locale",
            "bash"
        ],
        "{:?}",
        world.host.calls()
    );
}

/// A completion refresh that fails is a warning, never setup's failure
/// (ops spec, setup step 4).
#[tokio::test]
async fn a_failed_completion_refresh_only_warns() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new();
    let plan = Plan {
        found: Found::Nothing,
        branch: "master".to_owned(),
        notices: Vec::new(),
        reset: true,
    };
    let (ok, screen) = world.run_refreshing(&plan, supported, "missing").await;
    assert!(ok, "{screen}");
    in_order(
        &screen,
        &[
            "Saltbox installation completed",
            "Shell completions checked",
            "! Shell completion refresh failed: resolve completion executable: ",
        ],
    );
}

/// `--force` over a directory without `.git`: repo's init at the branch,
/// what was there kept.
#[tokio::test]
async fn a_forced_run_over_a_directory_makes_it_a_checkout() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new();
    std::fs::create_dir_all(world.root()).expect("root");
    std::fs::write(
        world.root().join("accounts.yml"),
        format!("user:\n  name: {USER}\n"),
    )
    .expect("accounts");
    let plan = Plan {
        found: Found::Directory,
        branch: "master".to_owned(),
        notices: vec!["a notice".to_owned()],
        reset: true,
    };
    let (ok, screen) = world.run(&plan, supported).await;
    assert!(ok, "{screen}");
    in_order(
        &screen,
        &[
            "a notice",
            "Initializing Git repository",
            "Resetting to branch master",
            "saltbox.fact is ready",
            "Copying settings.yml.default",
        ],
    );
    assert!(!screen.contains("Copying accounts.yml.default"), "{screen}");
    assert_eq!(world.branch(), ("master".to_owned(), "master".to_owned()));
}

/// A failing phase ends setup with its failing step's words (the
/// phase's own wrap a quiet exit, as gha's); nothing after it runs.
#[tokio::test]
async fn a_failing_phase_ends_setup_with_its_words() {
    let _spawns = SPAWNS.lock().await;
    let fresh = Plan {
        found: Found::Nothing,
        branch: "master".to_owned(),
        notices: Vec::new(),
        reset: true,
    };
    // An unsupported release: the first check, nothing run.
    let world = World::new();
    let (ok, screen) = world
        .run(&fresh, || sb_host::setup_check("ubuntu", Some("22.04")))
        .await;
    assert!(!ok);
    assert!(screen.contains("Checking system compatibility"), "{screen}");
    assert!(screen.contains("22.04"), "{screen}");
    // A failed setup ends before the completion refresh.
    assert!(!screen.contains("Shell completion"), "{screen}");
    assert!(world.host.calls().is_empty(), "{:?}", world.host.calls());

    // An LXC container: the desktop check and apt never run.
    let world = World::new();
    let _ = world.host.stand_in("systemd-detect-virt", "echo lxc");
    let (ok, screen) = world.run(&fresh, supported).await;
    assert!(!ok);
    assert!(
        screen.contains("Running in an LXC container is not supported"),
        "{screen}"
    );
    assert_eq!(world.host.calls(), ["systemd-detect-virt -c"]);

    // The desktop installed.
    let world = World::new();
    let _ = world.host.stand_in("dpkg-query", "printf ii");
    let (ok, screen) = world.run(&fresh, supported).await;
    assert!(!ok);
    assert!(
        screen.contains("Only Ubuntu Server is supported"),
        "{screen}"
    );
    assert!(!world.host.calls().iter().any(|c| c.starts_with("apt-get")));

    // The prerequisites.
    let world = World::new();
    world.host.fail("apt-get");
    let (ok, screen) = world.run(&fresh, supported).await;
    assert!(!ok);
    assert!(screen.contains("error updating apt cache"), "{screen}");
    assert!(!world.host.calls().iter().any(|c| c.starts_with("locale")));

    // The locale.
    let world = World::new();
    world.host.fail("locale-gen");
    let (ok, screen) = world.run(&fresh, supported).await;
    assert!(!ok);
    assert!(screen.contains("error generating locale"), "{screen}");
    assert!(!screen.contains("Cloning Saltbox repository"), "{screen}");

    // The clone: a branch the remote doesn't have.
    let world = World::new();
    let missing = Plan {
        branch: "nope".to_owned(),
        ..fresh.clone()
    };
    let (ok, screen) = world.run(&missing, supported).await;
    assert!(!ok);
    assert!(
        screen.contains("failed to clone Saltbox repository"),
        "{screen}"
    );
    assert!(!screen.contains("Activating Git hooks"), "{screen}");
    assert!(!world.host.calls().iter().any(|c| c.starts_with("bash")));

    // The hooks.
    let world = World::new();
    world.host.fail("bash");
    let (ok, screen) = world.run(&fresh, supported).await;
    assert!(!ok);
    assert!(screen.contains("error activating Git hooks"), "{screen}");
    assert!(!screen.contains("Installing Python runtime"), "{screen}");

    // The Saltbox user: accounts.yml's default without a user.
    let world = World::new();
    let _ = git(&world.remote.work(), &["checkout", "--quiet", "master"]);
    world
        .remote
        .commit("defaults/accounts.yml.default", "other: 1\n");
    let (ok, screen) = world.run(&fresh, supported).await;
    assert!(!ok);
    // The runner shows the chain a cause a line.
    assert!(
        screen.contains("get Saltbox user\n")
            && screen.contains("user section not found in accounts.yml"),
        "{screen}"
    );
}

/// The flags' answer: `--keep-branch` keeps, `--reset-branch` resets,
/// neither asks.
#[test]
fn the_branch_flags_answer_the_question() {
    use clap::Parser as _;
    #[derive(clap::Parser)]
    struct Cli {
        #[command(flatten)]
        args: setup::SetupArgs,
    }
    let parse = |args: &[&str]| Cli::try_parse_from([&["sb"], args].concat()).map(|cli| cli.args);
    let args = parse(&[]).expect("no flags");
    assert_eq!(
        (args.branch.as_str(), args.force, args.verbose),
        ("master", false, false)
    );
    assert_eq!(args.branch_flags.answer(), None);
    assert_eq!(
        parse(&["--keep-branch"])
            .expect("keep")
            .branch_flags
            .answer(),
        Some(Answer::Keep)
    );
    assert_eq!(
        parse(&["--reset-branch"])
            .expect("reset")
            .branch_flags
            .answer(),
        Some(Answer::Reset)
    );
    assert!(parse(&["--keep-branch", "--reset-branch"]).is_err());
    let args = parse(&["-b", "develop", "--force", "-v"]).expect("all");
    assert_eq!(
        (args.branch.as_str(), args.force, args.verbose),
        ("develop", true, true)
    );
}

/// Over a checkout with local changes (ops decision 42): asked after the
/// branch, as update asks; a no leaves the checkout as it is and the rest
/// of setup runs; a yes, or `--discard-local`, resets it, noted; without
/// a terminal, refused naming the counts and `--discard-local`.
#[tokio::test]
async fn local_changes_over_a_checkout_are_asked_about() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new();
    world.checkout("master");
    let root = world.root();
    std::fs::write(root.join("branch.txt"), "edited\n").expect("edit");
    std::fs::write(root.join("notes.txt"), "mine\n").expect("untracked");
    let summary = "1 tracked change and 1 untracked file";

    let (plan, asked) = plan_asking(root, "master", None, false, &[Some(false)]).await;
    let plan = plan.expect("declined");
    assert_eq!(
        asked,
        [(
            format!(
                "Saltbox: updating to 'master' would discard {summary}:\n  branch.txt\n  \
                 notes.txt\nDeclining leaves Saltbox as it is; the rest of setup goes on."
            ),
            "Saltbox: Discard these local changes and update?".to_owned()
        )]
    );
    assert!(!plan.reset);
    assert_eq!(
        plan.notices,
        [format!(
            "Saltbox: Left as is, keeping {summary} (your answer)"
        )]
    );
    std::fs::write(
        root.join("accounts.yml"),
        format!("user:\n  name: {USER}\n"),
    )
    .expect("accounts");
    let (ok, screen) = world.run(&plan, supported).await;
    assert!(ok, "{screen}");
    assert!(!screen.contains("Updating Saltbox repository"), "{screen}");
    in_order(
        &screen,
        &[
            "Left as is, keeping",
            "Preparing Saltbox repository",
            "saltbox.fact is ready",
        ],
    );
    assert_eq!(
        std::fs::read_to_string(root.join("branch.txt")).expect("kept"),
        "edited\n"
    );
    assert!(root.join("notes.txt").exists());

    let (plan, _) = plan_asking(root, "master", None, false, &[None]).await;
    let err = format!("{:#}", plan.expect_err("no terminal"));
    assert_eq!(
        err,
        format!(
            "can't ask before discarding local changes: no terminal; answer with \
             --discard-local (Saltbox has {summary} that the setup would discard)"
        )
    );

    let (plan, asked) = plan_asking(root, "master", None, true, &[]).await;
    let plan = plan.expect("discard");
    assert!(asked.is_empty(), "{asked:?}");
    assert!(plan.reset);
    assert_eq!(
        plan.notices,
        [format!("Saltbox: Discarding {summary} (--discard-local)")]
    );
    let (ok, screen) = world.run(&plan, supported).await;
    assert!(ok, "{screen}");
    assert!(!root.join("notes.txt").exists(), "{screen}");
    assert_eq!(world.branch(), ("master".to_owned(), "master".to_owned()));
}

/// Declined after the branch question's reset, the checkout stays on its
/// branch: only the answer is noted, not the reset it didn't get.
#[tokio::test]
async fn a_declined_checkout_isnt_noted_as_reset() {
    let _spawns = SPAWNS.lock().await;
    let world = World::new();
    world.checkout("develop");
    let root = world.root();
    std::fs::write(root.join("notes.txt"), "mine\n").expect("untracked");
    let (plan, asked) =
        plan_asking(root, "master", Some(Answer::Reset), false, &[Some(false)]).await;
    let plan = plan.expect("declined");
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(!plan.reset);
    assert_eq!(
        plan.notices,
        ["Saltbox: Left as is, keeping 1 untracked file (your answer)"]
    );
}
