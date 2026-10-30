//! The run log end to end (spec success criterion 6; CONSTRAINTS.md,
//! Logging): a root run records its command and outcome, never its
//! arguments (that an unprivileged run doesn't log is in unprivileged.rs).
//! Each test logs to its own temporary directory, never the host's
//! `/var/log/sb`.

mod common;
mod root_gate;

use std::path::Path;

fn log_text(dir: &Path) -> String {
    std::fs::read_dir(dir)
        .expect("log dir")
        .map(|e| std::fs::read_to_string(e.expect("entry").path()).expect("read"))
        .collect()
}

/// A root run records the command's name and outcome; the argument value
/// (`--json`, standing in for a secret such as `-e password=...`) never
/// reaches the log.
#[test]
fn a_root_run_logs_its_command_and_outcome_but_no_arguments() {
    if !root_gate::requested() {
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
    // The logged version matches `sb version --json`. A consistency check:
    // today every crate is 0.1.0, so it can't tell sb's version from
    // sb-cli-core's; build_info's unit tests guard that the binary
    // supplies its own.
    let json = common::sb()
        .args(["version", "--json"])
        .output()
        .expect("run sb");
    let json = String::from_utf8(json.stdout).expect("utf-8");
    let version = json
        .split("\"version\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("version in the JSON");
    assert!(text.contains(&format!("version=\"{version}\"")), "{text}");
    assert!(text.contains("outcome=\"done\""), "{text}");
    assert!(
        !text.contains("--json"),
        "an argument reached the log: {text}"
    );
}

/// Startup warnings (here: an invalid theme file) reach the run log too
/// (spec success criterion 8: "warns (and logs)").
#[test]
fn startup_warnings_reach_the_run_log() {
    if !root_gate::requested() {
        return;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let config = dir.path().join("config.toml");
    std::fs::write(&config, "[theme.colors]\nproblem = \"red\"\n").expect("write config");
    let logs = dir.path().join("logs");
    let out = common::sb()
        .env("SB_LOG_DIR", &logs)
        .env("SB_CONFIG", &config)
        .arg("version")
        .output()
        .expect("run sb");
    assert!(out.status.success());
    let text = log_text(&logs);
    assert!(
        text.contains("WARN") && text.contains("isn't a #RRGGBB colour"),
        "{text}"
    );
}

/// A failed root run points at its log from the error block.
#[test]
fn a_failed_run_points_at_the_log() {
    if !root_gate::requested() {
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
    // The pointer names the one file written, then the run id.
    let details = stderr
        .lines()
        .find_map(|l| l.strip_prefix("  details: "))
        .expect("a details line");
    let (path, run) = details.split_once(" (run ").expect("path (run id)");
    let written: Vec<_> = std::fs::read_dir(dir.path())
        .expect("dir")
        .map(|e| e.expect("entry").path())
        .collect();
    assert_eq!(written, [std::path::PathBuf::from(path)], "{stderr}");
    let id = run.strip_suffix(')').expect("closing paren");
    assert!(
        id.len() == 16 && id.chars().all(|c| c.is_ascii_hexdigit()),
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
    if !root_gate::requested() {
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
