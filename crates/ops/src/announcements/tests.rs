use std::os::unix::fs::symlink;

use clap::Parser as _;

use super::*;

/// Shaped as Saltbox's own file (public GitHub, saltyorg/Saltbox
/// master, 2026-11-15): a document marker, quoted dates, a block
/// message; synthetic text.
const SALTBOX_LIKE: &str = "---\nannouncements:\n  - date: \"2026-02-01\"\n    \
                            title: \"Breaking Changes\"\n    message: |\n      \
                            ## Heads up\n\n      Read https://docs.example.org first.\n";

/// Shaped as Sandbox's (saltyorg/Sandbox master, 2026-11-15): two
/// entries gated by a folder; synthetic text and paths.
fn sandbox_like(folder: &Path) -> String {
    format!(
        "---\nannouncements:\n  - date: \"2026-03-04\"\n    title: \"Roles moved\"\n    \
         required_folder: \"{folder}\"\n    message: |\n      ### Moved\n\n      \
         `/opt/sandbox/roles` changed.\n  - date: \"2026-03-01\"\n    title: \"Images\"\n    \
         required_folder: \"{folder}/missing\"\n    message: |\n      Image `x/y` renamed.\n",
        folder = folder.display()
    )
}

fn parsed(text: &str) -> Vec<Announcement> {
    parse_file("announcements.yml", text).unwrap_or_else(|issues| panic!("{issues:?}"))
}

fn issues(text: &str) -> Vec<String> {
    parse_file("announcements.yml", text)
        .expect_err(text)
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn entry(date: &str, title: &str) -> Announcement {
    Announcement {
        date: date.to_string(),
        title: title.to_string(),
        message: format!("{title}'s message"),
        ..Announcement::default()
    }
}

fn titles(list: &[Announcement]) -> Vec<&str> {
    list.iter().map(|a| a.title.as_str()).collect()
}

#[test]
fn saltboxs_shape_is_read_whole() {
    assert_eq!(
        parsed(SALTBOX_LIKE),
        [Announcement {
            date: "2026-02-01".into(),
            title: "Breaking Changes".into(),
            message: "## Heads up\n\nRead https://docs.example.org first.\n".into(),
            ..Announcement::default()
        }]
    );
}

#[test]
fn sandboxs_shape_gives_the_entries_its_folder_allows() {
    let dir = tempfile::tempdir().expect("temp dir");
    let after = parsed(&sandbox_like(dir.path()));
    assert_eq!(after.len(), 2);
    assert_eq!(after[0].required_folder, dir.path().display().to_string());
    assert_eq!(titles(&new_entries(None, Some(&after))), ["Roles moved"]);
}

#[test]
fn every_field_is_read_and_unknown_keys_are_ignored() {
    let text = "extra: 1\nannouncements:\n  - date: 2026-01-02\n    title: t\n    \
                message: m\n    required_folder: /a\n    required_file: /b\n    \
                colour: red\n    migration:\n      required: true\n      tag: x\n      \
                other: [1]\n";
    assert_eq!(
        parsed(text),
        [Announcement {
            date: "2026-01-02".into(),
            title: "t".into(),
            message: "m".into(),
            migration: Migration {
                required: true,
                tag: "x".into()
            },
            required_folder: "/a".into(),
            required_file: "/b".into(),
        }]
    );
}

#[test]
fn no_list_is_an_empty_one() {
    for text in [
        "",
        "---\n",
        "other: 1\n",
        "announcements:\n",
        "announcements: ~\n",
        "announcements: []\n",
    ] {
        assert_eq!(parsed(text), [], "{text:?}");
    }
}

#[test]
fn a_string_field_keeps_a_scalars_text_and_null_is_empty() {
    let text = "announcements:\n  - date: 2026-01-02\n    title: 007\n    message: 1.50\n    \
                required_folder: ~\n    required_file: true\n    migration:\n      tag: null\n";
    let read = &parsed(text)[0];
    assert_eq!(read.date, "2026-01-02");
    assert_eq!(read.title, "007");
    assert_eq!(read.message, "1.50");
    assert_eq!(read.required_folder, "");
    assert_eq!(read.required_file, "true");
    assert_eq!(read.migration.tag, "");
    assert_eq!(parsed("announcements:\n  - title: ''\n")[0].title, "");
}

#[test]
fn a_null_entry_or_migration_is_an_empty_one() {
    assert_eq!(
        parsed("announcements:\n  -\n  - title: t\n    migration:\n"),
        [Announcement::default(), entry("", "t").with_message("")]
    );
}

#[test]
fn required_is_yaml_v3s_typed_bool() {
    let required = |value: &str| {
        let text = format!("announcements:\n  - migration:\n      required: {value}\n");
        parsed(&text)[0].migration.required
    };
    for value in ["true", "True", "yes", "Y", "on", "'yes'", "\"ON\""] {
        assert!(required(value), "{value}");
    }
    for value in ["false", "no", "n", "Off", "~", ""] {
        assert!(!required(value), "{value}");
    }
    assert!(
        !parsed("announcements:\n  - migration:\n      tag: x\n")[0]
            .migration
            .required
    );
}

#[test]
fn a_value_of_the_wrong_kind_fails_the_file_at_its_place() {
    assert_eq!(
        issues("announcements: yes\n"),
        ["announcements.yml:1:16: announcements: must be a sequence, not a scalar"]
    );
    assert_eq!(
        issues("announcements:\n  - text\n  - [1]\n"),
        [
            "announcements.yml:2:5: announcements[0]: must be a mapping, not a scalar",
            "announcements.yml:3:5: announcements[1]: must be a mapping, not a sequence",
        ]
    );
    assert_eq!(
        issues(
            "announcements:\n  - title: [a]\n    message: {a: 1}\n    migration: [x]\n  \
             - migration:\n      required: 3\n      tag: {}\n"
        ),
        [
            "announcements.yml:2:12: announcements[0].title: must be a string, not a sequence",
            "announcements.yml:3:14: announcements[0].message: must be a string, not a mapping",
            "announcements.yml:4:16: announcements[0].migration: must be a mapping, not a sequence",
            "announcements.yml:6:17: announcements[1].migration.required: `3` must be a bool",
            "announcements.yml:7:12: announcements[1].migration.tag: must be a string, not a mapping",
        ]
    );
    assert_eq!(
        issues("announcements:\n  - migration:\n      required: [true]\n"),
        [
            "announcements.yml:3:17: announcements[0].migration.required: must be a bool, not a sequence"
        ]
    );
    assert_eq!(
        issues("announcements:\n  - migration:\n      required: maybe\n"),
        ["announcements.yml:3:17: announcements[0].migration.required: `maybe` must be a bool"]
    );
}

#[test]
fn a_tag_the_text_doesnt_fit_fails_the_file() {
    let found = issues("announcements:\n  - title: !!int abc\n");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].starts_with("announcements.yml:2:12: announcements[0].title: cannot decode"),
        "{found:?}"
    );
    let found = issues("announcements:\n  - migration:\n      required: !!int x\n");
    assert!(
        found[0].contains("announcements[0].migration.required: cannot decode"),
        "{found:?}"
    );
}

