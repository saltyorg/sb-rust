//! Stand-in programs for sb-host's tests (host spec, Testing Strategy):
//! shell scripts written to a temporary directory, under the ETXTBSY
//! guard docker's tests use.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

/// Tests that write a stand-in program and run it hold this from writing
/// it until its run is over, so no other test's fork holds the program's
/// write descriptor when it runs (exec fails with ETXTBSY otherwise).
pub(crate) static SPAWNS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// An executable `name` in `dir` running `script` with `sh`.
pub(crate) fn stand_in(dir: &Path, name: &str, script: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("write stand-in");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}
