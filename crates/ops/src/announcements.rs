//! Announcements (ops spec, "Announcements" and "`sb announcements`"):
//! `<repo>/announcements.yml` loaded through Saltbox's YAML entry, the new
//! entries since the file before an update, gated by their required
//! folder and file and sorted by date, the migrations they ask for, and
//! their headings. The rules are sb-go's
//! (saltbox/announcements.go:166-231, 460-571), as pure functions over
//! paths and parsed files; [`announcements`] is the command that reads
//! them.

use std::collections::HashSet;
use std::io::{self, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};

use saphyr::{MarkedYamlOwned, YamlDataOwned};
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::config::{read_small, yaml};
use sb_cli_core::exit;
use sb_cli_core::log::RunLog;
use sb_cli_core::theme::{self, Theme};
use sb_config::issue::Issue;
use sb_config::parse;
use sb_config::scalar::{Value, resolve};
use sb_term_ui::{Labels, Options, Runner, Step};
use sb_tui::Ending;
use sb_tui::announcements::Entry;
use sb_tui::markdown;

use crate::Checkout;

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

pub(crate) fn joined(issues: &[Issue]) -> String {
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
/// whose `date|title` the before file lacks. `update`'s list for its
/// viewer (tui spec, "The announcement viewer", Input).
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

/// Every entry of `after` that applies, by date: what `sb announcements`
/// shows, nothing compared and nothing called new (the maintainer,
/// 2026-11-17). None without an after file.
#[must_use]
pub fn applicable(after: Option<&[Announcement]>) -> Vec<Announcement> {
    let mut shown: Vec<Announcement> = after
        .unwrap_or_default()
        .iter()
        .filter(|a| applies(a))
        .cloned()
        .collect();
    shown.sort_by(|a, b| a.date.cmp(&b.date));
    shown
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
    pub repo: Checkout,
    pub tag: String,
    /// The announcement's date, as the table shows it.
    pub date: String,
}

/// The migrations `shown` asks for (sb-go's `PromptForMigrations`): each
/// entry with `migration.required` and a tag, across the repositories in
/// the order given, sorted by date, a stable sort (Saltbox's first on
/// equal dates when it comes first).
#[must_use]
pub fn migrations(shown: &[(Checkout, Vec<Announcement>)]) -> Vec<Pending> {
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
pub fn heading(repo: Checkout, date: &str) -> String {
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
/// isn't a terminal (`update` uses it when stdout isn't a terminal).
///
/// # Errors
///
/// cli-core's `OutputError` when stdout can't be written (a broken pipe
/// is its quiet exit).
pub fn show(shown: &[(Checkout, Vec<Announcement>)]) -> anyhow::Result<()> {
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
    let mut out = sb_cli_core::tail::Tracked::stdout(io::stdout().lock());
    markdown::write_inline(&mut out, &items, &theme, width).map_err(exit::output_error)?;
    Ok(())
}

/// The migrations `pending` names, on stdout after the announcements (ops
/// spec, "Migrations"; sb-go's `PromptForMigrations`): sb-go's line `The
/// following tags need to be run to handle migrations:`, then term-ui's
/// table `#`, `Repo`, `Tag`, `Announcement Date`, numbered from 1, in
/// colour as stdout's choice says, fitted to stdout's terminal when it is
/// one.
///
/// # Errors
///
/// cli-core's `OutputError` when stdout can't be written.
pub fn show_migrations(pending: &[Pending]) -> anyhow::Result<()> {
    let colour = sb_term_ui::colour_on(sb_term_ui::Stream::Stdout);
    let rendered = migrations_table(pending, colour, sb_term_ui::stdout_width());
    let mut out = sb_cli_core::tail::Tracked::stdout(io::stdout().lock());
    writeln!(out, "\n{rendered}")
        .and_then(|()| out.flush())
        .map_err(exit::output_error)?;
    Ok(())
}

/// The line `The following tags need to be run to handle migrations:`
/// and the migrations' table (term-ui's), in colour when `colour`, fitted
/// to `width` when given: on stdout, or as the question's context on the
/// alternate screen (`update`, decision 39).
#[must_use]
pub fn migrations_table(pending: &[Pending], colour: bool, width: Option<usize>) -> String {
    let mut table =
        sb_term_ui::Table::new(["#", "Repo", "Tag", "Announcement Date"]).right_align(0);
    for (number, migration) in (1..).zip(pending) {
        table.row([
            number.to_string(),
            migration.repo.name().to_owned(),
            migration.tag.clone(),
            migration.date.clone(),
        ]);
    }
    let theme = if colour {
        theme::current().clone()
    } else {
        Theme::plain()
    };
    let rendered = table.render(&theme, colour, width);
    format!("The following tags need to be run to handle migrations:\n\n{rendered}")
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

/// The viewer's entries for `listed` (tui spec, "The announcement
/// viewer", Input): each repository's entries in the order given, named
/// by their repository, each marked new as given (`update`'s
/// [`classify`]; `sb announcements` marks none).
#[must_use]
pub(crate) fn viewer_entries(listed: Vec<(Checkout, Vec<(Announcement, bool)>)>) -> Vec<Entry> {
    listed
        .into_iter()
        .flat_map(|(repo, entries)| {
            entries.into_iter().map(move |(a, new)| Entry {
                repo: repo.name().to_owned(),
                date: a.date,
                title: a.title,
                message: a.message,
                new,
            })
        })
        .collect()
}

/// tui's announcement viewer over `entries`, when there are any (ops
/// spec, "Shown" and "`sb announcements`"): one list, newest first,
/// Saltbox's first among equal dates (the viewer's stable sort over the
/// order given). How it ended is the caller's: Ctrl-C ends sb with 130,
/// as on the log screens; `update` names its pending migrations first.
/// Without entries it doesn't open and reads as closed.
///
/// # Errors
///
/// The screen's I/O error.
pub(crate) async fn view(entries: Vec<Entry>, shutdown: &Shutdown) -> anyhow::Result<Ending> {
    if entries.is_empty() {
        return Ok(Ending::Quit);
    }
    Ok(sb_tui::announcements::view(shutdown, entries, stdout_theme()).await?)
}

/// What the announcements step found: the new ones that apply, by date,
/// per checkout (the inline form's and the migrations'), and every one
/// that applies, marked new or not (the viewer's).
pub(crate) struct Found {
    pub(crate) shown: Vec<(Checkout, Vec<Announcement>)>,
    pub(crate) listed: Vec<(Checkout, Vec<(Announcement, bool)>)>,
}

/// Each checkout's file before, as `update` read it before its pull or
/// `preview` at its ref.
pub(crate) type Before = Result<Option<Vec<Announcement>>, LoadError>;

/// "Checking for new announcements" (success "Checked for new
/// announcements"), `update`'s step and `preview`'s: each checkout's
/// announcements now against its `before` ([`classify`]): the new ones
/// that apply, by date (ops spec, "Announcements"), in the order given,
/// with sb-go's info `Displaying new announcements` when there are any,
/// and every one that applies for the viewer. A file that can't be
/// loaded, before or after, is a warning naming it, and that checkout's
/// announcements are skipped this run (decision 23); a missing file is
/// none. With `since` (`preview`'s ref), an info line per checkout counts
/// its new ones since it.
///
/// # Errors
///
/// The step's quiet `Exit` (a step that can't fail otherwise).
pub(crate) async fn compare(
    runner: &mut Runner,
    root: &Path,
    before: Vec<(Checkout, Before)>,
    since: Option<&str>,
) -> Result<Found, exit::Exit> {
    let labels = Labels::new(
        "Checking for new announcements",
        "Checked for new announcements",
    );
    runner
        .step(labels, async |step| {
            let mut shown = Vec::new();
            let mut listed = Vec::new();
            for (checkout, before) in before {
                let path = own_file(checkout, root);
                let skipped = |step: &mut Step, which: &str, err: &LoadError| {
                    step.warn(format!(
                        "{} announcements skipped: error loading {which} announcement file '{}': {err}",
                        checkout.name(),
                        path.display()
                    ));
                };
                let before = match before {
                    Ok(before) => before,
                    Err(err) => {
                        skipped(step, "before", &err);
                        continue;
                    }
                };
                let after = match load(&path) {
                    Ok(after) => after,
                    Err(err) => {
                        skipped(step, "after", &err);
                        continue;
                    }
                };
                let classified = classify(before.as_deref(), after.as_deref());
                let new = only_new(classified.clone());
                tracing::info!(repo = checkout.name(), new = new.len(), "announcements compared");
                if let Some(since) = since {
                    step.info(format!(
                        "{}: {} new since {}",
                        checkout.name(),
                        new.len(),
                        exit::escaped(since)
                    ));
                }
                if !new.is_empty() {
                    shown.push((checkout, new));
                }
                listed.push((checkout, classified));
            }
            if !shown.is_empty() {
                step.info("Displaying new announcements");
            }
            Ok(Found { shown, listed })
        })
        .await
}

/// The file each repository keeps its announcements in.
pub(crate) const FILE: &str = "announcements.yml";

/// `repo`'s own announcements file (ops spec, assumption 7 and
/// "Announcements"): in its checkout under the Saltbox root `root`.
#[must_use]
pub fn own_file(repo: Checkout, root: &Path) -> PathBuf {
    repo.path(root).join(FILE)
}

/// The repositories `sb announcements` reads, in the order shown: both,
/// always (the maintainer, 2026-11-17; ops decision 30).
const REPOS: [Checkout; 2] = [Checkout::Saltbox, Checkout::Sandbox];

/// `sb announcements` (ops spec, "`sb announcements`"): the announcements
/// of Saltbox and Sandbox that apply, each repository's own file under
/// the Saltbox root `root` ([`own_file`]): on a terminal in tui's viewer,
/// else inline, Saltbox's first, each labelled with its repository. It
/// takes no flags of its own and calls nothing new: that is `sb update`'s
/// word, which compares its files in-process (the maintainer,
/// 2026-11-17). A missing file is no file, so a server without Sandbox
/// shows Saltbox's alone; a checkout that is there without its own file is
/// named in a notice; an invalid file warns and the other's show (ops
/// decision 31). It only shows: migrations run in `sb update` (maintainer,
/// 2026-11-15). The loading is one runner step, whose info line counts what
/// will be shown; the runner is done before anything reaches stdout.
///
/// # Errors
///
/// A failed write to stdout, the viewer's I/O error, or `Exit(130)` after
/// Ctrl-C on it.
pub async fn announcements(
    root: &Path,
    shutdown: &Shutdown,
    log: Option<&RunLog>,
) -> anyhow::Result<()> {
    let terminal = io::stdout().is_terminal();
    let mut runner = Runner::start("sb announcements", shutdown, log, Options::default());
    let read = runner
        .step(
            Labels::new("Reading announcements", "Read announcements"),
            async |step| {
                let mut listed = Vec::with_capacity(REPOS.len());
                for repo in REPOS {
                    let path = own_file(repo, root);
                    let (outcome, after) = match load(&path) {
                        Ok(Some(after)) => ("loaded", Some(after)),
                        // A checkout that is there without its own file: one
                        // older than the file, most likely (the maintainer's
                        // Sandbox, 2026-11-17); said, not silent. No checkout
                        // at all (a server without Sandbox) stays silent.
                        Ok(None) => {
                            if path.parent().is_some_and(Path::is_dir) {
                                step.info(format!(
                                    "{} has no announcements file: {}",
                                    repo.name(),
                                    path.display()
                                ));
                            }
                            ("missing", None)
                        }
                        // The warning reaches the run log as the step's notice.
                        Err(err) => {
                            step.warn(format!(
                                "{} announcements skipped: error loading announcement file '{}': {err}",
                                repo.name(),
                                path.display()
                            ));
                            ("error", None)
                        }
                    };
                    let entries = applicable(after.as_deref());
                    tracing::info!(
                        repo = repo.name(),
                        path = %sb_cli_core::config::escape_controls(&path.display().to_string()),
                        file = outcome,
                        shown = entries.len(),
                        "announcements read"
                    );
                    listed.push((repo, entries));
                }
                // What will be shown, on a terminal and piped alike.
                let shown: usize = listed.iter().map(|(_, entries)| entries.len()).sum();
                step.info(match shown {
                    0 => "No announcements".to_owned(),
                    1 => "1 announcement".to_owned(),
                    n => format!("{n} announcements"),
                });
                Ok::<_, anyhow::Error>(listed)
            },
        )
        .await;
    runner.finish();
    let listed = read?;
    if terminal {
        // Nothing is called new here (the maintainer, 2026-11-17).
        let unmarked = listed
            .into_iter()
            .map(|(repo, entries)| (repo, entries.into_iter().map(|a| (a, false)).collect()))
            .collect();
        return match view(viewer_entries(unmarked), shutdown).await? {
            Ending::Interrupted => Err(exit::Exit(130).into()),
            Ending::Quit | Ending::Cancelled => Ok(()),
        };
    }
    let shown: Vec<(Checkout, Vec<Announcement>)> = listed
        .into_iter()
        .filter(|(_, entries)| !entries.is_empty())
        .collect();
    if shown.is_empty() {
        return Ok(());
    }
    show(&shown)
}

pub mod check;
pub mod preview;

#[cfg(test)]
mod tests;
