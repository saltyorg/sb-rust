//! `sb version` end to end (spec success criterion 1, output shape).

mod common;

use assert_cmd::prelude::*;
use common::sb;
use predicates::prelude::*;

#[test]
fn version_prints_the_sb_go_line_then_the_channel() {
    sb().arg("version").assert().success().stdout(
        predicate::str::is_match(
            r"^Saltbox CLI version: \S+ \(commit: \S*\)\nRelease channel: (release|beta|none \(self-update disabled\))\n$",
        )
        .expect("regex"),
    );
}

/// Compact JSON, keys in sb-go's order and `channel` after them, ending
/// in a newline; `uv_version` a version.
#[test]
fn version_json_has_sb_go_keys_in_order() {
    sb().args(["version", "--json"]).assert().success().stdout(
        predicate::str::is_match(
            r#"^\{"version":"[^"]+","git_commit":"[^"]*","uv_version":"[0-9]+\.[0-9]+\.[0-9]+","channel":"(release|beta|none)"\}\n$"#,
        )
        .expect("regex"),
    );
}

/// `uv_version` keeps its name and its bare `X.Y.Z` form, and is the uv
/// this sb installs: Saltbox's toolchain workflow reads it from the latest
/// stable sb to raise master's `.uv-version` (ADR-031, D4).
#[test]
fn version_json_names_the_pinned_uv() {
    let out = sb().args(["version", "--json"]).output().expect("sb");
    assert!(out.status.success(), "{out:?}");
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    let uv = report["uv_version"].as_str().expect("uv_version, a string");
    assert_eq!(uv, sb_cli_core::build_info::UV_VERSION);
    let parts: Vec<&str> = uv.split('.').collect();
    assert_eq!(parts.len(), 3, "{uv}");
    assert!(
        parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())),
        "{uv}"
    );
}

#[test]
fn there_is_no_version_flag() {
    sb().arg("--version").assert().failure();
}
