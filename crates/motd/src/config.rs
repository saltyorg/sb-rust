//! motd.yml (motd spec, Command flow step 4; decisions 3, 4, 5, 17 and
//! 18; Differences 2, 3, 4 and 19): the file read and checked once
//! through sb-config's `check_file`, the one per-file check `sb edit` and
//! validate-config use, and its tree mapped to motd's typed model. A file
//! with issues, or one that can't be read or checked, gives one `Config:`
//! line and no model, so only the config-driven parts are skipped.
//! `--generate-config` writes sb-go's example file from Saltbox's schema.
//!
//! Library-backed: the read, the YAML parse, the schema and its rules,
//! the issues and their secret-safe wording are sb-config's; the colours
//! are resolved by cli-core's `Profile::resolve`. motd's own code is the
//! mapping from the checked tree (domain logic: Saltbox's motd.yml
//! layout), the `#rgb` form, and sb-go's example writer.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

use anstyle::RgbColor;
use sb_cli_core::theme::Role;
use sb_config::http::{Http, Proxy};
use sb_config::rules::go_float;
use sb_config::run::{self, Checked, FileError, Settings};
use sb_config::scalar::Value;
use sb_config::schema::{self, Kind, Rule, SchemaError};
use sb_config::tree::{Data, Node};
use sb_config::validators::Saltbox;

/// motd.yml under the Saltbox root, and its schema (sb-config's list).
const FILE: run::File = run::FILES[5];

/// One of motd.yml's nine colours (motd decision 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Label,
    Value,
    AppName,
    Warning,
    Success,
    Error,
    Low,
    High,
    Critical,
}

impl Slot {
    /// Every slot, in motd.yml's order.
    pub const ALL: [Slot; 9] = [
        Slot::Label,
        Slot::Value,
        Slot::AppName,
        Slot::Warning,
        Slot::Success,
        Slot::Error,
        Slot::Low,
        Slot::High,
        Slot::Critical,
    ];

    /// The role sb's palette gives the slot when motd.yml doesn't set it:
    /// Accent, Text, Accent2, Warn, Ok, Problem, Ok, Warn, Problem.
    #[must_use]
    pub fn role(self) -> Role {
        match self {
            Slot::Label => Role::Accent,
            Slot::Value => Role::Text,
            Slot::AppName => Role::Accent2,
            Slot::Warning | Slot::High => Role::Warn,
            Slot::Success | Slot::Low => Role::Ok,
            Slot::Error | Slot::Critical => Role::Problem,
        }
    }

    /// Its group and key under `colors` (sb-go's `MOTDColors`).
    fn key(self) -> (&'static str, &'static str) {
        match self {
            Slot::Label => ("text", "label"),
            Slot::Value => ("text", "value"),
            Slot::AppName => ("text", "app_name"),
            Slot::Warning => ("status", "warning"),
            Slot::Success => ("status", "success"),
            Slot::Error => ("status", "error"),
            Slot::Low => ("progress_bar", "low"),
            Slot::High => ("progress_bar", "high"),
            Slot::Critical => ("progress_bar", "critical"),
        }
    }

    /// Its place in [`Slot::ALL`], which is its declaration order.
    pub(crate) fn index(self) -> usize {
        self as usize
    }
}

/// motd.yml's colours: each slot's, where the file sets one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SlotColours([Option<RgbColor>; 9]);

impl SlotColours {
    /// The slot's colour, if motd.yml sets it.
    #[must_use]
    pub fn get(&self, slot: Slot) -> Option<RgbColor> {
        self.0[slot.index()]
    }

    /// `colors` read from the file: a key that is absent, empty or not a
    /// hex colour leaves its slot to sb's palette (sb-go keeps its
    /// default for an empty one).
    fn from_node(node: Option<&Node>) -> Self {
        let mut colours = SlotColours::default();
        for slot in Slot::ALL {
            let (group, key) = slot.key();
            colours.0[slot.index()] = node
                .and_then(|node| node.get(group))
                .and_then(|group| group.get(key))
                .and_then(text)
                .and_then(|value| hex(&value));
        }
        colours
    }
}

/// `#rrggbb`, or `#rgb` as `#rrggbb` (the schema's pattern; motd
/// decision 5). Why not cli-core's theme-file parser: it takes the six
/// digit form only, and is private to the theme file.
fn hex(value: &str) -> Option<RgbColor> {
    let digits = value.strip_prefix('#')?;
    if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let digits = match digits.len() {
        6 => digits.to_string(),
        3 => digits.chars().flat_map(|c| [c, c]).collect(),
        _ => return None,
    };
    let [_, r, g, b] = u32::from_str_radix(&digits, 16).ok()?.to_be_bytes();
    Some(RgbColor(r, g, b))
}

/// The HTTP sections motd.yml configures, in sb-go's `MOTDConfig` order of
/// keys sorted (the generator's order).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum App {
    Emby,
    Jellyfin,
    Lidarr,
    Nzbget,
    Plex,
    Qbittorrent,
    Radarr,
    Readarr,
    Rtorrent,
    Sabnzbd,
    Sonarr,
}

