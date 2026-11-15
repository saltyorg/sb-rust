//! The dialect's tests: sb-go's facts/*_test.go cases at the document
//! level (the session's come with it), the fixture corpus, Saltbox's own
//! reader as the consumer check, and a property test over generated
//! documents.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{Document, DocumentError, Edit};
use crate::facts::{Fact, Instance, Role};

fn parse(text: &str) -> Document {
    Document::parse(text.as_bytes()).expect("the text parses")
}

fn apply(text: &str, edits: &[Edit]) -> Result<String, DocumentError> {
    parse(text)
        .apply(edits)
        .map(|bytes| String::from_utf8(bytes).expect("the result is UTF-8"))
}

fn set(instance: &str, key: &str, value: &str) -> Edit {
    Edit::Set {
        instance: instance.to_owned(),
        key: key.to_owned(),
        value: value.to_owned(),
    }
}

fn delete_fact(instance: &str, key: &str) -> Edit {
    Edit::DeleteFact {
        instance: instance.to_owned(),
        key: key.to_owned(),
    }
}

fn delete_instance(instance: &str) -> Edit {
    Edit::DeleteInstance {
        instance: instance.to_owned(),
    }
}

/// The instances of `text` as name → key → value.
fn values(text: &str) -> BTreeMap<String, BTreeMap<String, String>> {
    parse(text)
        .role("role")
        .instances
        .into_iter()
        .map(|instance| {
            let facts = instance
                .facts
                .into_iter()
                .map(|fact| (fact.key, fact.value))
                .collect();
            (instance.name, facts)
        })
        .collect()
}

fn fact(key: &str, value: &str) -> Fact {
    Fact {
        key: key.to_owned(),
        value: value.to_owned(),
    }
}

/// sb-go `session_test.go`'s `openFixture` file.
const FIXTURE: &str = "; header\nhidden = secret\n[default]\n; token comment\ntoken = original\nempty =\nnone = None\n[empty]\n[other]\nk = untouched\n";

// sb-go TestCatalogPreservesLiteralAndEmptyInstances
#[test]
fn the_catalog_sorts_instances_and_keys_and_keeps_empty_ones() {
    let role =
        parse("; retained\nsecret = default-value\n[zulu]\nz = None\na =\n[empty]\n").role("alpha");
    assert_eq!(
        role,
        Role {
            name: "alpha".to_owned(),
            instances: vec![
                Instance {
                    name: "empty".to_owned(),
                    facts: vec![],
                },
                Instance {
                    name: "zulu".to_owned(),
                    facts: vec![fact("a", ""), fact("z", "None")],
                },
            ],
        }
    );
}

// sb-go TestCatalogAcceptsDefaultSectionAfterAnInstance
#[test]
fn a_default_section_after_an_instance_stays_hidden() {
    let role = parse("[default]\nk=v\n[DEFAULT]\nhidden=secret\n").role("role");
    assert_eq!(role.instances.len(), 1);
    assert_eq!(role.instances[0].name, "default");
}

#[test]
fn a_first_default_header_names_the_implicit_section() {
    let doc = parse("# top\n[DEFAULT]\nhidden = x\n[app]\nk = v\n");
    assert_eq!(doc.sections.len(), 2);
    assert_eq!((doc.sections[0].start, doc.sections[0].end), (1, 3));
    assert_eq!(doc.role("role").instances.len(), 1);
    // Only the first header, with no key before it, names it.
    assert_eq!(
        parse("hidden = x\n[DEFAULT]\nh = y\n[a]\n").sections.len(),
        3
    );
    let doc = parse("[a]\n[DEFAULT]\nhidden = x\n");
    assert_eq!(doc.sections.len(), 3);
    assert_eq!(values("[a]\n[DEFAULT]\nhidden = x\n")["a"], BTreeMap::new());
    // A second header named DEFAULT is a duplicate.
    assert_eq!(
        Document::parse(b"[DEFAULT]\n[DEFAULT]\n").unwrap_err(),
        DocumentError::DuplicateInstance("DEFAULT".to_owned())
    );
}

// sb-go TestCatalogRejectsDuplicateKeysAndInstances
#[test]
fn duplicate_keys_and_instances_are_refused() {
    assert_eq!(
        Document::parse(b"[default]\nk=a\nk=b\n").unwrap_err(),
        DocumentError::DuplicateKey {
            instance: "default".to_owned(),
            key: "k".to_owned(),
        }
    );
    assert_eq!(
        Document::parse(b"[default]\nk=a\n[default]\nj=b\n").unwrap_err(),
        DocumentError::DuplicateInstance("default".to_owned())
    );
    assert_eq!(
        DocumentError::DuplicateKey {
            instance: "a\u{1b}".to_owned(),
            key: "k\"".to_owned(),
        }
        .to_string(),
        "duplicate key \"k\\\"\" in a\\u{1b}"
    );
}

