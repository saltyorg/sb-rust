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
        let sb = test_build();
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

/// This test build of sb, never `SB_TEST_BINARY`'s, for every run in the
/// namespace: update ends with the completion refresh, and only a build
/// with sb-completion's `testing` feature refreshes under
/// `SB_TEST_COMPLETION_ROOT`; any other writes the host's shell
/// directories (as completion.rs's `test_build`).
fn test_build() -> String {
    assert_cmd::cargo::cargo_bin("sb")
        .to_string_lossy()
        .into_owned()
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
    // sb-go's `-v`, accepted (the maintainer, 2026-11-18): `--help` after
    // it parses only if it does, and runs nothing.
    common::sb()
        .args(["update", "-v", "--help"])
        .assert()
        .success();
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
            "Shell completions checked",
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
    let sb = test_build();
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
/// branch, on the alternate screen over the paused tree (term-ui's
/// `confirm_over`, ops decision 39), and only then apt and the checkouts;
/// `y` resets Saltbox, `n` keeps Sandbox's branch, each answer a notice in
/// the one tree printed at the end.
#[test]
fn on_a_terminal_both_questions_come_before_the_first_step() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new("develop", "develop");
    let sb = test_build();
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
    let (enter, leave) = ("\u{1b}[?1049h", "\u{1b}[?1049l");
    in_order(
        &text,
        &[
            enter,
            "Saltbox: Currently on branch 'develop'",
            saltbox,
            leave,
            enter,
            "Sandbox: Currently on branch 'develop'",
            sandbox,
            leave,
            "Updating apt package cache",
            "Saltbox: Reset from branch 'develop' to 'master' (your answer)",
            "Saltbox repository updated (master)",
            "Sandbox: Kept branch 'develop' (your answer)",
            "Sandbox repository updated (develop)",
        ],
    );
    assert_eq!(text.matches(enter).count(), 2, "{text}");
    // On the screen once it is drawn (vt100, taller than the PTY so
    // nothing scrolls away): one tree, its title and each step once.
    let mut screen = vt100::Parser::new(200, 100, 0);
    screen.process(text.as_bytes());
    let shown = screen.screen().contents();
    for once in [
        "sb update",
        "Checked for sb CLI updates",
        "(your answer)",
        "Shell completions checked",
    ] {
        let count = shown.matches(once).count();
        let want = if once == "(your answer)" { 2 } else { 1 };
        assert_eq!(count, want, "{once}:\n{shown}");
    }
    assert!(!shown.contains("Do you want to reset"), "{shown}");
    assert_eq!(
        World::branch(&world.saltbox()),
        ("master".into(), "master".into())
    );
    assert_eq!(
        World::branch(&world.sandbox()),
        ("develop".into(), "develop".into())
    );
}

/// Ctrl-C at a branch question on the alternate screen (ops decision 39):
/// the screen is left, the terminal restored, and sb ends by SIGINT,
/// nothing changed.
#[test]
fn on_a_terminal_ctrl_c_at_a_question_leaves_the_screen_and_ends_by_sigint() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new("develop", "master");
    let sb = test_build();
    let cmd = world.namespaced(
        Path::new(&sb),
        &["update", "--color=never", "--migrations", "skip"],
        &[],
    );
    let mut pty = Pty::spawn(&cmd);
    let question = "Saltbox: Do you want to reset to the 'master' branch?";
    assert!(pty.showing(question), "{}", pty.text());
    pty.send(b"\x03");
    let status = pty.exit();
    let text = pty.text();
    assert_eq!(status.signal(), Some("Interrupt"), "{status:?}: {text:?}");
    in_order(
        &text,
        &["\u{1b}[?1049h", question, "\u{1b}[?1049l", "interrupted"],
    );
    // The cursor shown, never left hidden by whatever drew last.
    let shown = text.rfind("\u{1b}[?25h").expect("the cursor is shown");
    if let Some(hidden) = text.rfind("\u{1b}[?25l") {
        assert!(shown > hidden, "{text:?}");
    }
    assert!(world.changes().is_empty(), "{:?}", world.changes());
    assert_eq!(
        World::branch(&world.saltbox()),
        ("develop".into(), "develop".into())
    );
}

