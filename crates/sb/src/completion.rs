//! `sb completion` and its subcommands (completion spec).

use std::io::Write as _;

use sb_cli_core::cancel::Shutdown;
use sb_cli_core::exit;
use sb_completion::Shell;
use sb_completion::install::{self, Paths};

use crate::CompletionCommand;

pub(crate) fn completion(
    command: Option<CompletionCommand>,
    shutdown: &Shutdown,
) -> anyhow::Result<()> {
    // sb-go's three spellings, one path (cmd/completion.go): every
    // shell, unavailable ones skipped; or one, unavailable an error
    // unless `--allow-unavailable`.
    let (shells, explicit) = match command {
        None | Some(CompletionCommand::Install { shell: None, .. }) => (Shell::ALL.to_vec(), false),
        Some(CompletionCommand::Bash) => (vec![Shell::Bash], true),
        Some(CompletionCommand::Zsh) => (vec![Shell::Zsh], true),
        Some(CompletionCommand::Fish) => (vec![Shell::Fish], true),
        Some(CompletionCommand::Install {
            shell: Some(shell),
            allow_unavailable,
        }) => (vec![shell], !allow_unavailable),
        Some(CompletionCommand::Generate { shell }) => return generate(shell),
    };
    install(&shells, explicit, shutdown)
}

/// The shells' files for every name sb runs under, a line for each
/// result on stdout; the failures, and with `explicit` an unavailable
/// shell, as one error after them (exit 1).
fn install(shells: &[Shell], explicit: bool, shutdown: &Shutdown) -> anyhow::Result<()> {
    let paths = Paths::for_sb();
    let executable = std::env::current_exe()
        .map_err(|err| anyhow::anyhow!("get completion executable: {err}"))?;
    let names = install::names(&paths, &executable).map_err(anyhow::Error::msg)?;
    let _children = sb_exec::on_forced_exit(shutdown);
    let token = shutdown.token();
    // sb-go's `os.Executable` directory: where a live executable of a
    // retired name is looked for besides the bin directories.
    let own_dir = executable.parent().unwrap_or(std::path::Path::new("/"));
    let report = crate::block_on(install::install(&paths, shells, &names, own_dir, &token))?;
    let mut out = sb_cli_core::tail::Tracked::stdout(std::io::stdout().lock());
    for line in &report.lines {
        writeln!(out, "{line}").map_err(exit::output_error)?;
    }
    out.flush().map_err(exit::output_error)?;
    if token.is_cancelled() {
        anyhow::bail!("interrupted");
    }
    let mut failures = report.failures;
    if explicit {
        failures.extend(
            report.unavailable.iter().map(|(shell, reason)| {
                format!("{} completion unavailable: {reason}", shell.name())
            }),
        );
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow::Error::msg(failures.join("\n")))
    }
}

/// clap's stub for `sb`, looked up on `PATH` at Tab time (completion
/// spec, "`sb completion generate`").
fn generate(shell: Shell) -> anyhow::Result<()> {
    let mut out = sb_cli_core::tail::Tracked::stdout(std::io::stdout().lock());
    out.write_all(&shell.registration("sb", "sb"))
        .map_err(exit::output_error)?;
    out.flush().map_err(exit::output_error)?;
    Ok(())
}