// sb-go TestOpenSessionRejectsUnsafeInputs, "malformed role"
#[test]
fn malformed_sections_and_lines_are_refused_with_their_line() {
    for (text, error) in [
        ("[broken\nk=v\n", DocumentError::MalformedSection(1)),
        ("[a]\n[]\n", DocumentError::MalformedSection(2)),
        ("[a] tail\n", DocumentError::SectionSuffix(1)),
        ("[a]\n[b]]\n", DocumentError::InvalidSection(2)),
        ("[a[b]\n", DocumentError::InvalidSection(1)),
        ("[a\u{7}]\n", DocumentError::InvalidSection(1)),
        ("[a]\nno delimiter\n", DocumentError::InvalidFact(2)),
        ("[a]\n = v\n", DocumentError::InvalidFact(2)),
        ("[a]\nk\u{7} = v\n", DocumentError::InvalidFact(2)),
    ] {
        assert_eq!(
            Document::parse(text.as_bytes()).unwrap_err(),
            error,
            "{text:?}"
        );
    }
    // A comment after the header is fine.
    assert_eq!(values("[a] # c\n[b] ; c\n").len(), 2);
    assert_eq!(
        DocumentError::SectionSuffix(3).to_string(),
        "unexpected section suffix at line 3"
    );
}

#[test]
fn text_that_is_not_utf8_or_holds_a_nul_is_refused() {
    assert_eq!(
        Document::parse(b"[a]\nk = \xff\n").unwrap_err(),
        DocumentError::NotText
    );
    assert_eq!(
        Document::parse(b"[a]\nk = a\0b\n").unwrap_err(),
        DocumentError::NotText
    );
    assert_eq!(
        DocumentError::NotText.to_string(),
        "facts must be UTF-8 text without NUL"
    );
}

#[test]
fn an_empty_file_is_an_empty_role() {
    let doc = parse("");
    assert_eq!(doc.role("role").instances, vec![]);
    assert_eq!(doc.apply(&[]).unwrap(), b"");
}

// sb-go TestApplyMultilineRoundtripRejectsLostCommentLines
#[test]
fn a_value_line_that_would_read_as_a_comment_is_refused() {
    let written = apply(FIXTURE, &[set("default", "token", "first\nsecond")]).unwrap();
    assert!(written.contains("token = first\n\tsecond\n"), "{written}");
    assert_eq!(
        apply(FIXTURE, &[set("default", "token", "first\n# lost")]).unwrap_err(),
        DocumentError::Lossy {
            instance: "default".to_owned(),
            key: "token".to_owned(),
        }
    );
    assert_eq!(
        DocumentError::Lossy {
            instance: "a".to_owned(),
            key: "b".to_owned(),
        }
        .to_string(),
        "value for a/b cannot be represented without loss"
    );
}

#[test]
fn whitespace_a_value_cannot_hold_is_refused() {
    for value in [" lead", "trail ", "a\n\nb", "a\n", "a\n b"] {
        assert_eq!(
            apply(FIXTURE, &[set("default", "token", value)]).unwrap_err(),
            DocumentError::Whitespace,
            "{value:?}"
        );
    }
    // A single empty value is fine.
    let written = apply(FIXTURE, &[set("default", "token", "")]).unwrap();
    assert!(written.contains("token = \n"), "{written}");
}

#[test]
fn a_multi_line_value_on_an_indented_key_is_refused() {
    // A tab is no deeper than the key's own indent, so the line would
    // read as a key: the result doesn't parse.
    assert_eq!(
        apply("[a]\n\n    k = v\n", &[set("a", "k", "one\ntwo")]).unwrap_err(),
        DocumentError::Merged
    );
}

#[test]
fn an_edit_that_would_change_another_fact_is_refused() {
    // The indented header follows a blank line; with the instance before it
    // gone, it would continue `k`'s value.
    let original = "[a]\nk = v\n[b]\n\n [c]\nj = w\n";
    assert_eq!(values(original).len(), 3);
    assert_eq!(
        apply(original, &[delete_instance("b")]).unwrap_err(),
        DocumentError::Merged
    );
    // A key added before it would take it as a continuation.
    assert_eq!(
        apply(original, &[set("b", "n", "x")]).unwrap_err(),
        DocumentError::Lossy {
            instance: "b".to_owned(),
            key: "n".to_owned(),
        }
    );
    // Deleting the instance before a blank line ends the value is fine.
    assert_eq!(
        apply(original, &[delete_instance("a")]).unwrap(),
        "[b]\n\n [c]\nj = w\n"
    );
    assert_eq!(
        DocumentError::Merged.to_string(),
        "the changes cannot be written without altering other facts in the file"
    );
}

// sb-go TestApplyTypedChangesPreservesMetadataAndUnrelatedData
#[test]
fn edits_keep_comments_hidden_values_and_literals() {
    let written = apply(
        FIXTURE,
        &[
            set("default", "token", "changed"),
            set("empty", "added", "new"),
            delete_fact("default", "empty"),
            delete_instance("other"),
        ],
    )
    .unwrap();
    assert_eq!(
        written,
        "; header\nhidden = secret\n[default]\n; token comment\ntoken = changed\nnone = None\n[empty]\nadded = new\n"
    );
    let role = parse(&written).role("role");
    assert_eq!(role.instances.len(), 2);
    assert_eq!(role.instances[0].name, "default");
    assert_eq!(role.instances[1].facts, vec![fact("added", "new")]);
}

