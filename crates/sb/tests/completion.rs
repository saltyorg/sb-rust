//! Tab completion through the binary, with no shell (completion spec,
//! Testing strategy): a request is `SB_COMPLETE=<shell> sb -- <words>`,
//! as clap's stubs make it, with the cache in the test's Saltbox root.
//! sb-go's `cmd/tag_completion_test.go` `TestInstallCompletionProtocol`
//! cases, `generate`, and that a request prints only candidates, is never
//! logged and runs as the calling user.

mod common;
mod nobody;
mod root_gate;

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use assert_cmd::prelude::*;

/// A Saltbox root whose cache holds `tags` for Saltbox, or `text` as the
/// file when given; world-readable, so `nobody` can read it too.
fn saltbox(tags: &[&str], text: Option<&str>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let cache = text.map_or_else(
        || {
            let entry = serde_json::json!({ "commit": "a", "tags": tags });
            serde_json::json!({ dir.path().display().to_string(): entry }).to_string()
        },
        ToString::to_string,
    );
    let file = dir.path().join("cache.json");
    std::fs::write(&file, cache).expect("cache");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    dir
}

/// A fish request for `words` (after `sb`) against `root`, logging under
/// `logs`: fish's stub passes the words up to the cursor, the current one
/// last, and no index; each candidate on its own line, a flag's help
/// after a tab (`clap_complete` 4.6.11, `env/shells.rs`).
fn request(root: &Path, logs: &Path, words: &[&str]) -> std::process::Output {
    common::sb()
        .env("SB_COMPLETE", "fish")
        .env("SB_SALTBOX_DIR", root)
        .env("SB_LOG_DIR", logs)
        .env_remove("_CLAP_COMPLETE_INDEX")
        .arg("--")
        .arg("sb")
        .args(words)
        .output()
        .expect("run sb")
}

/// The candidates `request` printed, after checking it succeeded and
/// printed nothing else.
fn candidates(output: &std::process::Output) -> Vec<String> {
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    String::from_utf8(output.stdout.clone())
        .expect("UTF-8")
        .lines()
        .map(ToString::to_string)
        .collect()
}

const TAGS: &[&str] = &["sonarr", "plex", "radarr"];
const VERBOSE: &str = "--verbose\tIncrease verbosity level (can be used multiple times, e.g. -vvv)";

/// sb-go's protocol cases, through the binary. One difference: on an
/// empty word `clap_complete` offers the flags as well as the tags (spec,
/// difference 6), so that case checks the tags and that the flags follow.
#[test]
fn sb_gos_protocol_cases() {
    let good = saltbox(TAGS, None);
    let corrupt = saltbox(&[], Some("{broken JSON"));
    let logs = tempfile::tempdir().expect("temp dir");
    let cases: &[(&str, &tempfile::TempDir, &[&str], &[&str])] = &[
        ("noun prefix", &good, &["install", "so"], &["sonarr"]),
        (
            "comma noun",
            &good,
            &["install", "plex,so"],
            &["plex,sonarr"],
        ),
        (
            "quoted comma whitespace",
            &good,
            &["install", "plex, so"],
            &["plex, sonarr"],
        ),
        ("long flags", &good, &["install", "--ver"], &[VERBOSE]),
        (
            "flags after noun",
            &good,
            &["install", "plex", "--no"],
            &["--no-cache\tSkip cache validation and always perform tag checks"],
        ),
        (
            "extra variable value",
            &good,
            &["install", "--extra-vars", "so"],
            &[],
        ),
        (
            "extra variable equals",
            &good,
            &["install", "--extra-vars=so"],
            &[],
        ),
        (
            "skip tag value",
            &good,
            &["install", "--skip-tags", "so"],
            &[],
        ),
        (
            "skip tags equals",
            &good,
            &["install", "--skip-tags=so"],
            &[],
        ),
        (
            "corrupt cache still no files",
            &corrupt,
            &["install", "so"],
            &[],
        ),
        (
            "flags unaffected by corrupt cache",
            &corrupt,
            &["install", "--ver"],
            &[VERBOSE],
        ),
    ];
    for (name, root, words, want) in cases {
        let got = candidates(&request(root.path(), logs.path(), words));
        assert_eq!(got, *want, "{name}");
    }
    let words = ["install", "--extra-vars", "sonarr", "plex", ""];
    let got = candidates(&request(good.path(), logs.path(), &words));
    assert_eq!(got[..2], ["radarr", "sonarr"], "{got:?}");
    assert!(got[2..].iter().all(|flag| flag.starts_with('-')), "{got:?}");
    assert!(
        got.iter().any(|flag| flag.starts_with("--extra-vars\t")),
        "{got:?}"
    );
    // A request is never logged: the directory a root run would log in
    // was never made.
    assert_eq!(std::fs::read_dir(logs.path()).expect("list").count(), 0);
}