#[test]
fn what_ansibles_loader_refuses_fails_the_file() {
    let found = issues("announcements:\n  - title: a\n    title: b\n");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].starts_with("announcements.yml:3:5:"), "{found:?}");
    let found = issues("announcements: [\n");
    assert!(found[0].contains("syntax error"), "{found:?}");
    let found = issues("- a\n");
    assert!(found[0].contains("must be a mapping of keys"), "{found:?}");
}

#[test]
fn load_reads_a_file_and_a_missing_one_is_none() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("announcements.yml");
    assert!(load(&path).expect("missing is fine").is_none());
    std::fs::write(&path, SALTBOX_LIKE).expect("write");
    let read = load(&path).expect("loads").expect("some");
    assert_eq!(titles(&read), ["Breaking Changes"]);
}

#[test]
fn load_errors_are_sb_gos_words() {
    let dir = tempfile::tempdir().expect("temp dir");
    let err = load(dir.path()).expect_err("a directory");
    assert!(
        err.to_string()
            .starts_with("error reading announcement file: "),
        "{err}"
    );
    let path = dir.path().join("announcements.yml");
    std::fs::write(&path, "announcements: 1\n").expect("write");
    let err = load(&path).expect_err("invalid");
    assert_eq!(
        err.to_string(),
        "error parsing announcement YAML: announcements.yml:1:16: announcements: must be a sequence, not a scalar"
    );
    std::fs::write(&path, "announcements: [\n- a: [1, 2\n").expect("write");
    let err = load(&path).expect_err("invalid").to_string();
    assert!(
        err.starts_with("error parsing announcement YAML: announcements.yml:")
            && err.contains("syntax error"),
        "{err}"
    );
}