// sb-go TestApplyKeepsEmptySectionsAndNoopBytes
#[test]
fn setting_a_value_to_itself_changes_nothing_and_empty_instances_stay() {
    assert_eq!(
        apply(FIXTURE, &[set("default", "token", "original")]).unwrap(),
        FIXTURE
    );
    let written = apply(FIXTURE, &[delete_fact("other", "k")]).unwrap();
    let role = parse(&written).role("role");
    assert_eq!(role.instances.len(), 3);
    assert_eq!(role.instances[2].facts, vec![]);
}

// sb-go TestApplyAdjacentInsertionAndDeletionIsIndependentOfChangeOrder
#[test]
fn an_insertion_before_a_deleted_instance_is_kept_in_either_order() {
    for edits in [
        [delete_instance("other"), set("empty", "added", "new")],
        [set("empty", "added", "new"), delete_instance("other")],
    ] {
        let written = apply(FIXTURE, &edits).unwrap();
        assert!(written.ends_with("[empty]\nadded = new\n"), "{written}");
    }
}

// sb-go TestEditingMultilineFactPreservesFollowingComment
#[test]
fn editing_a_multi_line_value_keeps_the_comment_after_it() {
    let written = apply(
        "[default]\nk = first\n\tsecond\n# preserve me\n",
        &[set("default", "k", "new")],
    )
    .unwrap();
    assert_eq!(written, "[default]\nk = new\n# preserve me\n");
}

// sb-go TestApplyRejectsInvalidAndConflictingChanges, the document's cases
#[test]
fn invalid_and_conflicting_edits_are_refused() {
    let unknown = |name: &str| DocumentError::UnknownInstance(name.to_owned());
    let key_name = |name: &str| DocumentError::KeyName(name.to_owned());
    let conflict = DocumentError::Conflict("default".to_owned());
    for (edits, error) in [
        (vec![set("missing", "k", "v")], unknown("missing")),
        (vec![set("DEFAULT", "k", "v")], unknown("DEFAULT")),
        (vec![delete_instance("DEFAULT")], unknown("DEFAULT")),
        (vec![delete_fact("", "k")], unknown("")),
        (vec![set("default", ";hidden", "v")], key_name(";hidden")),
        (vec![set("default", "#hidden", "v")], key_name("#hidden")),
        (
            vec![set("default", "[injected]", "v")],
            key_name("[injected]"),
        ),
        (
            vec![set("default", "k\ninjected", "v")],
            key_name("k\ninjected"),
        ),
        (vec![set("default", "a=b", "v")], key_name("a=b")),
        (vec![set("default", " k", "v")], key_name(" k")),
        (vec![set("default", "", "v")], key_name("")),
        (
            vec![set("default", "k", "v\0injected")],
            DocumentError::ValueCharacters,
        ),
        (
            vec![set("default", "token", "v\rinjected")],
            DocumentError::ValueCharacters,
        ),
        (
            vec![delete_instance("default"), set("default", "k", "v")],
            conflict.clone(),
        ),
        (
            vec![set("default", "k", "v"), delete_instance("default")],
            conflict.clone(),
        ),
        (
            vec![set("default", "k", "v"), set("default", "k", "other")],
            conflict.clone(),
        ),
        (
            vec![
                set("default", "token", "v"),
                delete_fact("default", "token"),
            ],
            conflict.clone(),
        ),
        (
            vec![delete_fact("default", "missing")],
            DocumentError::UnknownKey("missing".to_owned()),
        ),
    ] {
        assert_eq!(apply(FIXTURE, &edits).unwrap_err(), error, "{edits:?}");
    }
    // The same key in two instances, or two keys of one, don't conflict.
    let written = apply(
        FIXTURE,
        &[
            set("default", "k", "a"),
            set("empty", "k", "b"),
            set("default", "j", "c"),
            delete_instance("other"),
        ],
    )
    .unwrap();
    assert_eq!(values(&written)["empty"]["k"], "b");
    assert_eq!(
        DocumentError::KeyName("a\nb".to_owned()).to_string(),
        "key name \"a\\nb\" contains unsupported characters"
    );
}

// sb-go TestApplyBracketContainingKeys
#[test]
fn keys_with_brackets_are_updated_deleted_and_added() {
    let key = "headers[Authorization]";
    assert_eq!(
        apply(
            "[default]\nheaders[Authorization] = old\n",
            &[set("default", key, "new")]
        )
        .unwrap(),
        "[default]\nheaders[Authorization] = new\n"
    );
    assert_eq!(
        apply(
            "[default]\nheaders[Authorization] = old\n",
            &[delete_fact("default", key)]
        )
        .unwrap(),
        "[default]\n"
    );
    assert_eq!(
        apply("[default]\n", &[set("default", key, "new")]).unwrap(),
        "[default]\n\nheaders[Authorization] = new\n"
    );
}

