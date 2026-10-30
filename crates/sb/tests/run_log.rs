//! The run log end to end (spec success criterion 6; CONSTRAINTS.md,
//! Logging): a root run records its command and outcome, never its
//! arguments; an unprivileged run doesn't log. Each test logs to its own
//! temporary directory, never the host's `/var/log/sb`.

mod common;

use std::path::Path;

fn log_text(dir: &Path) -> String {
    std::fs::read_dir(dir)
        .expect("log dir")
        .map(|e| std::fs::read_to_string(e.expect("entry").path()).expect("read"))
        .collect()
}

/// The same question sb asks before logging.
fn is_root() -> bool {
    sb_cli_core::root::effective_uid() == 0
}

/// A root run records the command's name and outcome; the argument value
/// (`--json`, standing in for a secret such as `-e password=...`) never
/// reaches the log.
#[test]
fn a_root_run_logs_its_command_and_outcome_but_no_arguments() {
    if !is_root() {
        eprintln!("skipped: runs log only as root");
        return;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let status = common::sb()
        .env("SB_LOG_DIR", dir.path())
        .args(["version", "--json"])
        .status()
        .expect("run sb");
    assert!(status.success());
    let text = log_text(dir.path());
    assert!(text.contains("command=\"version\""), "{text}");
    assert!(text.contains("outcome=\"done\""), "{text}");
    assert!(
        !text.contains("--json"),
        "an argument reached the log: {text}"
    );
}

/// A failed root run points at its log from the error block.
#[test]
fn a_failed_run_points_at_the_log() {
    if !is_root() {
        eprintln!("skipped: runs log only as root");
        return;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let full = std::fs::File::create("/dev/full").expect("/dev/full");
    let out = common::sb()
        .env("SB_LOG_DIR", dir.path())
        .arg("version")
        .stdout(full)
        .output()
        .expect("run sb");
    let stderr = String::from_utf8(out.stderr).expect("utf-8");
    assert!(
        stderr.starts_with("✗ No space left on device (os error 28)\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("  details: {} (run ", dir.path().display())),
        "{stderr}"
    );
    let text = log_text(dir.path());
    assert!(text.contains("outcome=\"failed\""), "{text}");
    assert!(
        text.contains("error=No space left on device (os error 28)"),
        "{text}"
    );
}

/// A log directory that can't be created: a warning, and the run goes on.
#[test]
fn an_unusable_log_directory_warns_and_runs_on() {
    if !is_root() {
        eprintln!("skipped: runs log only as root");
        return;
    }
    let file = tempfile::NamedTempFile::new().expect("temp file");
    let out = common::sb()
        .env("SB_LOG_DIR", file.path().join("sb"))
        .arg("version")
        .output()
        .expect("run sb");
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("warning: no run log this time ("));
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("Saltbox CLI version:"));
}
