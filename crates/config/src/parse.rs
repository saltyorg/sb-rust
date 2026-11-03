//! A config file parsed for validation (config spec, "Parsing" and
//! "Duplicate keys"): the first YAML document, with every node's position
//! (saphyr's `MarkedYamlOwned`) and its scalars' source text kept for
//! yaml.v3-style resolution; a pass over saphyr-parser's events first
//! reports every duplicate key.

use std::collections::HashMap;

use saphyr::{MarkedYamlOwned, YamlDataOwned, YamlLoader};
use saphyr_parser::{Event, Marker, Parser, ScanError, Span};

use crate::issue::Issue;
use crate::scalar::{Value, resolve};

/// How deep collections may nest: saphyr-parser's own limit for flow
/// collections, applied to block ones too, before saphyr's loader (whose
/// recursion would overflow the stack on a few kilobytes of `- - - …`).
const MAX_DEPTH: usize = 256;

/// A 1-based (line, column) from saphyr's marker (its columns start at 0).
fn position(marker: &Marker) -> (usize, usize) {
    (marker.line(), marker.col() + 1)
}

/// `text`, the config file shown as `file`, as its root mapping, or its
/// issues: a syntax error, nesting too deep, the duplicate keys, or a
/// root that isn't a mapping. An empty file (or a null root) is an empty
/// mapping.
pub fn parse(file: &str, text: &str) -> Result<MarkedYamlOwned, Vec<Issue>> {
    let issue = |(line, column): (usize, usize), message: String| Issue {
        file: file.to_string(),
        line,
        column,
        path: String::new(),
        message,
    };
    let syntax = |err: &ScanError| {
        vec![issue(
            position(err.marker()),
            format!("syntax error: {}", err.info()),
        )]
    };
    let found = duplicates(file, text).map_err(|err| syntax(&err))?;
    if !found.is_empty() {
        return Err(found);
    }
    let mut loader = YamlLoader::<MarkedYamlOwned>::default();
    loader.early_parse(false);
    // The pass above decides duplicates, as yaml.v3 compares keys; the
    // loader would also compare an alias key with the text it stands for.
    loader.allow_duplicate_keys(true);
    // The first document only (`multi: false`), as yaml.v3 and cli-core.
    Parser::new_from_str(text)
        .load(&mut loader, false)
        .map_err(|err| syntax(&err))?;
    // Errors the loader keeps to itself (an alias expansion past its
    // budget, say), as saphyr's own `load_from_parser` checks.
    if let Some(err) = loader.error() {
        return Err(syntax(err));
    }
    let Some(root) = loader.into_documents().into_iter().next() else {
        return Ok(empty());
    };
    match kind_name(&root.data) {
        None => Ok(root),
        Some("null") => Ok(empty()),
        Some(found) => Err(vec![issue(
            position(&root.span.start),
            format!("the file must be a mapping of keys, not {found}"),
        )]),
    }
}

/// What a root that isn't a mapping is, for its issue (`null` for a null
/// one, which counts as empty); `None` for a mapping.
fn kind_name(data: &YamlDataOwned<MarkedYamlOwned>) -> Option<&'static str> {
    match data {
        YamlDataOwned::Mapping(_) => None,
        YamlDataOwned::Representation(text, style, tag)
            if resolve(text, *style, tag.as_ref()) == Ok(Value::Null) =>
        {
            Some("null")
        }
        YamlDataOwned::Sequence(_) => Some("a sequence"),
        _ => Some("a scalar"),
    }
}

/// An empty mapping, at the file's start (line 1, column 0 as saphyr
/// counts).
fn empty() -> MarkedYamlOwned {
    let start = Marker::new(0, 1, 0);
    MarkedYamlOwned {
        span: Span::new(start, start),
        data: YamlDataOwned::Mapping(saphyr::AnnotatedMappingOwned::new()),
    }
}

struct Frame {
    path: String,
    /// Where the collection starts (for a collection used as a key).
    start: (usize, usize),
    /// Its anchor (0: none), whose height is noted when it ends.
    anchor: usize,
    /// The tallest node in it so far: a collection's height, or an alias's
    /// anchor's (an alias copies its anchor's subtree when loaded).
    tallest: usize,
    kind: FrameKind,
}