/// Bash's and zsh's stubs pass the current word's index, and the words
/// after the cursor too: only the words before it are earlier tags.
#[test]
fn the_index_marks_the_current_word() {
    let root = saltbox(&["sonarr", "plex", "radarr", "sabnzbd"], None);
    for shell in ["bash", "zsh"] {
        let output = common::sb()
            .env("SB_COMPLETE", shell)
            .env("SB_SALTBOX_DIR", root.path())
            .env("_CLAP_COMPLETE_INDEX", "3")
            .args(["--", "sb", "install", "plex", "s", "sonarr"])
            .output()
            .expect("run sb");
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8(output.stdout).expect("UTF-8");
        let got: Vec<&str> = text
            .split(['\n', ':'])
            .filter(|line| !line.is_empty())
            .collect();
        assert_eq!(got, ["sabnzbd", "sonarr"], "{shell}: {text:?}");
    }
}

/// Without `SB_COMPLETE`, or with it empty or `0`, sb runs as normal.
#[test]
fn an_unset_empty_or_zero_variable_runs_sb() {
    for value in [None, Some(""), Some("0")] {
        let mut sb = common::sb();
        if let Some(value) = value {
            sb.env("SB_COMPLETE", value);
        } else {
            sb.env_remove("SB_COMPLETE");
        }
        let output = sb.args(["--", "sb", "v"]).output().expect("run sb");
        // `--` then words: a usage error of the real parse, not candidates.
        assert_eq!(output.status.code(), Some(2), "{value:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{value:?}: {output:?}");
    }
}

/// `completion generate <shell>` prints clap's stub for `sb`, calling
/// `sb` on `PATH` with `SB_COMPLETE`; another shell is a usage error.
#[test]
fn generate_prints_the_stub_for_sb() {
    for (shell, first) in [
        ("bash", "\n_clap_complete_sb() {"),
        ("zsh", "#compdef sb"),
        (
            "fish",
            "complete --keep-order --exclusive --command sb --arguments \"(SB_COMPLETE=fish sb -- ",
        ),
    ] {
        let output = common::sb()
            .args(["completion", "generate", shell])
            .output()
            .expect("run sb");
        assert!(output.status.success(), "{shell}: {output:?}");
        let text = String::from_utf8(output.stdout).expect("UTF-8");
        assert!(text.starts_with(first), "{shell}: {text}");
        assert!(text.contains("SB_COMPLETE"), "{shell}: {text}");
    }
    common::sb()
        .args(["completion", "generate", "powershell"])
        .assert()
        .code(2)
        .stdout("");
}

/// A broken pipe on `generate`'s output is sb's quiet 141, not a panic.
/// The read end is closed before sb starts, so the write always fails
/// (<https://doc.rust-lang.org/std/io/fn.pipe.html>), as errors.rs does.
#[test]
fn generate_into_a_closed_pipe_is_quiet() {
    let (reader, writer) = std::io::pipe().expect("pipe");
    drop(reader);
    let output = common::sb()
        .args(["completion", "generate", "bash"])
        .stdout(writer)
        .output()
        .expect("run sb");
    assert_eq!(output.status.code(), Some(141), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

/// Spec success criterion 3: a request as an unprivileged user reads the
/// cache, never calls sudo, writes nothing and isn't logged; so does
/// `completion generate`.
#[test]
fn a_request_runs_as_the_calling_user() {
    if !root_gate::requested() {
        return;
    }
    let root = saltbox(TAGS, None);
    let parent = tempfile::tempdir().expect("temp dir");
    std::fs::set_permissions(parent.path(), std::fs::Permissions::from_mode(0o777)).expect("chmod");
    let log_dir = parent.path().join("logs");
    let sudo_dir = tempfile::tempdir().expect("temp dir");
    let sudo = "#!/bin/sh\necho 'sudo was called' >&2\nexit 42\n";
    let saltbox_dir = root.path().display().to_string();
    let env = [
        ("SB_COMPLETE", "fish"),
        ("SB_SALTBOX_DIR", saltbox_dir.as_str()),
    ];
    let (_, output) = nobody::sb_as_nobody(
        &["--", "sb", "install", "so"],
        &log_dir,
        sudo_dir.path(),
        sudo,
        &env,
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"sonarr\n", "{output:?}");
    // Under the gate's coverage build, `nobody` can't write its profile
    // (LLVM says so on stderr); nothing else may be printed.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr
            .lines()
            .all(|line| line.starts_with("LLVM Profile Error")),
        "{output:?}"
    );
    let sudo_dir = tempfile::tempdir().expect("temp dir");
    let (_, output) = nobody::sb_as_nobody(
        &["completion", "generate", "zsh"],
        &log_dir,
        sudo_dir.path(),
        sudo,
        &[],
    );
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.starts_with(b"#compdef sb\n"), "{output:?}");
    assert!(!log_dir.exists(), "an unprivileged run logged");
    let files: Vec<_> = std::fs::read_dir(root.path()).expect("list").collect();
    assert_eq!(files.len(), 1, "the request wrote in the Saltbox root");
}