/// stderr not a terminal (redirected to a file) while the questions can
/// still be asked on the terminal: the runner draws plain, so there is
/// no live tree to keep, and the question is asked on the normal screen
/// with its context above it, no alternate screen (term-ui decision 14).
#[test]
fn with_stderr_redirected_the_question_stays_on_the_normal_screen() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new("develop", "master");
    let sb = test_build();
    let log = world.dir.path().join("stderr.log");
    let script = format!(
        "exec {sb} update --color=never --migrations skip 2>{}",
        log.display()
    );
    let cmd = world.namespaced(Path::new("/bin/sh"), &["-c", &script], &[]);
    let mut pty = Pty::spawn(&cmd);
    let question = "Saltbox: Do you want to reset to the 'master' branch?";
    assert!(pty.showing(question), "{}", pty.text());
    pty.send(b"n\r");
    let status = pty.exit();
    let text = pty.text();
    assert!(status.success(), "{text}");
    assert!(!text.contains("\u{1b}[?1049h"), "{text:?}");
    in_order(&text, &["Saltbox: Currently on branch 'develop'", question]);
    let stderr = std::fs::read_to_string(&log).expect("stderr");
    assert!(
        stderr.contains("Saltbox: Kept branch 'develop' (your answer)"),
        "{stderr}"
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
        stderr.contains(
            "failed to execute migration 'migrate-b' for Sandbox repository (to run by hand: sb \
             install sandbox-migrate-b, sb install migrate-a)"
        ),
        "{stderr}"
    );
    // The playbook ran and said why on the screen: no run-log pointer
    // (the maintainer, 2026-11-18).
    assert!(!stderr.contains("details:"), "{stderr}");
    assert_eq!(tag_runs(&world).len(), 1, "{:?}", world.host.calls());
    assert!(
        !stderr.contains("Validating Saltbox configuration"),
        "{stderr}"
    );
}

/// On a terminal and without `--migrations`: the new announcements open
/// in tui's viewer first (ops spec, "Shown"; M87), both repositories in
/// one list, marked new; `q` closes it, then the table, and term-ui's
/// confirm asks `Run all migration tags?` on the terminal; `n` skips
/// them, named in the warning.
#[test]
fn on_a_terminal_the_viewer_then_the_migrations_question() {
    if !root_gate::requested() {
        return;
    }
    let (world, _) = migrations_world();
    let sb = test_build();
    let cmd = world.namespaced(Path::new(&sb), &["update", "--color=never"], &[]);
    let mut pty = Pty::spawn(&cmd);
    // The raw screen: the viewer's texts arrive between escape codes.
    for shown in ["Announcements", " NEW ", "Saltbox", "Sandbox", "Moved"] {
        assert!(pty.showing(shown), "{shown}: {}", pty.text());
    }
    assert!(
        !pty.text().contains("Run all migration tags?"),
        "{}",
        pty.text()
    );
    assert!(tag_runs(&world).is_empty(), "{:?}", world.host.calls());
    pty.send(b"q");
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

/// Ctrl-C in the viewer ends sb with 130, as on the log screens (tui
/// spec, "Keys and mouse"): no migration question, nothing run, and the
/// pending migrations named as the lines that run them by hand, since
/// the next update won't offer them again (decision 22, difference 43).
#[test]
fn on_a_terminal_ctrl_c_in_the_viewer_ends_with_130() {
    if !root_gate::requested() {
        return;
    }
    let (world, _) = migrations_world();
    let sb = test_build();
    let cmd = world.namespaced(Path::new(&sb), &["update", "--color=never"], &[]);
    let mut pty = Pty::spawn(&cmd);
    assert!(pty.showing(" NEW "), "{}", pty.text());
    pty.send(b"\x03");
    let status = pty.exit();
    let text = pty.text();
    assert_eq!(status.exit_code(), 130, "{text}");
    assert!(!text.contains("Run all migration tags?"), "{text}");
    assert!(
        text.contains(
            "Migrations interrupted; to run by hand: sb install sandbox-migrate-b, sb install migrate-a"
        ),
        "{text}"
    );
    assert!(tag_runs(&world).is_empty(), "{:?}", world.host.calls());
}

/// The viewer lists every announcement that applies, not only the new
/// ones (tui spec, "Input"): one seen in an earlier update is listed
/// beside the new one, which alone is marked (`· 1 new`, its ` NEW `
/// badge).
#[test]
fn on_a_terminal_the_viewer_lists_the_older_announcements_too() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new("master", "master");
    let remote = &world.sb.saltbox_remote;
    let _ = sb_ops::testing::git(&remote.work(), &["checkout", "--quiet", "master"]);
    let old = "announcements:\n- date: '2026-10-01'\n  title: Earlier news\n  message: Seen\n";
    remote.commit("announcements.yml", old);
    let out = world.update(&["--migrations", "skip"]);
    assert!(out.status.success(), "{out:?}");
    remote.commit(
        "announcements.yml",
        &format!("{old}- date: '2026-11-03'\n  title: Later news\n  message: Fresh\n"),
    );
    let sb = test_build();
    let cmd = world.namespaced(
        Path::new(&sb),
        &["update", "--color=never", "--migrations", "skip"],
        &[],
    );
    let mut pty = Pty::spawn(&cmd);
    // Single words: the raw screen splits a row's text at its style changes.
    for shown in ["Announcements", " NEW ", "Later", "Earlier"] {
        assert!(pty.showing(shown), "{shown}: {}", pty.text());
    }
    pty.send(b"q");
    let status = pty.exit();
    assert!(status.success(), "{}", pty.text());
}

