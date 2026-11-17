//! setup's and gha's shared pieces against the harness (ops spec, Testing
//! Strategy: "Sequences", "locale", "default configs"): stand-in programs
//! recording their calls and a temporary root; nothing touches the host's
//! apt, sources, locale or Saltbox root.

use std::os::unix::fs::PermissionsExt as _;

use sb_cli_core::cancel::Shutdown;
use sb_ops::testing::Host;
use sb_ops::{configs, hooks, locale, prereq};
use sb_term_ui::{Labels, Options, Runner, Step, Target};

/// Tests that write a stand-in program and run it hold this from writing
/// it until its run is over (ETXTBSY otherwise).
static SPAWNS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// What a run left: its body's error chain, if any, and the screen.
struct Ran {
    error: Option<String>,
    screen: String,
}

/// `body` in a step of a runner drawing to memory.
async fn in_step(body: impl AsyncFnOnce(&mut Step) -> anyhow::Result<()>) -> Ran {
    let term = indicatif::InMemoryTerm::new(80, 200);
    let options = Options {
        target: Target::InMemory {
            term: term.clone(),
            colour: false,
        },
    };
    let shutdown = Shutdown::new();
    let mut runner = Runner::start("sb test", &shutdown, None, options);
    let mut error = None;
    let _ = runner
        .step(Labels::new("Shared", "Shared done"), async |step| {
            let result = body(step).await;
            error = result.as_ref().err().map(|err| format!("{err:#}"));
            result
        })
        .await;
    runner.finish();
    Ran {
        error,
        screen: term.contents(),
    }
}

/// A call without apt-get's `-o <option>` pairs, which every apt-get call
/// carries (host's policy).
fn short(call: &str) -> String {
    let mut words = call.split(' ');
    let mut kept = Vec::new();
    while let Some(word) = words.next() {
        if word == "-o" {
            let _ = words.next();
        } else {
            kept.push(word);
        }
    }
    kept.join(" ")
}

fn mode(path: &std::path::Path) -> u32 {
    std::fs::symlink_metadata(path)
        .expect("stat")
        .permissions()
        .mode()
        & 0o7777
}

#[tokio::test]
async fn prerequisites_run_sb_gos_sequence() {
    let _spawns = SPAWNS.lock().await;
    let host = Host::new();
    std::fs::write(host.sources_d().join("docker.list"), "deb x y\n").expect("source");
    let (apt, dirs) = (host.apt(), host.dirs());
    let ran = in_step(async |step| prereq::prerequisites(step, &apt, &dirs, "run1").await).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    let calls: Vec<String> = host.calls().iter().map(|c| short(c)).collect();
    let packages = prereq::PACKAGES.join(" ");
    assert_eq!(
        calls,
        [
            "apt-get update".to_string(),
            "apt-get install -y git curl".to_string(),
            "apt-get install -y software-properties-common apt-transport-https".to_string(),
            "lsb_release -sc".to_string(),
            "apt-get update".to_string(),
            format!("apt-get install -y {packages}"),
        ]
    );
    // Each step under sb-go's label, in sb-go's order: the directories
    // between the first two installs.
    let labels = [
        "Updating apt package cache ".to_string(),
        "Installing git and curl".to_string(),
        format!("Creating directory {}", dirs.git.display()),
        format!("Creating directory {}", dirs.ansible.display()),
        "Installing software-properties-common and apt-transport-https".to_string(),
        "Adding apt repositories".to_string(),
        "Updating apt package cache again".to_string(),
        "Installing additional required packages".to_string(),
    ];
    let mut at = 0;
    for label in &labels {
        let found = ran.screen[at..]
            .find(label.as_str())
            .unwrap_or_else(|| panic!("{label} after {at}:\n{}", ran.screen));
        at += found + label.len();
    }
    // Both there; the one the harness didn't make (it holds the Saltbox
    // checkout under /srv/git) got 0755, less a umask: never writable by
    // group or others.
    assert!(dirs.git.is_dir());
    assert!(dirs.ansible.is_dir());
    assert_eq!(mode(&dirs.ansible) & 0o022, 0);
    assert_eq!(mode(&dirs.ansible) & 0o700, 0o700);
    // The destructive step: the old source moved aside, not deleted.
    assert!(!host.sources_d().join("docker.list").exists());
    let moved = host.backups().join("apt-sources/run1/docker.list");
    assert_eq!(std::fs::read_to_string(moved).expect("moved"), "deb x y\n");
}

