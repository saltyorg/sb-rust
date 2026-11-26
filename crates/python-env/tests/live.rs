//! python-env's live acceptance (python-env spec, Testing strategy,
//! "Live"; module build M21): opt-in with `SB_LIVE_PYTHON=1`, it runs
//! `tests/live.sh`, which builds real venvs with sb and sb-go in a
//! throwaway `ubuntu:<release>` container per supported release, one at a
//! time, never on the developer's host. The script's header names what it
//! needs (`SBP_SB`, `SBP_SBGO`, `SBP_SALTBOX`) and what it checks; the
//! container reaches the release metadata proxy, GitHub, the Python
//! package index and Ubuntu's apt mirrors.
//!
//! ```text
//! SB_LIVE_PYTHON=1 SBP_SB=... SBP_SBGO=... SBP_SALTBOX=... \
//!   cargo test -p sb-python-env --test live -- --test-threads=1
//! ```
//!
//! `SBP_RELEASES` (space-separated) narrows the releases.

use std::path::Path;
use std::process::Command;

#[test]
fn sb_and_sb_go_build_and_accept_each_others_venvs_on_each_release() {
    // Any value but `1` (`true`, `yes`) looks like asking: refused rather
    // than quietly skipped, as sb-docker's live test does.
    let Some(value) = std::env::var_os("SB_LIVE_PYTHON") else {
        sb_cli_core::testing::skip("set SB_LIVE_PYTHON=1 to build venvs in containers");
        return;
    };
    assert_eq!(value, "1", "SB_LIVE_PYTHON must be 1 to run the containers");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/live.sh");
    let releases = std::env::var("SBP_RELEASES").unwrap_or_default();
    let status = Command::new("bash")
        .arg(&script)
        .args(releases.split_whitespace())
        .status()
        .expect("bash runs the script");
    assert!(status.success(), "live.sh failed: {status}");
}