// sb-go TestApplyPreservesPythonLiteralValuesAndUntouchedBytes
#[test]
fn literal_values_read_as_python_reads_them_and_other_bytes_stay() {
    let original = "# header\n[DEFAULT]\nhidden = `literal`\n[default]\nquoted = \"quoted\"\nbacktick = `literal`\ntriple = \"\"\"literal\"\"\"\nslash = end\\\ncolon:key = literal # ; value\nmultiline = first\n\tsecond\nchange=old\n\n[empty]\n";
    let want: BTreeMap<String, String> = [
        ("quoted", "\"quoted\""),
        ("backtick", "`literal`"),
        ("triple", "\"\"\"literal\"\"\""),
        ("slash", "end\\"),
        ("colon:key", "literal # ; value"),
        ("multiline", "first\nsecond"),
        ("change", "old"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    assert_eq!(values(original)["default"], want);
    assert_eq!(
        apply(original, &[set("default", "change", "new")]).unwrap(),
        original.replace("change=old", "change=new")
    );
}

#[test]
fn an_edited_key_keeps_its_spacing() {
    assert_eq!(
        apply("[a]\n  k  =   old  \nj=v\n", &[set("a", "k", "new")]).unwrap(),
        "[a]\n  k  =   new\nj=v\n"
    );
    assert_eq!(
        apply("[a]\nk =   \n", &[set("a", "k", "new")]).unwrap(),
        "[a]\nk =   new\n"
    );
}

#[test]
fn crlf_files_stay_crlf() {
    let original = "[a]\r\nk = v\r\nj = w\r\n";
    assert_eq!(
        apply(original, &[set("a", "k", "one\ntwo"), set("a", "n", "x")]).unwrap(),
        "[a]\r\nk = one\r\n\ttwo\r\nj = w\r\n\r\nn = x\r\n"
    );
    assert_eq!(values(original)["a"]["k"], "v");
}

#[test]
fn a_file_without_a_final_newline_keeps_it_missing() {
    assert_eq!(
        apply("[a]\nk = v", &[set("a", "k", "w")]).unwrap(),
        "[a]\nk = w"
    );
    assert_eq!(
        apply("[a]\nk = v", &[set("a", "n", "x")]).unwrap(),
        "[a]\nk = v\nn = x\n"
    );
    assert_eq!(
        apply("[a]\nk = v\n[b]", &[set("a", "n", "x")]).unwrap(),
        "[a]\nk = v\nn = x\n[b]"
    );
}

// sb-go TestCommentEndsContinuationMatchesPython
#[test]
fn a_comment_ends_a_continuation() {
    let original = "[default]\nk = first\n# comment\n    second = independent\n";
    assert_eq!(
        values(original)["default"],
        [("k", "first"), ("second", "independent")]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect::<BTreeMap<_, _>>()
    );
    let edited = apply(original, &[set("default", "k", "new")]).unwrap();
    assert_eq!(
        edited,
        "[default]\nk = new\n# comment\n    second = independent\n"
    );
    let deleted = apply(original, &[delete_fact("default", "k")]).unwrap();
    assert_eq!(deleted, "[default]\n# comment\n    second = independent\n");
    for text in [original, edited.as_str(), deleted.as_str()] {
        assert_eq!(python_reads(text), ours(text), "{text}");
    }
}

// sb-go TestApplyMatchesSaltboxPythonReader
#[test]
fn saltboxs_reader_reads_what_the_writer_writes() {
    let original = "# preserved\n[DEFAULT]\nhidden = `literal`\n[default]\nquoted = \"quoted\"\nbacktick = `literal`\ncolon:key = literal # ; value\nchange = old\n[empty]\n";
    let written = apply(
        original,
        &[
            set("default", "change", "first\nsecond"),
            set("empty", "new", "None"),
        ],
    )
    .unwrap();
    let python = python_reads(&written);
    assert_eq!(python["default"]["change"], "first\nsecond");
    assert_eq!(python["DEFAULT"]["hidden"], "`literal`");
    assert_eq!(python["empty"]["new"], "None");
    assert_eq!(python, ours(&written));
}

/// Python's only comment prefix is `#`: an indented `;` line after a value
/// continues it, in Saltbox's reader and in sb's.
#[test]
fn an_indented_semicolon_line_after_a_value_continues_it() {
    let original = "[a]\nj = w\nk = v\n\t; x\n; comment\n";
    assert_eq!(values(original)["a"]["k"], "v\n; x");
    let without_legacy = original.replace("; comment\n", "");
    assert_eq!(python_reads(&without_legacy), ours(&without_legacy));
    let edited = apply(original, &[set("a", "k", "new")]).unwrap();
    assert_eq!(edited, "[a]\nj = w\nk = new\n; comment\n");
    let deleted = apply(original, &[delete_fact("a", "k")]).unwrap();
    assert_eq!(deleted, "[a]\nj = w\n; comment\n");
    let semicolon = apply(original, &[set("a", "j", "first\n; second")]).unwrap();
    assert!(
        semicolon.starts_with("[a]\nj = first\n\t; second\nk = v\n"),
        "{semicolon}"
    );
    for text in [&edited, &deleted, &semicolon] {
        let without_legacy = text.replace("; comment\n", "");
        assert_eq!(
            python_reads(&without_legacy),
            ours(&without_legacy),
            "{text}"
        );
    }
}

#[test]
fn a_lone_carriage_return_is_refused() {
    for (text, line) in [
        ("[a]\nk = a\r  x = y\n", 2),
        ("[a]\r\nk = v\r", 2),
        ("\r[a]\n", 1),
    ] {
        assert_eq!(
            Document::parse(text.as_bytes()).unwrap_err(),
            DocumentError::LoneCarriageReturn(line),
            "{text:?}"
        );
    }
    assert_eq!(
        DocumentError::LoneCarriageReturn(4).to_string(),
        "carriage return without a line feed at line 4"
    );
}

/// Indents count characters and whitespace is Python's `str.isspace`.
#[test]
fn indents_and_whitespace_are_pythons() {
    let original = "[a]\n  k = v\n\u{3000}x = y\nz = w\u{1f}\n";
    assert_eq!(values(original)["a"].len(), 3);
    assert_eq!(values(original)["a"]["z"], "w");
    assert_eq!(python_reads(original), ours(original));
    assert_eq!(
        apply(original, &[set("a", "z", "w\u{1f}")]).unwrap_err(),
        DocumentError::Whitespace
    );
}

#[test]
fn deleting_the_instance_before_a_later_default_header_is_fine() {
    assert_eq!(
        apply("[a]\nk = v\n[DEFAULT]\nh = x\n", &[delete_instance("a")]).unwrap(),
        "[DEFAULT]\nh = x\n"
    );
}

#[test]
fn a_key_may_repeat_in_another_instance() {
    assert_eq!(values("[a]\nk = 1\n[b]\nk = 2\n")["b"]["k"], "2");
}

/// No `Debug` form shows a value (tui spec decision 32: no fact value is
/// ever logged).
#[test]
fn debug_forms_leave_values_out() {
    const SECRET: &str = "not-a-real-secret";
    let text = format!("[a]\ntoken = {SECRET}\n");
    let shown = [
        format!("{:?}", set("a", "token", SECRET)),
        format!("{:?}", delete_fact("a", "token")),
        format!("{:?}", delete_instance("a")),
        format!("{:?}", parse(&text)),
        format!("{:?}", parse(&text).role("role")),
    ];
    for shown in &shown {
        assert!(!shown.contains(SECRET), "{shown}");
    }
    assert_eq!(shown[0], "Set { instance: \"a\", key: \"token\", .. }");
    assert_eq!(shown[1], "DeleteFact { instance: \"a\", key: \"token\" }");
    assert_eq!(shown[2], "DeleteInstance { instance: \"a\" }");
    assert_eq!(shown[3], "{\"DEFAULT\": [], \"a\": [\"token\"]}");
    assert_eq!(
        shown[4],
        "Role { name: \"role\", instances: [Instance { name: \"a\", facts: [Fact { key: \"token\", .. }] }] }"
    );
}

/// The fixture corpus: synthetic files in the forms Saltbox's writer
/// gives (`key = value`, `True`/`False`, tab continuations, a blank line
/// after each section) and the forms a hand edit gives.
fn corpus() -> Vec<(PathBuf, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/facts");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("the corpus directory")
        .map(|entry| entry.expect("a corpus entry").path())
        .collect();
    files.sort();
    assert!(files.len() >= 8, "the corpus is there");
    files
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path).expect("a corpus file");
            (path, text)
        })
        .collect()
}

