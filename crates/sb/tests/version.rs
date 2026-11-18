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
/// in a newline; `uv_version` a version, as sb-go's update-saltbox-python
/// action reads it.
#[test]
fn version_json_has_sb_go_keys_in_order() {
    sb().args(["version", "--json"]).assert().success().stdout(
        predicate::str::is_match(
            r#"^\{"version":"[^"]+","git_commit":"[^"]*","uv_version":"[0-9]+\.[0-9]+\.[0-9]+","channel":"(release|beta|none)"\}\n$"#,
        )
        .expect("regex"),
    );
}

#[test]
fn there_is_no_version_flag() {
    sb().arg("--version").assert().failure();
}
