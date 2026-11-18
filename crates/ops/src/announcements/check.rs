//! `sb announcements check [file]` (ops spec, "`sb announcements check`";
//! decision 35): every problem in an announcement file, for developers
//! and Saltbox's CI. The file is loaded by the loader `update` uses (its
//! own words for a file that doesn't load); each entry's fields, date,
//! paths, Markdown (sb-tui's `markdown::problems`) and migration are
//! checked, and its migration tag against the repository's tags as the
//! tags cache holds them for the checkout's commit (sb-ansible's install
//! validation), never rebuilt: a cache that isn't fresh skips that check
//! and says so.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use sb_ansible::{Hint, Known, Routed, TagCache};
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::config::read_small;
use sb_cli_core::exit;

use super::{Announcement, own_file, parse_file};
use crate::Checkout;

/// What a check reads.
#[derive(Debug)]
pub struct Check<'a> {
    /// The Saltbox root: the checkouts, their files and the tags cache.
    pub root: &'a Path,
    /// The file to check (`[file]`), else both checkouts' own files.
    pub file: Option<&'a Path>,
    /// The repository a given file's migration tags belong to (`--repo`).
    pub repo: Option<Checkout>,
}

/// One problem of an entry: its number from 1 and what is wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub entry: usize,
    pub text: String,
}

/// The repositories' tags as the cache holds them for their checkouts'
/// commits; `None` for a repository whose tags aren't there.
#[derive(Clone, Debug, Default)]
pub struct Tags {
    pub saltbox: Option<Vec<String>>,
    pub sandbox: Option<Vec<String>>,
}

impl Tags {
    /// Each checkout's cached tags if they are fresh for its HEAD
    /// (sb-ansible's `TagCache::fresh`): read, never rebuilt.
    pub async fn cached(root: &Path, shutdown: &Shutdown) -> Self {
        let cache = TagCache::load(root);
        let token = shutdown.token();
        let fresh = async |checkout: Checkout| {
            let repo = sb_repo::Repo::open(checkout.name(), checkout.path(root)).ok()?;
            let head = repo.head(&token).await.ok()?;
            cache.fresh(&checkout.playbooks(root), &head)
        };
        Self {
            saltbox: fresh(Checkout::Saltbox).await,
            sandbox: fresh(Checkout::Sandbox).await,
        }
    }

    fn of(&self, checkout: Checkout) -> Option<&[String]> {
        match checkout {
            Checkout::Saltbox => self.saltbox.as_deref(),
            Checkout::Sandbox => self.sandbox.as_deref(),
        }
    }
}

/// The note for a repository whose tags can't be checked.
fn unchecked(repo: Checkout) -> String {
    format!(
        "migration tags not checked: no cached tags for {} at its checkout's commit (sb list caches them)",
        repo.name()
    )
}

/// The problems of `entries`, `repo`'s, in entry order: each rule of the
/// spec, the tag rule only when `tags` has `repo`'s.
#[must_use]
pub fn problems(entries: &[Announcement], repo: Checkout, tags: &Tags) -> Vec<Problem> {
    let mut found = Vec::new();
    let mut seen: HashMap<(&str, &str), usize> = HashMap::new();
    for (index, entry) in entries.iter().enumerate() {
        let number = index + 1;
        let mut add = |text: String| {
            found.push(Problem {
                entry: number,
                text,
            });
        };
        for (field, value) in [
            ("date", &entry.date),
            ("title", &entry.title),
            ("message", &entry.message),
        ] {
            if value.trim().is_empty() {
                add(format!("`{field}` is missing"));
            }
        }
        if !entry.date.is_empty() && super::long_date(&entry.date).is_none() {
            add(format!(
                "`date` '{}' isn't a date as YYYY-MM-DD",
                shown(&entry.date)
            ));
        }
        for (field, value) in [
            ("required_folder", &entry.required_folder),
            ("required_file", &entry.required_file),
        ] {
            if value.is_empty() {
                continue;
            }
            let path = Path::new(value);
            if !path.is_absolute() {
                add(format!(
                    "`{field}` '{}' isn't an absolute path",
                    shown(value)
                ));
            } else if path
                .components()
                .any(|part| part == std::path::Component::ParentDir)
            {
                add(format!("`{field}` '{}' has a '..' component", shown(value)));
            }
        }
        for problem in sb_tui::markdown::problems(&entry.message) {
            add(shown(&problem.to_string()));
        }
        let tag = &entry.migration.tag;
        if entry.migration.required && tag.trim().is_empty() {
            add("`migration.required` is true and `migration.tag` is empty".to_owned());
        }
        if !tag.trim().is_empty()
            && let Some(own) = tags.of(repo)
            && let Some(text) = tag_problem(tag, repo, own, tags)
        {
            add(text);
        }
        if !entry.date.is_empty() || !entry.title.is_empty() {
            let key = (entry.date.as_str(), entry.title.as_str());
            if let Some(first) = seen.get(&key) {
                add(format!("same date and title as entry {first}"));
            } else {
                seen.insert(key, number);
            }
        }
    }
    found
}