#[test]
fn several_issues_are_joined() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("announcements.yml");
    std::fs::write(&path, "announcements:\n  - 1\n  - 2\n").expect("write");
    let err = load(&path).expect_err("invalid").to_string();
    assert_eq!(
        err,
        "error parsing announcement YAML: announcements.yml:2:5: announcements[0]: must be a \
         mapping, not a scalar; announcements.yml:3:5: announcements[1]: must be a mapping, not a scalar"
    );
}

#[test]
fn no_after_file_shows_nothing() {
    assert_eq!(new_entries(Some(&[entry("2026-01-01", "a")]), None), []);
    assert_eq!(new_entries(None, None), []);
}

#[test]
fn no_before_file_or_an_empty_one_shows_every_entry() {
    let after = [entry("2026-01-02", "b"), entry("2026-01-01", "a")];
    assert_eq!(titles(&new_entries(None, Some(&after))), ["a", "b"]);
    assert_eq!(titles(&new_entries(Some(&[]), Some(&after))), ["a", "b"]);
}

#[test]
fn new_means_a_date_and_title_the_before_file_lacks() {
    let before = [entry("2026-01-01", "a"), entry("2026-01-02", "b")];
    let after = [
        entry("2026-01-01", "a").with_message("edited"),
        entry("2026-01-02", "b renamed"),
        entry("2026-01-05", "b"),
        entry("2026-01-03", "c"),
    ];
    assert_eq!(
        titles(&new_entries(Some(&before), Some(&after))),
        ["b renamed", "c", "b"]
    );
}

/// The viewer's list: every entry that applies, in the file's order,
/// each marked new or not by the same rule.
#[test]
fn classify_keeps_every_applicable_entry_marking_the_new_ones() {
    let before = [entry("2026-01-01", "a")];
    let dir = tempfile::tempdir().expect("dir");
    let mut gated = entry("2026-01-04", "gated");
    gated.required_file = dir.path().join("missing").display().to_string();
    let after = [
        entry("2026-01-03", "c"),
        entry("2026-01-01", "a"),
        gated,
        entry("2026-01-02", "b"),
    ];
    let marked = |listed: Vec<(Announcement, bool)>| -> Vec<(String, bool)> {
        listed.into_iter().map(|(a, new)| (a.title, new)).collect()
    };
    assert_eq!(
        marked(classify(Some(&before), Some(&after))),
        [("c".into(), true), ("a".into(), false), ("b".into(), true)]
    );
    assert!(classify(None, Some(&after)).iter().all(|(_, new)| *new));
    assert_eq!(classify(Some(&before), None), []);
    assert_eq!(
        titles(&only_new(classify(Some(&before), Some(&after)))),
        ["b", "c"]
    );
}

#[test]
fn the_identity_is_sb_gos_joined_text() {
    // `2026|x` + `y` and `2026` + `x|y` are one key in sb-go.
    let before = [entry("2026|x", "y")];
    assert_eq!(
        new_entries(Some(&before), Some(&[entry("2026", "x|y")])),
        []
    );
}

#[test]
fn equal_dates_keep_the_files_order() {
    let after = [
        entry("2026-01-02", "first"),
        entry("2026-01-01", "older"),
        entry("2026-01-02", "second"),
        entry("2026-01-02", "third"),
    ];
    assert_eq!(
        titles(&new_entries(None, Some(&after))),
        ["older", "first", "second", "third"]
    );
}

#[test]
fn a_required_folder_must_be_a_directory() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("file");
    std::fs::write(&file, "").expect("write");
    let link = dir.path().join("link");
    symlink(dir.path(), &link).expect("symlink");
    let gated = |path: &Path| Announcement {
        required_folder: path.display().to_string(),
        ..entry("2026-01-01", "a")
    };
    assert!(applies(&gated(dir.path())));
    assert!(applies(&gated(&link)), "a link to a directory");
    assert!(!applies(&gated(&file)));
    assert!(!applies(&gated(&dir.path().join("missing"))));
}

