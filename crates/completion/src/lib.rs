//! Shell completion (capability map, `completion`; spec
//! docs/specs/completion.md): Tab completion for bash, zsh and fish
//! through `clap_complete`'s dynamic engine, the tag completer for `sb
//! install`, and the shells' registration stubs.
//!
//! The engine is `clap_complete`'s `unstable-dynamic`, pinned exactly
//! (ADR-001): every use of it is in this crate.

mod describe;
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
    /// `unstable-dynamic`, ADR-001), with sb's patches ([`Shell::patch`]).
    /// No script of sb's own: the stub only calls sb (spec, difference 1).
    ///
    /// # Panics
    ///
    /// Never: it writes to memory, which can't fail.
    #[must_use]
    pub fn registration(self, name: &str, completer: &str) -> Vec<u8> {
        let mut stub = Vec::new();
        self.write_registration(VAR, name, name, completer, &mut stub)
            // Writing to a `Vec` can't fail.
            .expect("a stub in memory");
        stub
    }

    /// clap's `stub` for `name` (clap's identifier for the script) and
    /// `bin` (the command it registers) with sb's changes, each matched
    /// against `clap_complete` 4.6.11's exact text (pinned exactly:
    /// ADR-001), so a stub that no longer matches is left as clap wrote it
    /// and the tests fail: zsh's last line guarded ([`zsh_guard`]); bash's
    /// and fish's quiet when the completer fails ([`quiet_bash`],
    /// [`quiet_fish`]; spec, decision 12). sb passes the same for both
    /// (`registration`, and `CompleteEnv` for sb's tree, which sets no
    /// `bin_name`), but each goes where clap puts it.
    fn patch(self, stub: String, name: &str, bin: &str) -> String {
        match self {
            Self::Bash => bash_columns(quiet_bash(stub)),
            Self::Fish => quiet_fish(stub),
            Self::Zsh => {
                // clap's function is named from `name`, its `compdef`
                // registers `bin`, and compinit autoloads `_<bin>`.
                let function = format!("_clap_dynamic_completer_{}", name.replace('-', "_"));
                let compdef = format!("compdef {function} {bin}\n");
                // The stub ends with that line.
                match stub.strip_suffix(&compdef) {
                    Some(body) => body.to_owned() + &zsh_guard(&format!("_{bin}"), &function, bin),
                    None => stub,
                }
            }
        }
    }
}

/// The completer's call in clap's bash stub, the line's end
/// (`clap_complete` 4.6.11, `env/shells.rs`: `"COMPLETER" -- "${words[@]}"
/// \`).
const BASH_CALL: &str = " -- \"${words[@]}\" \\\n";

/// clap's bash stub with the completer's stderr discarded, as clap's zsh
/// stub discards it (spec, decision 12): without it, a missing or
/// unexecutable sb made bash print `/usr/local/bin/sb: No such file or
/// directory` above the prompt on every Tab, and a failing one its own
/// errors. The stub already drops the reply on a non-zero exit (its `$?`
/// check). Why not upstream first: sb's installed files can't wait for a
/// clap release; the change is proposed there with zsh's guard. Only a
/// stub with the call once is patched.
fn quiet_bash(stub: String) -> String {
    if stub.matches(BASH_CALL).count() == 1 {
        stub.replacen(BASH_CALL, " -- \"${words[@]}\" 2>/dev/null \\\n", 1)
    } else {
        stub
    }
}

/// The last of the variables clap's bash stub sets for the completer
/// (`clap_complete` 4.6.11, `env/shells.rs`), a line of its own.
const BASH_SPACE: &str = "        _CLAP_COMPLETE_SPACE=\"$_CLAP_COMPLETE_SPACE\" \\\n";