#[test]
fn every_corpus_file_round_trips_byte_for_byte() {
    for (path, text) in corpus() {
        let doc = Document::parse(text.as_bytes())
            .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        assert_eq!(
            doc.apply(&[]).unwrap(),
            text.as_bytes(),
            "{}",
            path.display()
        );
    }
}

#[test]
fn an_edit_of_a_corpus_key_changes_only_its_lines() {
    for (path, text) in corpus() {
        let doc = parse(&text);
        for section in doc
            .sections
            .iter()
            .filter(|section| section.name != "DEFAULT")
        {
            for entry in &section.keys {
                let before: String = doc.lines[..entry.start].concat();
                let after: String = doc.lines[entry.end..].concat();
                let at = format!("{} [{}] {}", path.display(), section.name, entry.name);
                for edit in [
                    set(&section.name, &entry.name, "edited"),
                    delete_fact(&section.name, &entry.name),
                ] {
                    let written =
                        String::from_utf8(doc.apply(std::slice::from_ref(&edit)).unwrap()).unwrap();
                    assert!(
                        written.starts_with(&before) && written.ends_with(&after),
                        "{at}: {written:?}"
                    );
                    let mut want = values(&text);
                    let facts = want.get_mut(&section.name).unwrap();
                    match edit {
                        Edit::Set { .. } => facts.insert(entry.name.clone(), "edited".to_owned()),
                        _ => facts.remove(&entry.name),
                    };
                    assert_eq!(values(&written), want, "{at}");
                }
            }
        }
    }
}

