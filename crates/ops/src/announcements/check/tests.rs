use super::*;
use crate::announcements::Migration;

fn entry(date: &str, title: &str, message: &str) -> Announcement {
    Announcement {
        date: date.to_owned(),
        title: title.to_owned(),
        message: message.to_owned(),
        ..Announcement::default()
    }
}

fn texts(found: &[Problem]) -> Vec<(usize, &str)> {
    found.iter().map(|p| (p.entry, p.text.as_str())).collect()
}

fn tags(saltbox: &[&str], sandbox: &[&str]) -> Tags {
    let owned = |list: &[&str]| Some(list.iter().map(|t| (*t).to_owned()).collect());
    Tags {
        saltbox: owned(saltbox),
        sandbox: owned(sandbox),
    }
}

/// A good entry has no problem; each rule finds its own, in entry order.
#[test]
fn each_rule_finds_its_problem() {
    let mut gated = entry("2026-03-01", "Gated", "Text.");
    gated.required_folder = "relative/dir".to_owned();
    gated.required_file = "/opt/../etc/file".to_owned();
    let mut migrating = entry("2026-03-02", "Migrating", "Text.");
    migrating.migration = Migration {
        required: true,
        tag: String::new(),
    };
    let entries = [
        entry(
            "2026-01-01",
            "Good",
            "**Fine** with a [link](https://example.com).",
        ),
        entry("", "", " "),
        entry("2026-02-30", "Not a date", "Text."),
        entry("26-01-01", "Short", "Text."),
        gated,
        entry("2026-03-03", "Html", "<b>bold</b> [x][y]"),
        migrating,
        entry("2026-01-01", "Good", "Again."),
    ];
    let found = problems(&entries, Checkout::Saltbox, &Tags::default());
    assert_eq!(
        texts(&found),
        [
            (2, "`date` is missing"),
            (2, "`title` is missing"),
            (2, "`message` is missing"),
            (3, "`date` '2026-02-30' isn't a date as YYYY-MM-DD"),
            (4, "`date` '26-01-01' isn't a date as YYYY-MM-DD"),
            (5, "`required_folder` 'relative/dir' isn't an absolute path"),
            (5, "`required_file` '/opt/../etc/file' has a '..' component"),
            (6, "`message` has HTML, which sb shows as text: '<b>'"),
            (6, "`message` has HTML, which sb shows as text: '</b>'"),
            (6, "`message` links to '[y]', which it doesn't define"),
            (
                7,
                "`migration.required` is true and `migration.tag` is empty"
            ),
            (8, "same date and title as entry 1"),
        ]
    );
}

fn migrating(tag: &str) -> Announcement {
    let mut entry = entry("2026-04-01", "Migrating", "Text.");
    entry.migration = Migration {
        required: true,
        tag: tag.to_owned(),
    };
    entry
}

/// A tag the repository has passes; a near one is "did you mean", the
/// other repository's tag says so, a near one of the other's names it,
/// one near nothing is the base text; Sandbox's own tags are bare.
#[test]
fn the_tag_rule_takes_installs_hints() {
    let known = tags(&["plex", "settings_move"], &["autobrr", "sonarr4k"]);
    let check = |tag: &str, repo: Checkout| {
        problems(&[migrating(tag)], repo, &known)
            .into_iter()
            .map(|p| p.text)
            .collect::<Vec<_>>()
    };
    assert_eq!(check("plex", Checkout::Saltbox), Vec::<String>::new());
    assert_eq!(check("autobrr", Checkout::Sandbox), Vec::<String>::new());
    assert_eq!(
        check("plx", Checkout::Saltbox),
        ["`migration.tag` 'plx' isn't one of Saltbox's tags; did you mean 'plex'?"]
    );
    assert_eq!(
        check("autobrr", Checkout::Saltbox),
        ["`migration.tag` 'autobrr' isn't one of Saltbox's tags; it is Sandbox's tag"]
    );
    assert_eq!(
        check("autobr", Checkout::Saltbox),
        ["`migration.tag` 'autobr' isn't one of Saltbox's tags; did you mean Sandbox's 'autobrr'?"]
    );
    assert_eq!(
        check("sonarr4", Checkout::Sandbox),
        ["`migration.tag` 'sonarr4' isn't one of Sandbox's tags; did you mean 'sonarr4k'?"]
    );
    assert_eq!(
        check("plexx", Checkout::Sandbox),
        ["`migration.tag` 'plexx' isn't one of Sandbox's tags; did you mean Saltbox's 'plex'?"]
    );
    assert_eq!(
        check("nothing-like-it", Checkout::Saltbox),
        ["`migration.tag` 'nothing-like-it' isn't one of Saltbox's tags"]
    );
    // Without the repository's tags, no tag is checked.
    assert_eq!(
        problems(&[migrating("plx")], Checkout::Saltbox, &Tags::default()),
        Vec::new()
    );
}

/// The entry's label: its date and title, or `no date` and `no title`;
/// file text on one line with controls escaped.
#[test]
fn entries_are_labelled() {
    assert_eq!(
        label(3, Some(&entry("2026-01-01", "Hi", ""))),
        "entry 3 (2026-01-01, \"Hi\")"
    );
    assert_eq!(
        label(1, Some(&entry("", "", ""))),
        "entry 1 (no date, no title)"
    );
    assert_eq!(
        label(2, Some(&entry("2026-01-01", "a\nb\u{1b}[31m", ""))),
        "entry 2 (2026-01-01, \"a\\nb\\u{1b}[31m\")"
    );
    assert_eq!(
        label(4, Some(&entry("2026-01-01", "Don't miss \"this\"", ""))),
        "entry 4 (2026-01-01, \"Don't miss \"this\"\")"
    );
    assert_eq!(
        unchecked(Checkout::Sandbox),
        "migration tags not checked: no cached tags for Sandbox at its checkout's commit (sb list caches them)"
    );
}
