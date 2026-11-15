//! sb-config's facts half (tui spec, "Editors", `sb fact`): Saltbox's
//! per-app INI files, `<appdata>/saltbox/<role>.ini`, which Saltbox's
//! `saltbox_facts` module writes
//! (<https://raw.githubusercontent.com/saltyorg/Saltbox/master/library/saltbox_facts.py>).
//! Not `saltbox.fact`, the Ansible local fact python-env installs.
//!
//! [`document`] is the dialect: a parser that keeps every line and a
//! writer that changes only the lines an edit touches (sb-go
//! facts/document.go). [`session`] opens the facts directory and takes
//! the locks, [`apply`] saves the staged changes, and [`secret`] says
//! which values the editor masks.

pub mod apply;
pub mod document;
pub mod secret;
pub mod session;

/// One fact: a key of an instance and its value, continuation lines
/// joined with `\n`. Its `Debug` leaves the value out (a fact can be a
/// secret, and no fact value is ever logged: tui spec decision 32).
#[derive(Clone, PartialEq, Eq)]
pub struct Fact {
    pub key: String,
    pub value: String,
}

impl std::fmt::Debug for Fact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fact")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

/// One copy of an app: an INI section and its facts, sorted by key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instance {
    pub name: String,
    pub facts: Vec<Fact>,
}

/// One app: `<role>.ini` and its instances, sorted by name. The
/// `DEFAULT` section stays on disk but isn't one of them (sb-go's
/// catalog hides it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Role {
    pub name: String,
    pub instances: Vec<Instance>,
}