#[test]
fn a_required_file_must_be_no_directory() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("file");
    std::fs::write(&file, "").expect("write");
    let gated = |path: &Path| Announcement {
        required_file: path.display().to_string(),
        ..entry("2026-01-01", "a")
    };
    assert!(applies(&gated(&file)));
    assert!(!applies(&gated(dir.path())));
    assert!(!applies(&gated(&dir.path().join("missing"))));
}

#[test]
fn with_both_set_both_must_hold() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("file");
    std::fs::write(&file, "").expect("write");
    let missing = dir.path().join("missing");
    let gated = |folder: &Path, file: &Path| Announcement {
        required_folder: folder.display().to_string(),
        required_file: file.display().to_string(),
        ..entry("2026-01-01", "a")
    };
    assert!(applies(&gated(dir.path(), &file)));
    assert!(!applies(&gated(dir.path(), &missing)));
    assert!(!applies(&gated(&missing, &file)));
    assert!(applies(&entry("2026-01-01", "ungated")));
}

#[test]
fn a_path_that_cant_be_checked_doesnt_hide_it() {
    // sb-go hides an entry only for a path that doesn't exist (`os.IsNotExist`):
    // a file where a directory should be is ENOTDIR, which shows it.
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("file");
    std::fs::write(&file, "").expect("write");
    let under_file = file.join("below");
    let gated = Announcement {
        required_folder: under_file.display().to_string(),
        ..entry("2026-01-01", "a")
    };
    assert!(applies(&gated));
}

#[test]
fn gated_entries_are_left_out_of_the_new_ones() {
    let dir = tempfile::tempdir().expect("temp dir");
    let after = [
        Announcement {
            required_folder: dir.path().join("missing").display().to_string(),
            ..entry("2026-01-01", "hidden")
        },
        entry("2026-01-02", "shown"),
    ];
    assert_eq!(titles(&new_entries(None, Some(&after))), ["shown"]);
    assert_eq!(titles(&new_entries(Some(&[]), Some(&after))), ["shown"]);
    assert_eq!(
        titles(&new_entries(Some(&[entry("x", "y")]), Some(&after))),
        ["shown"]
    );
}

fn migrating(date: &str, title: &str, required: bool, tag: &str) -> Announcement {
    Announcement {
        migration: Migration {
            required,
            tag: tag.to_string(),
        },
        ..entry(date, title)
    }
}

#[test]
fn a_migration_needs_required_and_a_tag() {
    let shown = [(
        Repo::Saltbox,
        vec![
            migrating("2026-01-01", "a", true, "one"),
            migrating("2026-01-02", "b", false, "two"),
            migrating("2026-01-03", "c", true, ""),
            entry("2026-01-04", "d"),
        ],
    )];
    assert_eq!(
        migrations(&shown),
        [Pending {
            repo: Repo::Saltbox,
            tag: "one".into(),
            date: "2026-01-01".into()
        }]
    );
}

#[test]
fn migrations_go_by_date_saltboxs_first_on_equal_dates() {
    let shown = [
        (
            Repo::Saltbox,
            vec![
                migrating("2026-01-03", "a", true, "s3"),
                migrating("2026-01-02", "b", true, "s2"),
            ],
        ),
        (
            Repo::Sandbox,
            vec![
                migrating("2026-01-02", "c", true, "x2"),
                migrating("2026-01-01", "d", true, "x1"),
            ],
        ),
    ];
    let pending = migrations(&shown);
    let listed: Vec<(Repo, &str)> = pending.iter().map(|p| (p.repo, p.tag.as_str())).collect();
    assert_eq!(
        listed,
        [
            (Repo::Sandbox, "x1"),
            (Repo::Saltbox, "s2"),
            (Repo::Sandbox, "x2"),
            (Repo::Saltbox, "s3"),
        ]
    );
}

#[test]
fn the_heading_spells_the_date_out() {
    assert_eq!(
        heading(Repo::Saltbox, "2026-02-01"),
        "Saltbox Announcement - February 1, 2026"
    );
    assert_eq!(
        heading(Repo::Sandbox, "2024-12-25"),
        "Sandbox Announcement - December 25, 2024"
    );
    assert_eq!(
        heading(Repo::Saltbox, "0099-01-02"),
        "Saltbox Announcement - January 2, 0099"
    );
    assert_eq!(
        heading(Repo::Saltbox, "2024-02-29"),
        "Saltbox Announcement - February 29, 2024"
    );
}