enum FrameKind {
    Mapping {
        /// Each key seen, with every position.
        seen: HashMap<KeyId, Vec<(usize, usize)>>,
        /// The key whose value comes next; `None` when a key comes next.
        key: Option<String>,
    },
    Sequence {
        index: usize,
    },
}

/// What yaml.v3 compares keys by (decode.go: their kind and text):
/// scalars by text whatever the quoting, aliases by anchor, collections by
/// kind alone (their text is empty).
#[derive(PartialEq, Eq, Hash)]
enum KeyId {
    Text(String),
    /// saphyr-parser's anchor id; yaml.v3 compares the anchor's name, which
    /// differs only for two `*x` keys after `&x` is defined twice.
    Alias(usize),
    Mapping,
    Sequence,
}

/// The key path of a collection starting now under `frames`' innermost
/// one: its key in a mapping, `<key>` when it is a mapping's key itself,
/// or `[n]` in a sequence.
fn child_path(frames: &[Frame]) -> String {
    match frames.last() {
        None => String::new(),
        Some(Frame {
            path,
            kind: FrameKind::Mapping { key: Some(key), .. },
            ..
        }) => join(path, key),
        Some(Frame {
            path,
            kind: FrameKind::Mapping { key: None, .. },
            ..
        }) => join(path, "<key>"),
        Some(Frame {
            path,
            kind: FrameKind::Sequence { index },
            ..
        }) => format!("{path}[{index}]"),
    }
}

