//! sb's own config file and the Saltbox paths it lives under (spec:
//! startup step 6).
//!
//! `<server_appdata_path>/sb/config.toml` (`/opt/sb/config.toml` by
//! default), so Saltbox backups cover it. `server_appdata_path` comes from
//! the Saltbox inventory with sb-go's rules (layout/paths.go
//! `loadServerAppdataPath`), read with saphyr. The file is TOML,
//! read with the `toml` crate through serde, strictly: an unknown key,
//! theme or role is an error, which the caller shows as a warning before
//! using the built-in theme. Both files are readable without root (the
//! config is 0664, the inventory world-readable), so help and version use
//! the theme too.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::theme::{Palette, Profile, Role};

pub mod yaml;

/// The Saltbox checkout (sb-go's `SaltboxRepoPath`), unless
/// `SB_SALTBOX_DIR` moves it.
const SALTBOX_DIR: &str = "/srv/git/saltbox";

/// The Saltbox root: `SB_SALTBOX_DIR` (tests and debugging, as
/// `SB_CONFIG`; sudo's relaunch drops it) when set and not empty, else
/// `/srv/git/saltbox` (config spec: one root for the inventory and the
/// config files). Why not clap's `Arg::env`: the root is read before the
/// parse (the theme colours help), and it would need a flag.
pub fn saltbox_dir(var: Option<OsString>) -> PathBuf {
    var.filter(|value| !value.is_empty())
        .map_or_else(|| PathBuf::from(SALTBOX_DIR), PathBuf::from)
}

/// The Saltbox inventory holding `server_appdata_path` under `root`
/// (sb-go's `SaltboxInventoryConfigPath`).
#[must_use]
pub fn inventory_path(root: &Path) -> PathBuf {
    root.join("inventories/host_vars/localhost.yml")
}

/// sb's config file, relative to the appdata path.
const CONFIG_FILE: &str = "sb/config.toml";

/// Files larger than this are not sb's or Saltbox's: refused, not read whole.
/// The cap for any text sb reads (exec's captured output too), except the
/// inventory ([`read_inventory`]), which has none.
pub const MAX_FILE_BYTES: usize = 1 << 20;

/// The open flags for both files: never block (a FIFO, or a lease the
/// file's owner holds, must not hang every sb start, root's included;
/// `NONBLOCK` has no effect on reading a regular file), never become the
/// controlling terminal, and close on exec
/// (<https://docs.rs/rustix/1.1.6/rustix/fs/fn.open.html>).
fn read_flags() -> rustix::fs::OFlags {
    use rustix::fs::OFlags;
    OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC
}