/// sb interrupted during a migration (ops difference 43): a SIGTERM to sb
/// (a shutdown, `kill`) stops the playbook (exec's `attached`), and sb
/// names the interrupted migration and those after it in a warning, each
/// with the line that runs it by hand, and in the run log, then dies of
/// the signal; nothing after it runs. The stand-in playbook sends it, to
/// its parent, sb, during the first run, then waits as the playbook,
/// `exec` keeping it the one process exec stops.
#[test]
fn an_interrupted_migration_names_what_is_left_and_ends_by_the_signal() {
    if !root_gate::requested() {
        return;
    }
    let (world, _) = migrations_world();
    let _ = world.host.stand_in(
        "ansible-playbook",
        &format!(
            "case \"$*\" in\n*--list-tags*) cat <<'LIST'\n{}LIST\n;;\n*) kill -TERM $PPID; exec sleep 30 ;;\nesac",
            sb_ops::testing::LIST_TAGS
        ),
    );
    let out = world.update(&["--reset-branch", "--migrations", "run"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.signal(), Some(15), "{:?}: {stderr}", out.status);
    assert!(
        stderr.contains(
            "Migrations interrupted; to run by hand: sb install sandbox-migrate-b, sb install \
             migrate-a"
        ),
        "{stderr}"
    );
    assert_eq!(tag_runs(&world).len(), 1, "{:?}", world.host.calls());
    assert!(
        !stderr.contains("Validating Saltbox configuration"),
        "{stderr}"
    );
    let log = run_log(&world);
    assert!(log.contains("migrations interrupted"), "{log}");
}

/// What moved (ops spec, "What moved, and the closing hint"; decisions
/// 36 and 37), piped: after the last tree, on stderr and plain, Saltbox
/// up to date and Sandbox's range with the tags its pull added and
/// removed, then Sandbox's hint; the run log keeps both commits and the
/// tag diff.
#[test]
fn what_moved_and_the_hint_end_the_run_and_reach_the_run_log() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new("master", "master");
    let out = world.update(&["--migrations", "skip"]);
    assert!(out.status.success(), "{out:?}");
    let short = |path: &Path| sb_ops::testing::git(path, &["rev-parse", "HEAD"])[..7].to_owned();
    let (saltbox, sandbox) = (short(&world.saltbox()), short(&world.sandbox()));
    let remote = &world.sb.sandbox_remote;
    let _ = sb_ops::testing::git(&remote.work(), &["checkout", "--quiet", "master"]);
    remote.commit(
        "tags.txt",
        "always, authelia, docker, karakeep, plex, sanity-check, sonarr, tracearr",
    );
    let out = world.update(&["--migrations", "skip"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    let moved = short(&world.sandbox());
    let block = format!(
        "\nSaltbox  already up to date ({saltbox})\nSandbox  {sandbox} → {moved}\n  \
         New tags:      sandbox-karakeep, sandbox-tracearr\n  \
         Removed tags:  sandbox-traefik\n\n\
         Sandbox's files are updated; your Sandbox apps are not yet. To apply it:\n  \
         sb install sandbox-<app>   each Sandbox app you run\n  \
         sb install sandbox-roles   the apps you list in sandbox_roles\n"
    );
    assert!(stderr.ends_with(&block), "{stderr}");
    assert!(!stderr.contains('\u{1b}'), "{stderr:?}");
    in_order(&stderr, &["Shell completions checked", "Sandbox  "]);
    let log = run_log(&world);
    let sandbox_full = sb_ops::testing::git(&world.sandbox(), &["rev-parse", "HEAD"]);
    assert!(
        log.lines().any(|line| line.contains("checkout pulled")
            && line.contains("Sandbox")
            && line.contains(&sandbox_full)),
        "{log}"
    );
    assert!(
        log.lines().any(|line| line.contains("tags compared")
            && line.contains("sandbox-karakeep, sandbox-tracearr")
            && line.contains("sandbox-traefik")),
        "{log}"
    );
}

/// sb's own files in the Saltbox checkout, which a run writes (ignored,
/// as Saltbox's .gitignore has them): the tags cache and the fact.
const OURS: [&str; 2] = ["cache.json", "ansible_facts.d"];

/// A path in a checkout and a file's bytes (none for a directory).
type Entry = (PathBuf, Option<Vec<u8>>);

/// A checkout as it stands, byte for byte, sb's own files ([`OURS`])
/// aside: every entry outside `.git` with each file's bytes (std's
/// `read_dir`, sorted), `git status` with ignored files, HEAD and the
/// local branches, the fetch refspec.
fn snapshot(checkout: &Path) -> (Vec<Entry>, String) {
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
/// `checkout`, beside its ignored configs (ops decision 40).
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

/// A new commit on master in each remote, which an update would pull.
fn upstream(world: &World) {
    for remote in [&world.sb.saltbox_remote, &world.sb.sandbox_remote] {
        let _ = sb_ops::testing::git(&remote.work(), &["checkout", "--quiet", "master"]);
        remote.commit("upstream.txt", "new\n");
    }
}

/// `--discard-local` in help; `--skip-git` hidden from help and from
/// completion, and refused with the flags it makes meaningless (clap's
/// conflict, exit 2), before anything runs (ops decisions 40 and 41).
#[test]
fn discard_local_is_shown_and_skip_git_is_hidden() {
    common::sb()
        .args(["update", "--help", "--color=never"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains(
                "--discard-local     Discard local commits, changes and untracked files",
            )
            .and(predicate::str::contains("--skip-git").not()),
        );
    for other in ["--keep-branch", "--reset-branch", "--discard-local"] {
        common::sb()
            .args(["update", "--skip-git", other])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("--skip-git"));
    }
    // clap's dynamic completion, as the installed scripts call it.
    let completed = |word: &str| {
        let out = common::sb()
            .env("SB_COMPLETE", "fish")
            .args(["--", "sb", "update", word])
            .output()
            .expect("complete");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    assert!(
        !completed("--s").contains("--skip-git"),
        "{}",
        completed("--s")
    );
    assert!(
        completed("--d").contains("--discard-local"),
        "{}",
        completed("--d")
    );
    for command in ["branch", "branch-sandbox"] {
        common::sb()
            .args([command, "--help", "--color=never"])
            .assert()
            .success()
            .stdout(predicate::str::contains("--discard-local"));
    }
}

/// Piped with local changes in Saltbox's checkout: refused before any
/// step, naming the checkout, its counts and both flags, exit 1, nothing
/// run or changed; `--discard-local` discards them and goes on.
#[test]
fn without_a_terminal_local_changes_are_refused_first() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new("master", "master");
    dirty(&world.saltbox());
    let before = snapshot(&world.saltbox());
    let out = world.update(&["--migrations", "skip"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "--discard-local or --skip-git (Saltbox has {DIRTY} that the update would discard)"
        )),
        "{stderr}"
    );
    assert!(!stderr.contains("Updating"), "{stderr}");
    assert!(world.host.calls().is_empty(), "{:?}", world.host.calls());
    assert_eq!(snapshot(&world.saltbox()), before);

    let out = world.update(&["--migrations", "skip", "--discard-local"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        stderr.contains(&format!("Saltbox: Discarding {DIRTY} (--discard-local)")),
        "{stderr}"
    );
    assert!(!world.saltbox().join("notes.txt").exists());
    assert!(world.saltbox().join("accounts.yml").exists());
}

/// On a terminal: the local changes question on the alternate screen
/// with what would go, after the tree is paused; `n` leaves the checkout
/// byte for byte as it was, the answer a notice in the one tree printed
/// at the end, and `left as is` in what moved (ops decision 40).
#[test]
fn on_a_terminal_a_no_leaves_the_checkout_as_it_was() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new("master", "master");
    dirty(&world.saltbox());
    upstream(&world);
    let before = snapshot(&world.saltbox());
    let sb = test_build();
    let cmd = world.namespaced(
        Path::new(&sb),
        &["update", "--color=never", "--migrations", "skip"],
        &[],
    );
    let mut pty = Pty::spawn(&cmd);
    let question = "Saltbox: Discard these local changes and update?";
    assert!(pty.showing(question), "{}", pty.text());
    assert!(world.host.calls().is_empty(), "{:?}", world.host.calls());
    pty.send(b"n\r");
    let status = pty.exit();
    let text = pty.text();
    assert!(status.success(), "{text}");
    let (enter, leave) = ("\u{1b}[?1049h", "\u{1b}[?1049l");
    in_order(
        &text,
        &[
            enter,
            &format!("Saltbox: updating to 'master' would discard {DIRTY}:"),
            "  branch.txt",
            "  drafts/",
            "  notes.txt",
            "Declining leaves Saltbox as it is; the rest of the update goes on.",
            question,
            leave,
            "Updating apt package cache",
            // The tree cuts the notice to the PTY's 100 columns.
            "Saltbox: Left as is, keeping 1 local commit on 'master'",
            "Sandbox repository updated (master)",
            "Saltbox  left as is (",
        ],
    );
    assert_eq!(text.matches(enter).count(), 1, "{text}");
    assert_eq!(snapshot(&world.saltbox()), before);
    assert!(world.sandbox().join("upstream.txt").exists());
}

/// `--skip-git` piped, with both checkouts dirty and behind their
/// remotes and one off master, without a branch flag or `--migrations`:
/// both left byte for byte as they were, the rest of update run, both
/// tags caches rebuilt, `left as is` for each and no hint, the run log
/// saying git was skipped (ops decision 41).
#[test]
fn skip_git_leaves_both_checkouts_byte_for_byte() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new("develop", "master");
    dirty(&world.sandbox());
    upstream(&world);
    let before = (snapshot(&world.saltbox()), snapshot(&world.sandbox()));
    let out = world.update(&["--skip-git"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert_eq!(snapshot(&world.saltbox()), before.0);
    assert_eq!(snapshot(&world.sandbox()), before.1);
    in_order(
        &stderr,
        &[
            "Updating apt package cache",
            "Ansible virtual environment ready",
            "Updating Saltbox tags cache",
            "Updating Sandbox tags cache",
            // Piped, a step's notice comes before its ending.
            "Checking for new announcements",
            "To see them: sb announcements preview --since <ref>",
            "Announcements not compared (--skip-git)",
            "Saltbox configuration validated",
            "Shell completions checked",
            "Saltbox  left as is (",
            ", develop, no local changes)",
            "Sandbox  left as is (",
            ", master, 3 local changes)",
        ],
    );
    assert!(!stderr.contains("Fetching repository changes"), "{stderr}");
    assert!(!stderr.contains("To apply it"), "{stderr}");
    let log = run_log(&world);
    for repo in ["Saltbox", "Sandbox"] {
        assert!(
            log.lines().any(|line| line.contains("git skipped")
                && line.contains(repo)
                && line.contains("--skip-git")),
            "{repo}: {log}"
        );
    }
}
