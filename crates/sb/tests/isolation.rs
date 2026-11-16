//! Every sb test run gets `common::sb()`'s environment, which keeps it off
//! the host (its log directory, Docker daemon, theme file and Saltbox
//! checkout; config plan Task 3). A test that starts sb by its path
//! through another program (`script`, `unshare`, `setpriv`) must copy that
//! environment with `common::apply_sb_env`.

use std::path::{Path, PathBuf};

/// Test files that use sb's path without starting it, each with why.
/// (A later launch by path in one of these goes unchecked: keep them few.)
const PATH_ONLY: [(&str, &str); 2] = [
    (
        "docker.rs",
        "compares the path a fake sudo recorded; sb isn't started from it",
    ),
    (
        "raw_pty/mod.rs",
        "copies a Command its caller built, with apply_sb_env already applied, onto a PTY",
    ),
];

/// Ways a test can name sb's binary other than through `common::sb()`.
const BY_PATH: [&str; 3] = ["get_program()", "cargo_bin(", "CARGO_BIN_EXE_sb"];

/// Every `.rs` file under `dir`, at any depth.
fn rust_files(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            rust_files(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

/// A file-level check: one `apply_sb_env(` call excuses the whole file.
#[test]
fn every_test_that_starts_sb_by_path_copies_its_environment() {
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut files = Vec::new();
    rust_files(&tests, &mut files);
    let mut missing = Vec::new();
    for path in files {
        let relative = path.strip_prefix(&tests).expect("under tests");
        let name = relative.to_string_lossy();
        if name == "common/mod.rs"
            || name == "isolation.rs"
            || PATH_ONLY.iter().any(|(file, _)| *file == name)
        {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read test");
        if BY_PATH.iter().any(|way| text.contains(way)) && !text.contains("apply_sb_env(") {
            missing.push(name.into_owned());
        }
    }
    missing.sort();
    assert!(
        missing.is_empty(),
        "start sb with common::apply_sb_env: {missing:?}"
    );
}
