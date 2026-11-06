//! `sb-host`: the host sb runs on. For now only the supported-release
//! check that every command passes first (cli-core spec, startup step 1);
//! the rest of the `host` module comes with its own spec.

// Output goes through `Write`; `print!`/`eprint!` panic on a closed pipe.
#![deny(clippy::print_stdout, clippy::print_stderr)]

use sysinfo::System;

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
/// the kernel is Linux; sb-rust ships only static Linux binaries, so that
/// can't fail here.
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
}
