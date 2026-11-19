//! `sb edit <file>` (tui spec, "Editors": `sb edit` and `sb edit <file>`;
//! module build M49, M50): the target checks, the editor choice and the
//! run, then the file validated as validate-config validates it and, with
//! issues, "Edit <file> again?". Bare `sb edit` shows sb-tui's picker
//! first (M53), then takes the same path with the file chosen. motd.yml
//! and providers.yml are sb's own two, listed only when they exist (tui
//! spec, Difference 38).

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail};
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::config;
use sb_cli_core::exit::Exit;
use sb_cli_core::log::RunLog;
use sb_config::run::{self, Checks, FileError, Issues};
use sb_exec::{Cmd, Ending, SpawnCause, SpawnError};

/// One of the files `sb edit` opens: sb-go's six (sb-go
/// layout/layout.go:13-21), then motd.yml and providers.yml (tui spec,
/// Difference 38).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum File {
    Accounts,
    Settings,
    AdvSettings,
    BackupConfig,
    HetznerVlan,
    Inventory,
    Motd,
    Providers,
}

impl File {
    /// All of them, in the picker's order: sb-go's six (sb-go
    /// cmd/edit.go:175-205), then sb's two.
    pub const ALL: [Self; 8] = [
        Self::Accounts,
        Self::Settings,
        Self::AdvSettings,
        Self::BackupConfig,
        Self::HetznerVlan,
        Self::Inventory,
        Self::Motd,
        Self::Providers,
    ];

    /// Whether the picker lists it only when its file exists: motd.yml and
    /// providers.yml, which not every host has. sb-go's six are always
    /// listed (a missing one fails the target check once chosen).
    fn optional(self) -> bool {
        matches!(self, Self::Motd | Self::Providers)
    }

    /// The picker's title for it (sb-go cmd/edit.go:175-205).
    pub fn title(self) -> &'static str {
        match self {
            Self::Accounts => "Accounts",
            Self::Settings => "Settings",
            Self::AdvSettings => "Advanced Settings",
            Self::BackupConfig => "Backup Settings",
            Self::HetznerVlan => "Hetzner VLAN Settings",
            Self::Inventory => "Inventory Settings",
            Self::Motd => "Motd Settings",
            Self::Providers => "Providers",
        }
    }

    /// The file's shown name (the picker's description, the log's `file`).
    pub fn shown(self) -> &'static str {
        match self {
            Self::Accounts => "accounts.yml",
            Self::Settings => "settings.yml",
            Self::AdvSettings => "adv_settings.yml",
            Self::BackupConfig => "backup_config.yml",
            Self::HetznerVlan => "hetzner_vlan.yml",
            Self::Inventory => "localhost.yml",
            Self::Motd => "motd.yml",
            Self::Providers => "providers.yml",
        }
    }

    /// Its `sb edit` subcommand's name (sb-go's, underscores kept), as a
    /// `Fix with:` line gives it.
    pub fn command(self) -> &'static str {
        match self {
            Self::Accounts => "accounts",
            Self::Settings => "settings",
            Self::AdvSettings => "adv_settings",
            Self::BackupConfig => "backup_config",
            Self::HetznerVlan => "hetzner_vlan",
            Self::Inventory => "inventory",
            Self::Motd => "motd",
            Self::Providers => "providers",
        }
    }

    /// The file `sb edit` opens for the one validate-config checks at
    /// `path` (its entry's path in `run::FILES`), if any.
    pub fn of_path(path: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|file| file.checked_as().path == path)
    }

    /// Its path under the Saltbox `root`: the inventory where cli-core
    /// finds it.
    fn path(self, root: &Path) -> PathBuf {
        match self {
            Self::Inventory => config::inventory_path(root),
            other => root.join(other.shown()),
        }
    }

    /// Its entry in validate-config's files (sb-config's `run::FILES`,
    /// in sb-go's order), so it is checked exactly as validate-config
    /// checks it; providers.yml, which validate-config doesn't check, its
    /// own schema-less entry (`run::PROVIDERS`).
    fn checked_as(self) -> run::File {
        let [
            accounts,
            adv_settings,
            backup_config,
            hetzner_vlan,
            settings,
            motd,
            inventory,
        ] = run::FILES;
        match self {
            Self::Accounts => accounts,
            Self::Settings => settings,
            Self::AdvSettings => adv_settings,
            Self::BackupConfig => backup_config,
            Self::HetznerVlan => hetzner_vlan,
            Self::Inventory => inventory,
            // Required here: the file was there when the editor started
            // (the target check), so one the editor removed is the file's
            // issue, offered again, as for the five required files;
            // validate-config skips a missing one (tui spec, the editors'
            // files).
            Self::Motd => run::File {
                required: true,
                ..motd
            },
            Self::Providers => run::File {
                required: true,
                ..run::PROVIDERS
            },
        }
    }
}