#[tokio::test]
async fn a_failing_update_stops_the_prerequisites_with_sb_gos_words() {
    let _spawns = SPAWNS.lock().await;
    let host = Host::new();
    host.fail("apt-get");
    let (apt, dirs) = (host.apt(), host.dirs());
    let ran = in_step(async |step| prereq::prerequisites(step, &apt, &dirs, "run1").await).await;
    assert!(ran.error.is_some(), "{}", ran.screen);
    assert!(
        ran.screen.contains("error updating apt cache"),
        "{}",
        ran.screen
    );
    assert!(
        host.calls().iter().all(|call| !call.contains("install")),
        "{:?}",
        host.calls()
    );
    // Nothing after the failed step ran: no directory was created.
    assert!(!dirs.ansible.exists());
}

/// A later step's failure ends the sequence with its own words: an
/// unsupported release refused by host's `add_repositories`.
#[tokio::test]
async fn an_unsupported_release_stops_at_the_repositories() {
    let _spawns = SPAWNS.lock().await;
    let host = Host::default();
    host.codename("focal");
    let (apt, dirs) = (host.apt(), host.dirs());
    let ran = in_step(async |step| prereq::prerequisites(step, &apt, &dirs, "run1").await).await;
    assert!(ran.error.is_some(), "{}", ran.screen);
    assert!(
        ran.screen.contains("error adding apt repositories"),
        "{}",
        ran.screen
    );
    let last = host.calls().last().map(|call| short(call));
    assert_eq!(last.as_deref(), Some("lsb_release -sc"));
}

#[test]
fn the_real_directories_and_locale_are_sb_gos() {
    let dirs = prereq::Dirs::saltbox();
    assert_eq!(dirs.git, std::path::Path::new("/srv/git"));
    assert_eq!(dirs.ansible, std::path::Path::new("/srv/ansible"));
    let system = locale::Locale::system();
    assert_eq!(system.file, std::path::Path::new("/etc/default/locale"));
    for (program, name) in [
        (&system.locale, "locale"),
        (&system.locale_gen, "locale-gen"),
        (&system.update_locale, "update-locale"),
        (&system.dpkg_reconfigure, "dpkg-reconfigure"),
    ] {
        assert_eq!(program, std::path::Path::new(name));
    }
}

/// `locale -a` that can't start lists nothing: the locale is generated.
#[tokio::test]
async fn a_locale_listing_that_cant_start_generates() {
    let _spawns = SPAWNS.lock().await;
    let host = Host::new();
    let mut locale = host.locale();
    locale.locale = host.path("no-such-locale");
    let ran = in_step(async |step| locale::configure(step, &locale).await).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(
        host.calls().first().map(String::as_str),
        Some("locale-gen en_US.UTF-8")
    );
}

#[tokio::test]
async fn a_missing_locale_is_generated_and_set() {
    let _spawns = SPAWNS.lock().await;
    let host = Host::new();
    let locale = host.locale();
    let ran = in_step(async |step| locale::configure(step, &locale).await).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(
        host.calls(),
        [
            "locale -a",
            "locale-gen en_US.UTF-8",
            "update-locale LC_ALL=en_US.UTF-8 LANG=en_US.UTF-8",
        ]
    );
    assert!(
        ran.screen.contains("Generating locale en_US.UTF-8")
            && ran
                .screen
                .contains("Setting system-wide locale (LC_ALL and LANG) to en_US.UTF-8"),
        "{}",
        ran.screen
    );
    assert!(!ran.screen.contains("Warning"), "{}", ran.screen);
}

#[tokio::test]
async fn an_installed_locale_is_not_generated_again() {
    let _spawns = SPAWNS.lock().await;
    let host = Host::new();
    let _ = host.stand_in("locale", "echo C.utf8; echo en_US.UTF-8");
    let locale = host.locale();
    let ran = in_step(async |step| locale::configure(step, &locale).await).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(
        host.calls(),
        [
            "locale -a",
            "update-locale LC_ALL=en_US.UTF-8 LANG=en_US.UTF-8",
        ]
    );
}

/// update-locale failing is an info line; dpkg-reconfigure runs
/// non-interactively and, when the file still lacks the settings, each
/// gets its warning: never an error.
#[tokio::test]
async fn a_failing_update_locale_falls_back_and_warns() {
    let _spawns = SPAWNS.lock().await;
    let host = Host::new();
    host.fail("update-locale");
    let frontend = host.path("frontend");
    let _ = host.stand_in(
        "dpkg-reconfigure",
        &format!("echo \"$DEBIAN_FRONTEND\" > '{}'", frontend.display()),
    );
    let locale = host.locale();
    let ran = in_step(async |step| locale::configure(step, &locale).await).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(
        host.calls(),
        [
            "locale -a",
            "locale-gen en_US.UTF-8",
            "update-locale LC_ALL=en_US.UTF-8 LANG=en_US.UTF-8",
            "dpkg-reconfigure locales",
        ]
    );
    assert_eq!(
        std::fs::read_to_string(frontend).expect("frontend"),
        "noninteractive\n"
    );
    let file = host.locale_file().display().to_string();
    for line in [
        "update-locale failed, attempting fallback: ".to_string(),
        "Locale not set correctly, reconfiguring locales...".to_string(),
        format!("Warning: LC_ALL was not set correctly in {file}. This might cause issues."),
        format!("Warning: LANG was not set correctly in {file}. This might cause issues."),
    ] {
        assert!(ran.screen.contains(&line), "{line}:\n{}", ran.screen);
    }
}