impl App {
    /// Every app.
    pub const ALL: [App; 11] = [
        App::Emby,
        App::Jellyfin,
        App::Lidarr,
        App::Nzbget,
        App::Plex,
        App::Qbittorrent,
        App::Radarr,
        App::Readarr,
        App::Rtorrent,
        App::Sabnzbd,
        App::Sonarr,
    ];

    /// Its key in motd.yml.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            App::Emby => "emby",
            App::Jellyfin => "jellyfin",
            App::Lidarr => "lidarr",
            App::Nzbget => "nzbget",
            App::Plex => "plex",
            App::Qbittorrent => "qbittorrent",
            App::Radarr => "radarr",
            App::Readarr => "readarr",
            App::Rtorrent => "rtorrent",
            App::Sabnzbd => "sabnzbd",
            App::Sonarr => "sonarr",
        }
    }
}

/// One app's section: `enabled` (absent means on, as sb-go's
/// `IsEnabled`) and its instances.
#[derive(Clone, Debug, PartialEq)]
pub struct AppSection {
    pub enabled: bool,
    pub instances: Vec<Instance>,
}

/// One instance. The credentials are whichever the app takes (`apikey`,
/// `token`, or `user` and `password`); its debug output names none.
#[derive(Clone, PartialEq)]
pub struct Instance {
    pub name: String,
    pub url: String,
    pub apikey: Option<String>,
    pub token: Option<String>,
    pub user: Option<String>,
    pub password: Option<String>,
    /// A `timeout` above zero, in seconds, fractions allowed
    /// (Difference 19); none for the default.
    pub timeout: Option<Duration>,
    pub enabled: bool,
}

/// The name, URL and timeout; the credentials only as whether they're set.
impl std::fmt::Debug for Instance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Instance")
            .field("name", &self.name)
            .field(
                "url",
                &sb_config::http::loggable_url(
                    self.url.split(['?', '#']).next().unwrap_or_default(),
                ),
            )
            .field("apikey", &self.apikey.is_some())
            .field("token", &self.token.is_some())
            .field("user", &self.user.is_some())
            .field("password", &self.password.is_some())
            .field("timeout", &self.timeout)
            .field("enabled", &self.enabled)
            .finish()
    }
}

/// `systemd`: Services' additions and display names (sb-go's
/// `SystemdConfig`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Systemd {
    pub enabled: bool,
    pub additional_services: Vec<String>,
    pub display_names: BTreeMap<String, String>,
}

/// A checked motd.yml, as motd uses it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Motd {
    pub colours: SlotColours,
    /// The apps configured as a section; an empty list (`radarr: []`,
    /// which validate-config passes, as sb-go does) is no section
    /// (decision 4).
    pub apps: BTreeMap<App, AppSection>,
    pub systemd: Option<Systemd>,
}

impl Motd {
    /// The model of a checked tree with no issues.
    #[must_use]
    pub fn from_tree(root: &Node) -> Self {
        let apps = App::ALL
            .into_iter()
            .filter_map(|app| {
                let section = root.get(app.key())?;
                matches!(section.data, Data::Mapping(_)).then(|| {
                    let instances = match section.get("instances").map(|list| &list.data) {
                        Some(Data::Sequence(items)) => items.iter().map(instance).collect(),
                        _ => Vec::new(),
                    };
                    let section = AppSection {
                        enabled: flag(section.get("enabled")),
                        instances,
                    };
                    (app, section)
                })
            })
            .collect();
        let systemd = root
            .get("systemd")
            .filter(|node| matches!(node.data, Data::Mapping(_)))
            .map(|node| Systemd {
                enabled: flag(node.get("enabled")),
                additional_services: match node.get("additional_services").map(|n| &n.data) {
                    Some(Data::Sequence(items)) => items.iter().filter_map(text).collect(),
                    _ => Vec::new(),
                },
                display_names: match node.get("display_names").map(|n| &n.data) {
                    Some(Data::Mapping(entries)) => entries
                        .iter()
                        .filter_map(|(key, value)| Some((key.text.clone(), text(value)?)))
                        .collect(),
                    _ => BTreeMap::new(),
                },
            });
        Motd {
            colours: SlotColours::from_node(root.get("colors")),
            apps,
            systemd,
        }
    }
}

/// One item of `instances`; a field of another type reads as absent.
fn instance(node: &Node) -> Instance {
    let field = |key| node.get(key).and_then(text);
    Instance {
        name: field("name").unwrap_or_default(),
        url: field("url").unwrap_or_default(),
        apikey: field("apikey"),
        token: field("token"),
        user: field("user"),
        password: field("password"),
        timeout: node.get("timeout").and_then(seconds),
        enabled: flag(node.get("enabled")),
    }
}

