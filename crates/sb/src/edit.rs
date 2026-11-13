//! `sb edit <file>` (tui spec, "Editors": `sb edit` and `sb edit <file>`;
//! module build M49): the target checks, the editor choice and the run.
//! The validation after the edit (M50) and the picker (M53) come later.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail};
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::config;
use sb_exec::{Cmd, Ending, SpawnCause, SpawnError};

/// One of the six files `sb edit` opens (sb-go layout/layout.go:13-21).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum File {
    Accounts,
    Settings,
    AdvSettings,
    BackupConfig,
    HetznerVlan,
    Inventory,
}

impl File {
    /// The file's shown name (the picker's description, the log's `file`).
    pub fn shown(self) -> &'static str {
        match self {
            Self::Accounts => "accounts.yml",
            Self::Settings => "settings.yml",
            Self::AdvSettings => "adv_settings.yml",
            Self::BackupConfig => "backup_config.yml",
            Self::HetznerVlan => "hetzner_vlan.yml",
            Self::Inventory => "localhost.yml",
        }
    }

    /// Its path under the Saltbox `root`: the inventory where cli-core
    /// finds it.
    fn path(self, root: &Path) -> PathBuf {
        match self {
            Self::Inventory => config::inventory_path(root),
            other => root.join(other.shown()),
        }
    }
}

/// The editors tried, in order, when neither `VISUAL` nor `EDITOR` names
/// one (sb-go `preferredEditor`, cmd/edit.go:135-155).
const FALLBACK: [&str; 4] = ["sensible-editor", "editor", "nano", "vi"];

/// `sb edit <file>`: `file` under `SB_SALTBOX_DIR`'s root (cli-core's
/// `saltbox_dir`) checked, then opened in the chosen editor, attached to
/// sb's terminal (exec's `attached`), and its ending logged with whether
/// the file's bytes changed.
///
/// # Errors
///
/// The target isn't an existing regular file, `VISUAL` or `EDITOR` doesn't
/// split, no editor starts, or the editor didn't exit 0 (`error opening
/// editor`); "interrupted" when a signal ended the run (cli-core's
/// `finish_run` shows it and ends sb by the signal).
pub fn edit(file: File, shutdown: &Shutdown) -> anyhow::Result<()> {
    let root = config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR"));
    let path = file.path(&root);
    check_target(&path)?;
    let chosen = chosen_editor(
        std::env::var_os("VISUAL").as_deref(),
        std::env::var_os("EDITOR").as_deref(),
    )?;
    // exec takes UTF-8 arguments; a Saltbox root that isn't is no
    // Saltbox's (its default is `/srv/git/saltbox`).
    let Some(shown_path) = path.to_str() else {
        bail!(
            "configuration path is not UTF-8: {}",
            path.to_string_lossy()
        );
    };
    let before = snapshot(&path);
    let (program, ending) = super::block_on(run_editor(chosen, shown_path, shutdown))??;
    tracing::info!(
        file = file.shown(),
        editor = %config::escape_controls(&program),
        ending = %ending.reason(),
        changed = changed(before.as_deref(), snapshot(&path).as_deref()),
        "editor ended"
    );
    match ending {
        Ending::Exited(0) => Ok(()),
        Ending::Interrupted => bail!("interrupted"),
        ending => bail!("error opening editor: {ending}"),
    }
}

/// `path` followed through symlinks must be an existing regular file
/// (sb-go `editorCommand`, cmd/edit.go:101-133). The path is shown as it
/// is: cli-core's error block escapes every line it prints.
fn check_target(path: &Path) -> anyhow::Result<()> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => Ok(()),
        Ok(_) => bail!(
            "configuration path is not a regular file: {}",
            path.display()
        ),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            bail!("configuration file does not yet exist: {}", path.display())
        }
        Err(err) => Err(anyhow::Error::new(err)
            .context(format!("inspect configuration file {}", path.display()))),
    }
}