/// A file with one setting reconfigures, and the reconfigure fixing it
/// leaves no warning.
#[tokio::test]
async fn a_half_set_file_is_reconfigured() {
    let _spawns = SPAWNS.lock().await;
    let host = Host::new();
    let file = host.locale_file();
    let _ = host.stand_in(
        "update-locale",
        &format!("echo LANG=en_US.UTF-8 > '{}'", file.display()),
    );
    let _ = host.stand_in(
        "dpkg-reconfigure",
        &format!(
            "printf 'LANG=en_US.UTF-8\\nLC_ALL=en_US.UTF-8\\n' > '{}'",
            file.display()
        ),
    );
    let locale = host.locale();
    let ran = in_step(async |step| locale::configure(step, &locale).await).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(
        host.calls().last().map(String::as_str),
        Some("dpkg-reconfigure locales")
    );
    assert!(!ran.screen.contains("Warning"), "{}", ran.screen);
}

#[tokio::test]
async fn a_failing_generation_or_reconfigure_ends_the_locale() {
    let _spawns = SPAWNS.lock().await;
    for (failing, words) in [
        ("locale-gen", "error generating locale"),
        ("dpkg-reconfigure", "error reconfiguring locales"),
    ] {
        let host = Host::new();
        host.fail(failing);
        if failing == "dpkg-reconfigure" {
            host.fail("update-locale");
        }
        let locale = host.locale();
        let ran = in_step(async |step| locale::configure(step, &locale).await).await;
        assert!(ran.error.is_some(), "{failing}: {}", ran.screen);
        assert!(ran.screen.contains(words), "{failing}:\n{}", ran.screen);
        assert!(
            ran.screen.contains(&format!("{failing} failed (stand-in)")),
            "{failing}:\n{}",
            ran.screen
        );
    }
}

/// An unreadable locale file is a warning, then read as empty.
#[tokio::test]
async fn an_unreadable_locale_file_is_a_warning() {
    let _spawns = SPAWNS.lock().await;
    let host = Host::new();
    // A directory in its place: reading it fails with something other
    // than "not found".
    let _ = host.stand_in("update-locale", "exit 0");
    std::fs::create_dir(host.locale_file()).expect("dir");
    let locale = host.locale();
    let ran = in_step(async |step| locale::configure(step, &locale).await).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    let file = host.locale_file().display().to_string();
    for line in [
        format!("Warning: could not read {file}: "),
        format!("Warning: could not read {file} after reconfigure: "),
    ] {
        assert!(ran.screen.contains(&line), "{line}:\n{}", ran.screen);
    }
}

