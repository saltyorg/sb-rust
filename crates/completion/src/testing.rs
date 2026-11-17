//! The test builds' install root (the `testing` feature, never in a
//! release build): `sb completion install` in sb's own tests writes under
//! [`ROOT_VAR`]'s directory, never the host's `/usr` or `/etc` (completion
//! spec, Testing strategy).

use std::path::PathBuf;

/// The variable naming the install root to the sb `cargo test` builds
/// ([`crate::install::Paths::for_sb`]).
pub const ROOT_VAR: &str = "SB_TEST_COMPLETION_ROOT";

/// [`ROOT_VAR`]'s root, when set and not empty.
#[must_use]
pub fn root() -> Option<PathBuf> {
    std::env::var_os(ROOT_VAR)
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
}