/// clap's bash stub passing `$COLUMNS` too, for bash's descriptions
/// (spec, decision 13; [`describe::COLUMNS_VAR`]): clap's stub passes the
/// `COMP_TYPE` but not the width. `${COLUMNS-}`: empty when unset, and
/// sb then lists names. bash keeps `COLUMNS` current in an interactive
/// shell (`checkwinsize`, on by default since bash 5.0, and SIGWINCH;
/// bash(1), "COLUMNS"), and cobra's bash script reads the width the same
/// way (`$COLUMNS`, `bash_completionsV2.go`, v1.10.2). Why not ask the
/// terminal from sb (crossterm's `terminal::size`): the completer's
/// stdout is the stub's command substitution, so it would open
/// `/dev/tty`, and where the ioctl fails crossterm starts `tput`, a
/// process on every Tab. Only a stub with that line once is patched.
fn bash_columns(stub: String) -> String {
    if stub.matches(BASH_SPACE).count() == 1 {
        let columns = format!("        {}=\"${{COLUMNS-}}\" \\\n", describe::COLUMNS_VAR);
        stub.replacen(BASH_SPACE, &format!("{BASH_SPACE}{columns}"), 1)
    } else {
        stub
    }
}

/// Where clap's fish stub's command substitution opens and closes
/// (`clap_complete` 4.6.11, `env/shells.rs`: `--arguments "(VAR=fish
/// COMPLETER -- ...)"`, the stub's only line).
const FISH_OPEN: &str = " --arguments \"(";
const FISH_CLOSE: &str = ")\"\n";

/// clap's fish stub with the completer's stderr discarded and its reply
/// used only when it exits 0 (spec, decision 12): fish takes a command
/// substitution's output whatever its status, so a failing sb's stdout
/// became a candidate (`Error:\ something\ broke`), and its stderr
/// reached the prompt. `set` passes the substitution's status on ("In
/// assignment mode, set does not modify the exit status, but passes along
/// whatever $status was set, including by command substitutions",
/// <https://fishshell.com/docs/3.3/cmds/set.html>, fish 3.3 being the
/// oldest sb supports), and `string join` prints the reply's lines again,
/// nothing for none
/// (<https://fishshell.com/docs/3.3/cmds/string-join.html>). The `$` is
/// escaped: the stub's outer double quotes are expanded when the file is
/// sourced, the inside at each Tab. Only a stub with one substitution at
/// its end is patched.
fn quiet_fish(stub: String) -> String {
    let patched = stub.split_once(FISH_OPEN).and_then(|(head, rest)| {
        let call = rest.strip_suffix(FISH_CLOSE)?;
        (!call.contains(FISH_OPEN)).then(|| {
            format!(
                "{head}{FISH_OPEN}set -l __sb_completion_reply ({call} 2>/dev/null); and string join \\n -- \\$__sb_completion_reply{FISH_CLOSE}"
            )
        })
    });
    patched.unwrap_or(stub)
}

