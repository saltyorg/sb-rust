//! Tab completion through the binary, with no shell (completion spec,
//! Testing strategy): a request is `SB_COMPLETE=<shell> sb -- <words>`,
//! as clap's stubs make it, with the cache in the test's Saltbox root.
//! sb-go's `cmd/tag_completion_test.go` `TestInstallCompletionProtocol`
//! cases, `generate`, and that a request prints only candidates, is never
//! logged and runs as the calling user.

mod common;
mod nobody;
mod root_gate;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use assert_cmd::prelude::*;

/// A Saltbox root whose cache holds `tags` for Saltbox, or `text` as the
/// file when given; world-readable, so `nobody` can read it too.
fn saltbox(tags: &[&str], text: Option<&str>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let cache = text.map_or_else(
        || {
            let entry = serde_json::json!({ "commit": "a", "tags": tags });
            serde_json::json!({ dir.path().display().to_string(): entry }).to_string()
        },
        ToString::to_string,
    );
    let file = dir.path().join("cache.json");
    std::fs::write(&file, cache).expect("cache");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    dir
}

/// A fish request for `words` (after `sb`) against `root`, logging under
/// `logs`: fish's stub passes the words up to the cursor, the current one
/// last, and no index; each candidate on its own line, a flag's help
/// after a tab (`clap_complete` 4.6.11, `env/shells.rs`).
fn request(root: &Path, logs: &Path, words: &[&str]) -> std::process::Output {
    common::sb()
        .env("SB_COMPLETE", "fish")
        .env("SB_SALTBOX_DIR", root)
        .env("SB_LOG_DIR", logs)
        .env_remove("_CLAP_COMPLETE_INDEX")
        .arg("--")
        .arg("sb")
        .args(words)
        .output()
        .expect("run sb")
}

/// The candidates `request` printed, after checking it succeeded and
/// printed nothing else.
fn candidates(output: &std::process::Output) -> Vec<String> {
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    String::from_utf8(output.stdout.clone())
        .expect("UTF-8")
        .lines()
        .map(ToString::to_string)
        .collect()
}

const TAGS: &[&str] = &["sonarr", "plex", "radarr"];
const VERBOSE: &str = "--verbose\tIncrease verbosity level (can be used multiple times, e.g. -vvv)";

/// sb-go's protocol cases, through the binary. One difference: on an
/// empty word `clap_complete` offers the flags as well as the tags (spec,
/// difference 6), so that case checks the tags and that the flags follow.
#[test]
fn sb_gos_protocol_cases() {
    let good = saltbox(TAGS, None);
    let corrupt = saltbox(&[], Some("{broken JSON"));
    let logs = tempfile::tempdir().expect("temp dir");
    let cases: &[(&str, &tempfile::TempDir, &[&str], &[&str])] = &[
        ("noun prefix", &good, &["install", "so"], &["sonarr"]),
        (
            "comma noun",
            &good,
            &["install", "plex,so"],
            &["plex,sonarr"],
        ),
        (
            "quoted comma whitespace",
            &good,
            &["install", "plex, so"],
            &["plex, sonarr"],
        ),
        ("long flags", &good, &["install", "--ver"], &[VERBOSE]),
        (
            "flags after noun",
            &good,
            &["install", "plex", "--no"],
            &["--no-cache\tSkip cache validation and always perform tag checks"],
        ),
        (
            "extra variable value",
            &good,
            &["install", "--extra-vars", "so"],
            &[],
        ),
        (
            "extra variable equals",
            &good,
            &["install", "--extra-vars=so"],
            &[],
        ),
        (
            "skip tag value",
            &good,
            &["install", "--skip-tags", "so"],
            &[],
        ),
        (
            "skip tags equals",
            &good,
            &["install", "--skip-tags=so"],
            &[],
        ),
        (
            "corrupt cache still no files",
            &corrupt,
            &["install", "so"],
            &[],
        ),
        (
            "flags unaffected by corrupt cache",
            &corrupt,
            &["install", "--ver"],
            &[VERBOSE],
        ),
    ];
    for (name, root, words, want) in cases {
        let got = candidates(&request(root.path(), logs.path(), words));
        assert_eq!(got, *want, "{name}");
    }
    let words = ["install", "--extra-vars", "sonarr", "plex", ""];
    let got = candidates(&request(good.path(), logs.path(), &words));
    assert_eq!(got[..2], ["radarr", "sonarr"], "{got:?}");
    assert!(got[2..].iter().all(|flag| flag.starts_with('-')), "{got:?}");
    assert!(
        got.iter().any(|flag| flag.starts_with("--extra-vars\t")),
        "{got:?}"
    );
    // A request is never logged: the directory a root run would log in
    // was never made.
    assert_eq!(std::fs::read_dir(logs.path()).expect("list").count(), 0);
}