/// The editor `VISUAL` or `EDITOR` names, split as a shell would split
/// words (quotes and backslashes; no expansion; sb-go cmd/edit.go:135-155):
/// <https://docs.rs/shlex/2.0.1/shlex/fn.split.html>. `None` when neither
/// is set, so the fallback list applies. The first non-blank one wins.
/// shlex reads a word starting with an unquoted `#` as a comment to the
/// end, as a shell does; go-shellquote kept it as a word. An editor
/// argument starting with `#` is quoted (`'#x'`).
///
/// # Errors
///
/// That variable doesn't split, isn't UTF-8 or names an empty program:
/// `invalid VISUAL environment variable` (or `EDITOR`). The value isn't
/// echoed: it can carry a path or a token.
fn chosen_editor(visual: Option<&OsStr>, editor: Option<&OsStr>) -> anyhow::Result<Option<Editor>> {
    let Some((name, value)) = [("VISUAL", visual), ("EDITOR", editor)]
        .into_iter()
        .find_map(|(name, value)| {
            value
                .filter(|v| !v.to_string_lossy().trim().is_empty())
                .map(|v| (name, v))
        })
    else {
        return Ok(None);
    };
    let mut words = value.to_str().and_then(shlex::split).unwrap_or_default();
    if words.first().is_none_or(String::is_empty) {
        bail!("invalid {name} environment variable");
    }
    let program = words.remove(0);
    Ok(Some(Editor {
        program,
        args: words,
    }))
}

/// An editor named by `VISUAL` or `EDITOR`: its program and extra words.
#[derive(Debug, PartialEq, Eq)]
struct Editor {
    program: String,
    args: Vec<String>,
}

/// Run the `chosen` editor (its extra words, then `path`), or the
/// first of [`FALLBACK`] that starts, attached to sb's terminal: its
/// program and how it ended.
///
/// # Errors
///
/// A named editor that doesn't start (`find editor "<program>": <cause>`,
/// sb-go faf9887's wording), or none of the fallback list found.
async fn run_editor(
    chosen: Option<Editor>,
    path: &str,
    shutdown: &Shutdown,
) -> anyhow::Result<(String, Ending)> {
    let not_started =
        |program: &str, err: &SpawnError| anyhow!("find editor {program:?}: {}", err.cause());
    if let Some(Editor { program, args }) = chosen {
        let cmd = args
            .into_iter()
            .fold(Cmd::new(&program), Cmd::value)
            .value(path);
        let ran = cmd
            .attached(shutdown)
            .await
            .map_err(|err| not_started(&program, &err))?;
        return Ok((program, ran.outcome.ending));
    }
    for program in FALLBACK {
        match Cmd::new(program).value(path).attached(shutdown).await {
            Ok(ran) => return Ok((program.to_string(), ran.outcome.ending)),
            // exec resolves `PATH` at spawn: a name not found, or found
            // but not executable, moves to the next one, as sb-go's
            // `exec.LookPath` passes both over (why not the `which` crate:
            // no lookup is needed).
            Err(err) if passed_over(&err) => {}
            Err(err) => return Err(not_started(program, &err)),
        }
    }
    bail!("no editor found; set VISUAL or EDITOR to an executable editor")
}

/// Whether a fallback editor that didn't start is passed over: not found,
/// or not executable.
fn passed_over(err: &SpawnError) -> bool {
    matches!(
        err.cause(),
        SpawnCause::Io(io) if matches!(
            io.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
        )
    )
}

/// The file's text, for the log's `changed`: cli-core's `read_small` (a
/// regular file of at most 1 MiB in UTF-8, opened without blocking, so a
/// FIFO swapped in can't hang sb); `None` when it can't be read so.
fn snapshot(path: &Path) -> Option<String> {
    config::read_small(path).ok().flatten()
}