/// The files the picker lists under the Saltbox `root`: every one but an
/// optional one with nothing at its path. Looked at without following a
/// link (`symlink_metadata`: `try_exists` follows one, so a broken link
/// would read as absent;
/// <https://doc.rust-lang.org/1.100.0/std/path/struct.Path.html#method.symlink_metadata>),
/// and only `NotFound` hides it, as `try_exists` counts only that as
/// absent: a broken link, or a path that can't be looked at, is listed,
/// and the target check names what is wrong once it is chosen.
fn pickable(root: &Path) -> Vec<File> {
    File::ALL
        .into_iter()
        .filter(|file| {
            !file.optional()
                || !matches!(
                    file.path(root).symlink_metadata(),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound
                )
        })
        .collect()
}

/// The picker's refusal without a terminal on stdin and stdout (tui
/// spec, editors, "Picker").
pub const NO_TERMINAL: &str = "sb edit requires an interactive terminal; use sb edit <file>";

/// Bare `sb edit`: sb-tui's picker over the files there are
/// ([`pickable`]), then [`edit`] on
/// the file chosen, once the picker has restored the terminal (alternate
/// screen left, mouse off, input drained), so both paths run the editor
/// the same way. `q` or Esc ends with nothing done, exit 0.
///
/// # Errors
///
/// [`NO_TERMINAL`] before anything is drawn when stdin or stdout isn't a
/// terminal; the screen's I/O error; `Exit(130)` after Ctrl-C ("interrupted"
/// is `finish_run`'s after a signal); then [`edit`]'s errors.
pub fn pick(no_edit_again: bool, shutdown: &Shutdown, log: Option<&RunLog>) -> anyhow::Result<()> {
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        bail!(NO_TERMINAL);
    }
    let root = config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR"));
    let items: Vec<_> = pickable(&root)
        .into_iter()
        .map(|file| {
            let item = sb_tui::edit_picker::Item {
                title: file.title(),
                shown: file.shown(),
            };
            (item, file)
        })
        .collect();
    let (ending, chosen) = super::block_on(sb_tui::edit_picker::pick(
        shutdown,
        &items,
        crate::screen::theme(),
    ))??;
    match chosen {
        Some(file) => edit(file, no_edit_again, shutdown, log),
        None => crate::screen::ended(ending),
    }
}

/// The editors tried, in order, when neither `VISUAL` nor `EDITOR` names
/// one (sb-go `preferredEditor`, cmd/edit.go:135-155).
const FALLBACK: [&str; 4] = ["sensible-editor", "editor", "nano", "vi"];