/// The text of an opened file: a regular file (checked on the handle, so
/// there's no race) of at most `cap` bytes (`None`: any length), in UTF-8.
fn read_opened(
    opened: rustix::io::Result<std::os::fd::OwnedFd>,
    cap: Option<usize>,
) -> Result<Option<String>, ConfigError> {
    let file = match opened {
        Ok(fd) => std::fs::File::from(fd),
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(errno) => return Err(ConfigError::Read(errno.into())),
    };
    if !file
        .metadata()
        .map_err(ConfigError::Read)?
        .file_type()
        .is_file()
    {
        return Err(ConfigError::NotAFile);
    }
    let mut bytes = Vec::new();
    match cap {
        Some(cap) => {
            file.take(cap as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(ConfigError::Read)?;
            if bytes.len() > cap {
                return Err(ConfigError::TooBig(cap));
            }
        }
        None => {
            (&file).read_to_end(&mut bytes).map_err(ConfigError::Read)?;
        }
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| ConfigError::NotUtf8)
}

/// `path`'s text, or `None` when there is no such file: a regular file of
/// at most 1 MiB in UTF-8, opened without blocking. For Saltbox's config
/// and schema files (their writer already controls root through Ansible,
/// and sb-go reads them the same way) and an `SB_CONFIG` path (the user's
/// own run).
///
/// # Errors
///
/// A [`ConfigError`] when the file can't be opened or read, isn't a regular
/// file, is over 1 MiB or isn't UTF-8.
pub fn read_small(path: &Path) -> Result<Option<String>, ConfigError> {
    read_opened(open_plain(path), Some(MAX_FILE_BYTES))
}

/// The inventory's text (localhost.yml), as [`read_small`] reads a file
/// but of any length: the inventory is as long as its owner makes it
/// (hardening plan decision 41), and Ansible reads it whole. Its writer
/// already controls root through Ansible, as for [`read_small`]'s files.
/// The YAML read limits its nesting depth ([`yaml::MAX_DEPTH`]) and its
/// aliases (saphyr's budget), not its length: reading and parsing it
/// costs time and memory in proportion (about 30 times its size at the
/// peak, measured in the platform note), on every sb start.
///
/// # Errors
///
/// A [`ConfigError`] when the file can't be opened or read, isn't a regular
/// file or isn't UTF-8.
pub fn read_inventory(path: &Path) -> Result<Option<String>, ConfigError> {
    read_opened(open_plain(path), None)
}

/// `path` opened with [`read_flags`], following links as sb-go does.
fn open_plain(path: &Path) -> rustix::io::Result<std::os::fd::OwnedFd> {
    rustix::fs::open(path, read_flags(), rustix::fs::Mode::empty())
}

/// `relative`'s text beneath `dir`, for the real config: its directory is
/// group-writable and root reads it, so the lookup refuses symlinks (a
/// link to a device would make root open it, and opening `/dev/watchdog`
/// arms it; a link to a root-only file would be read) and mount points (a
/// FUSE mount there could hang root's run), and must stay beneath `dir`:
/// `openat2` with `RESOLVE_NO_SYMLINKS | RESOLVE_NO_XDEV | RESOLVE_BENEATH`
/// (Linux 5.6, below every supported Ubuntu;
/// <https://docs.rs/rustix/1.1.6/rustix/fs/fn.openat2.html>). `dir` itself,
/// the appdata path, comes from the inventory and may be a link or mount.
fn read_beneath(dir: &Path, relative: &str) -> Result<Option<String>, ConfigError> {
    use rustix::fs::{Mode, OFlags, ResolveFlags};
    let dir_fd = match rustix::fs::open(
        dir,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(errno) => return Err(ConfigError::Read(errno.into())),
    };
    let resolve = ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_XDEV | ResolveFlags::BENEATH;
    match rustix::fs::openat2(&dir_fd, relative, read_flags(), Mode::empty(), resolve) {
        // What the resolve flags refuse (openat2(2): ELOOP, EXDEV), said
        // plainly: a config kept as a symlink would otherwise warn "Too
        // many levels of symbolic links" on every run.
        Err(rustix::io::Errno::LOOP | rustix::io::Errno::XDEV) => Err(ConfigError::Refused),
        opened => read_opened(opened, Some(MAX_FILE_BYTES)),
    }
}

/// `server_appdata_path` from `inventory`: an absolute value, cleaned; a
/// missing or unreadable file (read whole, at any length:
/// [`read_inventory`]), a file Ansible's loader refuses or a parse error
/// (a duplicate key included), or an empty or relative value all mean
/// `/opt`, as in sb-go. The YAML is what Ansible accepts
/// ([`inventory_appdata`]), as validate-config reads it, which reports the
/// refusals; unlike sb-go's yaml.v3, a second document and a tag Ansible's
/// loader has no constructor for (`!custom`) are refused, not ignored
/// (decisions 38 and 41). Like yaml.v3 it reads a scalar tagged `!unsafe`
/// or `!vault` as its string. Unlike yaml.v3, a merge
/// key (`<<`) isn't followed (saphyr has none); Saltbox's `host_vars` don't
/// use one. The inventory is Saltbox config, read with saphyr as the
/// capability map has it for Saltbox YAML
/// (<https://docs.rs/saphyr/0.3.3/saphyr/>); serde-saphyr would make sb
/// 741408 bytes bigger for this one key (ADR-004). Cleaning is std's
/// `Path::components` (repeated separators and `.` parts go:
/// <https://doc.rust-lang.org/std/path/struct.Path.html#method.components>);
/// unlike Go's `filepath.Clean` it keeps `..` (std's `normalize_lexically`
/// is unstable).
#[must_use]
pub fn appdata_path(inventory: &Path) -> PathBuf {
    read_inventory(inventory)
        .ok()
        .flatten()
        .and_then(|text| inventory_appdata(&text).ok().flatten())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map_or_else(|| PathBuf::from("/opt"), |path| path.components().collect())
}

/// The string `server_appdata_path` holds in the inventory's `text`, if
/// it holds one. The text goes through [`yaml::prepare`] first, as
/// validate-config's does, so the two refuse the same files: what
/// Ansible's loader refuses (decisions 28 and 38). Neither the read
/// ([`read_inventory`]) nor the YAML read limits its length, only depth
/// ([`yaml::MAX_DEPTH`]): a deep file is loaded on
/// [`yaml::with_stack`]'s thread, where its tree is dropped too. Public so
/// that sb-config's YAML corpus test can check that agreement.
///
/// # Errors
///
/// What Ansible's loader refuses, and saphyr's loader's own errors, with
/// the line and column in `text`; a deep file whose thread couldn't start,
/// at its start.
pub fn inventory_appdata(text: &str) -> Result<Option<String>, yaml::Refusal> {
    let prepared = yaml::prepare(text)?;
    let refusal = |err: &saphyr_parser::ScanError| {
        let marker = prepared.original(err.marker());
        yaml::Refusal {
            line: marker.line(),
            column: marker.col() + 1,
            message: err.info().to_string(),
        }
    };
    let load = || {
        let mut loader = saphyr::YamlLoader::<saphyr::YamlOwned>::default();
        // The first document (`prepare` has refused a second), its
        // scalars as libyaml reads them.
        prepared.load(&mut loader).map_err(|err| refusal(&err))?;
        if let Some(err) = loader.error() {
            return Err(refusal(err));
        }
        Ok(loader
            .into_documents()
            .into_iter()
            .next()
            .and_then(|document| {
                let value = document.as_mapping_get("server_appdata_path")?;
                value
                    .get_tagged_node()
                    .unwrap_or(value)
                    .as_str()
                    .map(str::to_string)
            }))
    };
    yaml::with_stack(prepared.depth(), load).unwrap_or_else(|err| {
        Err(yaml::Refusal {
            line: 1,
            column: 1,
            message: format!("can't start the YAML reader's thread: {err}"),
        })
    })
}

/// sb's config file under the appdata path.
#[must_use]
pub fn config_path(appdata: &Path) -> PathBuf {
    appdata.join(CONFIG_FILE)
}

/// What the theme file chose: the palette (a built-in theme with
/// overrides) and an optional colour profile (the environment wins).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThemeChoice {
    pub palette: Palette,
    pub color_profile: Option<Profile>,
}

