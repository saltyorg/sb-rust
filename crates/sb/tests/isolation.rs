//! Every sb test run gets `common::sb()`'s environment, which keeps it off
//! the host (its log directory, Docker daemon, theme file and Saltbox
//! checkout; config plan Task 3). A test that starts sb by its path
//! through another program (`script`, `unshare`, `setpriv`) must copy that
//! environment with `common::apply_sb_env`. And a test file that runs
//! one of sb's commands that need root asks for the root tests
//! (`root_gate::requested()`): without it, a run that isn't root would
//! relaunch sb through sudo.

use std::path::{Path, PathBuf};

/// Test files that use sb's path without starting it, each with why.
/// (A later launch by path in one of these goes unchecked: keep them few.)
const PATH_ONLY: [(&str, &str); 1] = [(
    "raw_pty/mod.rs",
    "copies a Command its caller built, with apply_sb_env already applied, onto a PTY",
)];

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

/// sb's commands that need root (`Command::needs_root`,
/// crates/sb/src/main.rs): every one but `version` and `completion`. Of
/// completion's forms only `generate` needs none, and the installing ones
/// aren't checked here (their tests, completion.rs, are gated). sb's unit
/// test `isolation_lists_every_command_that_needs_root` keeps this list
/// equal to clap's commands and to `needs_root`.
const ROOT_COMMANDS: [&str; 22] = [
    "announcements",
    "bench",
    "branch",
    "branch-sandbox",
    "diag",
    "docker",
    "edit",
    "fact",
    "gha",
    "install",
    "list",
    "logs",
    "motd",
    "reinstall-facts",
    "reinstall-python",
    "reinstall-venv",
    "restore",
    "self-update",
    "setup",
    "update",
    "update-venv",
    "validate-config",
];

/// Test files that name root commands but never run one, each with why.
/// (A later root run in one of these goes unchecked: keep them few.)
const ROOT_FREE: [(&str, &str); 1] = [(
    "help.rs",
    "runs each command with --help or under `sb help` only, which never relaunch, and lists the commands it walks",
)];

/// Whether `text` gives sb `command` as an argument: a string literal of
/// it in an argument array (`["logs"`, `, "logs"]`, `, "logs",`, or a line
/// of its own inside an array rustfmt spread over lines) or to `.arg(`. A
/// path segment such as `join("logs")` is none of these, and neither is a
/// line that asks for help (`--help`, `-h`, `sb help ...`), which never
/// relaunches. A tripwire, not a proof: a command in a shell string
/// (`exec "$0" logs`) or passed as a plain string to a helper
/// (`on_a_terminal("validate-config", ..)`) is not seen. Why not parse the
/// source (syn): it is no dependency of sb's, and this file's other check
/// reads the text the same way.
fn runs(text: &str, command: &str) -> bool {
    let quoted = format!("\"{command}\"");
    let mut spread = false;
    for line in text.lines() {
        let line = line.trim();
        let help = ["\"--help\"", "\"-h\"", "[\"help\""]
            .iter()
            .any(|asked| line.contains(asked));
        let inside = spread;
        if line.ends_with('[') {
            spread = true;
        } else if line.starts_with(']') {
            spread = false;
        }
        if help {
            continue;
        }
        if line.contains(&format!("[{quoted}"))
            || line.contains(&format!(", {quoted}]"))
            || line.contains(&format!(", {quoted},"))
            || line.contains(&format!("arg({quoted})"))
            || (inside && line.starts_with(&quoted))
        {
            return true;
        }
    }
    false
}

/// The test files under `dir` (a `#[test]` in them) that run one of
/// [`ROOT_COMMANDS`] and never call `root_gate::requested()`, each with
/// the command, sorted. A file-level check: one call excuses the whole
/// file.
fn ungated(dir: &Path) -> Vec<String> {
    let mut files = Vec::new();
    rust_files(dir, &mut files);
    let mut missing = Vec::new();
    for path in files {
        let name = path
            .strip_prefix(dir)
            .expect("under tests")
            .to_string_lossy()
            .into_owned();
        if name == "isolation.rs" || ROOT_FREE.iter().any(|(file, _)| *file == name) {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read test");
        if !text.contains("#[test]") || text.contains("root_gate::requested()") {
            continue;
        }
        if let Some(command) = ROOT_COMMANDS.iter().find(|c| runs(&text, c)) {
            missing.push(format!("{name} ({command})"));
        }
    }
    missing.sort();
    missing
}

#[test]
fn every_test_file_that_runs_a_root_command_asks_for_the_root_tests() {
    let missing = ungated(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests"));
    assert!(
        missing.is_empty(),
        "call root_gate::requested() in the tests that run these: {missing:?}"
    );
}

/// The check finds a planted file that runs `sb logs` with no gate, in
/// each way an argument is written, and passes it once it is gated; a
/// file that only names a directory `logs`, or runs `version`, passes.
#[test]
fn a_test_file_that_runs_a_root_command_without_the_gate_is_found() {
    let dir = tempfile::tempdir().expect("dir");
    let plant = |body: &str| {
        std::fs::write(
            dir.path().join("planted.rs"),
            format!("#[test]\nfn t() {{\n{body}\n}}\n"),
        )
        .expect("plant");
        ungated(dir.path())
    };
    for body in [
        "common::sb().args([\"logs\"]).assert();",
        "world.piped(&[\"--color=never\", \"logs\"]);",
        "Pty::spawn(&[\"-v\", \"logs\", \"x\"], envs);",
        "common::sb().arg(\"logs\").assert();",
        "common::sb()\n    .args([\n        \"logs\",\n    ])\n    .assert();",
        "common::sb()\n    .args([\n        \"--color=never\",\n        \"logs\",\n    ])\n    .assert();",
    ] {
        assert_eq!(plant(body), ["planted.rs (logs)"], "{body}");
    }
    let none: [&str; 0] = [];
    assert_eq!(
        plant("if !root_gate::requested() { return; }\ncommon::sb().args([\"logs\"]);"),
        none
    );
    assert_eq!(
        plant("let dir = world.join(\"logs\");\ncommon::sb().args([\"version\"]);"),
        none
    );
    assert_eq!(
        plant(
            "common::sb().args([\"logs\", \"--help\"]);\ncommon::sb().args(&[\"help\", \"logs\"]);"
        ),
        none
    );
}
