//! One playbook run by tag (ansible spec, `sb install` step 6 and
//! `sb diag`): the argv, element by element through sb-exec's `Cmd`, the
//! `Executing...` line, the attached run in the repository's directory,
//! the success line, and the failure's pass-through with sb-go's hint.

use std::fmt;
use std::io::Write;

use sb_cli_core::cancel::Shutdown;
use sb_cli_core::exit::{self, Exit, OutputError};
use sb_exec::{Cmd, Ending, Ran, SpawnError};

use crate::{Ansible, Repo};

/// `-` and 255 `v`s: clap's count is a `u8`, so every `-v...` is a slice of
/// this and stays a literal the log shows (capability map, open items).
/// The bytes are a `static`, so the `&'static str` can borrow them (a
/// temporary in a `const` can't be borrowed for `'static`).
static VERBOSE_BYTES: [u8; 256] = {
    let mut bytes = [b'v'; 256];
    bytes[0] = b'-';
    bytes
};
const VERBOSE: &str = match str::from_utf8(&VERBOSE_BYTES) {
    Ok(text) => text,
    Err(_) => panic!("ASCII"),
};

/// ansible-core's exit code after Ctrl-C (`ExitCode.KEYBOARD_INTERRUPT`,
/// lib/ansible/errors/__init__.py; spec, assumption 5).
const KEYBOARD_INTERRUPT: i32 = 99;

/// The tags a run passes to `--tags`.
#[derive(Clone, Copy)]
pub enum Tags<'a> {
    /// Written in sb's code (`sb diag`'s `diag`): the log shows it, as any
    /// literal (exec spec, "A command").
    Literal(&'static str),
    /// Anyone else's (the user's, an announcement's), joined with `,`:
    /// shown as given in the log and on the `Executing...` line (decision
    /// 17).
    Given(&'a [String]),
}

impl fmt::Debug for Tags<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Literal(tag) => f.debug_tuple("Literal").field(tag).finish(),
            // Counts only, as `RunArgs`.
            Self::Given(tags) => write!(f, "Given({} tags)", tags.len()),
        }
    }
}

/// What reaches every playbook of a run besides its tags (spec, `sb
/// install`'s flags). Its `Debug` shows counts only: the values are shown
/// and logged once, in the command (decision 17), not in every record that
/// carries them.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct RunArgs {
    /// Each `-e` value, whole, in the order given.
    pub extra_vars: Vec<String>,
    /// `--skip-tags`, joined with `,` into one element.
    pub skip_tags: Vec<String>,
    /// `-v`'s count: one `-v...` element of that many `v`s, none at 0.
    pub verbose: u8,
}

impl fmt::Debug for RunArgs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunArgs")
            .field("extra_vars", &self.extra_vars.len())
            .field("skip_tags", &self.skip_tags.len())
            .field("verbose", &self.verbose)
            .finish()
    }
}