#[test]
fn saltboxs_reader_reads_the_corpus_as_we_do() {
    // `;` comments and keys before the first header are sb's dialect only
    // (Known limits).
    let texts: Vec<(PathBuf, String)> = corpus()
        .into_iter()
        .filter(|(path, _)| {
            !path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("sb-only-"))
        })
        .collect();
    let python = python_reads_all(texts.iter().map(|(_, text)| text.as_str()));
    for ((path, text), python) in texts.iter().zip(python) {
        assert_eq!(python, Some(ours(text)), "{}", path.display());
    }
}

type Sections = BTreeMap<String, BTreeMap<String, String>>;

/// Each section's values as sb reads them, `DEFAULT` as one entry, as
/// [`python_reads_all`] gives Saltbox's.
fn ours(text: &str) -> Sections {
    parse(text).facts().into_iter().collect()
}

/// Each text read as Saltbox's module reads a fact file: `configparser`
/// with its settings (`saltbox_facts.py`'s `create_config_parser`, as
/// sb-go's `readSaltboxPythonFacts`), from a UTF-8 text stream with
/// universal newlines (its `open(path, encoding="utf-8")`), each section's
/// own keys (its `_sections`, no `DEFAULT` inherited) and `DEFAULT` as its
/// own entry. `None` where Python refuses the text. One python3 for all.
/// Saltbox runs where python3 is, so the test needs it too.
fn python_reads_all<'a>(texts: impl IntoIterator<Item = &'a str>) -> Vec<Option<Sections>> {
    const SCRIPT: &str = r"import configparser, io, json, sys
out = []
for text in json.load(sys.stdin):
    c = configparser.ConfigParser(interpolation=None, comment_prefixes=('#',), inline_comment_prefixes=None, default_section='DEFAULT', delimiters=('=',), empty_lines_in_values=False)
    c.optionxform = str
    try:
        c.read_file(io.TextIOWrapper(io.BytesIO(text.encode('utf-8')), encoding='utf-8'))
    except configparser.Error:
        out.append(None)
        continue
    sections = {s: dict(c._sections[s]) for s in c.sections()}
    sections['DEFAULT'] = dict(c._defaults)
    out.append(sections)
print(json.dumps(out))
";
    use std::io::Write as _;
    let texts: Vec<&str> = texts.into_iter().collect();
    let mut child = std::process::Command::new("python3")
        .args(["-c", SCRIPT])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("python3 runs (Saltbox's reader is the consumer check)");
    let input = serde_json::to_vec(&texts).expect("the texts as JSON");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(&input)
        .expect("python3 reads the texts");
    let output = child.wait_with_output().expect("python3 finishes");
    assert!(output.status.success(), "python3 failed");
    serde_json::from_slice(&output.stdout).expect("python3's JSON")
}

fn python_reads(text: &str) -> Sections {
    python_reads_all([text])
        .pop()
        .flatten()
        .unwrap_or_else(|| panic!("python3 refused {text:?}"))
}

/// A generated fact file and what it holds.
struct Generated {
    /// Lines with their newlines, the last perhaps without.
    lines: Vec<String>,
    /// For each key or continuation line, its key's indent in characters.
    value_lines: Vec<Option<usize>>,
    /// Each section in file order.
    sections: Vec<GeneratedSection>,
}

/// A section: its name, its header line, the line after its last, and
/// its keys.
struct GeneratedSection {
    name: String,
    header: usize,
    end: usize,
    keys: BTreeMap<String, GeneratedKey>,
}

/// A key: its value, its lines, and whether its line is indented.
struct GeneratedKey {
    value: String,
    start: usize,
    end: usize,
    indented: bool,
}

impl Generated {
    fn text(&self) -> String {
        self.lines.concat()
    }

    fn values(&self) -> Sections {
        self.sections
            .iter()
            .filter(|section| section.name != "DEFAULT")
            .map(|section| {
                let keys = section
                    .keys
                    .iter()
                    .map(|(key, generated)| (key.clone(), generated.value.clone()))
                    .collect();
                (section.name.clone(), keys)
            })
            .collect()
    }
}

fn pick<'a>(rng: &mut fastrand::Rng, items: &[&'a str]) -> &'a str {
    items[rng.usize(..items.len())]
}