/// `sb edit <file>`: `file` under `SB_SALTBOX_DIR`'s root (cli-core's
/// `saltbox_dir`) checked, then opened in the chosen editor, attached to
/// sb's terminal (exec's `attached`), its ending logged with whether the
/// file's bytes changed, then the file validated under term-ui's runner
/// ([`validate`]). With issues, term-ui's confirm asks `Edit <file>
/// again?` (default no; `no_edit_again` answers no without asking): yes
/// runs the editor and the validation again, as often as the user says
/// yes. The file stays as the editor saved it either way.
///
/// # Errors
///
/// The target isn't an existing regular file, `VISUAL` or `EDITOR` doesn't
/// split, no editor starts, or the editor didn't exit 0 (`error opening
/// editor`, no validation); "interrupted" when a signal ended the run
/// (cli-core's `finish_run` shows it and ends sb by the signal); the
/// file's failed step (`Exit(1)`, its block shown) when it has issues
/// and the answer is no, or it couldn't be checked; the confirm's error
/// (no terminal and no flag: it names `--no-edit-again`).
pub fn edit(
    file: File,
    no_edit_again: bool,
    shutdown: &Shutdown,
    log: Option<&RunLog>,
) -> anyhow::Result<()> {
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
    // Before the editor runs: the check's settings (the client in them
    // is never built, as the check makes no request).
    let settings = settings(root)?;
    let question = format!("Edit {} again?", file.shown());
    loop {
        let before = snapshot(&path);
        let (program, ending) =
            super::block_on(run_editor(chosen.as_ref(), shown_path, shutdown))??;
        tracing::info!(
            file = file.shown(),
            editor = %config::escape_controls(&program),
            ending = %ending.reason(),
            changed = changed(before.as_deref(), snapshot(&path).as_deref()),
            "editor ended"
        );
        match ending {
            Ending::Exited(0) => {}
            Ending::Interrupted => bail!("interrupted"),
            ending => bail!("error opening editor: {ending}"),
        }
        let failed = match super::block_on(validate(file, &settings, shutdown))? {
            Ok(Validated::Clean) => return Ok(()),
            Ok(Validated::Issues(failed)) => failed,
            Err(failed) => {
                print_closing(log, &[], shutdown);
                return Err(failed.into());
            }
        };
        // A signal during the validation ends the run, unasked.
        if shutdown.token().is_cancelled() {
            bail!("interrupted");
        }
        if !ask_again(&question, no_edit_again, shutdown)? {
            print_closing(log, &[file], shutdown);
            return Err(failed.into());
        }
    }
}

/// The lines after a failed validation's tree, for validate-config and
/// `sb edit`, whose runners draw no pointer under each failed step: the
/// run log's `pointer` once (cli-core's `details:` line), then `Fix with:`
/// and the `sb edit` line for each of `fix`, the files with issues.
pub(crate) fn closing(
    pointer: Option<&str>,
    fix: &[File],
    theme: &sb_cli_core::theme::Theme,
) -> String {
    let mut text =
        pointer.map_or_default(|pointer| sb_cli_core::exit::pointer_line(pointer, theme));
    if !fix.is_empty() {
        let lines: Vec<String> = fix
            .iter()
            .map(|file| format!("sb edit {}", file.command()))
            .collect();
        let _ = writeln!(text, "Fix with: {}", lines.join(", "));
    }
    text
}

/// [`closing`] on stderr, after a failed validation that `shutdown`
/// didn't end (an interrupted run's pointer is cli-core's block's).
pub(crate) fn print_closing(log: Option<&RunLog>, fix: &[File], shutdown: &Shutdown) {
    if shutdown.token().is_cancelled() {
        return;
    }
    let pointer = log.map(RunLog::pointer);
    let text = closing(pointer.as_deref(), fix, sb_cli_core::theme::current());
    // As cli-core's block: nothing better to do if stderr itself fails.
    let _ = std::io::Write::write_all(&mut crate::stderr(), text.as_bytes());
}

/// term-ui's confirm for `question`, `no_edit_again` answering no.
///
/// # Errors
///
/// The confirm's error: "interrupted" on Ctrl-C, the signal already
/// recorded by `shutdown` when the confirm returns (term-ui's confirm), so
/// cli-core's `finish_run` ends sb by it.
fn ask_again(question: &str, no_edit_again: bool, shutdown: &Shutdown) -> anyhow::Result<bool> {
    Ok(sb_term_ui::confirm(
        question,
        no_edit_again.then_some(false),
        "--no-edit-again",
        shutdown,
    )?)
}

