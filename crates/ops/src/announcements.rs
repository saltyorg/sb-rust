//! Announcements (ops spec, "Announcements" and "`sb announcements`"):
//! `<repo>/announcements.yml` loaded through Saltbox's YAML entry, the new
//! entries since the file before an update, gated by their required
//! folder and file and sorted by date, the migrations they ask for, and
//! their headings. The rules are sb-go's
//! (saltbox/announcements.go:166-231, 460-571), as pure functions over
//! paths and parsed files; [`announcements`] is the command that reads
//! them.

use std::collections::HashSet;
use std::io::{self, IsTerminal as _};
use std::path::{Path, PathBuf};

use saphyr::{MarkedYamlOwned, YamlDataOwned};
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::config::{self, read_small, yaml};
use sb_cli_core::exit;
use sb_cli_core::log::RunLog;
use sb_cli_core::theme::{self, Theme};
use sb_config::issue::Issue;
use sb_config::parse;
use sb_config::scalar::{Value, resolve};
use sb_term_ui::{Labels, Options, Runner};
use sb_tui::Ending;
use sb_tui::announcements::Entry;
use sb_tui::markdown;

/// The repositories that ship announcements (ops spec, assumption 7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repo {
    Saltbox,
    Sandbox,
}

impl Repo {
    /// Its name, as headings, the migration table and messages show it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Saltbox => "Saltbox",
            Self::Sandbox => "Sandbox",
        }
    }

    /// The repository `--repo` names, exactly as sb-go spells it.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "Saltbox" => Some(Self::Saltbox),
            "Sandbox" => Some(Self::Sandbox),
            _ => None,
        }
    }
}

/// One entry of `announcements.yml`, as sb-go's `Announcement` type holds
/// it: every field the file's text, empty when absent or null.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Announcement {
    /// `YYYY-MM-DD` by convention; compared and sorted as text.
    pub date: String,
    pub title: String,
    /// Markdown.
    pub message: String,
    pub migration: Migration,
    pub required_folder: String,
    pub required_file: String,
}

/// An entry's `migration`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Migration {
    pub required: bool,
    pub tag: String,
}

/// Why an announcement file wasn't loaded, in sb-go's words
/// (saltbox/announcements.go, `LoadSingleAnnouncementFile`).
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// The file couldn't be read (cli-core's `read_small`: not a regular
    /// file, over 1 MiB, not UTF-8, or an I/O error).
    #[error("error reading announcement file: {0}")]
    Read(String),
    /// Not YAML Ansible would load, or not of the file's shape; every
    /// issue, `; `-joined.
    #[error("error parsing announcement YAML: {0}")]
    Parse(String),
}

/// The announcements in the file at `path`, or `None` when there is no
/// such file (a checkout that has none, or the first run).
///
/// # Errors
///
/// [`LoadError`] when the file can't be read or isn't valid.
pub fn load(path: &Path) -> Result<Option<Vec<Announcement>>, LoadError> {
    let Some(text) = read_small(path).map_err(|err| LoadError::Read(err.to_string()))? else {
        return Ok(None);
    };
    let file = path
        .file_name()
        .map_or_else(|| path.display().to_string(), |n| n.display().to_string());
    parse_file(&file, &text)
        .map(Some)
        .map_err(|issues| LoadError::Parse(joined(&issues)))
}