/// Bash's and zsh's stubs pass the current word's index, and the words
/// after the cursor too: only the words before it are earlier tags.
#[test]
fn the_index_marks_the_current_word() {
    let root = saltbox(&["sonarr", "plex", "radarr", "sabnzbd"], None);
    for shell in ["bash", "zsh"] {
        let output = common::sb()
            .env("SB_COMPLETE", shell)
            .env("SB_SALTBOX_DIR", root.path())
            .env("_CLAP_COMPLETE_INDEX", "3")
            .args(["--", "sb", "install", "plex", "s", "sonarr"])
            .output()
            .expect("run sb");
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8(output.stdout).expect("UTF-8");
        let got: Vec<&str> = text
            .split(['\n', ':'])
            .filter(|line| !line.is_empty())
            .collect();
        assert_eq!(got, ["sabnzbd", "sonarr"], "{shell}: {text:?}");
    }
}

/// A request from `shell`'s stub for `words` (after `sb`), the current
/// word at `index` (bash's and zsh's `_CLAP_COMPLETE_INDEX`; fish passes
/// none), with `root`'s cache; its reply, after checking it succeeded and
/// printed nothing else.
fn reply(shell: &str, root: &Path, words: &[&str], index: usize) -> String {
    let logs = tempfile::tempdir().expect("temp dir");
    let output = common::sb()
        .env("SB_COMPLETE", shell)
        .env("SB_SALTBOX_DIR", root)
        .env("SB_LOG_DIR", logs.path())
        .env("_CLAP_COMPLETE_INDEX", index.to_string())
        .env("_CLAP_IFS", "\n")
        .args(["--", "sb"])
        .args(words)
        .output()
        .expect("run sb");
    assert!(output.status.success(), "{shell} {words:?}: {output:?}");
    assert!(output.stderr.is_empty(), "{shell} {words:?}: {output:?}");
    String::from_utf8(output.stdout).expect("UTF-8")
}

/// Hidden commands and flags are never offered, whatever the prefix,
/// in each shell (spec, "Hidden commands and flags are never offered"):
/// the completion report's cases, `help`'s copies of the commands, the
/// hidden `--verbose` and a hidden name typed in full. `clap_complete`
/// offers each when nothing shown matches.
#[test]
fn hidden_commands_and_flags_are_never_offered() {
    let root = saltbox(TAGS, None);
    let cases: &[&[&str]] = &[
        &["self"],
        &["comp"],
        &["setu"],
        &["gh"],
        &["update-"],
        &["rest"],
        &["self-update"],
        &["help", "self"],
        &["announcements", "p"],
        &["announcements", "c"],
        &["update", "--skip-g"],
        &["update", "--skip-git"],
        &["update", "--verify"],
        &["update", "--verb"],
        &["install", "--force"],
        &["completion", "install", "--a"],
    ];
    for shell in ["bash", "zsh", "fish"] {
        for words in cases {
            let got = reply(shell, root.path(), words, words.len());
            assert_eq!(got, "", "{shell} {words:?}");
        }
    }
}

/// What stays: a hidden command's own shown flags once its name is
/// typed, a shown command's hidden alias on a prefix, and shown items.
#[test]
fn shown_items_and_aliases_still_complete() {
    let root = saltbox(TAGS, None);
    let flags = reply("fish", root.path(), &["self-update", "--"], 2);
    let flags: Vec<&str> = flags
        .lines()
        .map(|line| line.split('\t').next().unwrap_or_default())
        .collect();
    assert_eq!(flags, ["--yes", "--color", "--channel", "--help"]);
    assert_eq!(
        reply("fish", root.path(), &["edit", "accounts."], 2),
        "accounts.yml\tAccounts\n"
    );
    assert_eq!(reply("bash", root.path(), &["up"], 1), "update");
    assert_eq!(
        reply("zsh", root.path(), &["up"], 1),
        "update:Update Saltbox & Sandbox"
    );
}

