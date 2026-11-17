//! `sb update` through the built binary (ops spec, "`sb update`" and
//! Testing Strategy, "Commands end to end"; module build M84 and M85): its
//! flags as any run; the runs as root (`SB_TEST_ROOT=1`) in sb-ops'
//! private mount namespace, where `/etc/apt`, `/var/backups`,
//! `/etc/default` and `/srv` are empty tmpfs and the stand-in apt-get and
//! ansible-playbook come first on `PATH`; piped runs in a session of their
//! own (util-linux's `setsid`), so there is no terminal. The host is
//! sb-ops' Saltbox harness: Saltbox's checkout (`SB_SALTBOX_DIR`) at
//! python-env's stand-in tree, Sandbox's under its inventory's appdata
//! path, clones of stand-in remotes (`file://`, never the network),
//! sb-config's fixtures with both credential checks sent to its fake; the
//! test build is raw, so self-update is off, and its release client talks
//! only to the tree's fake GitHub.

mod common;
mod raw_pty;
mod root_gate;

use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use assert_cmd::prelude::*;
use predicates::prelude::*;
use raw_pty::Pty;
use sb_ops::testing::{Host, Saltbox};
use sb_python_env::testing::ROOT_VAR;
use sb_release::testing::BASES_VARIABLE;

/// The harness's Saltbox host (its stand-in host apart, holding the
/// calls), a directory for the run log and backups.
struct World {
    sb: Saltbox,
    host: Host,
    dir: tempfile::TempDir,
}

