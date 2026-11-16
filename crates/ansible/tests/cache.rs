//! The tag cache against real git on a local checkout and the fake
//! `ansible-playbook` (ansible spec, "The tag cache"; module build M32):
//! the listing in its step, the commit check through repo's `head`, the
//! write, a stale or corrupt cache rebuilt, a failed listing shown. The
//! checkout is a temp dir (`git init`, one commit); no Ansible runs and
//! nothing touches a Saltbox checkout.

use std::path::{Path, PathBuf};
use std::process::Command;

use sb_ansible::{Ansible, CacheError, LazyRunner, Repo, TagCache};
use sb_cli_core::cancel::Shutdown;
use sb_term_ui::{Labels, Options, Runner, Target};

const FAKE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/fake-ansible-playbook"
);
const LISTING: &str = include_str!("fixtures/list-tags.txt");
const SALTBOX_TAGS: [&str; 7] = [
    "always",
    "authelia",
    "docker",
    "plex",
    "sanity-check",
    "sonarr",
    "traefik",
];

/// `git <args>` in `dir`, which must succeed; its stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
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
        .args(["-c", "user.name=sb", "-c", "user.email=sb"])
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
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A Saltbox checkout with one commit, the fake's settings directory in
/// it (untracked: HEAD doesn't change), and the cache at its root.
struct World {
    dir: tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("temp dir");
        let world = Self { dir };
        std::fs::create_dir_all(world.settings()).expect("settings");
        git(&world.root(), &["init", "--quiet"]);
        world.commit("one");
        world.set("FAKE_ANSIBLE_LISTING", LISTING);
        world
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("saltbox")
    }

    fn settings(&self) -> PathBuf {
        self.root().join(".fake-ansible")
    }

    fn repo(&self) -> Repo {
        Repo::saltbox(&self.root())
    }

    fn checkout(&self) -> sb_repo::Repo {
        sb_repo::Repo::open("Saltbox", self.root()).expect("checkout")
    }

    /// A new commit; its hash.
    fn commit(&self, text: &str) -> String {
        std::fs::write(self.root().join("roles.txt"), text).expect("write");
        git(&self.root(), &["add", "roles.txt"]);
        git(&self.root(), &["commit", "--quiet", "-m", text]);
        git(&self.root(), &["rev-parse", "HEAD"])
    }

    /// One of the fake's settings, as a file it reads in the checkout.
    fn set(&self, name: &str, value: &str) {
        std::fs::write(self.settings().join(name), value).expect("a setting");
    }

    /// How many times the fake ran, and each run's arguments.
    fn runs(&self) -> Vec<Vec<String>> {
        let mut runs = Vec::new();
        for n in 0.. {
            let Ok(bytes) = std::fs::read(self.settings().join(format!("argv.{n}"))) else {
                break;
            };
            let text = String::from_utf8(bytes).expect("UTF-8");
            let elements: Vec<String> = text.split('\0').map(str::to_string).collect();
            // The working directory and `$0` first, and a NUL at the end.
            runs.push(elements[2..elements.len() - 1].to_vec());
        }
        runs
    }

    fn cache_text(&self) -> Option<String> {
        std::fs::read_to_string(self.root().join(sb_ansible::CACHE_FILE)).ok()
    }
}

/// What a call left: its result and the screen.
struct Ran<T> {
    result: Result<T, String>,
    screen: String,
}

/// `refresh` (when `refresh`) or `tags` on `cache` for `world`'s Saltbox,
/// in a step of a runner drawing to memory.
async fn call(world: &World, cache: &mut TagCache, refresh: bool) -> Ran<(Vec<String>, bool)> {
    call_with(world, cache, refresh, &Shutdown::new()).await
}

