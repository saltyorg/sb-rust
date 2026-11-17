//! Shell completion (capability map, `completion`; spec
//! docs/specs/completion.md): Tab completion for bash, zsh and fish
//! through `clap_complete`'s dynamic engine, the tag completer for `sb
//! install`, and the shells' registration stubs.
//!
//! The engine is `clap_complete`'s `unstable-dynamic`, pinned exactly
//! (ADR-001): every use of it is in this crate.

pub mod tags;

use clap_complete::env::{Bash, CompleteEnv, EnvCompleter, Fish, Shells, Zsh};

/// The variable that makes a run of sb a completion request (spec,
/// decision 9: not clap's default `COMPLETE`, which a user's exported
/// variable could turn on for every run).
pub const VAR: &str = "SB_COMPLETE";

/// The shells sb completes in (sb-go's three; spec, difference 4).
const SHELLS: Shells<'static> = Shells(&[&Bash, &Zsh, &Fish]);

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

/// The line after clap's zsh stub (spec, verification item 1). compinit
/// autoloads the file `_<name>` from `fpath` as the body of a function of
/// that name (zshcompsys(1), "Autoloaded files"), so on the first Tab the
/// stub only defines clap's completer and registers it, and completes
/// nothing; tests/shells.sh showed that on Ubuntu 24.04's zsh 5.9. When the
/// file runs as that function, this calls the completer as well, as
/// zsh's own `#compdef` functions do and clap's static zsh script does
/// (`funcstack[1]`). Why not clap's stub alone: it is written for
/// `source`, where this line does nothing. `FILE` is the file's name,
/// `FUNCTION` clap's completer (`_clap_dynamic_completer_<name>`, `-` as
/// `_`, `clap_complete` 4.6.11, `env/shells.rs`).
const ZSH_FIRST_LOAD: &str = r#"if [[ "${funcstack[1]}" == "FILE" ]]; then
    FUNCTION "$@"
fi
"#;

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

    /// Its name, as sb-go's messages and the commands spell it.
    #[must_use]
    pub fn name(self) -> &'static str {
        self.engine().name()
    }

    /// clap's adapter for it (`unstable-dynamic`, ADR-001).
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
    /// calls sb (spec, difference 1); zsh's gets one `if` after it
    /// ([`ZSH_FIRST_LOAD`]).
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
            stub.extend(
                ZSH_FIRST_LOAD
                    .replace("FILE", &format!("_{name}"))
                    .replace("FUNCTION", &function)
                    .as_bytes(),
            );
        }
        stub
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each shell's stub registers `name`, calls `completer` with sb's
    /// variable, and is clap's (the same as `CompleteEnv` prints).
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
        assert!(
            zsh.contains("compdef _clap_dynamic_completer_sb_x sb-x\n"),
            "{zsh}"
        );
        assert!(
            zsh.ends_with(
                "\nif [[ \"${funcstack[1]}\" == \"_sb-x\" ]]; then\n    _clap_dynamic_completer_sb_x \"$@\"\nfi\n"
            ),
            "{zsh}"
        );
        for shell in [Shell::Bash, Shell::Fish] {
            let stub = String::from_utf8(shell.registration("sb", "sb")).expect("UTF-8");
            assert!(!stub.contains("funcstack"), "{shell:?}: {stub}");
        }
    }

    #[test]
    fn names_are_sb_gos() {
        let names: Vec<_> = Shell::ALL.iter().map(|shell| shell.name()).collect();
        assert_eq!(names, ["bash", "zsh", "fish"]);
    }
}