/// sb's adapter for each shell: clap's registration stub with sb's
/// patches ([`Shell::patch`]), as `generate` writes it and `CompleteEnv`
/// prints it for `SB_COMPLETE=<shell> sb`, and sb's reply, clap's
/// engine's candidates without the hidden commands and flags, in clap's
/// format (spec, decision 11; the module `reply`). `CompleteEnv::shells`
/// takes any `EnvCompleter`
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
        let mut stub = Vec::new();
        self.engine()
            .write_registration(var, name, bin, completer, &mut stub)?;
        // clap writes the stub from `&str`s: UTF-8 throughout.
        let stub = String::from_utf8_lossy(&stub).into_owned();
        buf.write_all(self.patch(stub, name, bin).as_bytes())
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

    /// clap 4.6.11's bash stub, whole, with sb's two changes: the
    /// completer's stderr discarded (spec, decision 12) and `$COLUMNS`
    /// passed for the descriptions (decision 13). A clap release that
    /// changes the stub fails here, not silently on a host.
    #[test]
    fn the_bash_stub_discards_the_completers_stderr() {
        let bash =
            String::from_utf8(Shell::Bash.registration("sb", "/usr/local/bin/sb")).expect("UTF-8");
        let want = r#"
_clap_complete_sb() {
    local IFS=$'\013'
    local _CLAP_COMPLETE_INDEX=${COMP_CWORD}
    local _CLAP_COMPLETE_COMP_TYPE=${COMP_TYPE}
    if compopt +o nospace 2> /dev/null; then
        local _CLAP_COMPLETE_SPACE=false
    else
        local _CLAP_COMPLETE_SPACE=true
    fi
    local words=("${COMP_WORDS[@]}")
    if [[ "${BASH_VERSINFO[0]}" -ge 4 ]]; then
        words[COMP_CWORD]="$2"
    fi
    COMPREPLY=( $( \
        _CLAP_IFS="$IFS" \
        _CLAP_COMPLETE_INDEX="$_CLAP_COMPLETE_INDEX" \
        _CLAP_COMPLETE_COMP_TYPE="$_CLAP_COMPLETE_COMP_TYPE" \
        _CLAP_COMPLETE_SPACE="$_CLAP_COMPLETE_SPACE" \
        _SB_COMPLETE_COLUMNS="${COLUMNS-}" \
        SB_COMPLETE="bash" \
        "/usr/local/bin/sb" -- "${words[@]}" 2>/dev/null \
    ) )
    if [[ $? != 0 ]]; then
        unset COMPREPLY
    elif [[ $_CLAP_COMPLETE_SPACE == false ]] && [[ "${COMPREPLY-}" =~ [=/:]$ ]]; then
        compopt -o nospace
    fi
}
if [[ "${BASH_VERSINFO[0]}" -eq 4 && "${BASH_VERSINFO[1]}" -ge 4 || "${BASH_VERSINFO[0]}" -gt 4 ]]; then
    complete -o nospace -o bashdefault -o nosort -F _clap_complete_sb sb
else
    complete -o nospace -o bashdefault -F _clap_complete_sb sb
fi

"#;
        assert_eq!(bash, want);
    }

    /// clap 4.6.11's fish stub, whole, with sb's change: the completer's
    /// stderr discarded and its reply used only when it exits 0 (spec,
    /// decision 12).
    #[test]
    fn the_fish_stub_uses_only_a_successful_reply() {
        let fish =
            String::from_utf8(Shell::Fish.registration("sb", "/usr/local/bin/sb")).expect("UTF-8");
        let want = r#"complete --keep-order --exclusive --command sb --arguments "(set -l __sb_completion_reply (SB_COMPLETE=fish /usr/local/bin/sb -- (commandline --current-process --tokenize --cut-at-cursor) (commandline --current-token) 2>/dev/null); and string join \n -- \$__sb_completion_reply)"
"#;
        assert_eq!(fish, want);
    }

    /// A stub the patches don't match is left as clap wrote it (and the
    /// tests above fail): never half patched.
    #[test]
    fn a_stub_the_patches_dont_match_is_kept() {
        assert_eq!(bash_columns("no line\n".to_owned()), "no line\n");
        let twice = format!("{BASH_SPACE}{BASH_SPACE}");
        assert_eq!(bash_columns(twice.clone()), twice);
        assert_eq!(quiet_bash("no call here\n".to_owned()), "no call here\n");
        let twice = format!("{BASH_CALL}{BASH_CALL}");
        assert_eq!(quiet_bash(twice.clone()), twice);
        assert_eq!(
            quiet_fish("complete --command sb\n".to_owned()),
            "complete --command sb\n"
        );
        let open = "complete --arguments \"(x)\" --arguments \"(y)\"\n";
        assert_eq!(quiet_fish(open.to_owned()), open);
        let unclosed = "complete --arguments \"(x\n";
        assert_eq!(quiet_fish(unclosed.to_owned()), unclosed);
    }

    /// The zsh guard follows clap's placement when `name` and `bin`
    /// differ: the function from `name`, the `compdef` and the autoloaded
    /// file from `bin`.
    #[test]
    fn the_zsh_guard_takes_name_and_bin_where_clap_puts_them() {
        let mut stub = Vec::new();
        EnvCompleter::write_registration(&Shell::Zsh, VAR, "sb-x", "sbx", "/bin/sbx", &mut stub)
            .expect("memory");
        let stub = String::from_utf8(stub).expect("UTF-8");
        let guard = zsh_guard("_sbx", "_clap_dynamic_completer_sb_x", "sbx");
        assert!(stub.ends_with(&guard), "{stub}");
        assert!(!stub.contains("\ncompdef "), "{stub}");
    }

    #[test]
    fn names_are_sb_gos() {
        let names = Shell::ALL.map(Shell::name);
        assert_eq!(names, ["bash", "zsh", "fish"]);
    }
}
