//! Tests that need root (a private mount namespace, switching to `nobody`,
//! a root run that logs) run only when asked with `SB_TEST_ROOT=1`
//! (cli-core spec, Testing strategy); `scripts/check.sh` asks when it runs
//! as root. Asked but not root is a failure, never a quiet pass.

/// Whether this run asked for the root tests; panics if it asked without
/// being root (a path no test exercises: the gate is the test harness).
pub fn requested() -> bool {
    let value = std::env::var_os("SB_TEST_ROOT");
    // Any value but `1` (`true`, `yes`) looks like asking; refuse it rather
    // than quietly not running the tests.
    assert!(
        value.as_ref().is_none_or(|v| v == "1"),
        "SB_TEST_ROOT must be 1 to run the root tests, got {value:?}"
    );
    let asked = value.is_some();
    if asked {
        assert_eq!(
            sb_cli_core::root::effective_uid(),
            0,
            "SB_TEST_ROOT=1 needs root"
        );
    } else {
        eprintln!("not run: set SB_TEST_ROOT=1 (as root) to run the root tests");
    }
    asked
}
