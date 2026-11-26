//! sb-repo's tests run git, their own and `Repo`'s, with none of the
//! caller's repository variables and none of the host's git
//! configuration. A hook or a caller's git that exported `GIT_DIR` would
//! otherwise send every command here to that repository (commits, config
//! and resets included), and a host's `branch.autoSetupMerge` or ignore
//! file would change what `Repo` does.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// The variables local to a repository, as git lists them
/// (`git rev-parse --local-env-vars`: `GIT_DIR`, `GIT_WORK_TREE`, the
/// command line's config, `GIT_CONFIG_PARAMETERS` and `GIT_CONFIG_COUNT`,
/// ...: <https://git-scm.com/docs/git-rev-parse#Documentation/git-rev-parse.txt---local-env-vars>,
/// as git-sh-setup's `clear_local_git_env` unsets them), and
/// `GIT_NAMESPACE`, which git(1) lists beside them
/// (<https://git-scm.com/docs/git#_the_git_repository>).
fn local_variables() -> &'static [String] {
    static LOCAL: OnceLock<Vec<String>> = OnceLock::new();
    LOCAL.get_or_init(|| {
        // Asked with no configuration of the host's or the caller's, which
        // could stop it (a bad line, a bad `-c`).
        let empty = tempfile::NamedTempFile::new().expect("empty config");
        let out = Command::new("git")
            .args(["rev-parse", "--local-env-vars"])
            .env("GIT_CONFIG_GLOBAL", empty.path())
            .env("GIT_CONFIG_SYSTEM", empty.path())
            .env_remove("GIT_CONFIG_PARAMETERS")
            .env_remove("GIT_CONFIG_COUNT")
            .output()
            .expect("run git rev-parse");
        assert!(
            out.status.success(),
            "git rev-parse --local-env-vars: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let mut names: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .map(str::to_string)
            .collect();
        assert!(names.iter().any(|name| name == "GIT_DIR"), "{names:?}");
        names.push("GIT_NAMESPACE".to_string());
        names
    })
}

/// Removes the caller's repository variables from `cmd`, a git, and stops
/// its search for a repository below the temporary directory, which holds
/// each test's own: git never moves up into a directory listed in
/// `GIT_CEILING_DIRECTORIES`
/// (<https://git-scm.com/docs/git#Documentation/git.txt-GITCEILINGDIRECTORIES>),
/// so a temporary directory inside a repository finds none.
pub fn isolate(cmd: &mut Command) -> &mut Command {
    for name in local_variables() {
        cmd.env_remove(name);
    }
    cmd.env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
}

/// `path` in single quotes for `sh`, a quote in it closed and escaped.
fn quoted(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

/// Writes a stand-in git at `path` (absolute) that runs the shell `body`.
///
/// `body` ends in `exec git ...` to run the real one, which runs as
/// [`isolate`] runs git, with no global or system configuration (an empty file beside it,
/// `GIT_CONFIG_GLOBAL` and `GIT_CONFIG_SYSTEM`:
/// <https://git-scm.com/docs/git#Documentation/git.txt-GITCONFIGGLOBAL>)
/// nor the per-user ignore and attributes files, whose default place is
/// under `XDG_CONFIG_HOME`, here a directory that doesn't exist
/// (<https://git-scm.com/docs/git-config#Documentation/git-config.txt-coreexcludesFile>).
/// For `Repo::with_git` and `Repo::clone_with_git`: `Repo`'s git inherits
/// the test's environment, which the test can't change while other tests
/// run, so only what the stand-in passes on reaches it. Written by a child
/// (`write_program`), so no other test's fork makes its exec fail with
/// ETXTBSY.
pub fn stand_in(path: &Path, body: &str) -> PathBuf {
    let dir = path.parent().expect("a directory");
    let empty = dir.join("empty.gitconfig");
    std::fs::write(&empty, "").expect("empty config");
    let script = format!(
        "#!/bin/sh\nunset {}\n\
         GIT_CEILING_DIRECTORIES={}\nGIT_CONFIG_GLOBAL={}\nGIT_CONFIG_SYSTEM={}\nXDG_CONFIG_HOME={}\n\
         export GIT_CEILING_DIRECTORIES GIT_CONFIG_GLOBAL GIT_CONFIG_SYSTEM XDG_CONFIG_HOME\n{body}",
        local_variables().join(" "),
        quoted(&std::env::temp_dir()),
        quoted(&empty),
        quoted(&empty),
        quoted(&dir.join("no-xdg")),
    );
    sb_cli_core::testing::write_program(path, script);
    path.to_path_buf()
}

/// The plain stand-in in `dir`: the real git, as [`stand_in`] runs it.
pub fn isolated_git(dir: &Path) -> PathBuf {
    stand_in(&dir.join("git"), "exec git \"$@\"\n")
}