/// Why the theme file couldn't be used. Every message is one line with any
/// control characters escaped: the file is group-writable (0664), and the
/// message reaches a terminal and the run log.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("can't read it: {0}")]
    Read(#[source] std::io::Error),
    #[error("isn't a regular file")]
    NotAFile,
    #[error("is reached through a symbolic link or another mount (refused: root reads this file)")]
    Refused,
    #[error("is bigger than {0} bytes")]
    TooBig(usize),
    #[error("isn't UTF-8")]
    NotUtf8,
    #[error("isn't a valid sb config{position}: {message}")]
    Parse { position: String, message: String },
    #[error("unknown theme {0:?} (built in: \"default\")")]
    UnknownTheme(String),
    #[error("unknown colour role {0:?}")]
    UnknownRole(String),
    #[error("{role}: {value:?} isn't a #RRGGBB colour")]
    BadColour { role: String, value: String },
    #[error("unknown colour profile {0:?} (truecolor, ansi256, ansi, ascii)")]
    UnknownProfile(String),
}

/// `text` on one line with control characters, invisible and direction
/// marks (which can reorder a line on screen) escaped: std's
/// `str::escape_debug`, which escapes every non-printable character
/// (<https://doc.rust-lang.org/std/primitive.str.html#method.escape_debug>);
/// it also escapes quotes and backslashes.
#[must_use]
pub fn escape_controls(text: &str) -> String {
    text.escape_debug().to_string()
}