/// On an empty word where only flags would be offered, nothing is
/// (spec, difference 6, the maintainer's rule of 2026-11-20): the shells
/// inserted `--`. A typed `-` gets the flags; subcommands, tags and
/// values are offered as before, flags with them.
#[test]
fn an_empty_word_with_only_flags_gets_nothing() {
    let tags = saltbox(TAGS, None);
    let empty = saltbox(&[], None);
    let corrupt = saltbox(&[], Some("{broken JSON"));
    let missing = tempfile::tempdir().expect("temp dir");
    let cases: &[(&Path, &[&str])] = &[
        (tags.path(), &["update", ""]),
        (tags.path(), &["version", ""]),
        (tags.path(), &["branch", ""]),
        (tags.path(), &["docker", "restart", ""]),
        (tags.path(), &["update", "--skip-self-update", ""]),
        (empty.path(), &["install", ""]),
        (corrupt.path(), &["install", ""]),
        (missing.path(), &["install", ""]),
    ];
    for shell in ["bash", "zsh", "fish"] {
        for (root, words) in cases {
            // `sb` is word 0.
            let index = words.len();
            assert_eq!(reply(shell, root, words, index), "", "{shell} {words:?}");
            // zsh's stub passes no word for an empty one at the end.
            if shell == "zsh" {
                let typed = &words[..index - 1];
                assert_eq!(reply(shell, root, typed, index), "", "{shell} {typed:?}");
            }
        }
        let dash = reply(shell, tags.path(), &["update", "-"], 2);
        assert!(dash.contains("--keep-branch"), "{shell}: {dash:?}");
        let install = reply(shell, tags.path(), &["install", ""], 2);
        assert!(install.starts_with("plex"), "{shell}: {install:?}");
        assert!(install.contains("--no-cache"), "{shell}: {install:?}");
        let docker = reply(shell, tags.path(), &["docker", ""], 2);
        assert!(docker.starts_with("ps"), "{shell}: {docker:?}");
        assert!(docker.contains("--color"), "{shell}: {docker:?}");
        let values = reply(shell, tags.path(), &["update", "--migrations", ""], 3);
        assert!(values.starts_with("run"), "{shell}: {values:?}");
    }
}

/// Without `SB_COMPLETE`, or with it empty or `0`, sb runs as normal.
#[test]
fn an_unset_empty_or_zero_variable_runs_sb() {
    for value in [None, Some(""), Some("0")] {
        let mut sb = common::sb();
        if let Some(value) = value {
            sb.env("SB_COMPLETE", value);
        } else {
            sb.env_remove("SB_COMPLETE");
        }
        let output = sb.args(["--", "sb", "v"]).output().expect("run sb");
        // `--` then words: a usage error of the real parse, not candidates.
        assert_eq!(output.status.code(), Some(2), "{value:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{value:?}: {output:?}");
    }
}

/// `completion generate <shell>` prints clap's stub for `sb`, calling
/// `sb` on `PATH` with `SB_COMPLETE`; another shell is a usage error.
#[test]
fn generate_prints_the_stub_for_sb() {
    for (shell, first) in [
        ("bash", "\n_clap_complete_sb() {"),
        ("zsh", "#compdef sb"),
        (
            "fish",
            "complete --keep-order --exclusive --command sb --arguments \"(SB_COMPLETE=fish sb -- ",
        ),
    ] {
        let output = common::sb()
            .args(["completion", "generate", shell])
            .output()
            .expect("run sb");
        assert!(output.status.success(), "{shell}: {output:?}");
        let text = String::from_utf8(output.stdout).expect("UTF-8");
        assert!(text.starts_with(first), "{shell}: {text}");
        assert!(text.contains("SB_COMPLETE"), "{shell}: {text}");
    }
    common::sb()
        .args(["completion", "generate", "powershell"])
        .assert()
        .code(2)
        .stdout("");
}