fn joined(issues: &[Issue]) -> String {
    issues
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// `text`, an announcement file shown as `file`, decoded as sb-go's
/// yaml.v3 decodes it into `AnnouncementFile` (non-strict: unknown keys
/// ignored). The text goes through sb-config's YAML entry first (ADR-022:
/// what Ansible's loader refuses, duplicate keys, a root that isn't a
/// mapping). A string field takes a scalar's text as written, a null
/// leaves it empty; `required` is a bool, or one of YAML 1.1's yes and no
/// words (yaml.v3's `decode.go` for a typed bool); a collection where a
/// scalar belongs, or the reverse, is an issue at its position, as
/// yaml.v3's type errors fail the file. Why not serde-saphyr (in the
/// tree through sb-config): it reads through its own parser, not ADR-022's
/// pre-pass and loader; its YAML 1.1 bools ignore case (`yEs`), which
/// yaml.v3's don't; and it stops at the first error, where every issue
/// is reported here.
///
/// # Errors
///
/// Every issue, sorted by position.
pub fn parse_file(file: &str, text: &str) -> Result<Vec<Announcement>, Vec<Issue>> {
    let prepared = parse::prepare(file, text)?;
    // A deep file is loaded, walked and dropped on yaml's deep stack, as
    // sb-config's readers do.
    yaml::with_stack(prepared.depth(), || {
        let root = parse::load(file, &prepared)?;
        let mut decoder = Decoder {
            file,
            issues: Vec::new(),
        };
        let list = decoder.list(get(&root, "announcements"));
        if decoder.issues.is_empty() {
            Ok(list)
        } else {
            decoder.issues.sort();
            Err(decoder.issues)
        }
    })
    .map_err(|err| {
        vec![Issue {
            file: file.to_string(),
            line: 1,
            column: 1,
            path: String::new(),
            message: format!("can't start the YAML reader's thread: {err}"),
        }]
    })?
}

/// `node` without a tag on a collection (saphyr keeps a tagged mapping or
/// sequence as `Tagged`; yaml.v3 decodes the collection itself).
fn untagged(node: &MarkedYamlOwned) -> &MarkedYamlOwned {
    match &node.data {
        YamlDataOwned::Tagged(_, inner) => untagged(inner),
        _ => node,
    }
}

/// The value under `key` in `node`, when it is a mapping holding it (a
/// key matched by its text, as yaml.v3 matches a struct's field).
fn get<'a>(node: &'a MarkedYamlOwned, key: &str) -> Option<&'a MarkedYamlOwned> {
    match &untagged(node).data {
        YamlDataOwned::Mapping(map) => map.iter().find_map(|(k, v)| {
            matches!(&untagged(k).data, YamlDataOwned::Representation(text, ..) if text == key)
                .then_some(v)
        }),
        _ => None,
    }
}

/// What a node is, for a type issue.
fn kind(node: &MarkedYamlOwned) -> &'static str {
    match &untagged(node).data {
        YamlDataOwned::Mapping(_) => "a mapping",
        YamlDataOwned::Sequence(_) => "a sequence",
        _ => "a scalar",
    }
}

/// Whether `node` is a null scalar.
fn null(node: &MarkedYamlOwned) -> bool {
    match &untagged(node).data {
        YamlDataOwned::Representation(text, style, tag) => {
            resolve(text, *style, tag.as_ref()) == Ok(Value::Null)
        }
        _ => false,
    }
}

/// yaml.v3's decode of the file's nodes into sb-go's types, gathering
/// every issue.
struct Decoder<'a> {
    file: &'a str,
    issues: Vec<Issue>,
}