/// The settings validate-config checks a file with
/// (`sb_config::run::Settings`), for a run with [`Checks::Skip`]: the
/// Saltbox root, the zoneinfo places as validate-config finds them
/// (`ZONEINFO`), and a client and service URLs that are never used (no
/// credential check runs, so no proxy is read and no override variable
/// is either).
fn settings(root: PathBuf) -> anyhow::Result<run::Settings> {
    use sb_config::{cloudflare, dockerhub, http, validators};
    Ok(run::Settings {
        root,
        zoneinfo: validators::Saltbox::zoneinfo_places(std::env::var_os("ZONEINFO")),
        http: http::Http::new(&http::Proxy::Off, http::TIMEOUT).map_err(anyhow::Error::msg)?,
        cloudflare_url: cloudflare::BASE_URL.to_string(),
        dockerhub_url: dockerhub::LOGIN_URL.to_string(),
        backoff: cloudflare::backoff,
    })
}

/// The issue count of a failed check that another edit can fix: the
/// file's issues, or the file itself unreadable or gone (not UTF-8, over
/// the cap, removed), which validate-config reports as the file's one
/// issue; `None` when its schema couldn't be loaded.
fn fixable(err: &anyhow::Error) -> Option<usize> {
    if let Some(issues) = err.downcast_ref::<Issues>() {
        return Some(issues.count());
    }
    match err.downcast_ref::<FileError>() {
        Some(FileError::Read(_) | FileError::Missing(_)) => Some(1),
        _ => None,
    }
}

/// How an edited file came through its validation.
#[derive(Debug)]
enum Validated {
    Clean,
    /// With issues, which another edit can fix: the step's failure.
    Issues(Exit),
}