/// A broken pipe on `generate`'s output is sb's quiet 141, not a panic.
/// The read end is closed before sb starts, so the write always fails
/// (<https://doc.rust-lang.org/std/io/fn.pipe.html>), as errors.rs does.
#[test]
fn generate_into_a_closed_pipe_is_quiet() {
    let (reader, writer) = std::io::pipe().expect("pipe");
    drop(reader);
    let output = common::sb()
        .args(["completion", "generate", "bash"])
        .stdout(writer)
        .output()
        .expect("run sb");
    assert_eq!(output.status.code(), Some(141), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

/// Spec success criterion 3: a request as an unprivileged user reads the
/// cache, never calls sudo, writes nothing and isn't logged; so does
/// `completion generate`.
#[test]
fn a_request_runs_as_the_calling_user() {
    if !root_gate::requested() {
        return;
    }
    let root = saltbox(TAGS, None);
    let parent = tempfile::tempdir().expect("temp dir");
    std::fs::set_permissions(parent.path(), std::fs::Permissions::from_mode(0o777)).expect("chmod");
    let log_dir = parent.path().join("logs");
    let sudo_dir = tempfile::tempdir().expect("temp dir");
    let sudo = "#!/bin/sh\necho 'sudo was called' >&2\nexit 42\n";
    let saltbox_dir = root.path().display().to_string();
    let env = [
        ("SB_COMPLETE", "fish"),
        ("SB_SALTBOX_DIR", saltbox_dir.as_str()),
    ];
    let (_, output) = nobody::sb_as_nobody(
        &["--", "sb", "install", "so"],
        &log_dir,
        sudo_dir.path(),
        sudo,
        &env,
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"sonarr\n", "{output:?}");
    // Under the gate's coverage build, `nobody` can't write its profile
    // (LLVM says so on stderr); nothing else may be printed.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr
            .lines()
            .all(|line| line.starts_with("LLVM Profile Error")),
        "{output:?}"
    );
    let sudo_dir = tempfile::tempdir().expect("temp dir");
    let (_, output) = nobody::sb_as_nobody(
        &["completion", "generate", "zsh"],
        &log_dir,
        sudo_dir.path(),
        sudo,
        &[],
    );
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.starts_with(b"#compdef sb\n"), "{output:?}");
    assert!(!log_dir.exists(), "an unprivileged run logged");
    let files: Vec<_> = std::fs::read_dir(root.path()).expect("list").collect();
    assert_eq!(files.len(), 1, "the request wrote in the Saltbox root");
}

/// Tests that write a stand-in shell and run it hold this from writing it
/// until its run is over (ETXTBSY otherwise; as sb-host's `SPAWNS`).
static SPAWNS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// An install root with stand-in shells: bash with bash-completion, and
/// zsh and fish answering their probes, each only if asked for.
fn install_root(shells: &[&str]) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("temp dir");
    let write = |path: &str, text: &str, mode: u32| {
        let path = root.path().join(path);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(&path, text).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    };
    let zsh_dir = root.path().join("usr/local/share/zsh/site-functions");
    let sysconf = root.path().join("etc/fish");
    for shell in shells {
        let script = match *shell {
            "bash" => "exit 0".to_owned(),
            "zsh" => format!("printf '%s\\n' '{}'", zsh_dir.display()),
            "fish" => format!("printf '3.6.1\\n%s\\n' '{}'", sysconf.display()),
            "broken-zsh" => "exit 4".to_owned(),
            other => panic!("no stand-in {other}"),
        };
        let name = shell.trim_start_matches("broken-");
        write(
            &format!("usr/bin/{name}"),
            &format!("#!/bin/sh\n{script}\n"),
            0o755,
        );
    }
    write("usr/share/bash-completion/bash_completion", "", 0o644);
    root
}

/// This test build of sb, never `SB_TEST_BINARY`'s: only a build with
/// sb-completion's `testing` feature installs under the test's root; any
/// other writes the host's shell directories.
fn test_build() -> PathBuf {
    assert_cmd::cargo::cargo_bin("sb")
}

