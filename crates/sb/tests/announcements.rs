//! `sb announcements` end to end (ops spec, Testing Strategy,
//! "`announcements`"): synthetic before and after files in a temporary
//! directory, which is also the Saltbox root, and the inline output on
//! stdout. Migrations are never listed, asked about or run here (a
//! stand-in `ansible-playbook` that would leave a mark). Root runs
//! (`SB_TEST_ROOT=1`): the command needs root; each run has no
//! controlling terminal (`setsid -w`).

mod common;
mod root_gate;

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::{Command, Output};

const BEFORE: &str = "---\nannouncements:\n  - date: \"2026-01-10\"\n    title: \"Old news\"\n    \
                      message: |\n      Already seen.\n";

/// The before file's entry, two new ones out of date order (one asking for
/// a migration, one with a code block), and one gated by a missing file
/// (`MISSING`, a path in the test's directory: [`Files::new`]).
const AFTER: &str = "---\nannouncements:\n  - date: \"2026-01-10\"\n    title: \"Old news\"\n    \
                     message: |\n      Already seen.\n  - date: \"2026-03-02\"\n    \
                     title: \"Settings moved\"\n    migration:\n      required: true\n      \
                     tag: settings_move\n    message: |\n      ## Settings moved\n\n      \
                     Run the **migration** when asked.\n  - date: \"2026-02-14\"\n    \
                     title: \"New defaults\"\n    message: |\n      ### What changed\n\n      \
                     ```yaml\n      network: saltbox\n      ```\n  - date: \"2026-02-20\"\n    \
                     title: \"Gated\"\n    required_file: \"MISSING\"\n    \
                     message: Not shown.\n";

struct Files {
    dir: tempfile::TempDir,
}

impl Files {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join("before.yml"), BEFORE).expect("before");
        // The gated entry's file: one that can't exist, inside the
        // directory, so no file on the host decides the test.
        let missing = dir.path().join("missing/file");
        let after = AFTER.replace("MISSING", &missing.display().to_string());
        std::fs::write(dir.path().join("after.yml"), after).expect("after");
        // A playbook stand-in that would mark any run.
        let playbook = dir.path().join("ansible-playbook");
        std::fs::write(
            &playbook,
            format!("#!/bin/sh\ntouch '{}'\n", dir.path().join("ran").display()),
        )
        .expect("stand-in");
        std::fs::set_permissions(&playbook, std::fs::Permissions::from_mode(0o755)).expect("mode");
        Self { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// `sb announcements <args>` with no controlling terminal, the
    /// stand-in as the playbook and on `PATH`.
    fn run(&self, args: &[&str]) -> Output {
        let sb = assert_cmd::cargo::cargo_bin("sb");
        let mut cmd = Command::new("setsid");
        common::apply_sb_env(&mut cmd);
        cmd.env("SB_ANSIBLE_PLAYBOOK", self.path("ansible-playbook"));
        cmd.env("SB_SALTBOX_DIR", self.dir.path());
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs = vec![self.dir.path().to_path_buf()];
        dirs.extend(std::env::split_paths(&path));
        cmd.env("PATH", std::env::join_paths(dirs).expect("PATH"));
        cmd.arg("-w").arg(sb).arg("announcements").args(args);
        cmd.current_dir(self.dir.path());
        cmd.output().expect("run sb")
    }

    fn ran(&self) -> bool {
        self.path("ran").exists()
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn both(out: &Output) -> String {
    format!("stdout:\n{}\nstderr:\n{}", stdout(out), stderr(out))
}

const SHOWN: &str = "# Saltbox Announcement - February 14, 2026\n\n### What changed\n\n\
                     ```yaml\nnetwork: saltbox\n```\n\n\
                     # Saltbox Announcement - March 2, 2026\n\n## Settings moved\n\n\
                     Run the migration when asked.\n";

const TABLE_LINE: &str = "The following tags need to be run to handle migrations:";

/// The new entries inline, oldest first; an entry asking for a migration
/// is shown, but nothing is listed, asked or run: migrations are
/// `sb update`'s.
#[test]
fn it_shows_the_new_entries_and_no_migrations() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    let out = files.run(&["-b", "before.yml", "-a", "after.yml"]);
    assert_eq!(out.status.code(), Some(0), "{}", both(&out));
    let shown = stdout(&out);
    assert_eq!(shown, SHOWN);
    assert!(!shown.contains('\x1b'), "piped: no colour\n{shown}");
    let steps = stderr(&out);
    assert!(steps.contains("Displaying new announcements"), "{steps}");
    assert!(steps.contains("Checked for new announcements"), "{steps}");
    for text in [TABLE_LINE, "Run all migration tags?", "Would execute"] {
        assert!(!both(&out).contains(text), "{text}: {}", both(&out));
    }
    assert!(!files.ran(), "a playbook ran");
}

/// `--migrations` went with the migrations: a parse error.
#[test]
fn migrations_is_no_flag() {
    let out = common::sb()
        .args(["announcements", "--migrations", "run", "--color=never"])
        .output()
        .expect("run sb");
    assert_eq!(out.status.code(), Some(2), "{}", both(&out));
    assert!(stderr(&out).contains("--migrations"), "{}", both(&out));
}

/// No `--before`: every applicable entry is new, for the repository
/// `--repo` names; a missing before file is no file too.
#[test]
fn without_a_before_file_every_entry_is_new() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    let expected = "# Sandbox Announcement - January 10, 2026\n\nAlready seen.\n";
    for args in [
        &["-a", "before.yml", "-r", "Sandbox"][..],
        &["-b", "nothing.yml", "-a", "before.yml", "-r", "Sandbox"],
    ] {
        let out = files.run(args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", both(&out));
        assert_eq!(stdout(&out), expected, "{args:?}");
    }
}

/// No `--after`: the repository's own file, at the Saltbox root
/// (`SB_SALTBOX_DIR`) for Saltbox.
#[test]
fn without_an_after_file_the_repositorys_own_is_read() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    std::fs::write(files.path("announcements.yml"), AFTER).expect("own file");
    let out = files.run(&["-b", "before.yml"]);
    assert_eq!(out.status.code(), Some(0), "{}", both(&out));
    assert_eq!(stdout(&out), SHOWN);
}