/// A scalar's text: strings as they are and whole numbers as digits;
/// other scalars, null and collections are none.
fn text(node: &Node) -> Option<String> {
    match &node.data {
        Data::Scalar(Value::Str(text) | Value::Timestamp(text)) => Some(text.clone()),
        Data::Scalar(Value::Int(number)) => Some(number.to_string()),
        _ => None,
    }
}

/// `enabled`: on unless it is `false` (sb-go's `IsEnabled`).
fn flag(node: Option<&Node>) -> bool {
    !matches!(
        node.map(|node| &node.data),
        Some(Data::Scalar(Value::Bool(false)))
    )
}

/// A `timeout` above zero, as a duration (sb-go took only whole seconds
/// above zero; Difference 19 takes fractions).
fn seconds(node: &Node) -> Option<Duration> {
    let seconds = match node.data {
        Data::Scalar(Value::Int(number)) => u32::try_from(number).ok().map(f64::from)?,
        Data::Scalar(Value::Float(number)) => number,
        _ => return None,
    };
    (seconds > 0.0)
        .then(|| Duration::try_from_secs_f64(seconds).ok())
        .flatten()
}

/// motd.yml as this run uses it: the model, or the `Config:` line's text
/// when the file has issues or can't be read or checked (decision 3);
/// neither when there is no file.
#[derive(Debug, Default)]
pub struct Loaded {
    pub motd: Option<Motd>,
    pub line: Option<String>,
}

/// The `Config:` line's text for a file with `count` issues
/// (Difference 2).
fn issues_line(count: usize) -> String {
    let issues = if count == 1 { "issue" } else { "issues" };
    format!("motd.yml has {count} {issues}; run sb validate-config")
}

/// The line for a file that can't be read.
const UNREADABLE: &str = "motd.yml can't be read; run sb validate-config";

/// The line for a schema that can't be loaded.
const UNCHECKED: &str = "motd.yml's schema can't be loaded; run sb validate-config";

/// motd.yml read and checked once (decision 17), the issues and causes
/// logged: each issue with its position, in sb-config's wording, which
/// never shows a credential's value.
#[must_use]
pub fn load(settings: &Settings) -> Loaded {
    loaded(run::check_file(settings, FILE))
}

/// [`load`] with the run's settings: the Saltbox root from
/// `SB_SALTBOX_DIR` and the zoneinfo places from `ZONEINFO`. The check
/// makes no request, so its client reads no proxy setting; the HTTP
/// sections get their own (M44). A client that can't be built leaves the
/// file unread.
#[must_use]
pub fn load_here() -> Loaded {
    let http = match Http::new(&Proxy::Off, Duration::from_secs(1)) {
        Ok(http) => http,
        Err(err) => {
            tracing::warn!(error = %err, "motd.yml not read: no HTTP client");
            return Loaded {
                motd: None,
                line: Some(UNREADABLE.to_string()),
            };
        }
    };
    load(&Settings {
        root: sb_cli_core::config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR")),
        zoneinfo: Saltbox::zoneinfo_places(std::env::var_os("ZONEINFO")),
        http,
        // No request is made: `check_file` runs no credential check.
        cloudflare_url: String::new(),
        dockerhub_url: String::new(),
        backoff: sb_config::cloudflare::backoff,
    })
}

/// The outcome of the check, as this run uses it.
fn loaded(checked: Result<Checked, FileError>) -> Loaded {
    match checked {
        Ok(Checked { report, tree }) if report.issues.is_empty() => Loaded {
            motd: tree.as_deref().map(Motd::from_tree),
            line: None,
        },
        Ok(Checked { report, .. }) => {
            for issue in &report.issues {
                tracing::warn!(issue = %issue, "motd.yml issue");
            }
            Loaded {
                motd: None,
                line: Some(issues_line(report.issues.len())),
            }
        }
        Err(FileError::Skipped(_)) => {
            tracing::info!("motd.yml not found; its sections are off");
            Loaded::default()
        }
        Err(err) => {
            tracing::warn!(error = %err, "motd.yml not checked");
            let line = match err {
                FileError::Schema(_) => UNCHECKED,
                _ => UNREADABLE,
            };
            Loaded {
                motd: None,
                line: Some(line.to_string()),
            }
        }
    }
}

/// sb-go's section order in the example (`motd/config.go`, `sectionOrder`).
const ORDER: [&str; 13] = [
    "colors",
    "emby",
    "jellyfin",
    "lidarr",
    "nzbget",
    "plex",
    "qbittorrent",
    "radarr",
    "readarr",
    "rtorrent",
    "sabnzbd",
    "sonarr",
    "systemd",
];

/// sb-go's colour block, its charmtone defaults written as sb-go writes
/// them (`motd/config.go:157-185`, `motd/styles.go`), so the printed file
/// looks as it does today.
const COLOURS: &str = r##"colors:
  text:
    label: "#FF79D0"
    value: "#12C78F"
    app_name: "#BF976F"
  status:
    warning: "#F5EF34"
    success: "#12C78F"
    error: "#EB4268"
  progress_bar:
    low: "#A8CC8C"
    high: "#DBAB79"
    critical: "#E88388"