#[test]
fn a_date_go_wouldnt_parse_is_shown_as_written() {
    for date in [
        "",
        "soon",
        "2026-2-01",
        "2026-02-1",
        "2026-02-01 ",
        " 2026-02-01",
        "2026-02-01x",
        "2026/02/01",
        "+026-02-01",
        "2026-13-01",
        "2026-00-10",
        "2026-02-30",
        "2025-02-29",
        "2026-01-00",
        "20260-01-01",
    ] {
        assert_eq!(
            heading(Repo::Saltbox, date),
            format!("Saltbox Announcement - {date}"),
            "{date:?}"
        );
    }
}

#[test]
fn repos_are_named_as_sb_go_spells_them() {
    assert_eq!(Repo::from_name("Saltbox"), Some(Repo::Saltbox));
    assert_eq!(Repo::from_name("Sandbox"), Some(Repo::Sandbox));
    assert_eq!(Repo::from_name("saltbox"), None);
    assert_eq!(Repo::from_name(""), None);
    assert_eq!(Repo::Saltbox.name(), "Saltbox");
    assert_eq!(Repo::Sandbox.name(), "Sandbox");
}

#[derive(clap::Parser)]
struct Wrapper {
    #[command(flatten)]
    args: AnnouncementsArgs,
}

fn args(line: &[&str]) -> AnnouncementsArgs {
    Wrapper::try_parse_from(std::iter::once("announcements").chain(line.iter().copied()))
        .expect("parses")
        .args
}

/// No flag is required (decision 27, amended): without `--before` every
/// announcement is new, without `--after` the repository's own file is
/// read; `--repo` defaults to Saltbox, as sb-go's.
#[test]
fn no_flag_is_required() {
    let parsed = args(&[]);
    assert_eq!(parsed.before, None);
    assert_eq!(parsed.after, None);
    assert_eq!(parsed.repo, "Saltbox");
    let parsed = args(&["-b", "x", "-a", "y", "-r", "Sandbox"]);
    assert_eq!(parsed.before.as_deref(), Some(Path::new("x")));
    assert_eq!(parsed.after.as_deref(), Some(Path::new("y")));
    assert_eq!(parsed.repo, "Sandbox");
    let parsed = args(&["--before", "p", "--after", "q", "--repo", "Saltbox"]);
    assert_eq!(parsed.before.as_deref(), Some(Path::new("p")));
    assert_eq!(parsed.after.as_deref(), Some(Path::new("q")));
}

/// There is no `--migrations` any more: migrations are `sb update`'s.
#[test]
fn migrations_is_no_flag() {
    assert!(Wrapper::try_parse_from(["announcements", "--migrations", "run"]).is_err());
}

/// Saltbox's file is at the root; Sandbox's under the inventory's
/// appdata path, `/opt` when the inventory doesn't say.
#[test]
fn each_repository_has_its_own_file() {
    let root = tempfile::tempdir().expect("root");
    assert_eq!(
        own_file(Repo::Saltbox, root.path()),
        root.path().join("announcements.yml")
    );
    assert_eq!(
        own_file(Repo::Sandbox, root.path()),
        Path::new("/opt/sandbox/announcements.yml")
    );
    let inventory = root.path().join("inventories/host_vars");
    std::fs::create_dir_all(&inventory).expect("inventory dir");
    let appdata = root.path().join("appdata");
    std::fs::write(
        inventory.join("localhost.yml"),
        format!("server_appdata_path: {}\n", appdata.display()),
    )
    .expect("inventory");
    assert_eq!(
        own_file(Repo::Sandbox, root.path()),
        appdata.join("sandbox/announcements.yml")
    );
}

impl Announcement {
    fn with_message(mut self, message: &str) -> Self {
        message.clone_into(&mut self.message);
        self
    }
}

/// A tagged collection is decoded as the collection itself, as yaml.v3
/// decodes a sequence or mapping node whatever its tag (saphyr's loader
/// keeps a tag it doesn't resolve, Ansible's `!unsafe`, as `Tagged`).
#[test]
fn a_tagged_collection_is_the_collection() {
    let text = "announcements: !unsafe\n  - !unsafe\n    title: t\n    \
                migration: !unsafe {required: true, tag: x}\n";
    let read = parsed(text);
    assert_eq!(read.len(), 1, "{read:?}");
    assert_eq!(read[0].title, "t");
    assert_eq!(
        read[0].migration,
        Migration {
            required: true,
            tag: "x".into()
        }
    );
}