/// Why a run couldn't happen.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// `ansible-playbook` couldn't start (missing, not executable, the
    /// repository's directory missing).
    #[error(transparent)]
    Spawn(#[from] SpawnError),
    /// sb's own lines couldn't be written (a broken pipe is cli-core's
    /// quiet 141: the error is in the chain).
    #[error("could not write the playbook's lines")]
    Output(#[from] OutputError),
}

impl Ansible {
    /// The command for one run: `<program> <playbook> --become --tags
    /// <tags> [--extra-vars=<v>]... [--skip-tags=<x,y>] [-v...]` in the
    /// repository's directory (spec, step 6). Every user value is one
    /// element, through `shown_value`: never split, joined with another,
    /// or passed through a shell, and shown as given on the `Executing...`
    /// line, in exec's failure block and in the run log, as sb-go prints
    /// them (decision 17: the maintainer, 2026-11-14; an `@file` keeps a
    /// value out).
    #[must_use]
    pub fn command(&self, repo: &Repo, tags: Tags<'_>, args: &RunArgs) -> Cmd {
        let cmd = Cmd::new(self.program())
            .cwd(repo.dir())
            // The file name: the run is in its directory (difference 3).
            .arg(repo.kind().playbook())
            .arg("--become")
            .arg("--tags");
        let mut cmd = match tags {
            Tags::Literal(tags) => cmd.arg(tags),
            Tags::Given(tags) => cmd.shown_value(tags.join(",")),
        };
        for value in &args.extra_vars {
            // One argv element: argparse refuses `--extra-vars -x=1`
            // (assumption 6; difference 1).
            cmd = cmd.shown_value(format!("--extra-vars={value}"));
        }
        if !args.skip_tags.is_empty() {
            cmd = cmd.shown_value(format!("--skip-tags={}", args.skip_tags.join(",")));
        }
        if args.verbose > 0 {
            cmd = cmd.arg(&VERBOSE[..=usize::from(args.verbose)]);
        }
        cmd
    }

    /// One attached run of `repo`'s playbook with `tags` (spec, step 6):
    /// `Executing Ansible playbook with command: <command>` on `out`, the
    /// command as exec shows it, every argument as given (decisions 4 and
    /// 17); the run, on sb's terminal;
    /// after exit 0, sb-go's `Playbook <path> executed successfully.`
    /// after a blank line. How it ended is the caller's: [`pass_through`]
    /// for `sb install` and `sb diag`.
    ///
    /// # Errors
    ///
    /// [`RunError::Spawn`] when the program can't start (exec refuses it
    /// while a term-ui runner draws, too); [`RunError::Output`] when `out`
    /// can't be written.
    pub async fn run(
        &self,
        repo: &Repo,
        tags: Tags<'_>,
        args: &RunArgs,
        shutdown: &Shutdown,
        out: &mut impl Write,
    ) -> Result<Ran, RunError> {
        let cmd = self.command(repo, tags, args);
        let shown = exit::escaped(&cmd.shown());
        writeln!(out, "Executing Ansible playbook with command: {shown}")
            .and_then(|()| out.flush())
            .map_err(exit::output_error)?;
        let ran = cmd.attached(shutdown).await?;
        if ran.outcome.ending == Ending::Exited(0) {
            let path = exit::escaped(&repo.playbook_path().display().to_string());
            writeln!(out, "\nPlaybook {path} executed successfully.")
                .and_then(|()| out.flush())
                .map_err(exit::output_error)?;
        }
        Ok(ran)
    }
}

/// A run's ending passed on (spec, step 6 and decision 6): `Ok` for exit 0;
/// otherwise exec's pass-through (one error block on `out` naming the
/// command, as shown, and its ending; the child's own code, or 128 plus a
/// signal's number), then sb-go's hint, `Scroll up to the failed task to
/// review.`, or `Ansible was interrupted.` for Ansible's Ctrl-C code (99,
/// step 7). A run sb's own signal ended prints nothing: cli-core shows the
/// interruption and ends sb by the signal.
///
/// # Errors
///
/// cli-core's quiet [`Exit`] with the code sb ends with, when the run
/// didn't exit 0.
pub fn pass_through(ran: Ran, out: &mut impl Write) -> Result<(), Exit> {
    let ending = ran.outcome.ending;
    let Err(exit) = ran.pass_through(out) else {
        return Ok(());
    };
    let hint = match ending {
        Ending::Interrupted => None,
        Ending::Exited(KEYBOARD_INTERRUPT) => Some("Ansible was interrupted."),
        _ => Some("Scroll up to the failed task to review."),
    };
    if let Some(hint) = hint {
        // As exec's block: nothing better to do if stderr itself fails.
        let _ = writeln!(out, "{hint}");
    }
    Err(exit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn ansible() -> Ansible {
        Ansible::with_program("/usr/local/bin/ansible-playbook")
    }

    fn saltbox() -> Repo {
        Repo::saltbox(Path::new("/srv/git/saltbox"))
    }

    /// `diag`'s command: the playbook's file name and every literal shown.
    #[test]
    fn diags_command_shows_its_literals() {
        let cmd = ansible().command(&saltbox(), Tags::Literal("diag"), &RunArgs::default());
        assert_eq!(
            cmd.shown(),
            "/usr/local/bin/ansible-playbook saltbox.yml --become --tags diag"
        );
    }

    /// Given tags, `-e`, `--skip-tags` and `-v` in sb-go's order, every
    /// value shown as given, as sb-go prints them (spec, step 6; decision
    /// 17).
    #[test]
    fn a_full_command_shows_every_value() {
        let tags = ["plex".to_string(), "sonarr".to_string()];
        let args = RunArgs {
            extra_vars: vec![r#"{"a": 1, "b": [2, 3]}"#.into(), "k=v".into()],
            skip_tags: vec!["x".into(), "y".into()],
            verbose: 3,
        };
        let cmd = ansible().command(&saltbox(), Tags::Given(&tags), &args);
        let shown = "/usr/local/bin/ansible-playbook saltbox.yml --become --tags plex,sonarr \
             --extra-vars={\"a\": 1, \"b\": [2, 3]} --extra-vars=k=v --skip-tags=x,y -vvv";
        assert_eq!(cmd.shown(), shown);
    }

    /// No `-v` element at 0, and the count's `v`s up to clap's 255.
    #[test]
    fn verbosity_is_one_element_of_its_count() {
        let shown = |verbose| {
            let args = RunArgs {
                verbose,
                ..RunArgs::default()
            };
            ansible()
                .command(&saltbox(), Tags::Literal("diag"), &args)
                .shown()
        };
        assert!(shown(0).ends_with("--tags diag"));
        assert!(shown(1).ends_with("--tags diag -v"));
        assert!(shown(4).ends_with("--tags diag -vvvv"));
        let most = shown(u8::MAX);
        let last = most.rsplit(' ').next().expect("an element");
        assert_eq!(last.len(), 256);
        assert_eq!(last.matches('v').count(), 255);
    }

    /// Neither the tags nor an `-e` value reach a `Debug` rendering.
    #[test]
    fn debug_shows_counts_only() {
        let args = RunArgs {
            extra_vars: vec!["shown=never".into()],
            skip_tags: vec!["x".into()],
            verbose: 2,
        };
        assert_eq!(
            format!("{args:?}"),
            "RunArgs { extra_vars: 1, skip_tags: 1, verbose: 2 }"
        );
        let tags = ["secret-tag".to_string()];
        assert_eq!(format!("{:?}", Tags::Given(&tags)), "Given(1 tags)");
        assert_eq!(format!("{:?}", Tags::Literal("diag")), "Literal(\"diag\")");
    }
}