/// A value line without surrounding whitespace: letters and the
/// characters Saltbox's reader keeps literally.
fn word(rng: &mut fastrand::Rng) -> String {
    const CHARS: &[char] = &[
        'a', 'z', '0', '9', '=', '#', ';', ':', '"', '\'', '[', ']', '`', '\\', '/', '.', 'é', '€',
    ];
    let len = rng.usize(1..8);
    let mut word: String = (0..len).map(|_| CHARS[rng.usize(..CHARS.len())]).collect();
    if len > 2 && rng.bool() {
        let second = word.chars().next().map_or(0, char::len_utf8);
        word.insert(second, ' ');
    }
    word
}

/// A continuation line's text: never one that reads as a `#` comment.
fn continuation(rng: &mut fastrand::Rng) -> String {
    let mut text = word(rng);
    if text.starts_with('#') {
        text.insert(0, 'x');
    }
    text
}

/// A generated fact file. With `python`, only the forms Saltbox's reader
/// accepts too: no `;` comment and no key before the first header.
fn generate(rng: &mut fastrand::Rng, python: bool) -> Generated {
    let newline = if rng.bool() { "\r\n" } else { "\n" };
    let comments: &[&str] = if python {
        &["", "   ", "# c", "  # c", "\u{3000}# c"]
    } else {
        &["", "   ", "# c", "  # c", "; c"]
    };
    let mut lines: Vec<String> = Vec::new();
    let mut value_lines = Vec::new();
    let mut sections: Vec<GeneratedSection> = Vec::new();
    // Whether the last line ended any value, so an indented line after it
    // is no continuation.
    let ended = |value_lines: &[Option<usize>]| value_lines.last().is_none_or(Option::is_none);
    for _ in 0..rng.usize(0..3) {
        lines.push(format!("{}{newline}", pick(rng, comments)));
        value_lines.push(None);
    }
    let default_header = rng.u8(..4) == 0;
    for s in 0..rng.usize(1..5) {
        let name = if s == 0 && default_header {
            "DEFAULT".to_owned()
        } else {
            format!("s{s}{}", pick(rng, &["", " copy", ".2", "-x", "é"]))
        };
        let header = lines.len();
        if let Some(previous) = sections.last_mut() {
            previous.end = header;
        }
        // An indented header right after a value would continue it.
        let indent = if ended(&value_lines) {
            pick(rng, &["", " "])
        } else {
            ""
        };
        lines.push(format!(
            "{indent}[{name}]{}{newline}",
            pick(rng, &["", " # c", " ; c", "  "])
        ));
        value_lines.push(None);
        let mut keys = BTreeMap::new();
        for k in 0..rng.usize(0..5) {
            let key = format!("k{k}{}", pick(rng, &["", "[a]", ":b", ".c", " d"]));
            let key_indent = if ended(&value_lines) {
                pick(rng, &["", "", " ", "  "])
            } else {
                ""
            };
            let generated = push_key(rng, &mut lines, &key, key_indent, newline);
            value_lines.resize(generated.end, Some(key_indent.chars().count()));
            keys.insert(key, generated);
            for _ in 0..rng.usize(0..2) {
                lines.push(format!("{}{newline}", pick(rng, comments)));
                value_lines.push(None);
            }
        }
        sections.push(GeneratedSection {
            name,
            header,
            end: 0,
            keys,
        });
    }
    if rng.bool()
        && let Some(last) = lines.last_mut()
        && !last.trim().is_empty()
    {
        last.truncate(last.trim_end_matches(['\r', '\n']).len());
    }
    let total = lines.len();
    if let Some(last) = sections.last_mut() {
        last.end = total;
    }
    Generated {
        lines,
        value_lines,
        sections,
    }
}

/// A key line `key` at `key_indent` and its continuation lines, pushed
/// onto `lines`.
fn push_key(
    rng: &mut fastrand::Rng,
    lines: &mut Vec<String>,
    key: &str,
    key_indent: &str,
    newline: &str,
) -> GeneratedKey {
    let first = if rng.u8(..8) == 0 {
        String::new()
    } else {
        word(rng)
    };
    let mut value = first.clone();
    let start = lines.len();
    lines.push(format!(
        "{key_indent}{key}{}{first}{}{newline}",
        pick(rng, &["=", " = ", "  =  ", " =", "= "]),
        pick(rng, &["", " ", "\t", "\u{1f}"])
    ));
    for _ in 0..rng.usize(0..3) {
        let more = continuation(rng);
        value.push('\n');
        value.push_str(&more);
        lines.push(format!(
            "{key_indent}{}{more}{}{newline}",
            pick(rng, &["\t", "  ", "    ", "\u{3000}"]),
            pick(rng, &["", " "])
        ));
    }
    GeneratedKey {
        value,
        start,
        end: lines.len(),
        indented: !key_indent.is_empty(),
    }
}

/// A value the file can hold: lines without surrounding whitespace, the
/// later ones not read as comments; one line for an indented key, whose
/// tab continuation would be no deeper than it.
fn representable(rng: &mut fastrand::Rng, one_line: bool) -> String {
    let mut value = word(rng);
    if !one_line {
        for _ in 0..rng.usize(0..3) {
            value.push('\n');
            value.push_str(&continuation(rng));
        }
    }
    value
}