impl Decoder<'_> {
    fn issue(&mut self, node: &MarkedYamlOwned, path: &str, message: String) {
        self.issues.push(Issue {
            file: self.file.to_string(),
            line: node.span.start.line(),
            column: node.span.start.col() + 1,
            path: path.to_string(),
            message,
        });
    }

    /// The node's scalar value, `None` for a collection.
    fn scalar(&mut self, node: &MarkedYamlOwned, path: &str) -> Option<(String, Value)> {
        match &untagged(node).data {
            YamlDataOwned::Representation(text, style, tag) => {
                match resolve(text, *style, tag.as_ref()) {
                    Ok(value) => Some((text.clone(), value)),
                    Err(message) => {
                        self.issue(node, path, message);
                        None
                    }
                }
            }
            _ => None,
        }
    }

    fn list(&mut self, node: Option<&MarkedYamlOwned>) -> Vec<Announcement> {
        let path = "announcements";
        let Some(node) = node.filter(|n| !null(n)) else {
            return Vec::new();
        };
        let YamlDataOwned::Sequence(items) = &untagged(node).data else {
            let found = kind(node);
            self.issue(node, path, format!("must be a sequence, not {found}"));
            return Vec::new();
        };
        items
            .iter()
            .enumerate()
            .map(|(index, item)| self.entry(item, &format!("{path}[{index}]")))
            .collect()
    }

    fn entry(&mut self, node: &MarkedYamlOwned, path: &str) -> Announcement {
        if null(node) {
            return Announcement::default();
        }
        if !matches!(untagged(node).data, YamlDataOwned::Mapping(_)) {
            let found = kind(node);
            self.issue(node, path, format!("must be a mapping, not {found}"));
            return Announcement::default();
        }
        let mut text = |key: &str| self.string(get(node, key), &format!("{path}.{key}"));
        let (date, title, message) = (text("date"), text("title"), text("message"));
        let (required_folder, required_file) = (text("required_folder"), text("required_file"));
        let migration = self.migration(get(node, "migration"), &format!("{path}.migration"));
        Announcement {
            date,
            title,
            message,
            migration,
            required_folder,
            required_file,
        }
    }

    fn migration(&mut self, node: Option<&MarkedYamlOwned>, path: &str) -> Migration {
        let Some(node) = node.filter(|n| !null(n)) else {
            return Migration::default();
        };
        if !matches!(untagged(node).data, YamlDataOwned::Mapping(_)) {
            let found = kind(node);
            self.issue(node, path, format!("must be a mapping, not {found}"));
            return Migration::default();
        }
        Migration {
            required: self.bool(get(node, "required"), &format!("{path}.required")),
            tag: self.string(get(node, "tag"), &format!("{path}.tag")),
        }
    }

    /// A string field: a scalar's text as written (yaml.v3 sets a Go
    /// string from the node's value whatever it resolves to), empty for
    /// null. Why the text and not sb-config's tree: its values are
    /// resolved (`007` would read `7`), yaml.v3's string isn't.
    fn string(&mut self, node: Option<&MarkedYamlOwned>, path: &str) -> String {
        let Some(node) = node else {
            return String::new();
        };
        match self.scalar(node, path) {
            Some((_, Value::Null)) => String::new(),
            Some((text, _)) => text,
            None => {
                if !matches!(untagged(node).data, YamlDataOwned::Representation(..)) {
                    let found = kind(node);
                    self.issue(node, path, format!("must be a string, not {found}"));
                }
                String::new()
            }
        }
    }

    /// A bool field: `true`/`false`, YAML 1.1's yes and no words
    /// (yaml.v3's `decode.go`, typed bool), false for null.
    fn bool(&mut self, node: Option<&MarkedYamlOwned>, path: &str) -> bool {
        let Some(node) = node else {
            return false;
        };
        match self.scalar(node, path) {
            Some((_, Value::Null)) => false,
            Some((_, Value::Bool(value))) => value,
            Some((text, value)) => {
                if let Value::Str(word) = &value
                    && let Some(answer) = yes_no(word)
                {
                    return answer;
                }
                self.issue(node, path, format!("`{text}` must be a bool"));
                false
            }
            None => {
                if !matches!(untagged(node).data, YamlDataOwned::Representation(..)) {
                    let found = kind(node);
                    self.issue(node, path, format!("must be a bool, not {found}"));
                }
                false
            }
        }
    }
}

/// YAML 1.1's bool words yaml.v3 accepts for a typed bool.
fn yes_no(word: &str) -> Option<bool> {
    match word {
        "y" | "Y" | "yes" | "Yes" | "YES" | "on" | "On" | "ON" => Some(true),
        "n" | "N" | "no" | "No" | "NO" | "off" | "Off" | "OFF" => Some(false),
        _ => None,
    }
}

