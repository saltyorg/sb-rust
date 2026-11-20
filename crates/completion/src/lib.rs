//! Shell completion (capability map, `completion`; spec
//! docs/specs/completion.md): Tab completion for bash, zsh and fish
//! through `clap_complete`'s dynamic engine, the tag completer for `sb
//! install`, and the shells' registration stubs.
//!
//! The engine is `clap_complete`'s `unstable-dynamic`, pinned exactly
//! (ADR-001): every use of it is in this crate.

pub mod install;
mod reply;
pub mod tags;
#[cfg(feature = "testing")]
pub mod testing;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap_complete::env::{Bash, CompleteEnv, EnvCompleter, Fish, Shells, Zsh};
use sb_term_ui::{Labels, Step};

/// The variable that makes a run of sb a completion request (spec,
/// decision 9: not clap's default `COMPLETE`, which a user's exported
/// variable could turn on for every run).
pub const VAR: &str = "SB_COMPLETE";

/// The shells sb completes in (sb-go's three; spec, difference 4), each
/// through sb's adapter ([`Shell`]'s `EnvCompleter`).
const SHELLS: Shells<'static> = Shells(&[&Shell::Bash, &Shell::Zsh, &Shell::Fish]);

/// Answer a completion request and exit, when sb runs as one (`VAR` set,
/// not empty or `0`); else return at once and let sb run. First in
/// `main`: nothing may be written to stdout before it
/// (<https://docs.rs/clap_complete/4.6.11/clap_complete/env/struct.CompleteEnv.html>;
/// `unstable-dynamic`, ADR-001). `command` is sb's tree, with the tag
/// completer on install's tags.
pub fn complete(command: fn() -> clap::Command) {
    CompleteEnv::with_factory(command)
        .var(VAR)
        .shells(SHELLS)
        .complete();
}

/// What replaces clap's zsh stub's last line, `compdef <function> <name>`
/// (spec, verification item 1). compinit autoloads the file
/// `_<name>` from `fpath` as the body of a function of that name: "the
/// function body ... is taken to be the complete contents of the file"
/// (zshmisc(1), "Autoloading Functions",
/// <https://zsh.sourceforge.io/Doc/Release/Functions.html#Autoloading-Functions>;
/// zshcompsys(1), "Autoloaded files",
/// <https://zsh.sourceforge.io/Doc/Release/Completion-System.html#Autoloaded-files>).
/// So on the first Tab clap's stub only defines its completer and runs
/// `compdef`, completing nothing, and the `compdef` remaps the name to a
/// function no file in `fpath` defines, which a rebuilt `.zcompdump`
/// keeps: later shells then have no completion for the name (clap
/// <https://github.com/clap-rs/clap/issues/6566>; both seen in
/// tests/shells.sh). Run as that function (`funcstack[1]` is its name,
/// zshmodules(1), zsh/parameter,
/// <https://zsh.sourceforge.io/Doc/Release/Zsh-Modules.html#index-funcstack>),
/// the file calls the completer and leaves the mapping alone; sourced, it
/// runs clap's `compdef`, as clap's static zsh script does
/// (`clap_complete` 4.6.11, `aot/shells/zsh.rs`). Why not clap's stub as
/// it is: it is written for `source` only (clap's reply on #6566; its
/// view of installs outside `source`:
/// <https://github.com/clap-rs/clap/issues/5668>).
/// `file` is the file's name, `function` clap's completer
/// (`_clap_dynamic_completer_<name>`, `-` as `_`), `name` the command
/// (`clap_complete` 4.6.11, `env/shells.rs`). Written with `format!`, not
/// a template's `replace`, so a name can't be rewritten.
fn zsh_guard(file: &str, function: &str, name: &str) -> String {
    format!(
        r#"if [[ "${{funcstack[1]}}" == "{file}" ]]; then
    {function} "$@"
else
    compdef {function} {name}
fi
"#
    )
}

/// Refresh the shells' files, for ops' `update` and `setup` (spec,
/// "Refresh for `update` and `setup`"): [`refresh_with`] `executable`,
/// sb's own as `std::env::current_exe` gave it, under `paths`
/// ([`install::Paths::for_sb`]; a test's root). An executable that can't
/// be had is the warning under `step`, as any other refresh failure.
pub async fn refresh(
    step: &mut Step,
    paths: &install::Paths,
    executable: &std::io::Result<PathBuf>,
) {
    match executable {
        Ok(executable) => refresh_with(step, paths, executable).await,
        Err(err) => step.warn(format!(
            "Shell completion refresh failed: get completion executable: {err}"
        )),
    }
}