/// [`call`] under `shutdown`.
async fn call_with(
    world: &World,
    cache: &mut TagCache,
    refresh: bool,
    shutdown: &Shutdown,
) -> Ran<(Vec<String>, bool)> {
    let term = indicatif::InMemoryTerm::new(40, 300);
    let options = Options {
        target: Target::InMemory {
            term: term.clone(),
            colour: false,
        },
    };
    let mut runner = Runner::start("sb test", shutdown, None, options);
    let ansible = Ansible::with_program(FAKE);
    let (repo, checkout) = (world.repo(), world.checkout());
    let mut result = None;
    let _ = runner
        .step(
            Labels::new("Updating Saltbox", "Saltbox updated"),
            async |step| {
                let listed = if refresh {
                    cache
                        .refresh(&ansible, &repo, &checkout, step)
                        .await
                        .map(|tags| (tags, false))
                } else {
                    cache
                        .tags(&ansible, &repo, &checkout, step)
                        .await
                        .map(|listed| (listed.tags, listed.cached))
                };
                let shown = listed.as_ref().map_err(error_text).cloned();
                result = Some(shown);
                listed.map(drop).map_err(anyhow::Error::from)
            },
        )
        .await;
    runner.finish();
    Ran {
        result: result.expect("the body ran"),
        screen: term.contents(),
    }
}