/// Each case's texts: the generated one and each edit's result.
fn edited_texts(seed: u64, python: bool) -> Vec<String> {
    let mut rng = fastrand::Rng::with_seed(seed);
    let generated = generate(&mut rng, python);
    let text = generated.text();
    let at = format!("seed {seed}: {text:?}");
    let doc = Document::parse(text.as_bytes()).unwrap_or_else(|err| panic!("{at}: {err}"));
    assert_eq!(doc.apply(&[]).unwrap(), text.as_bytes(), "{at}");
    assert_eq!(values(&text), generated.values(), "{at}");
    let mut texts = vec![text.clone()];

    let editable: Vec<&GeneratedSection> = generated
        .sections
        .iter()
        .filter(|section| section.name != "DEFAULT")
        .collect();
    let Some(section) = editable.get(rng.usize(..editable.len().max(1))) else {
        return texts;
    };
    let mut written = |edit: &Edit| {
        let bytes = doc
            .apply(std::slice::from_ref(edit))
            .unwrap_or_else(|err| panic!("{at}: {edit:?}: {err}"));
        let edited = String::from_utf8(bytes).unwrap();
        texts.push(edited.clone());
        edited
    };
    let lines = &generated.lines;
    // An indented header can follow its section's blank or comment line;
    // a key added, or a value left, before it would take it as a
    // continuation if it is deeper.
    let next_indent = lines.get(section.end).map_or(0, |line| {
        line.chars().take_while(|&c| super::is_space(c)).count()
    });

    // Add a key: the lines before the instance's end and after it stay.
    let value = representable(&mut rng, false);
    let add = set(&section.name, "added", &value);
    if next_indent > 0 {
        let error = doc.apply(std::slice::from_ref(&add)).unwrap_err();
        assert!(
            matches!(error, DocumentError::Lossy { .. } | DocumentError::Merged),
            "{at}: {error}"
        );
    } else {
        let added = written(&add);
        assert!(added.starts_with(&lines[..section.end].concat()), "{at}");
        assert!(added.ends_with(&lines[section.end..].concat()), "{at}");
        let mut want = generated.values();
        want.get_mut(&section.name)
            .unwrap()
            .insert("added".to_owned(), value);
        assert_eq!(values(&added), want, "{at}");
    }

    // Delete the instance: exactly its lines go, unless the next header
    // would continue the value before it.
    let delete = delete_instance(&section.name);
    let before = section
        .header
        .checked_sub(1)
        .and_then(|line| generated.value_lines[line]);
    if before.is_some_and(|indent| next_indent > indent) {
        assert_eq!(
            doc.apply(&[delete]).unwrap_err(),
            DocumentError::Merged,
            "{at}"
        );
    } else {
        let without = written(&delete);
        assert_eq!(
            without,
            lines[..section.header].concat() + &lines[section.end..].concat(),
            "{at}"
        );
    }

    let keys: Vec<_> = section.keys.iter().collect();
    let Some((key, generated_key)) = keys.get(rng.usize(..keys.len().max(1))) else {
        return texts;
    };
    // Set a key: the lines around it stay.
    let value = representable(&mut rng, generated_key.indented);
    let changed = written(&set(&section.name, key, &value));
    assert!(
        changed.starts_with(&lines[..generated_key.start].concat()),
        "{at}"
    );
    assert!(
        changed.ends_with(&lines[generated_key.end..].concat()),
        "{at}"
    );
    let mut want = generated.values();
    want.get_mut(&section.name)
        .unwrap()
        .insert((*key).clone(), value);
    assert_eq!(values(&changed), want, "{at}");

    // Delete a key: exactly its lines go.
    let deleted = written(&delete_fact(&section.name, key));
    assert_eq!(
        deleted,
        lines[..generated_key.start].concat() + &lines[generated_key.end..].concat(),
        "{at}"
    );
    texts
}

/// Parse then write gives the same bytes, the parse gives the generated
/// values, and each kind of edit changes only its own lines.
///
/// Why not proptest: it isn't in the tree, and the generator would still
/// be hand-written, since it tracks each key's line range and value;
/// fastrand (in the tree, sb-journal's fuzz harness uses it) seeds each
/// case, and a failure prints its seed and text.
#[test]
fn generated_documents_round_trip_and_edits_touch_only_their_lines() {
    for seed in 0..500 {
        edited_texts(seed, false);
    }
}

/// The same over the forms Saltbox's reader accepts, and every text, as
/// generated and as edited, reads in Python as it reads in sb.
#[test]
fn saltboxs_reader_reads_generated_and_edited_documents_as_we_do() {
    let texts: Vec<String> = (0..300).flat_map(|seed| edited_texts(seed, true)).collect();
    let python = python_reads_all(texts.iter().map(String::as_str));
    for (text, python) in texts.iter().zip(python) {
        assert_eq!(python, Some(ours(text)), "{text:?}");
    }
}