/// The entries of `after` that apply ([`applies`]: their required folder
/// and file are there), in the file's order, each with whether it is new
/// (sb-go's `CheckSingleRepoAnnouncements`): none without an after file;
/// with no before file, or an empty one, every entry is new; else those
/// whose `date|title` the before file lacks. The viewer's list (tui spec,
/// "The announcement viewer", Input).
#[must_use]
pub fn classify(
    before: Option<&[Announcement]>,
    after: Option<&[Announcement]>,
) -> Vec<(Announcement, bool)> {
    let Some(after) = after else {
        return Vec::new();
    };
    // sb-go's key: the two joined by `|`.
    let key = |a: &Announcement| format!("{}|{}", a.date, a.title);
    let seen: HashSet<String> = before.unwrap_or_default().iter().map(key).collect();
    after
        .iter()
        .filter(|a| applies(a))
        .map(|a| (a.clone(), !seen.contains(&key(a))))
        .collect()
}

/// The new ones of `classified`, sorted by their date's text, a stable
/// sort (sb-go's `sort.Slice` left equal dates in any order): what the
/// inline form shows.
#[must_use]
pub fn only_new(classified: Vec<(Announcement, bool)>) -> Vec<Announcement> {
    let mut new: Vec<Announcement> = classified
        .into_iter()
        .filter_map(|(a, new)| new.then_some(a))
        .collect();
    new.sort_by(|a, b| a.date.cmp(&b.date));
    new
}

/// The entries of `after` to show inline (sb-go's
/// `CheckSingleRepoAnnouncements`): [`classify`]'s new ones, by date
/// ([`only_new`]).
#[must_use]
pub fn new_entries(
    before: Option<&[Announcement]>,
    after: Option<&[Announcement]>,
) -> Vec<Announcement> {
    only_new(classify(before, after))
}

/// Whether `announcement`'s gates hold (sb-go's `shouldShowAnnouncement`):
/// a set `required_folder` must be a directory and a set `required_file`
/// something else, links followed (`os.Stat`). As in sb-go, only a path
/// that doesn't exist, or is of the other kind, hides it; a path that
/// can't be checked (a permission error, a file where a directory
/// should be) doesn't.
#[must_use]
pub fn applies(announcement: &Announcement) -> bool {
    let hides = |path: &str, directory: bool| {
        !path.is_empty()
            && match std::fs::metadata(path) {
                Err(err) => err.kind() == io::ErrorKind::NotFound,
                Ok(meta) => meta.is_dir() != directory,
            }
    };
    !hides(&announcement.required_folder, true) && !hides(&announcement.required_file, false)
}

/// A migration a new announcement asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub repo: Repo,
    pub tag: String,
    /// The announcement's date, as the table shows it.
    pub date: String,
}

/// The migrations `shown` asks for (sb-go's `PromptForMigrations`): each
/// entry with `migration.required` and a tag, across the repositories in
/// the order given, sorted by date, a stable sort (Saltbox's first on
/// equal dates when it comes first).
#[must_use]
pub fn migrations(shown: &[(Repo, Vec<Announcement>)]) -> Vec<Pending> {
    let mut pending: Vec<Pending> = shown
        .iter()
        .flat_map(|(repo, announcements)| {
            announcements
                .iter()
                .filter(|a| a.migration.required && !a.migration.tag.is_empty())
                .map(|a| Pending {
                    repo: *repo,
                    tag: a.migration.tag.clone(),
                    date: a.date.clone(),
                })
        })
        .collect();
    pending.sort_by(|a, b| a.date.cmp(&b.date));
    pending
}

/// An announcement's heading, sb-go's title: `<Repo> Announcement -
/// <Month D, YYYY>`, the date as written when it isn't `YYYY-MM-DD`.
#[must_use]
pub fn heading(repo: Repo, date: &str) -> String {
    let date = long_date(date).unwrap_or_else(|| date.to_string());
    format!("{} Announcement - {date}", repo.name())
}