/// The error's kind and text, for asserting on.
fn error_text(err: &CacheError) -> String {
    match err {
        CacheError::Head(err) => format!("head: {err}"),
        CacheError::Step(exit) => format!("step: {exit}"),
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(ToString::to_string).collect()
}

/// No cache: the listing runs in its step, sb-go's label, with the one
/// listing every writer uses; the entry is written for HEAD. The next
/// call, from a fresh load, uses the cache and runs nothing.
#[tokio::test]
async fn a_missing_cache_is_rebuilt_then_used() {
    let world = World::new();
    let head = git(&world.root(), &["rev-parse", "HEAD"]);
    let mut cache = TagCache::load(&world.root());
    let ran = call(&world, &mut cache, false).await;
    assert_eq!(
        ran.result,
        Ok((strings(&SALTBOX_TAGS), false)),
        "{}",
        ran.screen
    );
    assert!(
        ran.screen.contains("✓ Updating Saltbox tags cache"),
        "{}",
        ran.screen
    );
    assert_eq!(
        world.runs(),
        [strings(&[
            "saltbox.yml",
            "--become",
            "--list-tags",
            "--skip-tags=always"
        ])]
    );
    let written: serde_json::Value =
        serde_json::from_str(&world.cache_text().expect("written")).expect("JSON");
    let key = world.root().display().to_string();
    assert_eq!(
        written,
        serde_json::json!({ key: { "commit": head, "tags": SALTBOX_TAGS } })
    );

    let mut again = TagCache::load(&world.root());
    let ran = call(&world, &mut again, false).await;
    assert_eq!(ran.result, Ok((strings(&SALTBOX_TAGS), true)));
    assert!(!ran.screen.contains("tags cache"), "{}", ran.screen);
    assert_eq!(world.runs().len(), 1, "no second listing");
}

/// A new commit makes the entry stale: rebuilt and written for the new
/// HEAD (invalidated by commit only).
#[tokio::test]
async fn a_stale_commit_rebuilds() {
    let world = World::new();
    let mut cache = TagCache::load(&world.root());
    assert!(call(&world, &mut cache, false).await.result.is_ok());
    let head = world.commit("two");
    world.set("FAKE_ANSIBLE_LISTING", "      TASK TAGS: [always, plex]\n");
    let mut cache = TagCache::load(&world.root());
    let ran = call(&world, &mut cache, false).await;
    assert_eq!(ran.result, Ok((strings(&["always", "plex"]), false)));
    assert_eq!(world.runs().len(), 2);
    let entry = TagCache::load(&world.root())
        .entry(&world.repo())
        .expect("entry");
    assert_eq!(entry.commit, head);
}

/// `refresh` lists again whatever the cache holds.
#[tokio::test]
async fn refresh_always_rebuilds() {
    let world = World::new();
    let mut cache = TagCache::load(&world.root());
    assert!(call(&world, &mut cache, false).await.result.is_ok());
    let ran = call(&world, &mut cache, true).await;
    assert_eq!(ran.result, Ok((strings(&SALTBOX_TAGS), false)));
    assert!(
        ran.screen.contains("✓ Updating Saltbox tags cache"),
        "{}",
        ran.screen
    );
    assert_eq!(world.runs().len(), 2);
}

/// A corrupt file is treated as empty, rebuilt and written over.
#[tokio::test]
async fn a_corrupt_cache_is_rebuilt_and_written_over() {
    let world = World::new();
    std::fs::write(world.root().join(sb_ansible::CACHE_FILE), "{not json").expect("corrupt");
    let mut cache = TagCache::load(&world.root());
    let ran = call(&world, &mut cache, false).await;
    assert_eq!(ran.result, Ok((strings(&SALTBOX_TAGS), false)));
    let text = world.cache_text().expect("written");
    assert!(
        serde_json::from_str::<serde_json::Value>(&text).is_ok(),
        "{text}"
    );
}

/// A listing that fails ends its step ✗ with stderr's last lines (exec's
/// `Failed`); one with no `TASK TAGS:` line with sb-go's text. Nothing is
/// written.
#[tokio::test]
async fn a_failed_listing_fails_its_step() {
    let world = World::new();
    world.set("FAKE_ANSIBLE_EXIT", "4");
    world.set(
        "FAKE_ANSIBLE_STDERR",
        "ERROR! the playbook could not be parsed",
    );
    let mut cache = TagCache::load(&world.root());
    let ran = call(&world, &mut cache, false).await;
    assert_eq!(ran.result, Err("step: exit code 1".to_string()));
    assert!(
        ran.screen.contains("✗ Updating Saltbox tags cache"),
        "{}",
        ran.screen
    );
    assert!(
        ran.screen
            .contains("ERROR! the playbook could not be parsed"),
        "{}",
        ran.screen
    );
    assert_eq!(world.cache_text(), None);

    let world = World::new();
    world.set("FAKE_ANSIBLE_LISTING", "playbook: saltbox.yml\n");
    let mut cache = TagCache::load(&world.root());
    let ran = call(&world, &mut cache, false).await;
    assert!(ran.result.is_err());
    assert!(
        ran.screen
            .contains("'TASK TAGS:' not found in the ansible-playbook output"),
        "{}",
        ran.screen
    );
    assert_eq!(world.cache_text(), None);
}

/// A checkout whose HEAD can't be read (an unborn branch) is repo's
/// error, before any listing.
#[tokio::test]
async fn an_unreadable_head_is_an_error_before_any_listing() {
    let world = World::new();
    // An unborn branch: a repository with no commit yet. (Removing `.git`
    // wouldn't do: git would find the repository the temp dir is in.)
    std::fs::remove_dir_all(world.root().join(".git")).expect("no repository");
    git(&world.root(), &["init", "--quiet"]);
    let mut cache = TagCache::load(&world.root());
    let ran = call(&world, &mut cache, false).await;
    let err = ran.result.expect_err("no HEAD");
    assert!(err.starts_with("head: "), "{err}");
    assert_eq!(world.runs(), Vec::<Vec<String>>::new());
}

/// Ctrl-C while the listing runs: the child's group is stopped, the step
/// ends interrupted, the error comes back, and nothing is written (spec,
/// step 8).
#[tokio::test]
async fn ctrl_c_during_a_listing_writes_nothing() {
    let world = World::new();
    world.set("FAKE_ANSIBLE_MODE", "wait");
    let shutdown = Shutdown::new();
    let ctrl_c = shutdown.clone();
    let pid = world.settings().join("argv.pid");
    // What cli-core's signals thread does for SIGINT, once the listing
    // runs.
    let pressed = tokio::spawn(async move {
        while !pid.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        ctrl_c.signalled(2);
    });
    let mut cache = TagCache::load(&world.root());
    let ran = call_with(&world, &mut cache, false, &shutdown).await;
    pressed.await.expect("pressed");
    assert_eq!(ran.result, Err("step: exit code 1".to_string()));
    assert!(ran.screen.contains("interrupted"), "{}", ran.screen);
    assert!(
        world.settings().join("argv.term").exists(),
        "the listing got SIGTERM"
    );
    assert_eq!(world.cache_text(), None);
}

/// Sandbox's tags in the colour fixture, Sandbox's own listing.
const SANDBOX_LISTING: &str = include_str!("fixtures/list-tags-colour.txt");

/// A Sandbox checkout under `world`'s appdata root (`<dir>/appdata`), one
/// commit, listing `listing`.
fn sandbox_checkout(world: &World, listing: &str) -> PathBuf {
    let dir = world.dir.path().join("appdata/sandbox");
    std::fs::create_dir_all(dir.join(".fake-ansible")).expect("sandbox");
    git(&dir, &["init", "--quiet"]);
    std::fs::write(dir.join("roles.txt"), "one").expect("write");
    git(&dir, &["add", "roles.txt"]);
    git(&dir, &["commit", "--quiet", "-m", "one"]);
    std::fs::write(dir.join(".fake-ansible/FAKE_ANSIBLE_LISTING"), listing).expect("listing");
    dir
}

/// `world`'s repositories: Saltbox at its root, the appdata root beside it.
fn repos(world: &World) -> sb_ansible::Repos {
    sb_ansible::Repos::new(&world.root(), &world.dir.path().join("appdata"))
}

/// What a call on a lazy runner left: its result, whether the runner
/// started, and the screen.
struct Lazy<T> {
    result: T,
    started: bool,
    screen: String,
}

/// `body` with a lazy runner drawing to memory, finished after it.
async fn lazily<T>(
    body: impl AsyncFnOnce(&mut LazyRunner<Box<dyn FnOnce() -> Runner>>) -> T,
) -> Lazy<T> {
    lazily_with(false, body).await
}

/// [`lazily`], the runner's steps transient when `transient` is set
/// (`sb list`'s, ansible spec decision 18).
async fn lazily_with<T>(
    transient: bool,
    body: impl AsyncFnOnce(&mut LazyRunner<Box<dyn FnOnce() -> Runner>>) -> T,
) -> Lazy<T> {
    let term = indicatif::InMemoryTerm::new(40, 300);
    let shutdown = Shutdown::new();
    let start: Box<dyn FnOnce() -> Runner> = {
        let (term, shutdown) = (term.clone(), shutdown.clone());
        Box::new(move || {
            let options = Options {
                target: Target::InMemory {
                    term,
                    colour: false,
                },
            };
            Runner::start("sb install", &shutdown, None, options)
        })
    };
    let mut runner = LazyRunner::new(shutdown.token(), start);
    if transient {
        runner = runner.transient();
    }
    let result = body(&mut runner).await;
    let started = runner.started();
    // Dropping it ends the drawing.
    drop(runner);
    Lazy {
        result,
        started,
        screen: term.contents(),
    }
}

/// With no parent step (`install`, `list`): a valid cache starts no
/// runner and prints nothing; a stale one starts it, the rebuild a
/// top-level step under the command's title (the M32 follow-up).
#[tokio::test]
async fn tags_on_a_lazy_runner_start_it_only_to_rebuild() {
    let world = World::new();
    let ansible = Ansible::with_program(FAKE);
    let (repo, checkout) = (world.repo(), world.checkout());
    let mut cache = TagCache::load(&world.root());
    let ran = lazily(async |runner| {
        assert_eq!(format!("{runner:?}"), "LazyRunner { started: false, .. }");
        let listed = cache
            .tags_on(&ansible, &repo, &checkout, runner)
            .await
            .map(|listed| (listed.tags, listed.cached))
            .map_err(|err| error_text(&err));
        assert_eq!(format!("{runner:?}"), "LazyRunner { started: true, .. }");
        listed
    })
    .await;
    assert_eq!(ran.result, Ok((strings(&SALTBOX_TAGS), false)));
    assert!(ran.started);
    let lines: Vec<&str> = ran.screen.lines().map(str::trim_end).collect();
    assert_eq!(lines[0], "sb install", "{}", ran.screen);
    assert!(
        lines[1].starts_with("╰── ✓ Updating Saltbox tags cache"),
        "{}",
        ran.screen
    );

    let mut cache = TagCache::load(&world.root());
    let ran = lazily(async |runner| {
        cache
            .tags_on(&ansible, &repo, &checkout, runner)
            .await
            .map(|listed| (listed.tags, listed.cached))
            .map_err(|err| error_text(&err))
    })
    .await;
    assert_eq!(ran.result, Ok((strings(&SALTBOX_TAGS), true)));
    assert!(!ran.started);
    assert_eq!(ran.screen.trim(), "", "nothing drawn");
    assert_eq!(world.runs().len(), 1, "no second listing");
}

/// A transient lazy runner (`sb list`'s, decision 18): a rebuild that
/// succeeds leaves nothing, title included; one that fails stays under
/// the title with its cause.
#[tokio::test]
async fn tags_on_a_transient_lazy_runner_keep_only_a_failure() {
    let world = World::new();
    let ansible = Ansible::with_program(FAKE);
    let (repo, checkout) = (world.repo(), world.checkout());
    let mut cache = TagCache::load(&world.root());
    let ran = lazily_with(true, async |runner| {
        cache
            .tags_on(&ansible, &repo, &checkout, runner)
            .await
            .map(|listed| listed.cached)
            .map_err(|err| error_text(&err))
    })
    .await;
    assert_eq!(ran.result, Ok(false));
    assert!(ran.started);
    assert_eq!(ran.screen.trim(), "", "{}", ran.screen);

    std::fs::remove_file(world.root().join(sb_ansible::CACHE_FILE)).expect("stale again");
    world.set("FAKE_ANSIBLE_EXIT", "4");
    let mut cache = TagCache::load(&world.root());
    let ran = lazily_with(true, async |runner| {
        cache
            .tags_on(&ansible, &repo, &checkout, runner)
            .await
            .map(|listed| listed.cached)
            .map_err(|err| error_text(&err))
    })
    .await;
    let err = ran.result.expect_err("a failed rebuild");
    assert!(err.starts_with("step:"), "{err}");
    assert!(ran.screen.contains("exit 4"), "{}", ran.screen);
    let lines: Vec<&str> = ran.screen.lines().map(str::trim_end).collect();
    assert_eq!(lines[0], "sb install", "{}", ran.screen);
    assert!(
        lines[1].starts_with("╰── ✗ Updating Saltbox tags cache"),
        "{}",
        ran.screen
    );
}

/// `known_tags`: Saltbox's and Sandbox's tags, each rebuilt as a step
/// when stale, Saltbox first.
#[tokio::test]
async fn known_tags_are_saltboxs_and_sandboxs() {
    let world = World::new();
    sandbox_checkout(&world, SANDBOX_LISTING);
    let ansible = Ansible::with_program(FAKE);
    let mut cache = TagCache::load(&world.root());
    let ran = lazily(async |runner| {
        sb_ansible::known_tags(&mut cache, &ansible, &repos(&world), runner).await
    })
    .await;
    let known = ran.result.expect("known");
    assert_eq!(known.saltbox, strings(&SALTBOX_TAGS));
    assert_eq!(
        known.sandbox,
        strings(&["always", "autobrr", "bazarr", "overseerr", "tautulli"])
    );
    let saltbox = ran.screen.find("Updating Saltbox tags cache");
    let sandbox = ran.screen.find("Updating Sandbox tags cache");
    assert!(saltbox.is_some() && saltbox < sandbox, "{}", ran.screen);
}

/// Sandbox's tags that can't be had count as none: no checkout, or a
/// failed listing (its step shows ✗, and validation goes on).
#[tokio::test]
async fn sandboxs_failure_counts_as_no_tags() {
    let world = World::new();
    let ansible = Ansible::with_program(FAKE);
    let mut cache = TagCache::load(&world.root());
    let ran = lazily(async |runner| {
        sb_ansible::known_tags(&mut cache, &ansible, &repos(&world), runner).await
    })
    .await;
    let known = ran.result.expect("no Sandbox is no tags");
    assert_eq!(known.saltbox, strings(&SALTBOX_TAGS));
    assert_eq!(known.sandbox, Vec::<String>::new());
    assert!(!ran.screen.contains("Sandbox"), "{}", ran.screen);

    let world = World::new();
    sandbox_checkout(&world, "playbook: sandbox.yml\n");
    let mut cache = TagCache::load(&world.root());
    let ran = lazily(async |runner| {
        sb_ansible::known_tags(&mut cache, &ansible, &repos(&world), runner).await
    })
    .await;
    assert_eq!(ran.result.expect("lenient").sandbox, Vec::<String>::new());
    assert!(
        ran.screen.contains("✗ Updating Sandbox tags cache"),
        "{}",
        ran.screen
    );
}

/// Saltbox's tags that can't be had, or none, are fatal, with sb-go's
/// text: no checkout (repo's error the cause), an unreadable HEAD, a
/// failed listing (its step shows why), an empty listing.
#[tokio::test]
async fn saltboxs_failure_or_no_tags_is_fatal() {
    let ansible = Ansible::with_program(FAKE);
    let fatal = "saltbox install appears broken: tags cache missing or empty";
    let causes = |err: &sb_ansible::SaltboxBroken| -> Vec<String> {
        sb_cli_core::exit::chain(err)
            .skip(1)
            .map(ToString::to_string)
            .collect()
    };

    let world = World::new();
    std::fs::remove_dir_all(world.root()).expect("no Saltbox");
    let mut cache = TagCache::load(&world.root());
    let ran = lazily(async |runner| {
        sb_ansible::known_tags(&mut cache, &ansible, &repos(&world), runner).await
    })
    .await;
    let err = ran.result.expect_err("fatal");
    assert_eq!(err.to_string(), fatal);
    assert_eq!(causes(&err).len(), 1, "repo's error");
    assert!(!ran.started);

    let world = World::new();
    std::fs::remove_dir_all(world.root().join(".git")).expect("no repository");
    git(&world.root(), &["init", "--quiet"]);
    let mut cache = TagCache::load(&world.root());
    let ran = lazily(async |runner| {
        sb_ansible::known_tags(&mut cache, &ansible, &repos(&world), runner).await
    })
    .await;
    let err = ran.result.expect_err("fatal");
    assert_eq!(err.to_string(), fatal);
    assert!(
        causes(&err).first().is_some_and(|c| c.contains("HEAD")),
        "{:?}",
        causes(&err)
    );

    for (exit, listing) in [("4", LISTING), ("0", "      TASK TAGS: []\n")] {
        let world = World::new();
        sandbox_checkout(&world, SANDBOX_LISTING);
        world.set("FAKE_ANSIBLE_EXIT", exit);
        world.set("FAKE_ANSIBLE_LISTING", listing);
        let mut cache = TagCache::load(&world.root());
        let ran = lazily(async |runner| {
            sb_ansible::known_tags(&mut cache, &ansible, &repos(&world), runner).await
        })
        .await;
        let err = ran.result.expect_err("fatal");
        assert_eq!(err.to_string(), fatal, "{exit}");
        assert_eq!(
            causes(&err),
            Vec::<String>::new(),
            "the step showed why: {exit}"
        );
        assert!(
            !ran.screen.contains("Sandbox"),
            "Sandbox isn't read: {}",
            ran.screen
        );
    }
}