"##;

/// `--generate-config`: sb-go's `GenerateExampleConfig` on
/// `<root>/schema/motd.schema.yml`, loaded strictly by sb-config
/// (decision 18): each section in sb-go's order, its description as
/// comments, the schema's examples, one example instance per
/// `instances` list.
///
/// # Errors
///
/// The schema doesn't load: sb-config's error, naming the file and the
/// cause.
pub fn generate(root: &Path) -> Result<String, SchemaError> {
    let path = root
        .join("schema")
        .join(FILE.schema.unwrap_or("motd.schema.yml"));
    let shown = sb_config::issue::escape_controls(&path.display().to_string());
    let rules = schema::load(&path, &shown)?;
    Ok(example(&rules))
}

/// The example for `rules` (`GenerateExampleConfig`, `generateSection`,
/// `generateInstanceExample`).
fn example(rules: &schema::Rules) -> String {
    let mut out = String::from(
        "# Saltbox MOTD Configuration\n\
         # All sections are disabled by default. Set enabled: true and fill in the values to use.\n",
    );
    for name in ORDER {
        let Some(rule) = rules.get(name) else {
            continue;
        };
        out.push('\n');
        comment(&mut out, rule);
        if name == "colors" {
            out.push_str(COLOURS);
            continue;
        }
        let _ = writeln!(out, "{name}:");
        let Some(properties) = &rule.properties else {
            continue;
        };
        for field in [
            "enabled",
            "instances",
            "additional_services",
            "display_names",
        ] {
            let Some(property) = properties.get(field) else {
                continue;
            };
            if field == "instances"
                && property.kind == Some(Kind::Array)
                && let Some(items) = &property.items
            {
                out.push_str("  instances:\n");
                instance_example(&mut out, items, "    ");
            } else if let Some(value) = &property.example {
                let _ = writeln!(out, "  {field}: {}", go_value(value));
            }
        }
    }
    out
}

/// The description, trimmed, one `# ` line per line, when there is one.
fn comment(out: &mut String, rule: &Rule) {
    if let Some(description) = rule.description.as_deref().filter(|d| !d.is_empty()) {
        // YAML has already made every line break `\n`, so `lines` splits
        // as sb-go's `strings.SplitSeq(.., "\n")` does, but for nothing
        // left after the trim, which is one empty line in Go.
        let description = description.trim();
        if description.is_empty() {
            out.push_str("# \n");
        }
        for line in description.lines() {
            let _ = writeln!(out, "# {line}");
        }
    }
}

/// One example instance: `- ` and each field with an example, in sb-go's
/// order, the first on the dash's line.
fn instance_example(out: &mut String, items: &Rule, indent: &str) {
    let Some(properties) = &items.properties else {
        return;
    };
    let _ = write!(out, "{indent}- ");
    let mut first = true;
    let fields = [
        "name", "url", "apikey", "token", "user", "password", "timeout", "enabled",
    ];
    for field in fields {
        let Some(value) = properties.get(field).and_then(|rule| rule.example.as_ref()) else {
            continue;
        };
        if !first {
            let _ = write!(out, "{indent}  ");
        }
        first = false;
        let _ = writeln!(out, "{field}: {}", go_value(value));
    }
}

/// sb-go's `formatValue`: strings as `strconv.Quote`, booleans and
/// numbers as `%v`, anything else as `json.Marshal`.
fn go_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => go_quote(text),
        serde_json::Value::Bool(flag) => flag.to_string(),
        serde_json::Value::Number(number) => match number.as_f64() {
            Some(float) if number.is_f64() => go_float(float),
            _ => number.to_string(),
        },
        other => go_json(other),
    }
}

/// Go's `json.Marshal`: `serde_json`'s compact form (its map is sorted by
/// key, as Go's is) with Go's floats through `serde_json`'s `Formatter`
/// hook (<https://docs.rs/serde_json/1.0.154/serde_json/ser/trait.Formatter.html>),
/// and Go's HTML-safe escapes of `<`, `>` and `&` and its escapes of
/// `U+2028` and `U+2029`, which appear only inside strings.
fn go_json(value: &serde_json::Value) -> String {
    use serde::Serialize as _;
    let mut out = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, GoFloats);
    if value.serialize(&mut serializer).is_err() {
        return String::new();
    }
    String::from_utf8(out)
        .unwrap_or_default()
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

/// `serde_json`'s compact writer with Go's `encoding/json` floats.
struct GoFloats;

impl serde_json::ser::Formatter for GoFloats {
    fn write_f64<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        value: f64,
    ) -> std::io::Result<()> {
        writer.write_all(go_json_float(value).as_bytes())
    }
}

