//! `refresh(step)` for ops' `update` and `setup` (completion spec,
//! "Refresh for `update` and `setup`"; sb-go's
//! `regenerateInstalledCompletions` and its tests): one sub-step per
//! shell under "Refreshing shell completions", the install's lines as its
//! output, and a failure a warning, never the caller's failure. Under a
//! temporary root, with stand-in shells; on a runner drawing to memory.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use sb_completion::install::Paths;
use sb_term_ui::{Labels, Options, Runner, Target};

/// Tests that write a stand-in shell and run it hold this from writing it
/// until its run is over (ETXTBSY otherwise; as sb-host's `SPAWNS`).
static SPAWNS: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn write(path: &Path, text: &str, mode: u32) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
    std::fs::write(path, text).expect("write");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// A root with sb's own executable, bash with bash-completion, zsh as
/// `zsh_script` says, and no fish.
fn root(zsh_script: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let sb = dir.path().join("opt/sb/sb");
    write(&sb, "#!/bin/sh\n", 0o755);
    write(
        &dir.path().join("usr/bin/bash"),
        "#!/bin/sh\nexit 0\n",
        0o755,
    );
    write(
        &dir.path().join("usr/share/bash-completion/bash_completion"),
        "",
        0o644,
    );
    write(
        &dir.path().join("usr/bin/zsh"),
        &format!("#!/bin/sh\n{zsh_script}\n"),
        0o755,
    );
    (dir, sb)
}

/// What the run showed, and whether the caller's step succeeded.
struct Ran {
    screen: String,
    ok: bool,
}

/// `refresh_with` inside a caller's step, then that step's own end, on a
/// runner drawing to memory.
fn refreshed(paths: &Paths, executable: &Path) -> Ran {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let term = indicatif::InMemoryTerm::new(60, 200);
        let options = Options {
            target: Target::InMemory {
                term: term.clone(),
                colour: false,
            },
        };
        let shutdown = sb_cli_core::cancel::Shutdown::new();
        let mut runner = Runner::start("sb update", &shutdown, None, options);
        let result = runner
            .step(Labels::new("Updating", "Updated"), async |step| {
                sb_completion::refresh_with(step, paths, executable).await;
                Ok(())
            })
            .await;
        runner.finish();
        Ran {
            screen: term.contents(),
            ok: result.is_ok(),
        }
    })
}

/// A step per shell under the refresh's own, each checked; the files
/// written; nothing left of the details once they succeeded.
#[test]
fn each_shell_gets_a_step() {
    let _spawns = SPAWNS.lock();
    // zsh's clean fpath holds the root's site-functions (the stand-in is
    // at <root>/usr/bin/zsh).
    let (dir, sb) =
        root(r#"printf '%s/local/share/zsh/site-functions\n' "$(cd "$(dirname "$0")/.." && pwd)""#);
    let ran = refreshed(&Paths::under(dir.path()), &sb);
    assert!(ran.ok, "{}", ran.screen);
    let mut at = 0;
    for label in [
        "Shell completions refreshed",
        "Bash completion checked",
        "Zsh completion checked",
        "Fish completion checked",
    ] {
        let found = ran.screen[at..].find(label);
        assert!(found.is_some(), "{label} after {at}: {}", ran.screen);
        at += found.unwrap_or_default() + label.len();
    }
    assert!(
        !ran.screen.contains("Installed bash completion"),
        "{}",
        ran.screen
    );
    assert!(!ran.screen.contains('!'), "{}", ran.screen);
    assert!(
        dir.path()
            .join("usr/local/share/bash-completion/completions/sb.bash")
            .is_file()
    );
    assert!(
        dir.path()
            .join("usr/local/share/zsh/site-functions/_sb")
            .is_file()
    );
}

/// A shell that fails is its step's failure and a warning under the
/// caller's step; the other shells still run, and the caller succeeds.
#[test]
fn a_failed_shell_is_a_warning() {
    let _spawns = SPAWNS.lock();
    let (dir, sb) = root("echo broken >&2; exit 3");
    let ran = refreshed(&Paths::under(dir.path()), &sb);
    assert!(ran.ok, "the caller failed: {}", ran.screen);
    for text in [
        "✗ Zsh completion refresh",
        // The install's own line, the failed step's output.
        "Failed zsh completion: shell probe failed: exit status 3 (broken)",
        "✗ Shell completion refresh",
        "Bash completion checked",
        "Fish completion checked",
        "! Shell completion refresh failed: detect zsh: shell probe failed: exit status 3 (broken)",
    ] {
        assert!(ran.screen.contains(text), "{text}: {}", ran.screen);
    }
    assert!(
        dir.path()
            .join("usr/local/share/bash-completion/completions/sb.bash")
            .is_file()
    );
}

/// sb's names that can't be had are the warning, and no step runs.
#[test]
fn names_that_cant_be_had_are_a_warning() {
    let (dir, _) = root("exit 0");
    let ran = refreshed(&Paths::under(dir.path()), &dir.path().join("missing"));
    assert!(ran.ok, "{}", ran.screen);
    assert!(
        ran.screen
            .contains("! Shell completion refresh failed: resolve completion executable: "),
        "{}",
        ran.screen
    );
    assert!(!ran.screen.contains("Refreshing"), "{}", ran.screen);
}