#[tokio::test]
async fn missing_configs_are_copied_private_and_present_ones_secured() {
    let host = Host::new();
    let _ = host.saltbox_file("defaults/accounts.yml.default", "user: seed\n");
    let _ = host.saltbox_file("defaults/settings.yml.default", "default settings\n");
    let _ = host.saltbox_file("defaults/README.md", "not a default\n");
    let settings = host.saltbox_file("settings.yml", "mine\n");
    std::fs::set_permissions(&settings, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    let root = host.saltbox();
    let ran = in_step(async |step| configs::copy_default_configs(step, &root).await).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    let accounts = root.join("accounts.yml");
    assert_eq!(
        std::fs::read_to_string(&accounts).expect("copied"),
        "user: seed\n"
    );
    assert_eq!(mode(&accounts), 0o600);
    assert_eq!(std::fs::read_to_string(&settings).expect("kept"), "mine\n");
    assert_eq!(mode(&settings), 0o600);
    assert!(!root.join("README.md").exists());
    assert!(
        ran.screen.contains("Copying accounts.yml.default")
            && !ran.screen.contains("Copying settings.yml.default"),
        "{}",
        ran.screen
    );
}

#[tokio::test]
async fn a_link_or_a_special_file_in_a_configs_place_is_refused() {
    let host = Host::new();
    let _ = host.saltbox_file("defaults/accounts.yml.default", "user: seed\n");
    let root = host.saltbox();
    let target = host.path("elsewhere");
    std::fs::write(&target, "secret\n").expect("target");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    let config = root.join("accounts.yml");
    std::os::unix::fs::symlink(&target, &config).expect("link");
    let linked = format!(
        "configuration file {} must not be a symlink",
        config.display()
    );
    let ran = in_step(async |step| configs::copy_default_configs(step, &root).await).await;
    assert_eq!(ran.error.as_deref(), Some(linked.as_str()));
    let secured = configs::secure_existing_configs(&root).expect_err("refused");
    assert_eq!(secured.to_string(), linked);
    // The link's target is untouched.
    assert_eq!(mode(&target), 0o644);
    assert_eq!(
        std::fs::read_to_string(&target).expect("target"),
        "secret\n"
    );

    std::fs::remove_file(&config).expect("unlink");
    std::fs::create_dir(&config).expect("dir");
    let special = format!(
        "configuration file {} is not a regular file",
        config.display()
    );
    let ran = in_step(async |step| configs::copy_default_configs(step, &root).await).await;
    assert_eq!(ran.error.as_deref(), Some(special.as_str()));
    // A FIFO in its place is refused the same way, never opened.
    std::fs::remove_dir(&config).expect("rmdir");
    let made = std::process::Command::new("mkfifo")
        .arg(&config)
        .status()
        .expect("mkfifo");
    assert!(made.success());
    let secured = configs::secure_existing_configs(&root).expect_err("refused");
    assert_eq!(secured.to_string(), special);
}

/// A `defaults` that isn't a directory can't be listed (difference 40).
#[test]
fn an_unreadable_defaults_is_an_error() {
    let host = Host::new();
    let _ = host.saltbox_file("defaults", "not a directory\n");
    let err = configs::secure_existing_configs(&host.saltbox()).expect_err("listing");
    assert!(
        err.to_string()
            .starts_with("error listing default config files: "),
        "{err}"
    );
}

#[test]
fn existing_configs_are_secured_and_nothing_is_copied() {
    let host = Host::new();
    let _ = host.saltbox_file("defaults/accounts.yml.default", "user: seed\n");
    let _ = host.saltbox_file("defaults/settings.yml.default", "default\n");
    let settings = host.saltbox_file("settings.yml", "mine\n");
    std::fs::set_permissions(&settings, std::fs::Permissions::from_mode(0o664)).expect("chmod");
    let root = host.saltbox();
    configs::secure_existing_configs(&root).expect("secured");
    assert_eq!(mode(&settings), 0o600);
    assert_eq!(std::fs::read_to_string(&settings).expect("kept"), "mine\n");
    assert!(!root.join("accounts.yml").exists());
    // No defaults directory: nothing to do.
    let empty = Host::new();
    configs::secure_existing_configs(&empty.saltbox()).expect("nothing");
}

#[tokio::test]
async fn git_hooks_run_only_when_saltbox_has_them() {
    let _spawns = SPAWNS.lock().await;
    let host = Host::new();
    let root = host.saltbox();
    let bash = host.bash();
    let ran = in_step(async |step| hooks::git_hooks(step, &root, &bash).await).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert!(host.calls().is_empty(), "{:?}", host.calls());
    assert!(
        !ran.screen.contains("Activating Git hooks"),
        "{}",
        ran.screen
    );

    let script = host.saltbox_file(hooks::SCRIPT, "git config core.hooksPath .githooks\n");
    let cwd = host.path("cwd");
    let _ = host.stand_in("bash", &format!("pwd > '{}'", cwd.display()));
    let ran = in_step(async |step| hooks::git_hooks(step, &root, &bash).await).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(host.calls(), [format!("bash {}", script.display())]);
    assert_eq!(
        std::fs::read_to_string(&cwd).expect("cwd").trim_end(),
        root.display().to_string()
    );
    assert!(
        ran.screen.contains("Activating Git hooks"),
        "{}",
        ran.screen
    );

    host.fail("bash");
    let ran = in_step(async |step| hooks::git_hooks(step, &root, &bash).await).await;
    assert!(ran.error.is_some(), "{}", ran.screen);
    assert!(
        ran.screen.contains("error activating Git hooks"),
        "{}",
        ran.screen
    );
}

/// A script path that can't be inspected (its `bin` a file) is an error,
/// not a silent skip.
#[tokio::test]
async fn an_uninspectable_hooks_script_is_an_error() {
    let host = Host::new();
    let _ = host.saltbox_file("bin", "not a directory\n");
    let (root, bash) = (host.saltbox(), host.bash());
    let ran = in_step(async |step| hooks::git_hooks(step, &root, &bash).await).await;
    let error = ran.error.expect("refused");
    assert!(
        error.starts_with("error checking for init-hooks script: "),
        "{error}"
    );
    assert!(host.calls().is_empty(), "{:?}", host.calls());
}
