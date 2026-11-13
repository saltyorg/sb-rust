//! `sb-host`: the host sb runs on (host spec). So far:
//! - the supported-release check that every command passes first
//!   (cli-core spec, startup step 1), and the host's release;
//! - the service logs ([`journal`]): sb-journal's reader, re-exported, with
//!   the host's glue;
//! - file ownership ([`owner`]).

// Output goes through `Write`; `print!`/`eprint!` panic on a closed pipe.
#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod journal;
pub mod owner;

pub use owner::{OwnerError, ensure_owner, ensure_owner_recursive};

use sysinfo::System;

use journal::{Release, ReleaseError};

/// Ubuntu releases sb runs on: sb-go's `SupportedUbuntuRuntimeReleases`.
pub const SUPPORTED_RUNTIME_RELEASES: &[&str] = &["22.04", "24.04", "26.04"];

/// Why this host isn't supported, in sb-go's words.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Unsupported {
    #[error("not an Ubuntu distribution (detected ID: {0})")]
    NotUbuntu(String),
    #[error("a Ubuntu version ID not found in /etc/os-release")]
    NoVersion,
    #[error(
        "unsupported Ubuntu version (detected version: {found}, supported versions: {supported})"
    )]
    Release { found: String, supported: String },
}

/// Refuse to run anywhere but a supported Ubuntu release (cli-core spec,
/// startup step 1).
///
/// `ID` and `VERSION_ID` come from `/etc/os-release` through sysinfo
/// (<https://docs.rs/sysinfo/0.40.0/sysinfo/struct.System.html#method.distribution_id>),
/// which reads the file directly. Why not `os_info`: it runs `lsb_release -a`
/// first, a Python process on Ubuntu, on every sb start. sb-go also checks
/// the kernel is Linux; sb-rust ships only Linux binaries (glibc, ADR-020),
/// so that can't fail here.
///
/// Differences from sb-go's own parser, none reachable on a real Ubuntu
/// host (both files are always there and agree): a missing os-release or
/// `ID` reads as the OS name (`linux`), and a missing `VERSION_ID` falls
/// back to `/etc/lsb-release`'s `DISTRIB_RELEASE`.
///
/// # Errors
///
/// [`Unsupported`] when the host isn't Ubuntu, names no version, or runs a
/// release sb doesn't support.
pub fn check_support() -> Result<(), Unsupported> {
    check(
        &System::distribution_id(),
        System::os_version().ok().as_deref(),
        SUPPORTED_RUNTIME_RELEASES,
    )
}

/// The decision, from the os-release values (sb-go's
/// `validateUbuntuSupport`).
///
/// # Errors
///
/// [`Unsupported`] when `id` isn't `ubuntu`, `version` is missing, or isn't
/// in `supported`.
pub fn check(id: &str, version: Option<&str>, supported: &[&str]) -> Result<(), Unsupported> {
    if id != "ubuntu" {
        return Err(Unsupported::NotUbuntu(id.to_string()));
    }
    let version = version.ok_or(Unsupported::NoVersion)?;
    if supported.contains(&version) {
        Ok(())
    } else {
        Err(Unsupported::Release {
            found: version.to_string(),
            supported: supported.join(", "),
        })
    }
}

/// Why the host's release isn't one sb reads the journal of.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum UbuntuReleaseError {
    /// os-release names no `VERSION_ID`, in sb-go's words.
    #[error("a Ubuntu version ID not found in /etc/os-release")]
    NoVersion,
    /// The `VERSION_ID` is none of [`journal::RELEASES`].
    #[error(transparent)]
    Unsupported(#[from] ReleaseError),
}

/// The host's Ubuntu release: os-release's `VERSION_ID`, read through
/// sysinfo as [`check_support`] reads it
/// (<https://docs.rs/sysinfo/0.40.0/sysinfo/struct.System.html#method.os_version>),
/// as the [`Release`] the journal is opened and queried with (journal spec,
/// assumption 2). python-env's deadsnakes rule (22.04 only) reads its
/// [`Release::version_id`]. It doesn't check `ID`: every command passes
/// [`check_support`] first, which does.
///
/// # Errors
///
/// [`UbuntuReleaseError::NoVersion`] when no `VERSION_ID` is found, and
/// [`UbuntuReleaseError::Unsupported`] when it isn't a supported release.
pub fn ubuntu_release() -> Result<Release, UbuntuReleaseError> {
    release_of(System::os_version().ok().as_deref())
}

