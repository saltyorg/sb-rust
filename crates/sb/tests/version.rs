//! `sb version` end to end (spec success criterion 1, output shape).

mod common;

use assert_cmd::prelude::*;
use common::sb;
use predicates::prelude::*;

#[test]
fn version_prints_the_sb_go_line() {
    sb().arg("version").assert().success().stdout(
        predicate::str::is_match(r"^Saltbox CLI version: \S+ \(commit: \S*\)\n$").expect("regex"),
    );
}

/// Compact JSON, keys in sb-go's order, ending in a newline.
#[test]
fn version_json_has_sb_go_keys_in_order() {
    sb().args(["version", "--json"]).assert().success().stdout(
        predicate::str::is_match(
            r#"^\{"version":"[^"]+","git_commit":"[^"]*","uv_version":"[^"]+"\}\n$"#,
        )
        .expect("regex"),
    );
}

#[test]
fn there_is_no_version_flag() {
    sb().arg("--version").assert().failure();
}