/// `file` validated under term-ui's runner, titled `sb edit`, as
/// validate-config's step for that file (sb-config's
/// `validate_file_migrating`, credential checks skipped: motd.yml in its
/// older format is migrated first, backed up to `SB_BACKUPS_DIR`, as
/// validate-config migrates it), started only now the editor has exited
/// (exec refuses `attached` while a runner draws) and finished before
/// any question (term-ui's prompts refuse while one draws). The issue
/// count reaches the log; the issues themselves only the step's record,
/// as validate-config's do (a secret's value never: sb-config's rules).
///
/// # Errors
///
/// The step's failure when the file couldn't be checked (its schema
/// missing or broken: no edit of the file can fix that) or the run was
/// interrupted.
async fn validate(
    file: File,
    settings: &run::Settings,
    shutdown: &Shutdown,
) -> Result<Validated, Exit> {
    let checked = file.checked_as();
    // No log: the pointer comes once, after the run ([`print_closing`]).
    let mut runner =
        sb_term_ui::Runner::start("sb edit", shutdown, None, sb_term_ui::Options::default());
    let mut issues = None;
    let result = runner
        .step(run::file_labels(checked), async |step| {
            let result = run::validate_file_migrating(
                step,
                settings,
                checked,
                Checks::Skip,
                &crate::backups_dir(),
            )
            .await;
            issues = match &result {
                Ok(()) => Some(0),
                Err(err) => fixable(err),
            };
            result
        })
        .await;
    runner.finish();
    if let Some(issues) = issues {
        tracing::info!(file = file.shown(), issues, "edited file validated");
    }
    match (result, issues) {
        (Ok(()), _) => Ok(Validated::Clean),
        (Err(failed), Some(_)) => Ok(Validated::Issues(failed)),
        (Err(failed), None) => Err(failed),
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
    chosen: Option<&Editor>,
    path: &str,
    shutdown: &Shutdown,
) -> anyhow::Result<(String, Ending)> {
    let not_started =
        |program: &str, err: &SpawnError| anyhow!("find editor {program:?}: {}", err.cause());
    if let Some(Editor { program, args }) = chosen {
        let cmd = args
            .iter()
            .fold(Cmd::new(program), Cmd::shown_value)
            .shown_value(path);
        let ran = cmd
            .attached(shutdown)
            .await
            .map_err(|err| not_started(program, &err))?;
        return Ok((program.clone(), ran.outcome.ending));
    }
    for program in FALLBACK {
        match Cmd::new(program).shown_value(path).attached(shutdown).await {
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

    /// After a failed validation: the pointer once, then the `sb edit`
    /// lines for the files with issues; neither when there is nothing to
    /// say. validate-config's files map to the subcommand that opens each.
    #[test]
    fn the_closing_lines_point_once_and_name_the_fix() {
        let plain = sb_cli_core::theme::Theme::plain();
        let pointer = Some("/var/log/sb/sb.log (run 1a2b)");
        assert_eq!(
            closing(pointer, &[File::Accounts, File::Settings], &plain),
            "details: /var/log/sb/sb.log (run 1a2b)\nFix with: sb edit accounts, sb edit settings\n"
        );
        assert_eq!(
            closing(None, &[File::Inventory], &plain),
            "Fix with: sb edit inventory\n"
        );
        assert_eq!(
            closing(pointer, &[], &plain),
            "details: /var/log/sb/sb.log (run 1a2b)\n"
        );
        assert_eq!(closing(None, &[], &plain), "");
        let checked: Vec<Option<File>> = run::FILES.iter().map(|f| File::of_path(f.path)).collect();
        assert_eq!(
            checked,
            [
                Some(File::Accounts),
                Some(File::AdvSettings),
                Some(File::BackupConfig),
                Some(File::HetznerVlan),
                Some(File::Settings),
                Some(File::Motd),
                Some(File::Inventory),
            ]
        );
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

    /// The picker's titles and order are sb-go's.
    #[test]
    fn the_picker_lists_sb_go_s_titles_in_its_order() {
        let listed: Vec<_> = File::ALL.iter().map(|f| (f.title(), f.shown())).collect();
        assert_eq!(
            listed,
            [
                ("Accounts", "accounts.yml"),
                ("Settings", "settings.yml"),
                ("Advanced Settings", "adv_settings.yml"),
                ("Backup Settings", "backup_config.yml"),
                ("Hetzner VLAN Settings", "hetzner_vlan.yml"),
                ("Inventory Settings", "localhost.yml"),
                ("Motd Settings", "motd.yml"),
                ("Providers", "providers.yml"),
            ]
        );
    }

    /// The picker lists sb-go's six always, and motd.yml and
    /// providers.yml only when something is at their path.
    #[test]
    fn the_picker_leaves_out_a_missing_motd_or_providers() {
        let root = tempfile::tempdir().expect("root");
        let always: Vec<File> = File::ALL.into_iter().filter(|f| !f.optional()).collect();
        let six = always.as_slice();
        assert_eq!(pickable(root.path()), six);
        std::fs::write(root.path().join("providers.yml"), "a: 1\n").expect("providers");
        assert_eq!(pickable(root.path()), [six, &[File::Providers]].concat());
        std::os::unix::fs::symlink("gone", root.path().join("motd.yml")).expect("link");
        assert_eq!(pickable(root.path()), File::ALL);
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

    /// Each file is checked as validate-config checks the file at its
    /// path.
    #[test]
    fn each_file_is_checked_as_validate_config_checks_it() {
        let root = Path::new("/saltbox");
        for file in File::ALL {
            assert_eq!(
                root.join(file.checked_as().path),
                file.path(root),
                "{file:?}"
            );
        }
        assert_eq!(File::Motd.checked_as().schema, Some("motd.schema.yml"));
        assert_eq!(File::Providers.checked_as().schema, None);
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