/// Nothing new: no output on stdout, and the step says so.
#[test]
fn nothing_new_prints_nothing() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    for (before, after) in [("after.yml", "after.yml"), ("before.yml", "nothing.yml")] {
        let out = files.run(&["-b", before, "-a", after]);
        assert_eq!(out.status.code(), Some(0), "{}", both(&out));
        assert_eq!(stdout(&out), "", "{}", both(&out));
        assert!(!stderr(&out).contains("Displaying"), "{}", both(&out));
        assert!(
            stderr(&out).contains("No new announcements"),
            "{}",
            both(&out)
        );
    }
}

#[test]
fn colour_follows_the_choice() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    let out = files.run(&["-b", "before.yml", "-a", "after.yml", "--color=always"]);
    assert_eq!(out.status.code(), Some(0), "{}", both(&out));
    let shown = stdout(&out);
    assert!(shown.contains("\x1b["), "{shown}");
    assert!(
        !shown.contains("## Settings moved"),
        "markers hidden\n{shown}"
    );
    assert!(shown.contains("Settings moved"), "{shown}");
}

/// An invalid file is an error here, sb-go's words, exit 1.
#[test]
fn an_invalid_file_fails() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    std::fs::write(files.path("bad.yml"), "announcements:\n  - title: [x]\n").expect("bad");
    let out = files.run(&["-b", "bad.yml", "-a", "after.yml"]);
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert!(
        stderr(&out).contains(
            "error loading before announcement file 'bad.yml': error parsing announcement YAML: \
             bad.yml:2:12: announcements[0].title: must be a string, not a sequence"
        ),
        "{}",
        both(&out)
    );
    assert_eq!(stdout(&out), "");
}

#[test]
fn an_unknown_repository_is_sb_gos_error() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    let out = files.run(&["-b", "before.yml", "-a", "after.yml", "-r", "saltbox"]);
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert!(
        stderr(&out).contains("invalid repository name 'saltbox', must be 'Saltbox' or 'Sandbox'"),
        "{}",
        both(&out)
    );
}

/// Shown in `sb --help` now (the maintainer, 2026-11-15); its own help
/// has sb-go's flags, but no `-v` and no `--migrations`.
#[test]
fn it_is_shown_with_sb_gos_flags() {
    let out = common::sb()
        .args(["--help", "--color=never"])
        .output()
        .expect("run sb");
    assert!(
        stdout(&out).contains("Display announcements from announcement files"),
        "{}",
        both(&out)
    );
    let out = common::sb()
        .args(["announcements", "--help", "--color=never"])
        .output()
        .expect("run sb");
    let help = stdout(&out);
    for flag in ["--before", "--after", "--repo"] {
        assert!(help.contains(flag), "{flag}: {help}");
    }
    for flag in ["--verbose", "--migrations"] {
        assert!(!help.contains(flag), "{flag}: {help}");
    }
    assert!(
        help.contains("without --before,\n  every announcement is new"),
        "{help}"
    );
}