/// Whether the file changed: compared in memory, never logged.
fn changed(before: Option<&str>, after: Option<&str>) -> &'static str {
    match (before, after) {
        (Some(before), Some(after)) if before == after => "no",
        (Some(_), Some(_)) => "yes",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chosen(visual: Option<&str>, editor: Option<&str>) -> Option<Editor> {
        chosen_editor(visual.map(OsStr::new), editor.map(OsStr::new)).expect("valid")
    }

    fn editor(program: &str, args: &[&str]) -> Editor {
        Editor {
            program: program.to_string(),
            args: args.iter().map(ToString::to_string).collect(),
        }
    }

    /// sb-go's `edit_test.go` cases: `VISUAL` before `EDITOR`, blank values
    /// skipped, quotes and backslashes as a shell reads them, no
    /// expansion.
    #[test]
    fn the_editor_is_chosen_as_sb_go_chooses_it() {
        assert_eq!(chosen(Some("vim"), Some("nano")), Some(editor("vim", &[])));
        assert_eq!(chosen(None, Some("nano")), Some(editor("nano", &[])));
        assert_eq!(chosen(Some(""), Some("nano")), Some(editor("nano", &[])));
        assert_eq!(chosen(Some(" \t"), Some("nano")), Some(editor("nano", &[])));
        assert_eq!(chosen(None, None), None);
        assert_eq!(chosen(Some(""), Some("")), None);
        assert_eq!(
            chosen(Some(r#"code --wait "my dir/x" a\ b '$HOME' ~"#), None),
            Some(editor("code", &["--wait", "my dir/x", "a b", "$HOME", "~"]))
        );
    }

    /// A value that doesn't split, or names no program, is refused by the
    /// variable's name, never echoing the value.
    #[test]
    fn an_invalid_variable_is_named_not_echoed() {
        let invalid = |visual: Option<&OsStr>, editor: Option<&OsStr>| {
            chosen_editor(visual, editor)
                .expect_err("invalid")
                .to_string()
        };
        assert_eq!(
            invalid(Some(OsStr::new("vim \"secret-token")), None),
            "invalid VISUAL environment variable"
        );
        assert_eq!(
            invalid(None, Some(OsStr::new("'' -x"))),
            "invalid EDITOR environment variable"
        );
        assert_eq!(
            invalid(None, Some(OsStr::new("vim\\"))),
            "invalid EDITOR environment variable"
        );
        assert_eq!(
            invalid(
                Some(std::os::unix::ffi::OsStrExt::from_bytes(b"vi\xff")),
                None
            ),
            "invalid VISUAL environment variable"
        );
    }

    #[test]
    fn each_file_has_its_shown_name_and_path() {
        let root = Path::new("/srv/x");
        assert_eq!(File::Accounts.path(root), root.join("accounts.yml"));
        assert_eq!(File::AdvSettings.shown(), "adv_settings.yml");
        assert_eq!(File::Inventory.shown(), "localhost.yml");
        assert_eq!(
            File::Inventory.path(root),
            root.join("inventories/host_vars/localhost.yml")
        );
    }

    /// `changed` compares bytes; a file that couldn't be read is unknown.
    #[test]
    fn changed_compares_the_bytes() {
        assert_eq!(changed(Some("a"), Some("a")), "no");
        assert_eq!(changed(Some("a"), Some("b")), "yes");
        assert_eq!(changed(None, Some("a")), "unknown");
        assert_eq!(changed(Some("a"), None), "unknown");
    }

    /// The target checks name what they found; a path that can't be
    /// looked at is neither missing nor the wrong kind.
    #[test]
    fn the_target_checks_name_the_problem() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("f");
        std::fs::write(&file, "a: 1\n").expect("write");
        assert!(check_target(&file).is_ok());
        let missing = check_target(&dir.path().join("none")).expect_err("missing");
        assert!(
            missing
                .to_string()
                .starts_with("configuration file does not yet exist: "),
            "{missing}"
        );
        let directory = check_target(dir.path()).expect_err("a directory");
        assert!(
            directory
                .to_string()
                .starts_with("configuration path is not a regular file: "),
            "{directory}"
        );
        // Under a regular file: not a directory, not "does not yet exist".
        let under = check_target(&file.join("settings.yml")).expect_err("under a file");
        assert!(
            under.to_string().starts_with("inspect configuration file "),
            "{under}"
        );
    }

    /// A snapshot reads a regular file up to the cap, and nothing else.
    #[test]
    fn a_snapshot_reads_regular_files_up_to_the_cap() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("f");
        std::fs::write(&file, b"a: 1\n").expect("write");
        assert_eq!(snapshot(&file).as_deref(), Some("a: 1\n"));
        std::fs::write(&file, vec![b'x'; config::MAX_FILE_BYTES]).expect("write");
        assert_eq!(
            snapshot(&file).map(|b| b.len()),
            Some(config::MAX_FILE_BYTES)
        );
        std::fs::write(&file, vec![b'x'; config::MAX_FILE_BYTES + 1]).expect("write");
        assert_eq!(snapshot(&file), None);
        assert_eq!(snapshot(dir.path()), None);
        assert_eq!(snapshot(&dir.path().join("none")), None);
    }
}