/// A float as Go's `encoding/json` writes it (`floatEncoder`): the
/// shortest digits, plain (`1`, `100000000000000000000`), or in exponent
/// form below 1e-6 or from 1e21 on, the exponent with a sign and no
/// leading zero (`1e+21`, `1e-7`); checked against Go 1.27's output.
fn go_json_float(float: f64) -> String {
    let abs = float.abs();
    if abs != 0.0 && !(1e-6..1e21).contains(&abs) {
        let scientific = format!("{float:e}");
        match scientific.split_once('e') {
            Some((mantissa, exponent)) if !exponent.starts_with('-') => {
                format!("{mantissa}e+{exponent}")
            }
            _ => scientific,
        }
    } else {
        float.to_string()
    }
}

/// Go's `strconv.Quote`: `\a`-style escapes, `\xNN` for other ASCII
/// controls, `\uNNNN` and `\UNNNNNNNN` for what Go doesn't print, checked
/// against Go 1.27's output.
fn go_quote(text: &str) -> String {
    let mut out = String::from('"');
    for c in text.chars() {
        let code = u32::from(c);
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{7}' => out.push_str("\\a"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{b}' => out.push_str("\\v"),
            ' '..='~' => out.push(c),
            _ if c.is_ascii() => {
                let _ = write!(out, "\\x{code:02x}");
            }
            _ if go_prints(c) => out.push(c),
            _ if let Ok(code) = u16::try_from(code) => {
                let _ = write!(out, "\\u{code:04x}");
            }
            _ => {
                let _ = write!(out, "\\U{code:08x}");
            }
        }
    }
    out.push('"');
    out
}

