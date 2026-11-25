//! Build information and the `version` command (spec: "Build info").
//!
//! Release builds set `SB_VERSION`, `SB_GIT_COMMIT` and `SB_CHANNEL` in
//! the build environment, as sb-go's release sets its values with
//! `-ldflags -X`. `option_env!` reads them at compile time, and rustc
//! records them in its dep-info, so Cargo rebuilds when they change
//! (<https://doc.rust-lang.org/std/macro.option_env.html>). Any of them
//! exported in a developer's shell is baked in the same way. cross runs
//! the build in its container, which sees them only through Cross.toml's
//! `[build.env] passthrough` (a test below holds the two together). Why not
//! vergen or built: they work the commit out in build.rs, while here the
//! release injects the values, as sb-go's ldflags do.

use serde::Serialize;

/// The uv version sb installs, pinned in source as in sb-go's `buildinfo`,
/// so raw builds, tests and releases agree.
pub const UV_VERSION: &str = "0.13.10";

/// The release channel a build follows (release spec, "Release
/// channels"): baked in at build time through `SB_CHANNEL`, as sb-go's
/// `disableSelfUpdate` is. Also `self-update --channel`'s values, through
/// clap's `ValueEnum` with `None` skipped, so `--channel none` is refused
/// (<https://docs.rs/clap/4.6.7/clap/_derive/index.html#possible-value-attributes>).
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Channel {
    /// The newest stable release and its plain asset.
    Release,
    /// The newest release, prereleases included, and its `_beta` asset.
    Beta,
    /// Self-update off: local and raw builds, and package-manager ones.
    #[value(skip)]
    None,
}

impl Channel {
    /// `SB_CHANNEL`'s value as a channel: unset is [`Channel::None`];
    /// anything but `release`, `beta` and `none` is no channel, which the
    /// build refuses ([`BUILT`]). A `const fn`, so that refusal is a
    /// compile error rather than a binary that quietly never updates.
    /// Matched as bytes: a `str` pattern can't be matched in a `const fn`
    /// (E0658), a byte-string one can.
    #[must_use]
    pub const fn parse(value: Option<&str>) -> Option<Self> {
        let Some(value) = value else {
            return Some(Self::None);
        };
        match value.as_bytes() {
            b"release" => Some(Self::Release),
            b"beta" => Some(Self::Beta),
            b"none" => Some(Self::None),
            _ => None,
        }
    }

    /// The channel's name, as `SB_CHANNEL` and `--channel` spell it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Beta => "beta",
            Self::None => "none",
        }
    }
}

/// The channel's name, as [`Channel::name`] gives it.
impl std::fmt::Display for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// This build's channel; an `SB_CHANNEL` that names none fails the build.
const BUILT: Channel = match Channel::parse(option_env!("SB_CHANNEL")) {
    Some(channel) => channel,
    None => panic!("SB_CHANNEL must be release, beta or none"),
};

/// What `sb version` reports, fixed when the binary was built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildInfo {
    pub version: String,
    pub git_commit: String,
    pub uv_version: String,
    /// The release channel self-update follows.
    pub channel: Channel,
    /// `SB_VERSION` was set at build time: a release build. Self-update
    /// treats any other build as disabled, so a developer build never
    /// replaces itself with a release (release spec, decision 11); `sb
    /// version` reads the same either way.
    pub released: bool,
}

/// sb-go's `version --json` shape, then the channel; field order is the
/// output order.
#[derive(Serialize)]
struct VersionJson<'a> {
    version: &'a str,
    git_commit: &'a str,
    uv_version: &'a str,
    channel: &'a str,
}

impl BuildInfo {
    /// The binary's build information. `package_version` is the binary's
    /// own `env!("CARGO_PKG_VERSION")`: expanded here it would be this
    /// library's version.
    #[must_use]
    pub fn current(package_version: &str) -> Self {
        Self::from_env(
            package_version,
            option_env!("SB_VERSION"),
            option_env!("SB_GIT_COMMIT"),
            BUILT,
        )
    }

    /// Build information from the release variables, falling back to the
    /// binary's package version and an empty commit, as sb-go's raw builds.
    #[must_use]
    pub fn from_env(
        package_version: &str,
        version: Option<&str>,
        git_commit: Option<&str>,
        channel: Channel,
    ) -> Self {
        Self {
            version: version.unwrap_or(package_version).to_string(),
            git_commit: git_commit.unwrap_or_default().to_string(),
            uv_version: UV_VERSION.to_string(),
            channel,
            released: version.is_some(),
        }
    }

    /// Whether self-update is off: the `none` channel, or a raw build
    /// (release spec, decision 11). `--force-update` exists only then.
    #[must_use]
    pub fn self_update_off(&self) -> bool {
        self.channel == Channel::None || !self.released
    }

    /// `sb version`: sb-go's `Saltbox CLI version: %s (commit: %s)\n`,
    /// then the channel's line.
    #[must_use]
    pub fn version_line(&self) -> String {
        let off = if self.channel == Channel::None {
            " (self-update disabled)"
        } else {
            ""
        };
        format!(
            "Saltbox CLI version: {} (commit: {})\nRelease channel: {}{off}\n",
            self.version,
            self.git_commit,
            self.channel.name()
        )
    }

