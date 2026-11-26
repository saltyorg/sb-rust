//! A skip names the file of the test that skips, through `root_tests`
//! too (`#[track_caller]`): an integration test's name is its function's
//! alone, and two test binaries can hold a test of the same name, which
//! the gate's list would otherwise show once.
//!
//! The test re-runs this binary as the child with `SB_TEST_ROOT` unset
//! and `SB_TEST_SKIPS` naming a file of its own, so the gate's file and
//! the parent's environment are untouched.

use std::process::Command;

const CHILD: &str = "SB_SKIP_CHILD";

#[test]
fn a_skip_names_the_file_of_the_test_that_skips() {
    if std::env::var_os(CHILD).is_some() {
        // In the child: not asked for, so skipped, from this file.
        assert!(!sb_cli_core::testing::root_tests());
        sb_cli_core::testing::skip("a reason of its own");
        return;
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let skips = dir.path().join("skips");
    let out = Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "a_skip_names_the_file_of_the_test_that_skips",
            "--exact",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .env_remove("SB_TEST_ROOT")
        .env("SB_TEST_SKIPS", &skips)
        .output()
        .expect("run the child");
    assert!(
        out.status.success(),
        "{}: {}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&skips).expect("skips"),
        "skipped: a_skip_names_the_file_of_the_test_that_skips (crates/cli-core/tests/skip.rs): \
         needs root: set SB_TEST_ROOT=1 (as root) to run it\n\
         skipped: a_skip_names_the_file_of_the_test_that_skips (crates/cli-core/tests/skip.rs): \
         a reason of its own\n"
    );
}