/// `key` under `path`; an empty key shows as `""`.
fn join(path: &str, key: &str) -> String {
    let key = if key.is_empty() { "\"\"" } else { key };
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

/// Every duplicate key in `text`'s first document, from saphyr-parser's
/// events (before any loading, which would stop at the first), sorted by
/// position: each with its key path and every earlier position of the
/// same key (`KeyId`) in the same mapping. Nesting past `MAX_DEPTH`,
/// counting what aliases copy in, is the one issue instead.
fn duplicates(file: &str, text: &str) -> Result<Vec<Issue>, ScanError> {
    let mut issues = Vec::new();
    let mut frames: Vec<Frame> = Vec::new();
    // Each anchor's height: what an alias to it adds below the alias.
    let mut heights: HashMap<usize, usize> = HashMap::new();
    let too_deep = |path: String, here: (usize, usize)| {
        Ok(vec![Issue {
            file: file.to_string(),
            line: here.0,
            column: here.1,
            path,
            message: format!("nested deeper than {MAX_DEPTH} levels"),
        }])
    };
    // A node (of `height`) has ended in the innermost collection: a key is
    // noted and compared, a value moves its mapping back to expecting a
    // key or its sequence to the next index.
    let mut ended =
        |frames: &mut Vec<Frame>, id: KeyId, label: String, here: (usize, usize), height: usize| {
            let Some(frame) = frames.last_mut() else {
                return;
            };
            frame.tallest = frame.tallest.max(height);
            match &mut frame.kind {
                FrameKind::Mapping {
                    key: next @ Some(_),
                    ..
                } => *next = None,
                FrameKind::Mapping { seen, key: next } => {
                    let earlier = seen.entry(id).or_default();
                    if !earlier.is_empty() {
                        let list: Vec<String> = earlier
                            .iter()
                            .map(|(line, column)| format!("line {line}, column {column}"))
                            .collect();
                        issues.push(Issue {
                            file: file.to_string(),
                            line: here.0,
                            column: here.1,
                            path: join(&frame.path, &label),
                            message: format!("duplicate key (also at {})", list.join(" and ")),
                        });
                    }
                    earlier.push(here);
                    *next = Some(label);
                }
                FrameKind::Sequence { index } => *index += 1,
            }
        };
    for event in Parser::new_from_str(text) {
        let (event, span) = event?;
        let here = position(&span.start);
        match event {
            Event::DocumentEnd => break,
            Event::MappingStart(anchor, _) | Event::SequenceStart(anchor, _) => {
                let kind = if matches!(event, Event::MappingStart(..)) {
                    FrameKind::Mapping {
                        seen: HashMap::new(),
                        key: None,
                    }
                } else {
                    FrameKind::Sequence { index: 0 }
                };
                let path = child_path(&frames);
                if frames.len() == MAX_DEPTH {
                    return too_deep(path, here);
                }
                frames.push(Frame {
                    path,
                    start: here,
                    anchor,
                    tallest: 0,
                    kind,
                });
            }
            Event::MappingEnd | Event::SequenceEnd => {
                if let Some(frame) = frames.pop() {
                    let height = frame.tallest + 1;
                    if frame.anchor != 0 {
                        heights.insert(frame.anchor, height);
                    }
                    let id = match frame.kind {
                        FrameKind::Mapping { .. } => KeyId::Mapping,
                        FrameKind::Sequence { .. } => KeyId::Sequence,
                    };
                    ended(&mut frames, id, "<key>".to_string(), frame.start, height);
                }
            }
            Event::Scalar(value, ..) => {
                ended(
                    &mut frames,
                    KeyId::Text(value.to_string()),
                    value.to_string(),
                    here,
                    0,
                );
            }
            Event::Alias(anchor) => {
                let height = heights.get(&anchor).copied().unwrap_or(0);
                if frames.len() + height > MAX_DEPTH {
                    return too_deep(child_path(&frames), here);
                }
                ended(
                    &mut frames,
                    KeyId::Alias(anchor),
                    "<alias>".to_string(),
                    here,
                    height,
                );
            }
            _ => {}
        }
    }
    issues.sort();
    Ok(issues)
}

#[cfg(test)]
mod tests {
    use super::*;
    use saphyr::{ScalarStyle, YamlDataOwned};

    fn root(text: &str) -> MarkedYamlOwned {
        parse("test.yml", text).unwrap_or_else(|issues| panic!("{issues:?}"))
    }

    fn issues(text: &str) -> Vec<String> {
        parse("test.yml", text)
            .expect_err("parses")
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    fn keys(node: &MarkedYamlOwned) -> Vec<String> {
        let YamlDataOwned::Mapping(map) = &node.data else {
            panic!("not a mapping: {node:?}");
        };
        map.keys()
            .map(|key| match &key.data {
                YamlDataOwned::Representation(text, ..) => text.clone(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_mapping_keeps_positions_and_source_text() {
        let node = root("a: 017\nb:\n  c: 'x'\n");
        assert_eq!(keys(&node), ["a", "b"]);
        let YamlDataOwned::Mapping(map) = &node.data else {
            panic!("mapping");
        };
        let (key, value) = map.iter().next().expect("a");
        assert_eq!((key.span.start.line(), key.span.start.col()), (1, 0));
        assert_eq!((value.span.start.line(), value.span.start.col()), (1, 3));
        assert_eq!(
            value.data,
            YamlDataOwned::Representation("017".into(), ScalarStyle::Plain, None)
        );
    }

    /// As sb-go: an empty file has every required key missing.
    #[test]
    fn empty_files_are_empty_mappings() {
        for text in ["", "# only a comment\n", "---\n", "~\n", "null\n"] {
            assert_eq!(keys(&root(text)), Vec::<String>::new(), "{text:?}");
        }
    }

    #[test]
    fn a_root_that_is_not_a_mapping_is_one_issue() {
        assert_eq!(
            issues("- a\n- b\n"),
            ["test.yml:1:1: the file must be a mapping of keys, not a sequence"]
        );
        assert_eq!(
            issues("hello\n"),
            ["test.yml:1:1: the file must be a mapping of keys, not a scalar"]
        );
    }

    #[test]
    fn a_syntax_error_is_one_issue_with_its_position() {
        let found = issues("a: 1\nb: [2\n");
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].starts_with("test.yml:3:1: syntax error: "),
            "{found:?}"
        );
    }

    /// The first document only, as yaml.v3 and cli-core read it: a later
    /// document isn't read, even a broken one.
    #[test]
    fn only_the_first_document_counts() {
        assert_eq!(keys(&root("a: 1\n---\nb: 2\n")), ["a"]);
        assert_eq!(keys(&root("a: 1\n---\nb: [\n")), ["a"]);
    }

    /// saphyr doesn't follow merge keys (yaml.v3 does): `<<` stays a key,
    /// which the rules then report as unknown.
    #[test]
    fn merge_keys_stay_keys() {
        let node = root("base: &b\n  x: 1\nuse:\n  <<: *b\n");
        let YamlDataOwned::Mapping(map) = &node.data else {
            panic!("mapping");
        };
        let used = map.values().nth(1).expect("use");
        assert_eq!(keys(used), ["<<"]);
    }

    #[test]
    fn every_duplicate_is_reported_with_every_earlier_position() {
        assert_eq!(
            issues("a: 1\nb: 2\na: 3\na: 4\n"),
            [
                "test.yml:3:1: a: duplicate key (also at line 1, column 1)",
                "test.yml:4:1: a: duplicate key (also at line 1, column 1 and line 3, column 1)",
            ]
        );
    }

    #[test]
    fn duplicates_carry_their_key_path() {
        assert_eq!(
            issues("x:\n  a: 1\n  a: 2\nl:\n  - k: 1\n    k: 2\n"),
            [
                "test.yml:3:3: x.a: duplicate key (also at line 2, column 3)",
                "test.yml:6:5: l[0].k: duplicate key (also at line 5, column 5)",
            ]
        );
    }

    /// yaml.v3 compares keys by their text, whatever the quoting.
    #[test]
    fn quoting_does_not_hide_a_duplicate() {
        assert_eq!(
            issues("a: 1\n'a': 2\n1: x\n\"1\": y\n"),
            [
                "test.yml:2:1: a: duplicate key (also at line 1, column 1)",
                "test.yml:4:1: 1: duplicate key (also at line 3, column 1)",
            ]
        );
    }

    #[test]
    fn the_same_key_in_different_mappings_is_fine() {
        assert_eq!(
            keys(&root("x:\n  a: 1\ny:\n  a: 2\nl:\n  - a: 1\n  - a: 2\n")),
            ["x", "y", "l"]
        );
    }

    /// A collection key's own mappings are checked too, and an alias key
    /// isn't the text key it stands beside.
    #[test]
    fn collection_keys_are_walked() {
        assert_eq!(
            issues("? {k: 1, k: 2}\n: v\n"),
            ["test.yml:1:10: <key>.k: duplicate key (also at line 1, column 4)"]
        );
        let node = root("a: &n x\n*n : y\n");
        assert_eq!(keys(&node).len(), 2);
    }

    /// Errors saphyr's loader keeps to itself (an alias expansion past its
    /// budget, here) are the file's issue, not an empty file.
    #[test]
    fn a_loader_error_is_an_issue() {
        let mut text = String::from("a: &a [x, x, x, x, x, x, x, x, x, x]\n");
        for level in 1..8 {
            let prev = level - 1;
            text.push_str(&format!(
                "l{level}: &l{level} [*{p}, *{p}, *{p}, *{p}, *{p}, *{p}, *{p}, *{p}, *{p}, *{p}]\n",
                p = if prev == 0 { "a".to_string() } else { format!("l{prev}") }
            ));
        }
        text.push_str("required: 1\n");
        let found = issues(&text);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("syntax error: "), "{found:?}");
    }

    /// Block nesting past 256 levels (saphyr-parser's own limit for flow
    /// nesting) is refused before the loader, whose recursion would
    /// overflow the stack.
    #[test]
    fn nesting_too_deep_is_refused() {
        let text = format!("a:\n {}x\n", "- ".repeat(20_000));
        let found = issues(&text);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].contains("nested deeper than 256 levels"),
            "{found:?}"
        );
        // The root mapping and 255 sequences: exactly the limit.
        let fine = format!("a:\n {}x\n", "- ".repeat(255));
        assert_eq!(keys(&root(&fine)), ["a"]);
        let over = format!("a:\n {}x\n", "- ".repeat(256));
        assert_eq!(issues(&over).len(), 1);
    }

    /// An alias copies its anchor's whole subtree into the loaded tree, so
    /// a chain of deep anchors counts its full depth: refused, not loaded
    /// into a stack overflow.
    #[test]
    fn aliases_count_toward_the_depth() {
        let mut text = String::new();
        for k in 0..3 {
            let inner = if k == 0 {
                "x".to_string()
            } else {
                format!("*a{}", k - 1)
            };
            text.push_str(&format!("a{k}: &a{k}\n  {}{inner}\n", "- ".repeat(100)));
        }
        let found = issues(&text);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].contains("nested deeper than 256 levels"),
            "{found:?}"
        );
        // Two such anchors stay within the limit.
        let two: String = text.lines().take(4).map(|l| format!("{l}\n")).collect();
        assert_eq!(keys(&root(&two)), ["a0", "a1"]);
    }

    /// Each sequence item has its own index in a duplicate's path.
    #[test]
    fn a_later_item_has_its_index() {
        assert_eq!(
            issues("l:\n  - a: 1\n  - k: 1\n    k: 2\n"),
            ["test.yml:4:5: l[1].k: duplicate key (also at line 3, column 5)"]
        );
    }

    /// An alias exactly at the depth limit loads; one level deeper doesn't.
    #[test]
    fn an_alias_at_the_limit_loads() {
        let anchor = format!("a: &a\n  {}x\n", "- ".repeat(255));
        assert_eq!(keys(&root(&format!("{anchor}b: *a\n"))), ["a", "b"]);
        let found = issues(&format!("{anchor}b: [*a]\n"));
        assert!(
            found[0].contains("nested deeper than 256 levels"),
            "{found:?}"
        );
    }

    /// A quoted key can't pass for a collection or alias key.
    #[test]
    fn a_text_key_never_matches_a_collection_key() {
        assert_eq!(keys(&root("\"\\0mapping\": 1\n? {a: 1}\n: 2\n")).len(), 2);
    }

    /// Duplicates come out by position, though a collection key is only
    /// known once it ends.
    #[test]
    fn duplicates_come_out_by_position() {
        let found = issues("? {k: 1}\n: 1\n? {z: 1, z: 2}\n: 2\n");
        assert_eq!(
            found,
            [
                "test.yml:3:3: <key>: duplicate key (also at line 1, column 3)",
                "test.yml:3:10: <key>.z: duplicate key (also at line 3, column 4)",
            ]
        );
    }

    /// yaml.v3 compares alias keys by their anchor, and collection keys by
    /// kind alone (their text is empty).
    #[test]
    fn alias_and_collection_keys_compare_as_yaml_v3() {
        assert_eq!(
            issues("a: &x k\n*x : 1\n*x : 2\n"),
            ["test.yml:3:1: <alias>: duplicate key (also at line 2, column 1)"]
        );
        assert_eq!(
            issues("? [a]\n: 1\n? [b]\n: 2\n"),
            ["test.yml:3:3: <key>: duplicate key (also at line 1, column 3)"]
        );
        assert_eq!(keys(&root("? [a]\n: 1\n? {b: 1}\n: 2\n")).len(), 2);
    }

    /// An alias key and the key it stands for aren't duplicates to yaml.v3
    /// (different kinds), so the loader doesn't decide that either.
    #[test]
    fn an_alias_key_does_not_duplicate_its_text() {
        // The later value wins, as in the Go map yaml.v3 decodes into.
        assert_eq!(keys(&root("a: &n x\nx: 1\n*n : 2\n")), ["a", "x"]);
    }

    /// An empty key gets `""` in its path, not nothing.
    #[test]
    fn an_empty_key_is_named() {
        assert_eq!(
            issues("x:\n  '': 1\n  '': 2\n"),
            ["test.yml:3:3: x.\"\": duplicate key (also at line 2, column 3)"]
        );
    }

    /// An empty file's root sits at 1:1, as everything else does.
    #[test]
    fn an_empty_root_is_at_the_start() {
        let node = root("");
        assert_eq!((node.span.start.line(), node.span.start.col()), (1, 0));
    }

    /// Duplicates are reported only from the first document too.
    #[test]
    fn duplicates_in_a_later_document_do_not_count() {
        assert_eq!(keys(&root("a: 1\n---\nb: 1\nb: 2\n")), ["a"]);
    }
}