/// `text` as Go's `time.Parse("2006-01-02")` reads it, then formatted as
/// `January 2, 2006`: four digits, two, two, a real date, nothing more.
/// Why not jiff's `strptime`: its `%m` and `%d` take one digit too, and
/// Go's layout needs two. jiff checks the date
/// (<https://docs.rs/jiff/0.2.38/jiff/civil/struct.Date.html#method.new>)
/// and names its month (`%B`).
fn long_date(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let shape = bytes.len() == 10
        && bytes.iter().enumerate().all(|(i, b)| match i {
            4 | 7 => *b == b'-',
            _ => b.is_ascii_digit(),
        });
    if !shape {
        return None;
    }
    let year: i16 = text[..4].parse().ok()?;
    let month: i8 = text[5..7].parse().ok()?;
    let day: i8 = text[8..].parse().ok()?;
    let date = jiff::civil::Date::new(year, month, day).ok()?;
    // Go's `January 2, 2006`: jiff's `%-d` unpadded, `%Y` zero-padded to
    // four digits.
    Some(date.strftime("%B %-d, %Y").to_string())
}

/// Write `shown` on stdout in tui's inline Markdown form, each under its
/// heading (ops spec, "Shown"), in colour as stdout's choice says, tables
/// fitted to stdout's terminal when it is one: the form for stdout that
/// isn't a terminal (`update` uses it on a terminal too until M87).
///
/// # Errors
///
/// cli-core's `OutputError` when stdout can't be written (a broken pipe
/// is its quiet exit).
pub fn show(shown: &[(Repo, Vec<Announcement>)]) -> anyhow::Result<()> {
    let headings: Vec<(String, &str)> = shown
        .iter()
        .flat_map(|(repo, announcements)| {
            announcements
                .iter()
                .map(|a| (heading(*repo, &a.date), a.message.as_str()))
        })
        .collect();
    let items: Vec<markdown::Announcement<'_>> = headings
        .iter()
        .map(|(heading, message)| markdown::Announcement { heading, message })
        .collect();
    let theme = stdout_theme();
    let width = sb_term_ui::stdout_width().and_then(|w| u16::try_from(w).ok());
    let mut out = io::stdout().lock();
    markdown::write_inline(&mut out, &items, &theme, width).map_err(exit::output_error)?;
    Ok(())
}

/// The run's theme, or none when stdout gets no colour (`NO_COLOR`,
/// `--color=never`, a pipe), as sb's other output reads it (term-ui's
/// `colour_on`): the inline form's and the viewer's, as sb's log
/// screens choose theirs.
fn stdout_theme() -> Theme {
    if sb_term_ui::colour_on(sb_term_ui::Stream::Stdout) {
        theme::current().clone()
    } else {
        Theme::plain()
    }
}

/// tui's announcement viewer over `listed`, `repo`'s, when any apply
/// (ops spec, "`sb announcements`"). Ctrl-C on it ends sb with 130, as on
/// the log screens.
///
/// # Errors
///
/// The screen's I/O error; cli-core's `Exit(130)` after Ctrl-C.
async fn view(
    repo: Repo,
    listed: Vec<(Announcement, bool)>,
    shutdown: &Shutdown,
) -> anyhow::Result<()> {
    if listed.is_empty() {
        return Ok(());
    }
    let entries = listed
        .into_iter()
        .map(|(a, new)| Entry {
            repo: repo.name().to_owned(),
            date: a.date,
            title: a.title,
            message: a.message,
            new,
        })
        .collect();
    match sb_tui::announcements::view(shutdown, entries, stdout_theme()).await? {
        Ending::Interrupted => Err(exit::Exit(130).into()),
        Ending::Quit | Ending::Cancelled => Ok(()),
    }
}

/// The file each repository keeps its announcements in.
const FILE: &str = "announcements.yml";

/// `repo`'s own announcements file (ops spec, assumption 7 and
/// "Announcements"): Saltbox's at the Saltbox root `root`, Sandbox's in
/// `<server_appdata_path>/sandbox`, the appdata path read from the
/// inventory under `root` as cli-core reads it.
#[must_use]
pub fn own_file(repo: Repo, root: &Path) -> PathBuf {
    match repo {
        Repo::Saltbox => root.join(FILE),
        Repo::Sandbox => config::appdata_path(&config::inventory_path(root))
            .join("sandbox")
            .join(FILE),
    }
}