    /// `sb version --json`: compact, ending in a newline, as Go's
    /// `json.Encoder`; sb-go's three fields first, then `channel`.
    #[must_use]
    pub fn version_json(&self) -> String {
        let json = VersionJson {
            version: &self.version,
            git_commit: &self.git_commit,
            uv_version: &self.uv_version,
            channel: self.channel.name(),
        };
        // Serialising four strings can't fail.
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
            channel: Channel::Release,
            released: true,
        }
    }

    /// sb-go's `Saltbox CLI version: %s (commit: %s)\n`, then the channel.
    #[test]
    fn version_line_is_sb_gos_then_the_channel() {
        assert_eq!(
            info().version_line(),
            "Saltbox CLI version: 1.2.3 (commit: 81558b9)\nRelease channel: release\n"
        );
        let beta = BuildInfo {
            channel: Channel::Beta,
            ..info()
        };
        assert!(beta.version_line().ends_with("\nRelease channel: beta\n"));
        let none = BuildInfo {
            channel: Channel::None,
            ..info()
        };
        assert!(
            none.version_line()
                .ends_with("\nRelease channel: none (self-update disabled)\n")
        );
    }

    /// sb-go encodes `{version, git_commit, uv_version}` with Go's JSON
    /// encoder: compact, in that order, ending in a newline; `channel`
    /// comes after them.
    #[test]
    fn version_json_is_sb_gos_then_the_channel() {
        assert_eq!(
            info().version_json(),
            "{\"version\":\"1.2.3\",\"git_commit\":\"81558b9\",\"uv_version\":\"0.13.4\",\"channel\":\"release\"}\n"
        );
    }

    /// A raw build (no release variables) still has a version, from the
    /// package, and the uv version pinned in source; the commit is empty, as
    /// in sb-go's raw builds.
    #[test]
    fn current_falls_back_to_package_values() {
        let current = BuildInfo::from_env(env!("CARGO_PKG_VERSION"), None, None, Channel::None);
        assert_eq!(current.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(current.git_commit, "");
        assert_eq!(current.uv_version, UV_VERSION);
        assert!(!current.released);
    }

    /// The fallback is the version the caller passes: the binary's own
    /// package version, not this library's.
    #[test]
    fn the_fallback_is_the_binary_package_version() {
        let current = BuildInfo::from_env("9.8.7", None, None, Channel::None);
        assert_eq!(current.version, "9.8.7");
    }

    #[test]
    fn release_variables_override_the_defaults() {
        let current = BuildInfo::from_env("0.1.0", Some("3.0.0"), Some("abc1234"), Channel::Beta);
        assert_eq!(current.version, "3.0.0");
        assert_eq!(current.git_commit, "abc1234");
        assert_eq!(current.channel, Channel::Beta);
        assert!(current.released);
    }

    /// Self-update is on only for a released build on `release` or `beta`.
    #[test]
    fn self_update_is_off_on_none_and_in_raw_builds() {
        let on = |channel, version| {
            !BuildInfo::from_env("0.1.0", version, None, channel).self_update_off()
        };
        assert!(on(Channel::Release, Some("1.0.0")));
        assert!(on(Channel::Beta, Some("1.0.0-rc.1")));
        assert!(!on(Channel::None, Some("1.0.0")));
        assert!(!on(Channel::Release, None));
        assert!(!on(Channel::Beta, None));
        // Released or not, `sb version` reads the same.
        let raw = BuildInfo::from_env("0.1.0", None, None, Channel::Release);
        let released = BuildInfo::from_env("0.1.0", Some("0.1.0"), None, Channel::Release);
        assert_eq!(raw.version_line(), released.version_line());
        assert_eq!(raw.version_json(), released.version_json());
    }

    /// `SB_CHANNEL`: unset is `none`; only the three names are channels,
    /// exactly as spelt, so a typo fails the build.
    #[test]
    fn sb_channel_names_three_channels() {
        assert_eq!(Channel::parse(None), Some(Channel::None));
        assert_eq!(Channel::parse(Some("release")), Some(Channel::Release));
        assert_eq!(Channel::parse(Some("beta")), Some(Channel::Beta));
        assert_eq!(Channel::parse(Some("none")), Some(Channel::None));
        for value in ["", "stable", "Release", "BETA", "release ", "true"] {
            assert_eq!(Channel::parse(Some(value)), None, "{value:?}");
        }
        for channel in [Channel::Release, Channel::Beta, Channel::None] {
            assert_eq!(Channel::parse(Some(channel.name())), Some(channel));
        }
    }

    /// sb-M4: cross runs the release build in its container, which sees a
    /// build variable only when Cross.toml passes it through. Every
    /// variable this file reads must be in that list.
    #[test]
    fn cross_passes_every_build_variable_through() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let cross: toml::Table = std::fs::read_to_string(root.join("Cross.toml"))
            .expect("Cross.toml")
            .parse()
            .expect("Cross.toml is TOML");
        let passed: Vec<&str> = cross["build"]["env"]["passthrough"]
            .as_array()
            .expect("[build.env] passthrough")
            .iter()
            .filter_map(toml::Value::as_str)
            .collect();
        let source = include_str!("build_info.rs");
        let read: Vec<&str> = source
            .split("option_env!(\"")
            .skip(1)
            .filter_map(|rest| rest.split('"').next())
            .collect();
        assert_eq!(read, ["SB_CHANNEL", "SB_VERSION", "SB_GIT_COMMIT"]);
        for name in read {
            assert!(passed.contains(&name), "{name} not in {passed:?}");
        }
    }
}