/// `tag`'s problem when `repo`'s tags (`own`) lack it: sb-ansible's
/// install validation over the two repositories' tags, its hint as the
/// spec words it.
fn tag_problem(tag: &str, repo: Checkout, own: &[String], tags: &Tags) -> Option<String> {
    let other = match repo {
        Checkout::Saltbox => Checkout::Sandbox,
        Checkout::Sandbox => Checkout::Saltbox,
    };
    let other_tags = tags.of(other).unwrap_or_default().to_vec();
    let (known, routed) = match repo {
        Checkout::Saltbox => (
            Known {
                saltbox: own.to_vec(),
                sandbox: other_tags,
            },
            Routed {
                saltbox: vec![tag.to_owned()],
                ..Routed::default()
            },
        ),
        Checkout::Sandbox => (
            Known {
                saltbox: other_tags,
                sandbox: own.to_vec(),
            },
            Routed {
                sandbox: vec![tag.to_owned()],
                ..Routed::default()
            },
        ),
    };
    let suggestion = sb_ansible::validate(&routed, &known).into_iter().next()?;
    let lacks = format!(
        "`migration.tag` '{}' isn't one of {}'s tags",
        shown(tag),
        repo.name()
    );
    // The hints name a tag as `sb install` takes it, Sandbox's with its
    // `sandbox-` prefix; a migration's tag is the bare one, as the file
    // gives it.
    let bare = |near: &str, sandbox: bool| {
        let near = if sandbox {
            near.strip_prefix("sandbox-").unwrap_or(near)
        } else {
            near
        };
        shown(near)
    };
    Some(match suggestion.hint {
        Hint::Typo(near) => format!(
            "{lacks}; did you mean '{}'?",
            bare(&near, repo == Checkout::Sandbox)
        ),
        Hint::Elsewhere(_) => format!("{lacks}; it is {}'s tag", other.name()),
        Hint::TypoElsewhere(near) => format!(
            "{lacks}; did you mean {}'s '{}'?",
            other.name(),
            bare(&near, other == Checkout::Sandbox)
        ),
        Hint::NotFound => lacks,
    })
}

/// File text in a problem line: on one line, controls escaped, quotes
/// as they are (cli-core's `escaped`, as sb's other output; text sb-config
/// escaped already stays as it is).
fn shown(text: &str) -> String {
    exit::escaped(text)
}

/// The entry as a problem line names it: `entry <n> (<date>, "<title>")`.
fn label(number: usize, entry: Option<&Announcement>) -> String {
    let (date, title) = entry.map_or((String::new(), String::new()), |entry| {
        (entry.date.clone(), entry.title.clone())
    });
    let date = if date.is_empty() {
        "no date".to_owned()
    } else {
        shown(&date)
    };
    let title = if title.is_empty() {
        "no title".to_owned()
    } else {
        format!("\"{}\"", shown(&title))
    };
    format!("entry {number} ({date}, {title})")
}