/// `sb announcements`' flags (ops spec, "Commands and help"): sb-go's,
/// persistent as there (`global`), but `-v` (decision 18). None is
/// required: the command reads announcements (decision 27).
#[derive(clap::Args, Debug, Clone)]
pub struct AnnouncementsArgs {
    /// Path to the before announcements.yml file
    // Without it every announcement is new, sb-go's rule for no before
    // file (ops spec, "Announcements").
    #[arg(short, long, global = true, value_name = "file")]
    pub before: Option<PathBuf>,
    /// Path to the after announcements.yml file
    // Without it, the repository's own file ([`own_file`]).
    #[arg(short, long, global = true, value_name = "file")]
    pub after: Option<PathBuf>,
    /// Repository name (Saltbox or Sandbox)
    // Why not a `ValueEnum`: clap would refuse a wrong name while parsing,
    // exit 2 in its words; sb-go's harness fails with its own text and
    // exit 1 ([`announcements`]), which this keeps.
    #[arg(short, long, global = true, default_value = "Saltbox")]
    pub repo: String,
}

/// `sb announcements` (ops spec, "`sb announcements`"): the announcements
/// of the repository `--repo` names, from `--after` (else its own file
/// under the Saltbox root `root`, [`own_file`]), those `--before` lacks
/// marked new (without it, all of them): on a terminal every one that
/// applies in tui's viewer, else the new ones inline. A missing file is
/// no file. It only shows: migrations run in `sb update`
/// (maintainer, 2026-11-15). The loading is one runner step, whose info
/// line says whether announcements follow; the runner is done before
/// anything reaches stdout.
///
/// # Errors
///
/// An unknown repository name, or a file that can't be loaded (sb-go's
/// texts, exit 1), a failed write to stdout, the viewer's I/O error, or
/// `Exit(130)` after Ctrl-C on it.
pub async fn announcements(
    args: &AnnouncementsArgs,
    root: &Path,
    shutdown: &Shutdown,
    log: Option<&RunLog>,
) -> anyhow::Result<()> {
    let repo = Repo::from_name(&args.repo).ok_or_else(|| {
        anyhow::anyhow!(
            "invalid repository name '{}', must be 'Saltbox' or 'Sandbox'",
            args.repo
        )
    })?;
    let after = args.after.clone().unwrap_or_else(|| own_file(repo, root));
    let mut runner = Runner::start("sb announcements", shutdown, log, Options::default());
    let found = runner
        .step(
            Labels::new(
                "Checking for new announcements",
                "Checked for new announcements",
            ),
            async |step| {
                let loaded = |which: &str, path: &Path| {
                    load(path).map_err(|err| {
                        anyhow::anyhow!(
                            "error loading {which} announcement file '{}': {err}",
                            path.display()
                        )
                    })
                };
                let before = match &args.before {
                    Some(path) => loaded("before", path)?,
                    None => None,
                };
                let after = loaded("after", &after)?;
                let listed = classify(before.as_deref(), after.as_deref());
                let new = listed.iter().filter(|(_, new)| *new).count();
                tracing::info!(
                    repo = repo.name(),
                    before = args.before.is_some(),
                    listed = listed.len(),
                    new,
                    "announcements compared"
                );
                step.info(if new == 0 {
                    "No new announcements"
                } else {
                    "Displaying new announcements"
                });
                Ok(listed)
            },
        )
        .await;
    runner.finish();
    let listed = found?;
    if io::stdout().is_terminal() {
        return view(repo, listed, shutdown).await;
    }
    let new = only_new(listed);
    if new.is_empty() {
        return Ok(());
    }
    show(&[(repo, new)])
}

#[cfg(test)]
mod tests;