/// sb-go's `regenerateInstalledCompletions`: under `step`, a step
/// "Refreshing shell completions" with a sub-step per shell, in sb-go's
/// order, each the install for that shell with its result lines as the
/// sub-step's output (sb-go's labels: "Refreshing Bash completion", "Bash
/// completion checked", "Bash completion refresh"). A shell that fails
/// fails its sub-step and the others still run; then the refresh's step
/// fails and `step` gets sb-go's warning, "Shell completion refresh
/// failed: ...", but `step` itself goes on: a refresh never fails its
/// caller (sb-go's `runner.Warning`).
pub async fn refresh_with(step: &mut Step, paths: &install::Paths, executable: &Path) {
    let names = match install::names(paths, executable) {
        Ok(names) => names,
        Err(err) => {
            step.warn(format!("Shell completion refresh failed: {err}"));
            return;
        }
    };
    let own_dir = executable.parent().unwrap_or(Path::new("/"));
    let mut failures = Vec::new();
    let labels = Labels::new(
        "Refreshing shell completions",
        "Shell completions refreshed",
    )
    .failure("Shell completion refresh");
    let refreshed = step
        .step(labels, async |parent| {
            // One after another, not term-ui's `group_all`: sb-go's order,
            // each shell's step under the last.
            for shell in Shell::ALL {
                // Checked here, though term-ui skips a step on a cancelled
                // token, so the warning says why.
                if parent.token().is_cancelled() {
                    failures.push("interrupted".to_owned());
                    break;
                }
                let title = shell.title();
                let labels = Labels::new(
                    format!("Refreshing {title} completion"),
                    format!("{title} completion checked"),
                )
                .failure(format!("{title} completion refresh"));
                let mut failed = Vec::new();
                // Its failure is shown by its own step and counted below.
                let _ = parent
                    .step(labels, async |child| {
                        let token = child.token();
                        let report =
                            install::install(paths, &[shell], &names, own_dir, &token).await;
                        for line in &report.lines {
                            child.output(line.to_string());
                        }
                        failed = report.failures;
                        if failed.is_empty() {
                            Ok(())
                        } else {
                            Err(anyhow::Error::msg(failed.join("\n")))
                        }
                    })
                    .await;
                failures.extend(failed);
            }
            if failures.is_empty() {
                Ok(())
            } else {
                Err(anyhow::Error::msg(failures.join("\n")))
            }
        })
        .await;
    // A refresh skipped by Ctrl-C has nothing to say: the run ends by the
    // signal. A failure is one warning line each, as sb-go's
    // `errors.Join`.
    if refreshed.is_err() && !failures.is_empty() {
        step.warn(format!(
            "Shell completion refresh failed: {}",
            failures.join("\n")
        ));
    }
}

/// A shell sb completes in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Shell {
    /// Bash, with bash-completion.
    Bash,
    /// Zsh.
    Zsh,
    /// Fish.
    Fish,
}

impl Shell {
    /// The three, in sb-go's order.
    pub const ALL: [Self; 3] = [Self::Bash, Self::Zsh, Self::Fish];

    /// Its name capitalised, as sb-go's refresh labels spell it.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Bash => "Bash",
            Self::Zsh => "Zsh",
            Self::Fish => "Fish",
        }
    }

    /// Its name, as sb-go's messages and the commands spell it.
    #[must_use]
    pub fn name(self) -> &'static str {
        self.engine().name()
    }

    /// clap's adapter for it (`unstable-dynamic`, ADR-001), whose stub
    /// sb writes and whose reply format sb's follows.
    fn engine(self) -> &'static dyn EnvCompleter {
        match self {
            Self::Bash => &Bash,
            Self::Zsh => &Zsh,
            Self::Fish => &Fish,
        }
    }

    /// clap's registration stub for `name`, which runs `completer` (a path,
    /// or a name looked up on `PATH`) at Tab time with [`VAR`] set
    /// (<https://docs.rs/clap_complete/4.6.11/clap_complete/env/trait.EnvCompleter.html#tymethod.write_registration>;
    /// `unstable-dynamic`, ADR-001). No script of sb's own: the stub only
    /// calls sb (spec, difference 1); zsh's last line is guarded
    /// ([`zsh_guard`]).
    ///
    /// # Panics
    ///
    /// Never: it writes to memory, which can't fail.
    #[must_use]
    pub fn registration(self, name: &str, completer: &str) -> Vec<u8> {
        let mut stub = Vec::new();
        self.engine()
            .write_registration(VAR, name, name, completer, &mut stub)
            // Writing to a `Vec` can't fail.
            .expect("a stub in memory");
        if self == Self::Zsh {
            let function = format!("_clap_dynamic_completer_{}", name.replace('-', "_"));
            let compdef = format!("compdef {function} {name}\n");
            let guard = zsh_guard(&format!("_{name}"), &function, name);
            // The stub ends with that line (clap_complete 4.6.11, pinned
            // exactly: ADR-001; the tests check it).
            if let Some(body) = stub.strip_suffix(compdef.as_bytes()) {
                let mut guarded = body.to_vec();
                guarded.extend_from_slice(guard.as_bytes());
                stub = guarded;
            }
        }
        stub
    }
}