/// A toml error as one line: toml's own message (`Error::message`) and
/// the line and column of its span, not its Display, which quotes the
/// offending line verbatim
/// (<https://docs.rs/toml/1.1.8/toml/de/struct.Error.html>).
fn parse_error(text: &str, err: &toml::de::Error) -> ConfigError {
    let position = err.span().map_or_else(String::new, |span| {
        let before = &text[..text.floor_char_boundary(span.start.min(text.len()))];
        let line = before.matches('\n').count() + 1;
        let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
        format!(" (line {line}, column {column})")
    });
    ConfigError::Parse {
        position,
        message: escape_controls(err.message()),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    theme: ThemeSection,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeSection {
    name: Option<String>,
    color_profile: Option<String>,
    #[serde(default)]
    colors: BTreeMap<String, String>,
}

/// The theme the config at `config` chooses (an `SB_CONFIG` path); no
/// file means the built-in theme.
///
/// # Errors
///
/// A [`ConfigError`] when the file can't be read (see [`read_small`]),
/// isn't valid TOML, or names an unknown theme, colour role or profile, or
/// a colour that isn't #RRGGBB.
pub fn load_theme(config: &Path) -> Result<ThemeChoice, ConfigError> {
    parse_theme(read_small(config)?)
}

/// The theme `<appdata>/sb/config.toml` chooses, read beneath `appdata`
/// (see [`read_beneath`]); no file means the built-in theme.
///
/// # Errors
///
/// As [`load_theme`], and [`ConfigError::Refused`] when the file is reached
/// through a symbolic link or another mount.
pub fn load_theme_in(appdata: &Path) -> Result<ThemeChoice, ConfigError> {
    parse_theme(read_beneath(appdata, CONFIG_FILE)?)
}

fn parse_theme(text: Option<String>) -> Result<ThemeChoice, ConfigError> {
    let Some(text) = text else {
        return Ok(ThemeChoice::default());
    };
    let file: File = toml::from_str(&text).map_err(|err| parse_error(&text, &err))?;
    let mut palette = match file.theme.name.as_deref() {
        None | Some("default") => Palette::default(),
        Some(other) => return Err(ConfigError::UnknownTheme(other.to_string())),
    };
    for (name, value) in file.theme.colors {
        let role = Role::from_name(&name).ok_or_else(|| ConfigError::UnknownRole(name.clone()))?;
        let colour = hex_colour(&value).ok_or(ConfigError::BadColour { role: name, value })?;
        palette.set(role, colour);
    }
    let color_profile = file
        .theme
        .color_profile
        .map(|name| Profile::from_name(&name).ok_or(ConfigError::UnknownProfile(name)))
        .transpose()?;
    Ok(ThemeChoice {
        palette,
        color_profile,
    })
}

/// `#RRGGBB`. Why not a colour crate: this one format is all the theme
/// file takes; anstyle has no parser for it.
fn hex_colour(value: &str) -> Option<anstyle::RgbColor> {
    let digits = value.strip_prefix('#')?;
    if digits.len() != 6 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let [_, r, g, b] = u32::from_str_radix(digits, 16).ok()?.to_be_bytes();
    Some(anstyle::RgbColor(r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Saltbox root: `SB_SALTBOX_DIR` when set and not empty, else
    /// `/srv/git/saltbox`; the inventory lives under it.
    #[test]
    fn the_saltbox_root_follows_sb_saltbox_dir() {
        assert_eq!(saltbox_dir(None), Path::new("/srv/git/saltbox"));
        assert_eq!(saltbox_dir(Some("".into())), Path::new("/srv/git/saltbox"));
        assert_eq!(saltbox_dir(Some("/tmp/sbx".into())), Path::new("/tmp/sbx"));
        assert_eq!(
            inventory_path(Path::new("/tmp/sbx")),
            Path::new("/tmp/sbx/inventories/host_vars/localhost.yml")
        );
    }

    /// `read`'s result, or a failure (not a hung test) when it blocks: a
    /// FIFO opened without `NONBLOCK` never returns.
    fn within_seconds<T: Send + 'static>(read: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(read()));
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("the read blocked")
    }

    fn file(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, text).expect("write");
        path
    }

    /// sb-go's `loadServerAppdataPath`: the inventory's absolute
    /// `server_appdata_path`, cleaned; anything else means `/opt`.
    #[test]
    fn the_appdata_path_follows_sb_go() {
        let dir = tempfile::tempdir().expect("temp dir");
        let case = |text: &str| appdata_path(&file(dir.path(), "localhost.yml", text));
        assert_eq!(
            case("server_appdata_path: /data/appdata\n"),
            PathBuf::from("/data/appdata")
        );
        assert_eq!(
            case("server_appdata_path: /data//appdata/./\n"),
            PathBuf::from("/data/appdata")
        );
        assert_eq!(
            case("server_appdata_path: relative/path\n"),
            PathBuf::from("/opt")
        );
        assert_eq!(case("server_appdata_path: \"\"\n"), PathBuf::from("/opt"));
        assert_eq!(case("other: 1\n"), PathBuf::from("/opt"));
        assert_eq!(
            case("server_appdata_path: [not, a, string\n"),
            PathBuf::from("/opt")
        );
        assert_eq!(
            appdata_path(&dir.path().join("missing.yml")),
            PathBuf::from("/opt")
        );
    }

    /// yaml.v3 refuses a mapping with a duplicate key, so sb-go falls back
    /// to `/opt`. A second document, which sb-go ignored, is refused as
    /// Ansible's loader refuses it (decision 38): `/opt` too.
    #[test]
    fn duplicate_keys_and_extra_documents_mean_opt() {
        let dir = tempfile::tempdir().expect("temp dir");
        let case = |text: &str| appdata_path(&file(dir.path(), "localhost.yml", text));
        assert_eq!(
            case("server_appdata_path: /a\nserver_appdata_path: /b\n"),
            PathBuf::from("/opt")
        );
        for text in [
            "server_appdata_path: /a\n---\nserver_appdata_path: /b\n",
            "server_appdata_path: /a\n---\n[unclosed\n",
            "  server_appdata_path: /a\nx: 1\n",
        ] {
            assert_eq!(case(text), PathBuf::from("/opt"), "{text:?}");
        }
        assert_eq!(
            case("server_appdata_path: /a\n...\n# c\n"),
            PathBuf::from("/a")
        );
        // A tag Ansible's loader has no constructor for is refused
        // (decision 41); its own `!unsafe` and `!vault` are read, as
        // yaml.v3 decodes them, into the string.
        assert_eq!(
            case("server_appdata_path: !custom /srv\n"),
            PathBuf::from("/opt")
        );
        for text in [
            "server_appdata_path: !unsafe /srv\n",
            "server_appdata_path: !vault /srv\n",
        ] {
            assert_eq!(case(text), PathBuf::from("/srv"), "{text:?}");
        }
        // A block scalar at the end without a final line break has none,
        // as in libyaml and yaml.v3.
        assert_eq!(
            inventory_appdata("server_appdata_path: |\n  /srv").expect("reads"),
            Some("/srv".to_string())
        );
    }

    /// The inventory read refuses what Ansible's loader refuses at the line
    /// and column validate-config gives, saphyr's loader errors included.
    #[test]
    fn the_inventory_read_says_where_it_refuses() {
        let at = |text: &str| {
            let refusal = inventory_appdata(text).expect_err(text);
            (refusal.line, refusal.column)
        };
        assert_eq!(at("a: 1\n---\nb: 2\n"), (2, 1));
        assert_eq!(at("a: 1\n: x\n"), (2, 1));
        assert_eq!(at("a: [\n  1,\n  2\n"), (4, 1));
        assert_eq!(at("a: *nowhere\n"), (1, 4));
        assert_eq!(
            inventory_appdata("server_appdata_path: [\n  1]\n"),
            Ok(None)
        );
    }

    /// Depth is limited, never length: a 40 KB `- - - …` inventory, which
    /// overflowed the stack before, is read on the deep stack, and one
    /// past `MAX_DEPTH` is refused where it goes too deep.
    #[test]
    fn a_deep_inventory_is_read_or_refused_without_overflow() {
        let deep = |n: usize| format!("x:\n  {}v\nserver_appdata_path: /data\n", "- ".repeat(n));
        assert_eq!(
            inventory_appdata(&deep(20_000)),
            Ok(Some("/data".to_string()))
        );
        assert_eq!(
            inventory_appdata(&deep(yaml::MAX_DEPTH - 1)),
            Ok(Some("/data".to_string()))
        );
        let refusal = inventory_appdata(&deep(yaml::MAX_DEPTH)).expect_err("too deep");
        assert_eq!(
            (refusal.line, refusal.column),
            (2, 3 + 2 * (yaml::MAX_DEPTH - 1))
        );
        assert!(refusal.message.starts_with("nested deeper than"));
        let dir = tempfile::tempdir().expect("temp dir");
        assert_eq!(
            appdata_path(&file(dir.path(), "localhost.yml", &deep(20_000))),
            PathBuf::from("/data")
        );
    }

    /// The inventory is read as Ansible reads it (decision 28): a layout
    /// libyaml accepts and saphyr alone refuses keeps its appdata path,
    /// and a tab libyaml refuses means `/opt`, as a parse error does.
    #[test]
    fn the_inventory_is_read_as_ansible_reads_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let case = |text: &str| appdata_path(&file(dir.path(), "localhost.yml", text));
        assert_eq!(
            case("domains: [\n  \"a\",\n]\nserver_appdata_path: /data\n"),
            PathBuf::from("/data")
        );
        assert_eq!(
            case("server_appdata_path:\t/data\n"),
            PathBuf::from("/data")
        );
        assert_eq!(
            case("server_appdata_path: /data\n\t\nx: 1\n"),
            PathBuf::from("/opt")
        );
    }

    #[test]
    fn the_config_lives_in_sb_under_appdata() {
        assert_eq!(
            config_path(Path::new("/opt")),
            PathBuf::from("/opt/sb/config.toml")
        );
        assert_eq!(
            config_path(Path::new("/opt")),
            Path::new("/opt").join(CONFIG_FILE)
        );
    }

    /// No file means the built-in theme, with no profile set.
    #[test]
    fn no_file_means_the_built_in_theme() {
        let dir = tempfile::tempdir().expect("temp dir");
        let choice = load_theme(&dir.path().join("config.toml")).expect("no file is fine");
        assert_eq!(choice, ThemeChoice::default());
        assert_eq!(choice.palette, Palette::default());
        assert_eq!(choice.color_profile, None);
    }

    /// A named built-in theme plus per-role overrides, and a profile.
    #[test]
    fn a_theme_names_a_built_in_and_overrides_roles() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = file(
            dir.path(),
            "config.toml",
            "[theme]\nname = \"default\"\ncolor_profile = \"ansi256\"\n\n[theme.colors]\nproblem = \"#ff0000\"\nok = \"#00FF00\"\n",
        );
        let choice = load_theme(&path).expect("valid");
        assert_eq!(
            choice.palette.get(Role::Problem),
            anstyle::RgbColor(0xFF, 0, 0)
        );
        assert_eq!(choice.palette.get(Role::Ok), anstyle::RgbColor(0, 0xFF, 0));
        assert_eq!(
            choice.palette.get(Role::Muted),
            Palette::default().get(Role::Muted)
        );
        assert_eq!(choice.color_profile, Some(crate::theme::Profile::Ansi256));
    }

    /// Mistakes are errors with the reason; the caller warns and falls back.
    #[test]
    fn mistakes_are_errors_that_say_what() {
        let dir = tempfile::tempdir().expect("temp dir");
        let error = |text: &str| {
            load_theme(&file(dir.path(), "config.toml", text))
                .expect_err("invalid")
                .to_string()
        };
        assert!(error("[theme]\nname = \"solarized\"\n").contains("unknown theme \"solarized\""));
        assert!(
            error("[theme.colors]\nproblme = \"#ff0000\"\n")
                .contains("unknown colour role \"problme\"")
        );
        assert!(
            error("[theme.colors]\nproblem = \"red\"\n").contains("\"red\" isn't a #RRGGBB colour")
        );
        assert!(error("[theme.colors]\nproblem = \"#ff00\"\n").contains("isn't a #RRGGBB colour"));
        assert!(error("[thme]\n").contains("unknown field"));
        assert!(error("[theme\n").contains("isn't a valid sb config"));
        assert!(
            error("[theme]\ncolor_profile = \"bogus\"\n")
                .contains("unknown colour profile \"bogus\"")
        );
    }

    /// A config that exists but isn't a file (here a directory) is an
    /// error, not "no file".
    #[test]
    fn a_directory_config_is_an_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        let err = load_theme(dir.path()).expect_err("a directory");
        assert!(err.to_string().contains("isn't a regular file"), "{err}");
    }

    /// A path that can't be opened for another reason than "missing" (here
    /// it runs through a file) is an error too.
    #[test]
    fn a_config_that_cant_be_opened_is_an_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        let not_a_dir = file(dir.path(), "plain", "");
        let err = load_theme(&not_a_dir.join("config.toml")).expect_err("ENOTDIR");
        assert!(err.to_string().contains("can't read it"), "{err}");
    }

    /// A FIFO (or device) in the config's place must not hang sb: every
    /// command reads it at startup, and the directory is group-writable.
    #[test]
    fn a_fifo_is_refused_without_hanging() {
        let dir = tempfile::tempdir().expect("temp dir");
        let fifo = dir.path().join("config.toml");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )
        .expect("mkfifo");
        let path = fifo.clone();
        let err = within_seconds(move || load_theme(&path)).expect_err("not a regular file");
        assert!(err.to_string().contains("isn't a regular file"), "{err}");
        assert_eq!(
            within_seconds(move || appdata_path(&fifo)),
            PathBuf::from("/opt")
        );
    }

    /// Content toml does echo back (a quoted key's escapes) is escaped, and
    /// so is content in the other messages.
    #[test]
    fn echoed_content_is_escaped() {
        let dir = tempfile::tempdir().expect("temp dir");
        let key = file(
            dir.path(),
            "key.toml",
            "[theme]\n\"a\\u001b]0;pwn\\u0007\" = 1\n",
        );
        let message = load_theme(&key).expect_err("unknown key").to_string();
        assert!(!message.chars().any(char::is_control), "{message:?}");
        assert!(
            message.contains("\\u{1b}") && message.contains("line 2, column 1"),
            "{message:?}"
        );
        let value = file(
            dir.path(),
            "value.toml",
            "[theme]\nname = \"\\u001b[31m\"\n",
        );
        let message = load_theme(&value).expect_err("unknown theme").to_string();
        assert!(!message.chars().any(char::is_control), "{message:?}");
    }

    /// The real config is opened beneath the appdata path, refusing
    /// symlinks: a link to a device must not make root open the device
    /// (opening `/dev/watchdog` arms it), and a link to a root-only file
    /// must not be read.
    #[test]
    fn the_config_beneath_appdata_refuses_symlinks() {
        let appdata = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(appdata.path().join("sb")).expect("mkdir");
        std::os::unix::fs::symlink("/dev/null", appdata.path().join("sb/config.toml"))
            .expect("symlink");
        let err = load_theme_in(appdata.path()).expect_err("a symlink");
        assert_eq!(
            err.to_string(),
            "is reached through a symbolic link or another mount (refused: root reads this file)"
        );
    }

    #[test]
    fn a_symlinked_sb_directory_is_refused() {
        let appdata = tempfile::tempdir().expect("temp dir");
        let elsewhere = tempfile::tempdir().expect("temp dir");
        file(elsewhere.path(), "config.toml", "[theme]\n");
        std::os::unix::fs::symlink(elsewhere.path(), appdata.path().join("sb")).expect("symlink");
        assert!(matches!(
            load_theme_in(appdata.path()),
            Err(ConfigError::Refused)
        ));
    }

    /// A link that stays beneath appdata is refused too (`BENEATH` alone
    /// would follow it).
    #[test]
    fn a_relative_symlink_beneath_appdata_is_refused() {
        let appdata = tempfile::tempdir().expect("temp dir");
        let sb = appdata.path().join("sb");
        std::fs::create_dir(&sb).expect("mkdir");
        file(&sb, "real.toml", "[theme]\n");
        std::os::unix::fs::symlink("real.toml", sb.join("config.toml")).expect("symlink");
        assert!(matches!(
            load_theme_in(appdata.path()),
            Err(ConfigError::Refused)
        ));
    }

    /// A mount point on the way is refused (a FUSE mount could hang root's
    /// run). `/proc` is its own mount on every Linux host; `proc/version`
    /// is a plain file (`proc/self` would be refused as a link).
    #[test]
    fn crossing_a_mount_is_refused() {
        assert!(matches!(
            read_beneath(Path::new("/"), "proc/version"),
            Err(ConfigError::Refused)
        ));
    }

    /// An appdata path that isn't a directory can't be read beneath.
    #[test]
    fn an_appdata_file_is_an_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        let not_a_dir = file(dir.path(), "appdata", "");
        let err = load_theme_in(&not_a_dir).expect_err("not a directory");
        assert!(err.to_string().contains("can't read it"), "{err}");
    }

    /// The root path (openat2) opens a FIFO without blocking and refuses
    /// it, as the `SB_CONFIG` path does.
    #[test]
    fn a_fifo_beneath_appdata_is_refused_without_hanging() {
        let appdata = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(appdata.path().join("sb")).expect("mkdir");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            appdata.path().join(CONFIG_FILE),
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )
        .expect("mkfifo");
        let path = appdata.path().to_path_buf();
        assert!(matches!(
            within_seconds(move || load_theme_in(&path)),
            Err(ConfigError::NotAFile)
        ));
    }

    /// The normal cases: a real file beneath appdata, and none at all.
    #[test]
    fn the_config_beneath_appdata_is_read() {
        let appdata = tempfile::tempdir().expect("temp dir");
        assert_eq!(
            load_theme_in(appdata.path()).expect("no file"),
            ThemeChoice::default()
        );
        std::fs::create_dir(appdata.path().join("sb")).expect("mkdir");
        file(
            &appdata.path().join("sb"),
            "config.toml",
            "[theme.colors]\nok = \"#000001\"\n",
        );
        let choice = load_theme_in(appdata.path()).expect("valid");
        assert_eq!(choice.palette.get(Role::Ok), anstyle::RgbColor(0, 0, 1));
    }

    /// Invisible direction marks could reorder a warning line on screen.
    #[test]
    fn direction_marks_are_escaped() {
        assert_eq!(
            escape_controls("a\u{202e}b\u{2066}c"),
            "a\\u{202e}b\\u{2066}c"
        );
        // Marks the first hand-written list missed: Arabic letter mark,
        // zero-width space, byte order mark.
        let marks = escape_controls("\u{61c}\u{200b}\u{feff}");
        assert!(
            !marks.contains(['\u{61c}', '\u{200b}', '\u{feff}']),
            "{marks:?}"
        );
    }

    /// One byte over the limit is refused.
    #[test]
    fn one_byte_over_the_limit_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let comment = format!("#{}\n", "x".repeat(MAX_FILE_BYTES - 1));
        assert_eq!(comment.len(), MAX_FILE_BYTES + 1);
        assert!(load_theme(&file(dir.path(), "config.toml", &comment)).is_err());
    }

    /// Bytes that aren't UTF-8 say so, rather than reading like a
    /// permissions problem.
    #[test]
    fn non_utf8_says_so() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, b"[theme]\nname = \"\xff\"\n").expect("write");
        let message = load_theme(&path).expect_err("not UTF-8").to_string();
        assert!(message.contains("isn't UTF-8"), "{message}");
    }

    /// The limit is a size, not a cut: a file of exactly the limit is read.
    #[test]
    fn a_file_of_exactly_the_limit_is_read() {
        let dir = tempfile::tempdir().expect("temp dir");
        let comment = format!("#{}\n", "x".repeat(MAX_FILE_BYTES - 2));
        assert_eq!(comment.len(), MAX_FILE_BYTES);
        assert!(load_theme(&file(dir.path(), "config.toml", &comment)).is_ok());
    }

    /// The position is the line and column (1-based) where toml found the
    /// mistake.
    #[test]
    fn the_position_is_exact() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = file(
            dir.path(),
            "config.toml",
            "[theme]\nname = \"default\"\n  bogus = 1\n",
        );
        let message = load_theme(&path).expect_err("unknown key").to_string();
        assert!(message.contains("(line 3, column 3)"), "{message}");
    }

    /// The file is 0664: whatever it holds must not reach a terminal or the
    /// log as control characters. Errors are one line, with the position.
    #[test]
    fn errors_are_one_line_without_control_characters() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = file(
            dir.path(),
            "config.toml",
            "[theme]\nx\x1b[31m = 1\n# \x1b]0;pwn\x07\n",
        );
        let message = load_theme(&path).expect_err("invalid").to_string();
        assert!(!message.chars().any(char::is_control), "{message:?}");
        assert!(message.contains("line 2"), "{message:?}");
    }

    /// Bigger than any real file: invalid, not read whole.
    #[test]
    fn an_oversized_file_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let big = format!("# {}\n", "x".repeat(MAX_FILE_BYTES));
        let message = load_theme(&file(dir.path(), "config.toml", &big))
            .expect_err("too big")
            .to_string();
        assert!(message.contains("bigger than"), "{message}");
    }

    /// The inventory has no length cap (decision 41): one well over the
    /// cap is read whole, its last key included.
    #[test]
    fn the_inventory_has_no_length_cap() {
        use std::fmt::Write as _;
        let dir = tempfile::tempdir().expect("temp dir");
        let mut long = String::new();
        for i in 0..200_000 {
            writeln!(long, "key_{i}: value").expect("a String");
        }
        assert!(long.len() > 3 * MAX_FILE_BYTES, "{}", long.len());
        let inventory = file(
            dir.path(),
            "localhost.yml",
            &format!("{long}server_appdata_path: /data\n"),
        );
        assert!(read_inventory(&inventory).expect("read").is_some());
        assert!(read_small(&inventory).is_err());
        assert_eq!(appdata_path(&inventory), PathBuf::from("/data"));
    }
}
