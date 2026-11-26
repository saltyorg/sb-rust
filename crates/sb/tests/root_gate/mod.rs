//! Tests that need root (a private mount namespace, switching to `nobody`,
//! a root run that logs) run only when asked with `SB_TEST_ROOT=1`
//! (cli-core spec, Testing strategy); `scripts/check.sh` asks when it runs
//! as root. Asked but not root is a failure, never a quiet pass; not asked,
//! the test is listed as skipped (sb-cli-core's `testing::root_tests`,
//! the one skip mechanism of the crates' tests).

pub use sb_cli_core::testing::root_tests as requested;