/// The files to check, each with its repository: the given one (its
/// repository `--repo`'s, else Sandbox's when it is Sandbox's own file,
/// else Saltbox's), or both checkouts' own files that are there, a
/// checkout without its own file named in a note.
fn targets(check: &Check<'_>, notes: &mut Vec<String>) -> Vec<(PathBuf, Checkout)> {
    if let Some(file) = check.file {
        let sandbox = own_file(Checkout::Sandbox, check.root);
        let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        };
        let repo = check.repo.unwrap_or(if same(file, &sandbox) {
            Checkout::Sandbox
        } else {
            Checkout::Saltbox
        });
        return vec![(file.to_path_buf(), repo)];
    }
    let repos = match check.repo {
        Some(repo) => vec![repo],
        None => vec![Checkout::Saltbox, Checkout::Sandbox],
    };
    let mut found = Vec::new();
    for repo in repos {
        let path = own_file(repo, check.root);
        if path.is_file() {
            found.push((path, repo));
        } else if path.parent().is_some_and(Path::is_dir) {
            notes.push(format!(
                "{} has no announcements file: {}",
                repo.name(),
                path.display()
            ));
        }
    }
    found
}

/// `sb announcements check`: each file as `<path>:`, its problems as
/// `  entry <n> (<date>, "<title>"): <problem>`, its notes as `  note:
/// ...`, then `  <n> problems` or `  no problems`, on `out`.
///
/// # Errors
///
/// `announcements check found <n> problems` (exit 1) when there are
/// any; `no announcements file to check` when no file is there; a failed
/// write.
pub async fn check(
    check: &Check<'_>,
    shutdown: &Shutdown,
    out: &mut impl Write,
) -> anyhow::Result<()> {
    let mut notes = Vec::new();
    let files = targets(check, &mut notes);
    let write = crate::update::write_line;
    for note in &notes {
        write(out, &format!("note: {}", shown(note)))?;
    }
    if files.is_empty() {
        anyhow::bail!("no announcements file to check");
    }
    let tags = Tags::cached(check.root, shutdown).await;
    let mut total = 0;
    for (path, repo) in files {
        write(out, &format!("{}:", shown(&path.display().to_string())))?;
        let name = path
            .file_name()
            .map_or_else(|| path.display().to_string(), |n| n.display().to_string());
        // The loader's own words for a file that doesn't load.
        let loaded = match read_small(&path) {
            Ok(Some(text)) => parse_file(&name, &text).map_err(|issues| {
                issues
                    .iter()
                    .map(|issue| format!("error parsing announcement YAML: {issue}"))
                    .collect::<Vec<_>>()
            }),
            Ok(None) => Err(vec![format!(
                "error reading announcement file: {} doesn't exist",
                path.display()
            )]),
            Err(err) => Err(vec![format!("error reading announcement file: {err}")]),
        };
        let count = match loaded {
            Err(lines) => {
                for line in &lines {
                    write(out, &format!("  {}", shown(line)))?;
                }
                lines.len()
            }
            Ok(entries) => {
                let found = problems(&entries, repo, &tags);
                for problem in &found {
                    let entry = entries.get(problem.entry - 1);
                    write(
                        out,
                        &format!("  {}: {}", label(problem.entry, entry), problem.text),
                    )?;
                }
                let tagged = entries.iter().any(|e| !e.migration.tag.trim().is_empty());
                if tagged && tags.of(repo).is_none() {
                    write(out, &format!("  note: {}", unchecked(repo)))?;
                }
                found.len()
            }
        };
        total += count;
        write(
            out,
            &match count {
                0 => "  no problems".to_owned(),
                1 => "  1 problem".to_owned(),
                n => format!("  {n} problems"),
            },
        )?;
    }
    tracing::info!(problems = total, "announcements checked");
    match total {
        0 => Ok(()),
        1 => anyhow::bail!("announcements check found 1 problem"),
        n => anyhow::bail!("announcements check found {n} problems"),
    }
}

#[cfg(test)]
mod tests;
