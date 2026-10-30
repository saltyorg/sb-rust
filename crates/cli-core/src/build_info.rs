//! Build information and the `version` command (spec: "Build info").
//!
//! Release builds set `SB_VERSION`, `SB_GIT_COMMIT` and
//! `SB_DISABLE_SELF_UPDATE` in the build environment, as sb-go's release
//! sets them with `-ldflags -X`. `option_env!` reads them at compile time,
//! and rustc records them in its dep-info, so Cargo rebuilds when they
//! change (https://doc.rust-lang.org/std/macro.option_env.html). Any of
//! them exported in a developer's shell is baked in the same way. Why not
//! vergen or built: they work the commit out in build.rs, while here the
//! release injects the values, as sb-go's ldflags do.

use serde::Serialize;

/// The uv version sb installs, pinned in source as in sb-go's `buildinfo`,
/// so raw builds, tests and releases agree.
pub const UV_VERSION: &str = "0.13.4";

/// What `sb version` reports, fixed when the binary was built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildInfo {
    pub version: String,
    pub git_commit: String,
    pub uv_version: String,
    /// Release builds for package managers turn self-update off.
    pub self_update_disabled: bool,
}

/// sb-go's `version --json` shape; field order is the output order.
#[derive(Serialize)]
struct VersionJson<'a> {
    version: &'a str,
    git_commit: &'a str,
    uv_version: &'a str,
}

impl BuildInfo {
    /// The binary's build information. `package_version` is the binary's
    /// own `env!("CARGO_PKG_VERSION")`: expanded here it would be this
    /// library's version.
    pub fn current(package_version: &str) -> Self {
        Self::from_env(
            package_version,
            option_env!("SB_VERSION"),
            option_env!("SB_GIT_COMMIT"),
            option_env!("SB_DISABLE_SELF_UPDATE"),
        )
    }

    /// Build information from the release variables, falling back to the
    /// binary's package version and an empty commit, as sb-go's raw builds.
    pub fn from_env(
        package_version: &str,
        version: Option<&str>,
        git_commit: Option<&str>,
        disable_self_update: Option<&str>,
    ) -> Self {
        Self {
            version: version.unwrap_or(package_version).to_string(),
            git_commit: git_commit.unwrap_or_default().to_string(),
            uv_version: UV_VERSION.to_string(),
            self_update_disabled: disable_self_update == Some("true"),
        }
    }

    /// `sb version`: sb-go's `Saltbox CLI version: %s (commit: %s)\n`.
    pub fn version_line(&self) -> String {
        format!(
            "Saltbox CLI version: {} (commit: {})\n",
            self.version, self.git_commit
        )
    }

    /// `sb version --json`: compact, ending in a newline, as Go's
    /// `json.Encoder`.
    pub fn version_json(&self) -> String {
        let json = VersionJson {
            version: &self.version,
            git_commit: &self.git_commit,
            uv_version: &self.uv_version,
        };
        // Serialising three strings can't fail.
        let mut text = serde_json::to_string(&json).unwrap_or_default();
        text.push('\n');
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> BuildInfo {
        BuildInfo {
            version: "1.2.3".into(),
            git_commit: "81558b9".into(),
            uv_version: "0.13.4".into(),
            self_update_disabled: false,
        }
    }

    /// sb-go: `Saltbox CLI version: %s (commit: %s)\n`.
    #[test]
    fn version_line_matches_sb_go() {
        assert_eq!(
            info().version_line(),
            "Saltbox CLI version: 1.2.3 (commit: 81558b9)\n"
        );
    }

    /// sb-go encodes `{version, git_commit, uv_version}` with Go's JSON
    /// encoder: compact, in that order, ending in a newline.
    #[test]
    fn version_json_matches_sb_go() {
        assert_eq!(
            info().version_json(),
            "{\"version\":\"1.2.3\",\"git_commit\":\"81558b9\",\"uv_version\":\"0.13.4\"}\n"
        );
    }

    /// A raw build (no release variables) still has a version, from the
    /// package, and the uv version pinned in source; the commit is empty, as
    /// in sb-go's raw builds.
    #[test]
    fn current_falls_back_to_package_values() {
        let current = BuildInfo::from_env(env!("CARGO_PKG_VERSION"), None, None, None);
        assert_eq!(current.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(current.git_commit, "");
        assert_eq!(current.uv_version, UV_VERSION);
        assert!(!current.self_update_disabled);
    }

    /// The fallback is the version the caller passes: the binary's own
    /// package version, not this library's.
    #[test]
    fn the_fallback_is_the_binary_package_version() {
        let current = BuildInfo::from_env("9.8.7", None, None, None);
        assert_eq!(current.version, "9.8.7");
    }

    #[test]
    fn release_variables_override_the_defaults() {
        let current = BuildInfo::from_env("0.1.0", Some("3.0.0"), Some("abc1234"), Some("true"));
        assert_eq!(current.version, "3.0.0");
        assert_eq!(current.git_commit, "abc1234");
        assert!(current.self_update_disabled);
    }

    /// Only the exact value "true" disables self-update, as sb-go's
    /// `disableSelfUpdate == "true"`.
    #[test]
    fn only_true_disables_self_update() {
        for value in ["1", "yes", "TRUE", ""] {
            assert!(
                !BuildInfo::from_env("0.1.0", None, None, Some(value)).self_update_disabled,
                "{value}"
            );
        }
    }
}