/// The test build of sb, with its install root at `root`, under a 0077
/// umask (sh's `umask`, then exec sb). Only with the root tests asked for
/// (`root_gate`): the installing forms need root, and a run that hands
/// over to sudo would lose `SB_TEST_COMPLETION_ROOT`.
fn completion_run(root: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = std::process::Command::new("sh");
    common::apply_sb_env(&mut cmd);
    cmd.env("SB_TEST_COMPLETION_ROOT", root)
        .args(["-c", "umask 077; exec \"$0\" \"$@\""])
        .arg(test_build())
        .args(args)
        .output()
        .expect("run sb")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Bare `sb completion` and `completion install` install every
/// installed shell's file, 0644 and in 0755 directories under a 0077
/// umask, with sb-go's lines; a missing shell is skipped, not an error.
#[test]
fn completion_installs_every_installed_shell() {
    if !root_gate::requested() {
        return;
    }
    let _spawns = SPAWNS.lock();
    let root = install_root(&["bash", "zsh"]);
    let at = |path: &str| root.path().join(path);
    for args in [&["completion"][..], &["completion", "install"][..]] {
        let output = completion_run(root.path(), args);
        assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
        assert_eq!(
            text(&output.stdout),
            format!(
                "Installed bash completion: {}\nInstalled zsh completion: {}\nSkipped fish completion: shell is not installed\n",
                at("usr/local/share/bash-completion/completions/sb.bash").display(),
                at("usr/local/share/zsh/site-functions/_sb").display(),
            ),
            "{args:?}"
        );
    }
    let mode = |path: &str| {
        std::fs::metadata(at(path))
            .expect("meta")
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(
        mode("usr/local/share/bash-completion/completions/sb.bash"),
        0o644
    );
    assert_eq!(mode("usr/local/share/zsh/site-functions/_sb"), 0o644);
    assert_eq!(mode("usr/local/share/bash-completion"), 0o755);
    assert_eq!(mode("usr/local/share/bash-completion/completions"), 0o755);
    let zsh = std::fs::read_to_string(at("usr/local/share/zsh/site-functions/_sb")).expect("zsh");
    assert!(
        zsh.starts_with("#compdef sb\n# sb completion zsh sb\n"),
        "{zsh}"
    );
}

/// Naming a shell makes it unavailable an error, after its line (exit
/// 1); `install <shell> --allow-unavailable` doesn't (released sb-go
/// 0.1.1's self-update passes it); another shell is a usage error.
#[test]
fn a_named_unavailable_shell_is_an_error_unless_allowed() {
    if !root_gate::requested() {
        return;
    }
    let _spawns = SPAWNS.lock();
    let root = install_root(&["bash"]);
    for args in [
        &["completion", "fish"][..],
        &["completion", "install", "fish"][..],
    ] {
        let output = completion_run(root.path(), args);
        assert_eq!(output.status.code(), Some(1), "{args:?}: {output:?}");
        assert_eq!(
            text(&output.stdout),
            "Skipped fish completion: shell is not installed\n"
        );
        let stderr = text(&output.stderr);
        assert!(
            stderr.contains("fish completion unavailable: shell is not installed"),
            "{stderr}"
        );
    }
    let output = completion_run(
        root.path(),
        &["completion", "install", "fish", "--allow-unavailable"],
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        text(&output.stdout),
        "Skipped fish completion: shell is not installed\n"
    );
    let output = completion_run(root.path(), &["completion", "bash"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let output = completion_run(root.path(), &["completion", "install", "powershell"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
}

/// A failed shell doesn't stop the others; the run ends with exit 1 and
/// the failure after every line.
#[test]
fn a_failed_shell_fails_the_run_after_the_others() {
    if !root_gate::requested() {
        return;
    }
    let _spawns = SPAWNS.lock();
    let root = install_root(&["bash", "broken-zsh", "fish"]);
    let output = completion_run(root.path(), &["completion"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stdout = text(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 3, "{stdout}");
    assert!(
        lines[0].starts_with("Installed bash completion: "),
        "{stdout}"
    );
    assert_eq!(
        lines[1],
        "Failed zsh completion: shell probe failed: exit status 4 ()"
    );
    assert!(
        lines[2].starts_with("Installed fish completion: "),
        "{stdout}"
    );
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("detect zsh: shell probe failed: exit status 4 ()"),
        "{stderr}"
    );
}

/// A symlink to sb in the bin directories gets its own file, calling the
/// symlink.
#[test]
fn a_symlink_name_gets_its_own_file() {
    if !root_gate::requested() {
        return;
    }
    let _spawns = SPAWNS.lock();
    let root = install_root(&["zsh"]);
    let link = root.path().join("usr/local/bin/saltbox");
    std::fs::create_dir_all(link.parent().expect("parent")).expect("dir");
    let sb = std::fs::canonicalize(test_build()).expect("sb");
    std::os::unix::fs::symlink(&sb, &link).expect("link");
    let output = completion_run(root.path(), &["completion", "zsh"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let file = root
        .path()
        .join("usr/local/share/zsh/site-functions/_saltbox");
    let zsh = std::fs::read_to_string(&file).expect("saltbox's file");
    assert!(
        zsh.starts_with("#compdef saltbox\n# sb completion zsh saltbox\n"),
        "{zsh}"
    );
    assert!(zsh.contains(&link.display().to_string()), "{zsh}");
    let own = std::fs::read_to_string(root.path().join("usr/local/share/zsh/site-functions/_sb"))
        .expect("sb's file");
    assert!(own.contains(&sb.display().to_string()), "{own}");
}