/// The release a `VERSION_ID` names, if any.
fn release_of(version_id: Option<&str>) -> Result<Release, UbuntuReleaseError> {
    let version_id = version_id.ok_or(UbuntuReleaseError::NoVersion)?;
    Ok(Release::parse(version_id)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUPPORTED: &[&str] = &["22.04", "24.04", "26.04"];

    #[test]
    fn a_supported_ubuntu_passes() {
        assert_eq!(check("ubuntu", Some("24.04"), SUPPORTED), Ok(()));
    }

    /// sb-go: `not an Ubuntu distribution (detected ID: %s)`.
    #[test]
    fn another_distribution_is_refused() {
        let err = check("debian", Some("12"), SUPPORTED).expect_err("refused");
        assert_eq!(
            err.to_string(),
            "not an Ubuntu distribution (detected ID: debian)"
        );
    }

    /// sb-go: `a Ubuntu version ID not found in /etc/os-release`.
    #[test]
    fn a_missing_version_is_refused() {
        let err = check("ubuntu", None, SUPPORTED).expect_err("refused");
        assert_eq!(
            err.to_string(),
            "a Ubuntu version ID not found in /etc/os-release"
        );
    }

    /// sb-go: `unsupported Ubuntu version (detected version: %s, supported
    /// versions: %s)`, the list joined with ", ".
    #[test]
    fn an_unsupported_release_is_refused_with_the_list() {
        let err = check("ubuntu", Some("20.04"), SUPPORTED).expect_err("refused");
        assert_eq!(
            err.to_string(),
            "unsupported Ubuntu version (detected version: 20.04, supported versions: 22.04, 24.04, 26.04)"
        );
    }

    /// The runtime list is sb-go's `SupportedUbuntuRuntimeReleases`.
    #[test]
    fn the_runtime_releases_match_sb_go() {
        assert_eq!(SUPPORTED_RUNTIME_RELEASES, SUPPORTED);
    }

    /// The journal reads exactly the releases sb runs on (journal spec,
    /// assumption 2): a new LTS is a row in both lists, in the same order.
    #[test]
    fn the_journal_releases_are_the_runtime_releases() {
        let journal: Vec<&str> = journal::RELEASES.iter().map(Release::version_id).collect();
        assert_eq!(journal, SUPPORTED_RUNTIME_RELEASES);
    }

    /// A supported `VERSION_ID` gives its release, with its rules.
    #[test]
    fn a_supported_version_id_gives_its_release() {
        for v in SUPPORTED_RUNTIME_RELEASES {
            let release = release_of(Some(v)).expect("supported");
            assert_eq!(release, Release::parse(v).expect("a row"));
            assert_eq!(release.version_id(), *v);
        }
    }

    /// No `VERSION_ID`: sb-go's text, as `check` gives it.
    #[test]
    fn a_missing_version_id_is_a_typed_error() {
        let err = release_of(None).expect_err("refused");
        assert_eq!(err, UbuntuReleaseError::NoVersion);
        assert_eq!(err.to_string(), Unsupported::NoVersion.to_string());
    }

    /// A `VERSION_ID` that names no supported release, empty, padded or
    /// another release, is sb-journal's `Unsupported`, the value escaped.
    #[test]
    fn an_unparsable_version_id_is_a_typed_error() {
        for v in ["", " 24.04", "24.04.1", "20.04", "24.04\n"] {
            let err = release_of(Some(v)).expect_err("refused");
            assert_eq!(
                err,
                UbuntuReleaseError::Unsupported(ReleaseError::Unsupported(v.to_owned()))
            );
            assert_eq!(
                err.to_string(),
                format!("Ubuntu release {v:?} isn't supported")
            );
        }
    }

    /// `ubuntu_release` reads the `VERSION_ID` `check_support` reads: on a
    /// host `check_support` passes, it is that supported release.
    #[test]
    fn ubuntu_release_reads_the_version_id_check_support_reads() {
        let release = ubuntu_release();
        if check_support().is_ok() {
            let release = release.expect("a supported host's release");
            assert!(SUPPORTED_RUNTIME_RELEASES.contains(&release.version_id()));
        }
    }
}