impl World {
    fn new(saltbox: &str, sandbox: &str) -> Self {
        let mut sb = Saltbox::new(saltbox, sandbox);
        let host = std::mem::replace(&mut sb.host, Host::new());
        Self {
            sb,
            host,
            dir: tempfile::tempdir().expect("tempdir"),
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

    /// The calls but ansible-playbook's listings.
    fn changes(&self) -> Vec<String> {
        self.host
            .calls()
            .into_iter()
            .filter(|call| !call.starts_with("ansible-playbook "))
            .collect()
    }

    /// `sb update <args>` in the namespace and a session of its own: no
    /// terminal.
    fn update(&self, args: &[&str]) -> Output {
        let sb = common::sb().get_program().to_string_lossy().into_owned();
        let mut command = vec!["-w", sb.as_str(), "update", "--color=never"];
        command.extend_from_slice(args);
        self.namespaced(Path::new("/usr/bin/setsid"), &command, &[])
            .output()
            .expect("run sb update")
    }

    /// `program <args>` in the namespace, with sb's test environment, the
    /// world's, and `git` (`key`, `value`) settings besides.
    fn namespaced(&self, program: &Path, args: &[&str], git: &[(&str, &str)]) -> Command {
        let mut cmd = self
            .host
            .namespaced(program, args, "mkdir -p /etc/apt/sources.list.d");
        common::apply_sb_env(&mut cmd);
        let (cloudflare, dockerhub) = self.sb.check_urls();
        cmd.env("SB_SALTBOX_DIR", self.saltbox())
            .env(ROOT_VAR, self.sb.tree.root.path())
            .env(BASES_VARIABLE, self.sb.tree.bases())
            .env(
                "SB_ANSIBLE_PLAYBOOK",
                self.host.stand_ins().join("ansible-playbook"),
            )
            .env("CLOUDFLARE_BASE_URL", cloudflare)
            .env("SB_DOCKERHUB_URL", dockerhub)
            .env("SB_LOG_DIR", self.dir.path().join("logs"))
            .env("SB_BACKUPS_DIR", self.dir.path().join("backups"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null");
        // The checkouts are the Saltbox user's after a run; Saltbox adds
        // them to root's global safe.directory (repo spec, assumption 2),
        // the environment's config stands in for it.
        let mut settings = vec![("safe.directory", "*")];
        settings.extend_from_slice(git);
        cmd.env("GIT_CONFIG_COUNT", settings.len().to_string());
        for (i, (key, value)) in settings.iter().enumerate() {
            cmd.env(format!("GIT_CONFIG_KEY_{i}"), key)
                .env(format!("GIT_CONFIG_VALUE_{i}"), value);
        }
        cmd
    }
}

/// The lines' positions in `text`, which must rise.
fn in_order(text: &str, lines: &[&str]) {
    let mut at = 0;
    for line in lines {
        let found = text[at..]
            .find(line)
            .unwrap_or_else(|| panic!("{line} after {at}:\n{text}"));
        at += found + line.len();
    }
}

#[test]
fn update_is_shown_with_sb_gos_flags_and_migrations() {
    common::sb()
        .args(["--help", "--color=never"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Update Saltbox & Sandbox"));
    common::sb()
        .args(["update", "--help", "--color=never"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("--keep-branch")
                .and(predicate::str::contains("--reset-branch"))
                .and(predicate::str::contains("--skip-self-update"))
                .and(predicate::str::contains("--migrations"))
                .and(predicate::str::contains("--verbose").not()),
        );
    common::sb()
        .args(["update", "--keep-branch", "--reset-branch"])
        .assert()
        .code(2);
    common::sb()
        .args(["update", "--migrations", "maybe"])
        .assert()
        .code(2);
    common::sb().args(["update", "-v"]).assert().code(2);
}

/// Piped with every flag, every step in sb-go's order: self-update off
/// (a raw build), apt, Saltbox reset with its runtime and tags, the config
/// permissions, Sandbox reset with its tags, the announcements the pull
/// brought printed inline on stdout, validate-config's checks.
#[test]
fn piped_with_every_flag_every_step_runs_in_order() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new("develop", "develop");
    let _ = sb_ops::testing::git(
        &world.sb.saltbox_remote.work(),
        &["checkout", "--quiet", "master"],
    );
    world.sb.saltbox_remote.commit(
        "announcements.yml",
        "announcements:\n- date: '2026-11-17'\n  title: New\n  message: A **fresh** message\n",
    );
    let out = world.update(&["--reset-branch", "--migrations", "skip"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stderr}");
    in_order(
        &stderr,
        &[
            "Self-update is disabled in this build",
            "Updating apt package cache",
            "Updating Saltbox",
            "Saltbox repository updated (master)",
            "Ansible virtual environment ready",
            "saltbox.fact is ready",
            "Updating Saltbox tags cache",
            "Saltbox configuration permissions secured",
            "Updating Sandbox",
            "Sandbox repository updated (master)",
            "Updating Sandbox tags cache",
            "Displaying new announcements",
            "Validating Saltbox configuration",
            "Saltbox configuration validated",
        ],
    );
    assert!(
        stdout.contains("Saltbox Announcement - November 17, 2026"),
        "{stdout}"
    );
    assert!(stdout.contains("fresh"), "{stdout}");
    assert_eq!(
        World::branch(&world.saltbox()),
        ("master".into(), "master".into())
    );
    assert_eq!(
        World::branch(&world.sandbox()),
        ("master".into(), "master".into())
    );
    let calls = world.changes();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(
        calls[0].starts_with("apt-get ") && calls[0].ends_with(" update"),
        "{calls:?}"
    );
}

/// SIGINT during the Saltbox fetch (ops spec, Testing Strategy): sb stops
/// the fetch, says it was interrupted and dies of the signal, the
/// checkout untouched and nothing after the fetch run. A stand-in
/// upload-pack (git's `remote.origin.uploadpack`) holds the fetch open,
/// marking that it started.
#[test]
fn sigint_during_the_saltbox_fetch_ends_sb_by_the_signal() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new("develop", "develop");
    let marker = world.dir.path().join("fetch-started");
    let pack = world.dir.path().join("upload-pack");
    std::fs::write(
        &pack,
        format!(
            "#!/bin/sh\ntouch '{}'\nsleep 60\nexec git-upload-pack \"$@\"\n",
            marker.display()
        ),
    )
    .expect("upload-pack");
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&pack, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let before = sb_ops::testing::git(&world.saltbox(), &["rev-parse", "HEAD"]);
    let sb = common::sb().get_program().to_string_lossy().into_owned();
    // sb is the process started: unshare and sh exec into it.
    let child = world
        .namespaced(
            Path::new(&sb),
            &[
                "update",
                "--color=never",
                "--reset-branch",
                "--migrations",
                "skip",
            ],
            &[("remote.origin.uploadpack", pack.to_str().expect("utf-8"))],
        )
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("start sb update");
    let deadline = Instant::now() + Duration::from_secs(60);
    while !marker.exists() {
        assert!(Instant::now() < deadline, "the fetch never started");
        std::thread::sleep(Duration::from_millis(50));
    }
    let pid = rustix::process::Pid::from_raw(child.id().try_into().expect("pid")).expect("pid");
    rustix::process::kill_process(pid, rustix::process::Signal::INT).expect("kill");
    let out = child.wait_with_output().expect("wait");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.signal(), Some(2), "{:?}: {stderr}", out.status);
    assert!(stderr.contains("interrupted"), "{stderr}");
    assert!(!stderr.contains("Updating Sandbox"), "{stderr}");
    assert_eq!(
        sb_ops::testing::git(&world.saltbox(), &["rev-parse", "HEAD"]),
        before
    );
    assert!(
        !stderr.contains("Validating Saltbox configuration"),
        "{stderr}"
    );
}

/// Without a terminal: no --migrations, or a checkout off master and no
/// branch flag, is refused before any step, naming each flag; nothing
/// ran and nothing changed.
#[test]
fn without_a_terminal_missing_flags_are_refused_first() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new("master", "develop");
    let out = world.update(&["--skip-self-update"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    for text in [
        "--keep-branch or --reset-branch (Sandbox is on branch 'develop', not 'master')",
        "--migrations run or --migrations skip",
    ] {
        assert!(stderr.contains(text), "{text}: {stderr}");
    }
    assert!(!stderr.contains("Updating"), "{stderr}");
    assert!(world.host.calls().is_empty(), "{:?}", world.host.calls());
    assert_eq!(
        World::branch(&world.sandbox()),
        ("develop".into(), "develop".into())
    );

    let world = World::new("master", "master");
    let out = world.update(&["--reset-branch"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("--migrations run or --migrations skip"),
        "{stderr}"
    );
    assert!(!stderr.contains("--keep-branch"), "{stderr}");
    assert!(world.host.calls().is_empty(), "{:?}", world.host.calls());
}

/// On a terminal with both checkouts off master (ops spec, Testing
/// Strategy: "`update` on a PTY: the branch questions before the first
/// step"): Saltbox's question, then Sandbox's, each after its current
/// branch, on the terminal (term-ui's confirm), and only then apt and the
/// checkouts; `y` resets Saltbox, `n` keeps Sandbox's branch.
#[test]
fn on_a_terminal_both_questions_come_before_the_first_step() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new("develop", "develop");
    let sb = common::sb().get_program().to_string_lossy().into_owned();
    let cmd = world.namespaced(
        Path::new(&sb),
        &["update", "--color=never", "--migrations", "skip"],
        &[],
    );
    let mut pty = Pty::spawn(&cmd);
    let saltbox = "Saltbox: Do you want to reset to the 'master' branch?";
    assert!(pty.showing(saltbox), "{}", pty.text());
    assert!(world.host.calls().is_empty(), "{:?}", world.host.calls());
    pty.send(b"y\r");
    let sandbox = "Sandbox: Do you want to reset to the 'master' branch?";
    assert!(pty.showing(sandbox), "{}", pty.text());
    assert!(world.host.calls().is_empty(), "{:?}", world.host.calls());
    pty.send(b"n\r");
    let status = pty.exit();
    let text = pty.text();
    assert!(status.success(), "{text}");
    in_order(
        &text,
        &[
            "Saltbox: Currently on branch 'develop'",
            saltbox,
            "Sandbox: Currently on branch 'develop'",
            sandbox,
            "Updating apt package cache",
            "Saltbox repository updated (master)",
            "Sandbox: Updating the current branch 'develop'",
            "Sandbox repository updated (develop)",
        ],
    );
    assert_eq!(
        World::branch(&world.saltbox()),
        ("master".into(), "master".into())
    );
    assert_eq!(
        World::branch(&world.sandbox()),
        ("develop".into(), "develop".into())
    );
}

/// A world with announcements asking for migrations upstream, Sandbox's
/// the older; the stand-in ansible-playbook lists tags as before and, for
/// a run by tag, records its working directory in the file returned.
fn migrations_world() -> (World, PathBuf) {
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
    let cwd = world.dir.path().join("migration-cwd");
    let _ = world.host.stand_in(
        "ansible-playbook",
        &format!(
            "case \"$*\" in\n*--list-tags*) cat <<'LIST'\n{}LIST\n;;\n*) pwd >> '{}' ;;\nesac",
            sb_ops::testing::LIST_TAGS,
            cwd.display()
        ),
    );
    (world, cwd)
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

/// `--migrations run` (ops spec, Testing Strategy): sb-go's table on
/// stdout, each tag run in its checkout, Sandbox's older one first (the
/// stand-in recording `--tags <tag>` and its working directory), between
/// sb-go's lines; validation after.
#[test]
fn migrations_run_with_the_flag_in_their_checkouts() {
    if !root_gate::requested() {
        return;
    }
    let (world, cwd) = migrations_world();
    let out = world.update(&["--reset-branch", "--migrations", "run"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    in_order(
        &stdout,
        &[
            "The following tags need to be run to handle migrations:",
            "Announcement Date",
            "Sandbox",
            "migrate-b",
            "2026-11-01",
            "Saltbox",
            "migrate-a",
            "2026-11-02",
            // ansible's lines on stdout, as `sb install`'s.
            "Executing Ansible playbook with command:",
            "--tags migrate-b",
            "Executing Ansible playbook with command:",
            "--tags migrate-a",
        ],
    );
    in_order(
        &stderr,
        &[
            "Starting migration execution",
            "Running migration 'migrate-b' for Sandbox repository",
            "Running migration 'migrate-a' for Saltbox repository",
            "All migrations completed successfully",
            "Saltbox configuration validated",
        ],
    );
    assert_eq!(
        tag_runs(&world),
        [
            "ansible-playbook sandbox.yml --become --tags migrate-b",
            "ansible-playbook saltbox.yml --become --tags migrate-a"
        ]
    );
    assert_eq!(
        std::fs::read_to_string(&cwd)
            .expect("cwd")
            .lines()
            .collect::<Vec<_>>(),
        [
            world.sandbox().display().to_string(),
            world.saltbox().display().to_string()
        ]
    );
}

/// The run log's text, every file under the world's log directory.
fn run_log(world: &World) -> String {
    let mut text = String::new();
    for entry in std::fs::read_dir(world.dir.path().join("logs")).expect("logs") {
        let path = entry.expect("log").path();
        text.push_str(&std::fs::read_to_string(&path).unwrap_or_default());
    }
    text
}

/// `--migrations skip`: none run, each named in a warning on the screen
/// and in the run log (decision 22).
#[test]
fn migrations_skipped_are_named_in_a_warning() {
    if !root_gate::requested() {
        return;
    }
    let (world, _) = migrations_world();
    let out = world.update(&["--reset-branch", "--migrations", "skip"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        stderr.contains("Skipped migrations: Sandbox migrate-b, Saltbox migrate-a"),
        "{stderr}"
    );
    assert!(tag_runs(&world).is_empty(), "{:?}", world.host.calls());
    let log = run_log(&world);
    assert!(
        log.contains("migrations skipped") && log.contains("Sandbox migrate-b, Saltbox migrate-a"),
        "{log}"
    );
}

/// A failing migration stops sb with the spec's text and exit 1: the
/// later migration and validation don't run.
#[test]
fn a_failing_migration_exits_1_with_its_words() {
    if !root_gate::requested() {
        return;
    }
    let (world, _) = migrations_world();
    let _ = world.host.stand_in(
        "ansible-playbook",
        &format!(
            "case \"$*\" in\n*--list-tags*) cat <<'LIST'\n{}LIST\n;;\n*) exit 2 ;;\nesac",
            sb_ops::testing::LIST_TAGS
        ),
    );
    let out = world.update(&["--reset-branch", "--migrations", "run"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("failed to execute migration 'migrate-b' for Sandbox repository"),
        "{stderr}"
    );
    assert_eq!(tag_runs(&world).len(), 1, "{:?}", world.host.calls());
    assert!(
        !stderr.contains("Validating Saltbox configuration"),
        "{stderr}"
    );
}

/// On a terminal and without `--migrations`: after the table, term-ui's
/// confirm asks `Run all migration tags?` on the terminal; `n` skips
/// them, named in the warning.
#[test]
fn on_a_terminal_the_migrations_question_is_asked() {
    if !root_gate::requested() {
        return;
    }
    let (world, _) = migrations_world();
    let sb = common::sb().get_program().to_string_lossy().into_owned();
    let cmd = world.namespaced(Path::new(&sb), &["update", "--color=never"], &[]);
    let mut pty = Pty::spawn(&cmd);
    assert!(pty.showing("Run all migration tags?"), "{}", pty.text());
    assert!(tag_runs(&world).is_empty(), "{:?}", world.host.calls());
    pty.send(b"n\r");
    let status = pty.exit();
    let text = pty.text();
    assert!(status.success(), "{text}");
    in_order(
        &text,
        &[
            "The following tags need to be run to handle migrations:",
            "Run all migration tags?",
            "Skipped migrations: Sandbox migrate-b, Saltbox migrate-a",
            "Saltbox configuration validated",
        ],
    );
    assert!(tag_runs(&world).is_empty(), "{:?}", world.host.calls());
}