/// Whether Go's `unicode.IsPrint` takes a non-ASCII character: its
/// general category is a letter, mark, number, punctuation or symbol
/// (<https://pkg.go.dev/unicode#IsPrint>), read from ICU4X's data
/// (<https://docs.rs/icu_properties/2.4.0/icu_properties/props/struct.GeneralCategoryGroup.html>).
/// ICU4X's Unicode can be newer than Go's: a character assigned since
/// Go's tables were made is written as it is, where Go escapes it.
fn go_prints(c: char) -> bool {
    use icu_properties::CodePointMapData;
    use icu_properties::props::{GeneralCategory, GeneralCategoryGroup};
    const PRINTED: GeneralCategoryGroup = GeneralCategoryGroup::Letter
        .union(GeneralCategoryGroup::Mark)
        .union(GeneralCategoryGroup::Number)
        .union(GeneralCategoryGroup::Punctuation)
        .union(GeneralCategoryGroup::Symbol);
    PRINTED.contains(CodePointMapData::<GeneralCategory>::new().get(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_takes_both_lengths() {
        assert_eq!(hex("#FF79D0"), Some(RgbColor(0xFF, 0x79, 0xD0)));
        assert_eq!(hex("#abc"), Some(RgbColor(0xAA, 0xBB, 0xCC)));
        for bad in ["FF79D0", "#FF79D", "#ggg", "#", "", "#abcd", "#+bc"] {
            assert_eq!(hex(bad), None, "{bad}");
        }
    }

    #[test]
    fn each_slot_has_its_palette_role_and_key() {
        let roles: Vec<Role> = Slot::ALL.iter().map(|slot| slot.role()).collect();
        assert_eq!(
            roles,
            [
                Role::Accent,
                Role::Text,
                Role::Accent2,
                Role::Warn,
                Role::Ok,
                Role::Problem,
                Role::Ok,
                Role::Warn,
                Role::Problem
            ]
        );
        let keys: Vec<String> = Slot::ALL
            .iter()
            .map(|slot| format!("{}.{}", slot.key().0, slot.key().1))
            .collect();
        assert_eq!(
            keys,
            [
                "text.label",
                "text.value",
                "text.app_name",
                "status.warning",
                "status.success",
                "status.error",
                "progress_bar.low",
                "progress_bar.high",
                "progress_bar.critical"
            ]
        );
        for (i, slot) in Slot::ALL.iter().enumerate() {
            assert_eq!(slot.index(), i);
        }
    }

    #[test]
    fn go_quote_is_strconv_quotes() {
        // Go 1.27's `strconv.Quote` of the same strings.
        assert_eq!(go_quote("a\"b\\c"), r#""a\"b\\c""#);
        assert_eq!(
            go_quote("\x07\x08\x0c\n\r\t\x0b\x00\x1f\x7f"),
            r#""\a\b\f\n\r\t\v\x00\x1f\x7f""#
        );
        assert_eq!(go_quote("é ü"), "\"é ü\"");
        assert_eq!(
            go_quote("\u{a0}\u{ad}\u{200b}\u{2028}\u{feff}\u{e000}"),
            r#""\u00a0\u00ad\u200b\u2028\ufeff\ue000""#
        );
        assert_eq!(go_quote("\u{1F600}"), "\"\u{1F600}\"");
        assert_eq!(go_quote("\u{F0000}"), r#""\U000f0000""#);
        // Tag, format and unassigned characters and a noncharacter.
        assert_eq!(
            go_quote("\u{E0001}\u{180E}\u{61C}\u{FFFE}\u{378}"),
            r#""\U000e0001\u180e\u061c\ufffe\u0378""#
        );
        assert_eq!(go_quote("your-api-key-here"), r#""your-api-key-here""#);
    }

    #[test]
    fn go_float_is_percent_vs() {
        // Go 1.27's `%v` of the same float64s.
        for (float, shown) in [
            (1.5, "1.5"),
            (100_000.0, "100000"),
            (1e6, "1e+06"),
            (1_234_567.0, "1.234567e+06"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (-2.5e-7, "-2.5e-07"),
            (1e21, "1e+21"),
            (123_456.789, "123456.789"),
            (0.5, "0.5"),
        ] {
            assert_eq!(go_float(float), shown, "{float}");
        }
    }

    #[test]
    fn go_values_are_format_values() {
        use serde_json::json;
        assert_eq!(go_value(&json!("x")), r#""x""#);
        assert_eq!(go_value(&json!(false)), "false");
        assert_eq!(go_value(&json!(0)), "0");
        assert_eq!(go_value(&json!(-3)), "-3");
        assert_eq!(go_value(&json!(1.5)), "1.5");
        // A float as `%v`, an integer beyond f64's as its digits.
        assert_eq!(go_value(&json!(1e6)), "1e+06");
        assert_eq!(
            go_value(&json!(9_007_199_254_740_993_u64)),
            "9007199254740993"
        );
        assert_eq!(go_value(&json!([])), "[]");
        assert_eq!(go_value(&json!({})), "{}");
        // Go 1.27's `json.Marshal` of the same floats.
        assert_eq!(
            go_value(&json!([
                1.0, 2.5, 1e21, 1e20, 1e-7, 1.5e-10, -0.0, 0.000_001
            ])),
            "[1,2.5,1e+21,100000000000000000000,1e-7,1.5e-10,-0,0.000001]"
        );
        assert_eq!(go_value(&json!({"a": 1.0})), r#"{"a":1}"#);
        // Go 1.27's `json.Marshal` of the same map.
        assert_eq!(
            go_value(&json!({"b": "<&>", "a": [1, "x\u{2028}\u{2029}"]})),
            r#"{"a":[1,"x\u2028\u2029"],"b":"\u003c\u0026\u003e"}"#
        );
    }

    /// The pinned copy of Saltbox's motd schema (sb-config's fixture).
    fn pinned_schema() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../config/tests/fixtures/saltbox-schemas/motd.schema.yml")
    }

    /// A Saltbox root holding the pinned schema and, if given, motd.yml.
    fn root_with(motd: Option<&str>) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("schema")).unwrap();
        std::fs::copy(pinned_schema(), root.path().join("schema/motd.schema.yml")).unwrap();
        if let Some(motd) = motd {
            std::fs::write(root.path().join("motd.yml"), motd).unwrap();
        }
        root
    }

    fn settings(root: &Path) -> Settings {
        Settings {
            root: root.to_path_buf(),
            zoneinfo: Vec::new(),
            http: Http::new(&Proxy::Off, Duration::from_secs(1)).unwrap(),
            cloudflare_url: String::new(),
            dockerhub_url: String::new(),
            backoff: sb_config::cloudflare::backoff,
        }
    }

    #[test]
    fn the_file_is_motd_yml_with_its_schema() {
        assert_eq!(FILE.path, "motd.yml");
        assert_eq!(FILE.schema, Some("motd.schema.yml"));
        assert_eq!((FILE.required, FILE.capped), (false, true));
    }

    /// sb-go's `GenerateExampleConfig` on the pinned schema, captured once
    /// (a copy of its code run by Go 1.27 against this file; motd spec,
    /// Testing), byte for byte.
    #[test]
    fn generate_config_is_sb_gos_golden() {
        let root = root_with(None);
        let golden = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/generate-config.golden"),
        )
        .unwrap();
        assert_eq!(generate(root.path()).unwrap(), golden);
    }

    #[test]
    fn generate_config_without_a_schema_names_the_file() {
        let root = tempfile::tempdir().unwrap();
        let err = generate(root.path()).unwrap_err().to_string();
        assert!(err.contains("schema/motd.schema.yml"), "{err}");
        assert!(err.contains("doesn't exist"), "{err}");
    }

    /// The writer's corners, as sb-go's: a description trimmed and split,
    /// a section without properties, `instances` that isn't a list of
    /// objects, an item rule without properties, a field without an
    /// example, and sections it doesn't know.
    #[test]
    fn the_example_follows_sb_gos_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.schema.yml");
        std::fs::write(
            &path,
            "emby:\n  description: \"  two\\n lines \"\n\
             jellyfin:\n  properties:\n    instances:\n      type: array\n      example: []\n\
             lidarr:\n  properties:\n    instances:\n      type: array\n      items:\n        type: object\n\
             nzbget:\n  description: \"\"\n  properties:\n    instances:\n      type: array\n      items:\n        properties:\n          name:\n            example: \"Nz\"\n          url:\n            type: string\n          timeout:\n            example: 2.5\n    enabled:\n      type: boolean\n\
             other:\n  description: not written\n",
        )
        .unwrap();
        let rules = schema::load(&path, "x.schema.yml").unwrap();
        assert_eq!(
            example(&rules),
            "# Saltbox MOTD Configuration\n\
             # All sections are disabled by default. Set enabled: true and fill in the values to use.\n\
             \n# two\n#  lines\nemby:\n\
             \njellyfin:\n  instances: []\n\
             \nlidarr:\n  instances:\n\
             \nnzbget:\n  instances:\n    - name: \"Nz\"\n      timeout: 2.5\n"
        );
    }

    /// An item rule with properties but no example writes the dash alone,
    /// with no line end, as sb-go does.
    #[test]
    fn an_instance_without_examples_is_a_bare_dash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.schema.yml");
        std::fs::write(
            &path,
            "sonarr:\n  properties:\n    instances:\n      type: array\n      items:\n        properties:\n          name:\n            type: string\n",
        )
        .unwrap();
        let rules = schema::load(&path, "x.schema.yml").unwrap();
        assert!(example(&rules).ends_with("\nsonarr:\n  instances:\n    - "));
    }

    const VALID: &str = "colors:\n  text:\n    label: '#abc'\n    value: '#12C78F'\n    app_name: ''\n  status:\n    error: '#EB4268'\n  progress_bar:\n    critical: '#E88388'\n\
        sonarr:\n  instances:\n    - name: Sonarr\n      url: http://sonarr:8989\n      apikey: sonarr-key-zz\n      timeout: 2\n    - name: Sonarr 4K\n      url: http://sonarr4k:8989\n      apikey: other-key-zz\n      enabled: false\n      timeout: 0\n\
        plex:\n  enabled: false\n  instances:\n    - name: Plex\n      url: http://plex:32400\n      token: plex-token-zz\n      timeout: 3\n\
        radarr: []\n\
        nzbget:\n  instances:\n    - name: NZBGet\n      url: http://nzbget:6789\n      user: nzb-user\n      password: nzb-pass-zz\n\
        systemd:\n  additional_services: [rclone_vfs, mergerfs]\n  display_names:\n    rclone_vfs: Rclone\n";

    /// A valid file drives the model: the colours it sets (`#abc` as
    /// `#aabbcc`, an empty one left to sb's palette), each app's
    /// `enabled` and instances, the timeouts above zero, Services'
    /// additions; an empty list is no section (decision 4).
    #[test]
    fn a_valid_file_becomes_the_model() {
        let root = root_with(Some(VALID));
        let loaded = load(&settings(root.path()));
        assert_eq!(loaded.line, None);
        let motd = loaded.motd.expect("a model");
        let colours = motd.colours;
        assert_eq!(colours.get(Slot::Label), Some(RgbColor(0xAA, 0xBB, 0xCC)));
        assert_eq!(colours.get(Slot::Value), Some(RgbColor(0x12, 0xC7, 0x8F)));
        assert_eq!(colours.get(Slot::AppName), None);
        assert_eq!(colours.get(Slot::Warning), None);
        assert_eq!(colours.get(Slot::Error), Some(RgbColor(0xEB, 0x42, 0x68)));
        assert_eq!(
            colours.get(Slot::Critical),
            Some(RgbColor(0xE8, 0x83, 0x88))
        );
        assert_eq!(colours.get(Slot::Low), None);
        let apps: Vec<App> = motd.apps.keys().copied().collect();
        assert_eq!(apps, [App::Nzbget, App::Plex, App::Sonarr]);
        let sonarr = &motd.apps[&App::Sonarr];
        assert!(sonarr.enabled);
        assert_eq!(sonarr.instances.len(), 2);
        let first = &sonarr.instances[0];
        assert_eq!(
            (first.name.as_str(), first.url.as_str()),
            ("Sonarr", "http://sonarr:8989")
        );
        assert_eq!(first.apikey.as_deref(), Some("sonarr-key-zz"));
        assert_eq!(
            (first.token.as_deref(), first.user.as_deref()),
            (None, None)
        );
        assert_eq!(first.timeout, Some(Duration::from_secs(2)));
        assert!(first.enabled);
        let second = &sonarr.instances[1];
        assert!(!second.enabled);
        assert_eq!(second.timeout, None);
        let plex = &motd.apps[&App::Plex];
        assert!(!plex.enabled);
        assert_eq!(plex.instances[0].token.as_deref(), Some("plex-token-zz"));
        assert_eq!(plex.instances[0].timeout, Some(Duration::from_secs(3)));
        let nzbget = &motd.apps[&App::Nzbget].instances[0];
        assert_eq!(nzbget.user.as_deref(), Some("nzb-user"));
        assert_eq!(nzbget.password.as_deref(), Some("nzb-pass-zz"));
        let systemd = motd.systemd.expect("systemd");
        assert!(systemd.enabled);
        assert_eq!(systemd.additional_services, ["rclone_vfs", "mergerfs"]);
        assert_eq!(systemd.display_names["rclone_vfs"], "Rclone");
        assert_eq!(systemd.display_names.len(), 1);
    }

    /// An instance's debug output names no credential.
    #[test]
    fn an_instances_debug_output_hides_its_credentials() {
        let root = root_with(Some(VALID));
        let motd = load(&settings(root.path())).motd.unwrap();
        let shown = format!("{:?}", motd.apps);
        for secret in ["sonarr-key-zz", "plex-token-zz", "nzb-pass-zz", "nzb-user"] {
            assert!(!shown.contains(secret), "{shown}");
        }
        assert!(
            shown.contains("Sonarr 4K") && shown.contains("apikey: true"),
            "{shown}"
        );
    }

    /// A file with issues gives the `Config:` line with their count and
    /// no model: a pattern miss, the older bare list (decision 4), an
    /// unknown key.
    #[test]
    fn a_file_with_issues_gives_the_config_line() {
        for (motd, count) in [
            ("colors:\n  text:\n    label: pink\n", "1 issue"),
            (
                "sonarr:\n  - name: Sonarr\n    url: http://sonarr:8989\n    apikey: k\n",
                "1 issue",
            ),
            ("colors:\n  text:\n    label: pink\nsonar: {}\n", "2 issues"),
        ] {
            let root = root_with(Some(motd));
            let loaded = load(&settings(root.path()));
            assert_eq!(loaded.motd, None, "{motd}");
            assert_eq!(
                loaded.line.as_deref(),
                Some(format!("motd.yml has {count}; run sb validate-config").as_str()),
                "{motd}"
            );
        }
    }

    /// No file: no model and no line (sb's palette, no HTTP section).
    #[test]
    fn no_file_is_no_model_and_no_line() {
        let root = root_with(None);
        let loaded = load(&settings(root.path()));
        assert_eq!((loaded.motd, loaded.line), (None, None));
    }

    /// An unreadable file and an unloadable schema each have their own
    /// line (decision 3).
    #[test]
    fn an_unreadable_file_or_schema_has_its_own_line() {
        let root = root_with(None);
        std::fs::create_dir(root.path().join("motd.yml")).unwrap();
        assert_eq!(
            load(&settings(root.path())).line.as_deref(),
            Some(UNREADABLE)
        );
        let root = root_with(Some("colors: {}\n"));
        std::fs::write(
            root.path().join("schema/motd.schema.yml"),
            "colors:\n  kind: x\n",
        )
        .unwrap();
        let loaded = load(&settings(root.path()));
        assert_eq!(loaded.line.as_deref(), Some(UNCHECKED));
        assert_eq!(loaded.motd, None);
        assert_eq!(UNREADABLE, "motd.yml can't be read; run sb validate-config");
        assert_eq!(
            UNCHECKED,
            "motd.yml's schema can't be loaded; run sb validate-config"
        );
    }

    /// Today's reading of an unquoted credential that looks like a number
    /// (resolved, not its source text; spec, verification notes): M44
    /// takes the source text before any provider sends one.
    #[test]
    fn an_unquoted_number_reads_as_resolved_until_m44() {
        let root = root_with(Some(
            "nzbget:\n  instances:\n    - name: NZBGet\n      url: http://nzbget:6789\n      user: 0x1F\n      password: 0123\n",
        ));
        let motd = load(&settings(root.path())).motd.expect("a model");
        let nzbget = &motd.apps[&App::Nzbget].instances[0];
        assert_eq!(nzbget.user.as_deref(), Some("31"));
        assert_eq!(nzbget.password.as_deref(), Some("83"));
    }

    /// Fields of another type than the schema's read as absent; a
    /// non-mapping section is none.
    #[test]
    fn odd_values_read_as_absent() {
        let root = sb_config::tree::Node {
            line: 1,
            column: 1,
            data: Data::Mapping(Vec::new()),
        };
        assert_eq!(Motd::from_tree(&root), Motd::default());
        let node = |data| sb_config::tree::Node {
            line: 1,
            column: 1,
            data,
        };
        assert_eq!(
            text(&node(Data::Scalar(Value::Int(7)))).as_deref(),
            Some("7")
        );
        assert_eq!(text(&node(Data::Scalar(Value::Bool(true)))), None);
        assert_eq!(text(&node(Data::Scalar(Value::Null))), None);
        assert_eq!(seconds(&node(Data::Scalar(Value::Int(-1)))), None);
        assert_eq!(seconds(&node(Data::Scalar(Value::Float(-0.5)))), None);
        assert_eq!(
            seconds(&node(Data::Scalar(Value::Float(f64::INFINITY)))),
            None
        );
        assert_eq!(seconds(&node(Data::Scalar(Value::Str("1".into())))), None);
        assert_eq!(
            seconds(&node(Data::Scalar(Value::Float(0.25)))),
            Some(Duration::from_millis(250))
        );
        assert!(flag(None));
        assert!(flag(Some(&node(Data::Scalar(Value::Bool(true))))));
        assert!(!flag(Some(&node(Data::Scalar(Value::Bool(false))))));
    }
}