/// sb's adapter for each shell: clap's registration stub, as
/// `CompleteEnv` prints it for `SB_COMPLETE=<shell> sb`, and sb's reply,
/// clap's engine's candidates without the hidden commands and flags, in
/// clap's format (spec, decision 11; the module `reply`). `CompleteEnv::shells` takes any `EnvCompleter`
/// (<https://docs.rs/clap_complete/4.6.11/clap_complete/env/struct.CompleteEnv.html#method.shells>).
impl EnvCompleter for Shell {
    fn name(&self) -> &'static str {
        self.engine().name()
    }

    fn is(&self, name: &str) -> bool {
        self.engine().is(name)
    }

    fn write_registration(
        &self,
        var: &str,
        name: &str,
        bin: &str,
        completer: &str,
        buf: &mut dyn std::io::Write,
    ) -> std::io::Result<()> {
        self.engine()
            .write_registration(var, name, bin, completer, buf)
    }

    fn write_complete(
        &self,
        cmd: &mut clap::Command,
        args: Vec<OsString>,
        current_dir: Option<&Path>,
        buf: &mut dyn std::io::Write,
    ) -> std::io::Result<()> {
        reply::write(*self, cmd, args, current_dir, buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each shell's stub registers `name` and calls `completer` with sb's
    /// variable: clap's, as `CompleteEnv` prints it, but for zsh's last
    /// line (`zsh_guard`).
    #[test]
    fn each_stub_is_claps_for_the_name_and_completer() {
        for shell in Shell::ALL {
            let stub = String::from_utf8(shell.registration("saltbox", "/usr/bin/saltbox"))
                .expect("UTF-8");
            assert!(stub.contains("saltbox"), "{shell:?}: {stub}");
            assert!(stub.contains("/usr/bin/saltbox"), "{shell:?}: {stub}");
            assert!(stub.contains("SB_COMPLETE"), "{shell:?}: {stub}");
            assert!(!stub.contains("COMPLETER"), "{shell:?}: {stub}");
        }
        let zsh = String::from_utf8(Shell::Zsh.registration("sb", "sb")).expect("UTF-8");
        assert!(zsh.starts_with("#compdef sb\n"), "{zsh}");
        let bash = String::from_utf8(Shell::Bash.registration("sb", "sb")).expect("UTF-8");
        assert!(bash.contains("-F _clap_complete_sb sb"), "{bash}");
        let fish = String::from_utf8(Shell::Fish.registration("sb", "sb")).expect("UTF-8");
        assert!(
            fish.starts_with("complete --keep-order --exclusive --command sb"),
            "{fish}"
        );
    }

    /// zsh's file ends by completing when it runs as the autoloaded
    /// `_<name>` (verification item 1), and only then.
    #[test]
    fn the_zsh_stub_completes_on_its_first_load() {
        let zsh = String::from_utf8(Shell::Zsh.registration("sb-x", "/usr/local/bin/sb-x"))
            .expect("UTF-8");
        // clap's trailing `compdef` is replaced, not followed: when the
        // file is autoloaded it would remap `sb-x` to a function with no
        // file (clap #6566).
        let guard = "\nif [[ \"${funcstack[1]}\" == \"_sb-x\" ]]; then\n    _clap_dynamic_completer_sb_x \"$@\"\nelse\n    compdef _clap_dynamic_completer_sb_x sb-x\nfi\n";
        assert!(zsh.ends_with(guard), "{zsh}");
        assert_eq!(
            zsh.matches("compdef _clap_dynamic_completer_sb_x sb-x")
                .count(),
            1,
            "{zsh}"
        );
        assert!(!zsh.contains("\ncompdef "), "an unguarded compdef: {zsh}");
        assert!(zsh.starts_with("#compdef sb-x\n"), "{zsh}");
        assert!(
            zsh.contains("function _clap_dynamic_completer_sb_x() {"),
            "{zsh}"
        );
        for shell in [Shell::Bash, Shell::Fish] {
            let stub = String::from_utf8(shell.registration("sb", "sb")).expect("UTF-8");
            assert!(!stub.contains("funcstack"), "{shell:?}: {stub}");
        }
    }

    /// The guard replaces clap's last line for the names sb writes
    /// (`generate`'s `sb`, a Saltbox alias, and the name pattern's other
    /// characters): a name whose `compdef` line didn't match would leave
    /// clap's unguarded stub.
    #[test]
    fn every_name_gets_the_guard() {
        for name in ["sb", "saltbox", "sb.2", "sb_x-2"] {
            let zsh = String::from_utf8(Shell::Zsh.registration(name, "/usr/local/bin/sb"))
                .expect("UTF-8");
            let function = format!("_clap_dynamic_completer_{}", name.replace('-', "_"));
            assert!(
                zsh.ends_with(&zsh_guard(&format!("_{name}"), &function, name)),
                "{zsh}"
            );
            assert!(!zsh.contains("\ncompdef "), "{name}: {zsh}");
        }
    }

    #[test]
    fn names_are_sb_gos() {
        let names = Shell::ALL.map(Shell::name);
        assert_eq!(names, ["bash", "zsh", "fish"]);
    }
}
