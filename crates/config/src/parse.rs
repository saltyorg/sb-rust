//! A config file parsed for validation (config spec, "Parsing" and
//! "Duplicate keys"): the first YAML document, with every node's position
//! (saphyr's `MarkedYamlOwned`) and its scalars' source text kept for
//! yaml.v3-style resolution; the same pass over saphyr-parser's events
//! reports every duplicate key. The text goes through cli-core's YAML
//! pre-pass first, so sb accepts what Ansible's libyaml accepts (decision
//! 28), and every position points into the file as written.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use saphyr::{MarkedYamlOwned, YamlDataOwned, YamlLoader};
use saphyr_parser::{Event, Marker, Parser, ScanError, Span, SpannedEventReceiver};
use sb_cli_core::config::yaml::{self, Prepared};

use crate::issue::Issue;
use crate::scalar::{Value, resolve};

/// A 1-based (line, column) in the original text from saphyr's marker
/// on the prepared text (its columns start at 0).
fn position(prepared: &Prepared, marker: &Marker) -> (usize, usize) {
    let marker = prepared.original(marker);
    (marker.line(), marker.col() + 1)
}

/// `text`, the config file shown as `file`, as its root mapping, or its
/// issues: what Ansible's loader refuses (cli-core's pre-pass: a syntax
/// error, a tab or `:` libyaml refuses, a second document, nesting past
/// `yaml::MAX_DEPTH`), the duplicate keys, or a root that isn't a mapping.
/// An empty file (or a null root) is an empty mapping. Positions, the
/// returned nodes' included, are the file's. [`prepare`] then [`load`]: a
/// tree deeper than `yaml::SHALLOW` levels is loaded, walked and dropped
/// within `yaml::with_stack`, as validate-config does.
///
/// # Errors
///
/// The issues above, sorted by position.
pub fn parse(file: &str, text: &str) -> Result<MarkedYamlOwned, Vec<Issue>> {
    load(file, &prepare(file, text)?)
}

/// `text`, the config file shown as `file`, through cli-core's pre-pass:
/// ready for [`load`], its depth known.
///
/// # Errors
///
/// What Ansible's loader refuses, as the one issue.
pub fn prepare(file: &str, text: &str) -> Result<Prepared, Vec<Issue>> {
    yaml::prepare(text).map_err(|refusal| {
        vec![Issue {
            file: file.to_string(),
            line: refusal.line,
            column: refusal.column,
            path: String::new(),
            message: format!("syntax error: {}", refusal.message),
        }]
    })
}

/// The prepared file shown as `file` as its root mapping, or its issues:
/// the duplicate keys, the loader's errors, or a root that isn't a
/// mapping (as [`parse`]).
///
/// # Errors
///
/// The issues above, sorted by position.
pub fn load(file: &str, prepared: &Prepared) -> Result<MarkedYamlOwned, Vec<Issue>> {
    let mut loader = loader();
    // One parser pass for both (two cost validate-config 7% more,
    // performance round 5). Each event of the first document (the
    // pre-pass has refused a second) goes to the duplicate-key check as
    // saphyr-parser gives it, then to the loader with its scalar as
    // libyaml reads it and its span moved back into the file as written.
    // The loader's tree is dropped when a key repeats.
    let mut both = Both {
        keys: Keys::new(file),
        original: Original {
            prepared,
            loader: &mut loader,
        },
    };
    prepared
        .load_raw(&mut both)
        .map_err(|err| syntax(file, prepared, &err))?;
    let found = both.keys.finish();
    if !found.is_empty() {
        return Err(found);
    }
    loaded(file, loader)
}

/// saphyr's loader as [`load`] uses it: scalars kept as their source text
/// (`early_parse` off), and duplicates left to [`Keys`], as yaml.v3
/// compares keys (the loader would also compare an alias key with the
/// text it stands for).
fn loader<'input>() -> YamlLoader<'input, MarkedYamlOwned> {
    let mut loader = YamlLoader::<MarkedYamlOwned>::default();
    loader.early_parse(false);
    loader.allow_duplicate_keys(true);
    loader
}

/// The verdict on a text `loader` has loaded, its duplicate keys checked:
/// the loader's own errors, else the root mapping, or the issue of a root
/// that isn't one.
fn loaded(
    file: &str,
    loader: YamlLoader<'_, MarkedYamlOwned>,
) -> Result<MarkedYamlOwned, Vec<Issue>> {
    // Errors the loader keeps to itself (an alias expansion past its
    // budget, say), as saphyr's own `load_from_parser` checks. Their
    // positions come from the moved spans: the file's already.
    if let Some(err) = loader.error() {
        return Err(vec![issue(
            file,
            (err.marker().line(), err.marker().col() + 1),
            format!("syntax error: {}", err.info()),
        )]);
    }
    let Some(root) = loader.into_documents().into_iter().next() else {
        return Ok(empty());
    };
    match kind_name(&root.data) {
        None => Ok(root),
        Some("null") => Ok(empty()),
        Some(found) => Err(vec![not_a_mapping(
            file,
            (root.span.start.line(), root.span.start.col() + 1),
            found,
        )]),
    }
}

/// An issue in `file` at a 1-based (line, column), for no key path.
fn issue(file: &str, (line, column): (usize, usize), message: String) -> Issue {
    Issue {
        file: file.to_string(),
        line,
        column,
        path: String::new(),
        message,
    }
}

/// saphyr-parser's syntax error as `file`'s one issue.
fn syntax(file: &str, prepared: &Prepared, err: &ScanError) -> Vec<Issue> {
    vec![issue(
        file,
        position(prepared, err.marker()),
        format!("syntax error: {}", err.info()),
    )]
}

/// The issue of a root that is `found`, not a mapping.
fn not_a_mapping(file: &str, at: (usize, usize), found: &str) -> Issue {
    issue(
        file,
        at,
        format!("the file must be a mapping of keys, not {found}"),
    )
}

/// [`load`]'s verdict without its tree, for a file whose tree nobody
/// reads (one with no schema: the inventory). The duplicate keys and the
/// root's kind come from one pass over the events. The loader's tree
/// decides only two cases, so a text with either is loaded as before:
/// an alias (the loader's expansion budget; duplicate keys are this
/// check's) and a root collection with a tag outside YAML's core schema
/// (`!unsafe`, `!vault`, `!`: the loader keeps the tag on it, and
/// [`kind_name`] then calls it a scalar).
///
/// # Errors
///
/// As [`load`].
pub fn check(file: &str, prepared: &Prepared) -> Result<(), Vec<Issue>> {
    let mut only = KeysOnly {
        prepared,
        keys: Keys::new(file),
        root: None,
        needs_tree: false,
    };
    // saphyr-parser's events, the first document's, as the duplicate-key
    // pass before the loader read them (the pre-pass has refused a
    // second).
    for event in Parser::new_from_str(prepared.text()) {
        let (event, span) = event.map_err(|err| syntax(file, prepared, &err))?;
        if event == Event::DocumentEnd {
            break;
        }
        only.on_event(event, span);
    }
    let (needs_tree, root) = (only.needs_tree, only.root);
    let found = only.keys.finish();
    if !found.is_empty() {
        return Err(found);
    }
    if needs_tree {
        // The loader alone: the keys are checked already.
        let mut loader = loader();
        prepared
            .load(&mut Original {
                prepared,
                loader: &mut loader,
            })
            .map_err(|err| syntax(file, prepared, &err))?;
        return loaded(file, loader).map(|_| ());
    }
    match root {
        Some((found, at)) => Err(vec![not_a_mapping(file, at, found)]),
        None => Ok(()),
    }
}

/// [`check`]'s receiver: the duplicate-key check, whether the verdict
/// needs the loader's tree ([`check`]), and what the root is when it
/// isn't a mapping or null, with where.
struct KeysOnly<'a> {
    prepared: &'a Prepared,
    keys: Keys<'a>,
    root: Option<(&'static str, (usize, usize))>,
    needs_tree: bool,
}

impl<'input> SpannedEventReceiver<'input> for KeysOnly<'_> {
    fn on_event(&mut self, event: Event<'input>, span: Span) {
        let top = self.keys.frames.is_empty();
        self.keys.event(self.prepared, &event, &span);
        // Below the root only an alias matters: libyaml's reading of a
        // scalar is for the root's kind, and costs its look at every
        // event otherwise.
        if !top {
            self.needs_tree |= matches!(event, Event::Alias(_));
            return;
        }
        let at = || {
            let start = self.prepared.original(&span.start);
            (start.line(), start.col() + 1)
        };
        match self.prepared.libyaml(event, &span) {
            Event::Alias(_) => self.needs_tree = true,
            Event::MappingStart(_, Some(tag)) | Event::SequenceStart(_, Some(tag))
                if !tag.is_yaml_core_schema() =>
            {
                self.needs_tree = true;
            }
            Event::SequenceStart(..) => self.root = Some(("a sequence", at())),
            Event::Scalar(text, style, _, tag)
                if resolve(&text, style, tag.as_deref()) != Ok(Value::Null) =>
            {
                self.root = Some(("a scalar", at()));
            }
            _ => {}
        }
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

/// The duplicate-key check and the loader, fed from one pass: the check
/// sees each event as saphyr-parser gives it, the loader its scalar as
/// libyaml reads it and its span in the file as written (saphyr's
/// `YamlLoader` implements `SpannedEventReceiver`:
/// <https://docs.rs/saphyr-parser/0.3.3/saphyr_parser/trait.SpannedEventReceiver.html>).
struct Both<'a, 'input> {
    keys: Keys<'a>,
    original: Original<'a, 'input>,
}

impl<'input> SpannedEventReceiver<'input> for Both<'_, 'input> {
    fn on_event(&mut self, event: Event<'input>, span: Span) {
        let prepared = self.original.prepared;
        self.keys.event(prepared, &event, &span);
        self.original.on_event(prepared.libyaml(event, &span), span);
    }
}

/// The loader, fed spans in the file as written: every node's span comes
/// from its events' spans (saphyr's `YamlLoader` implements
/// `SpannedEventReceiver`:
/// <https://docs.rs/saphyr-parser/0.3.3/saphyr_parser/trait.SpannedEventReceiver.html>).
struct Original<'a, 'input> {
    prepared: &'a Prepared,
    loader: &'a mut YamlLoader<'input, MarkedYamlOwned>,
}

impl<'input> SpannedEventReceiver<'input> for Original<'_, 'input> {
    fn on_event(&mut self, event: Event<'input>, span: Span) {
        let span = Span::new(
            self.prepared.original(&span.start),
            self.prepared.original(&span.end),
        );
        self.loader.on_event(event, span);
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
    /// What it adds to its parent's key path (`a`, `.b`, `[0]`; nothing
    /// for the root): the whole path is joined only for an issue, so a
    /// deep file costs its depth, not its depth squared.
    segment: String,
    /// Where the collection starts (for a collection used as a key).
    start: (usize, usize),
    kind: FrameKind,
}

enum FrameKind {
    Mapping {
        /// Each key seen, with where it first was.
        seen: HashMap<KeyId, (usize, usize)>,
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

/// The key path segment of a collection starting now under `frames`'
/// innermost one: its key in a mapping, `<key>` when it is a mapping's key
/// itself, or `[n]` in a sequence.
fn child_segment(frames: &[Frame]) -> String {
    // Only the root's own path is empty.
    let top = frames.len() == 1;
    match frames.last().map(|frame| &frame.kind) {
        None => String::new(),
        Some(FrameKind::Mapping { key: Some(key), .. }) => key_segment(top, key),
        Some(FrameKind::Mapping { key: None, .. }) => key_segment(top, "<key>"),
        Some(FrameKind::Sequence { index }) => format!("[{index}]"),
    }
}

/// `key`'s segment under a path, empty when `top`; an empty key shows as
/// `""`.
fn key_segment(top: bool, key: &str) -> String {
    let key = if key.is_empty() { "\"\"" } else { key };
    if top {
        key.to_string()
    } else {
        format!(".{key}")
    }
}

/// Every duplicate key in a text's first document, from saphyr-parser's
/// events as they come, each with its key path and the first position of
/// the same key (`KeyId`) in the same mapping. The pre-pass has limited
/// the nesting, so the frames are at most `yaml::MAX_DEPTH`.
struct Keys<'a> {
    file: &'a str,
    issues: Vec<Issue>,
    frames: Vec<Frame>,
}

impl<'a> Keys<'a> {
    fn new(file: &'a str) -> Self {
        Self {
            file,
            issues: Vec::new(),
            frames: Vec::new(),
        }
    }

    /// The issues, sorted by position.
    fn finish(mut self) -> Vec<Issue> {
        self.issues.sort();
        self.issues
    }

    fn event(&mut self, prepared: &Prepared, event: &Event<'_>, span: &Span) {
        let here = position(prepared, &span.start);
        match event {
            Event::MappingStart(..) | Event::SequenceStart(..) => {
                let kind = if matches!(event, Event::MappingStart(..)) {
                    FrameKind::Mapping {
                        seen: HashMap::new(),
                        key: None,
                    }
                } else {
                    FrameKind::Sequence { index: 0 }
                };
                self.frames.push(Frame {
                    segment: child_segment(&self.frames),
                    start: here,
                    kind,
                });
            }
            Event::MappingEnd | Event::SequenceEnd => {
                if let Some(frame) = self.frames.pop() {
                    let id = match frame.kind {
                        FrameKind::Mapping { .. } => KeyId::Mapping,
                        FrameKind::Sequence { .. } => KeyId::Sequence,
                    };
                    let node = Node {
                        id,
                        label: "<key>".to_string(),
                        here: frame.start,
                    };
                    ended(&mut self.issues, self.file, &mut self.frames, node);
                }
            }
            Event::Scalar(value, ..) => {
                let node = Node {
                    id: KeyId::Text(value.to_string()),
                    label: value.to_string(),
                    here,
                };
                ended(&mut self.issues, self.file, &mut self.frames, node);
            }
            Event::Alias(anchor) => {
                let node = Node {
                    id: KeyId::Alias(*anchor),
                    label: "<alias>".to_string(),
                    here,
                };
                ended(&mut self.issues, self.file, &mut self.frames, node);
            }
            _ => {}
        }
    }
}

/// A node that has ended: its key identity, its label in a path and where
/// it started.
struct Node {
    id: KeyId,
    label: String,
    here: (usize, usize),
}

/// A node has ended in the innermost collection: a key is noted and
/// compared (a repeat is an issue), a value moves its mapping back to
/// expecting a key or its sequence to the next index.
fn ended(issues: &mut Vec<Issue>, file: &str, frames: &mut [Frame], node: Node) {
    let Some((frame, outer)) = frames.split_last_mut() else {
        return;
    };
    match &mut frame.kind {
        FrameKind::Mapping {
            key: next @ Some(_),
            ..
        } => *next = None,
        FrameKind::Mapping { seen, key: next } => {
            // The first position only: listing every earlier one made
            // the report of a key repeated N times grow as N² (config
            // spec, "Duplicate keys").
            match seen.entry(node.id) {
                Entry::Occupied(first) => {
                    let (line, column) = *first.get();
                    let mut path: String = outer.iter().map(|f| f.segment.as_str()).collect();
                    path.push_str(&frame.segment);
                    path.push_str(&key_segment(outer.is_empty(), &node.label));
                    issues.push(Issue {
                        file: file.to_string(),
                        line: node.here.0,
                        column: node.here.1,
                        path,
                        message: format!("duplicate key (also at line {line}, column {column})"),
                    });
                }
                Entry::Vacant(slot) => {
                    slot.insert(node.here);
                }
            }
            *next = Some(node.label);
        }
        FrameKind::Sequence { index } => *index += 1,
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

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

    /// One document, as Ansible's loader reads it (decision 38; yaml.v3
    /// read the first): a second is refused at its start, even a broken
    /// one; an end marker alone is no document.
    #[test]
    fn a_second_document_is_refused() {
        let second = "syntax error: a second document (Ansible reads one document per file)";
        assert_eq!(
            issues("a: 1\n---\nb: 2\n"),
            [format!("test.yml:2:1: {second}")]
        );
        assert_eq!(
            issues("a: 1\n---\nb: [\n"),
            [format!("test.yml:2:1: {second}")]
        );
        assert_eq!(keys(&root("a: 1\n...\n")), ["a"]);
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
    fn every_duplicate_is_reported_with_the_first_position() {
        assert_eq!(
            issues("a: 1\nb: 2\na: 3\na: 4\n"),
            [
                "test.yml:3:1: a: duplicate key (also at line 1, column 1)",
                "test.yml:4:1: a: duplicate key (also at line 1, column 1)",
            ]
        );
    }

    /// A key repeated many times costs one short issue per repeat, not a
    /// list of every earlier one (10,000 repeats took 102 s and 5.8 GB).
    #[test]
    fn many_repeats_stay_linear() {
        let text = "a: 1\n".repeat(10_000);
        let found = issues(&text);
        assert_eq!(found.len(), 9_999);
        assert_eq!(
            found.last().map(String::as_str),
            Some("test.yml:10000:1: a: duplicate key (also at line 1, column 1)")
        );
        // Each issue is about 61 bytes here; the old list made the last
        // one alone about 220 KB.
        let total: usize = found.iter().map(String::len).sum();
        assert!(total < 9_999 * 100, "{total}");
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

    /// A collection key is refused before its own duplicates, and an
    /// alias key isn't the text key it stands beside.
    #[test]
    fn collection_keys_are_walked() {
        assert_eq!(
            issues("? {k: 1, k: 2}\n: v\n"),
            ["test.yml:1:3: syntax error: a mapping key must be a scalar, not a collection"]
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
            let _ = writeln!(
                text,
                "l{level}: &l{level} [*{p}, *{p}, *{p}, *{p}, *{p}, *{p}, *{p}, *{p}, *{p}, *{p}]",
                p = if prev == 0 {
                    "a".to_string()
                } else {
                    format!("l{prev}")
                }
            );
        }
        text.push_str("required: 1\n");
        let found = issues(&text);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("syntax error: "), "{found:?}");
    }

    /// Nesting past cli-core's limit (`yaml::MAX_DEPTH`, under the depth
    /// Ansible's loader takes) is refused before the loader, where it
    /// goes too deep; at the limit the tree loads, on the deep stack.
    #[test]
    fn nesting_too_deep_is_refused() {
        let over = format!("a:\n {}x\n", "- ".repeat(yaml::MAX_DEPTH));
        assert_eq!(
            issues(&over),
            [format!(
                "test.yml:2:{}: syntax error: nested deeper than {} levels",
                2 + 2 * (yaml::MAX_DEPTH - 1),
                yaml::MAX_DEPTH
            )]
        );
        // The root mapping and the sequences: exactly the limit.
        let fine = format!("a:\n {}x\n", "- ".repeat(yaml::MAX_DEPTH - 1));
        let loaded = yaml::with_stack(yaml::MAX_DEPTH, || keys(&root(&fine))).expect("runs");
        assert_eq!(loaded, ["a"]);
    }

    /// An alias copies its anchor's whole subtree into the loaded tree, so
    /// a chain of deep anchors counts its full depth: refused, not loaded
    /// into a stack overflow.
    #[test]
    fn aliases_count_toward_the_depth() {
        let third = yaml::MAX_DEPTH / 3 + 1;
        let mut text = String::new();
        for k in 0..3 {
            let inner = if k == 0 {
                "x".to_string()
            } else {
                format!("*a{}", k - 1)
            };
            let _ = writeln!(text, "a{k}: &a{k}\n  {}{inner}", "- ".repeat(third));
        }
        let found = issues(&text);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].ends_with(&format!("nested deeper than {} levels", yaml::MAX_DEPTH)),
            "{found:?}"
        );
        // Two such anchors stay within the limit.
        let two: String = text.lines().take(4).flat_map(|l| [l, "\n"]).collect();
        let loaded = yaml::with_stack(yaml::MAX_DEPTH, || keys(&root(&two))).expect("runs");
        assert_eq!(loaded, ["a0", "a1"]);
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
        let anchor = format!("a: &a\n  {}x\n", "- ".repeat(yaml::MAX_DEPTH - 1));
        let loaded = yaml::with_stack(yaml::MAX_DEPTH, || keys(&root(&format!("{anchor}b: *a\n"))))
            .expect("runs");
        assert_eq!(loaded, ["a", "b"]);
        let found = issues(&format!("{anchor}b: [*a]\n"));
        assert_eq!(
            found,
            [format!(
                "test.yml:3:5: syntax error: nested deeper than {} levels",
                yaml::MAX_DEPTH
            )]
        );
    }

    /// A collection key, which a quoted key could once pass for, is
    /// refused before any duplicate compares it (decision 38).
    #[test]
    fn a_text_key_never_matches_a_collection_key() {
        assert_eq!(
            issues("\"\\0mapping\": 1\n? {a: 1}\n: 2\n"),
            ["test.yml:2:3: syntax error: a mapping key must be a scalar, not a collection"]
        );
    }

    /// Duplicates come out by position, a later mapping's before an
    /// earlier one's end.
    #[test]
    fn duplicates_come_out_by_position() {
        let found = issues("a:\n  k: 1\n  b:\n    z: 1\n    z: 2\n  k: 2\n");
        assert_eq!(
            found,
            [
                "test.yml:5:5: a.b.z: duplicate key (also at line 4, column 5)",
                "test.yml:6:3: a.k: duplicate key (also at line 2, column 3)",
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
        // Collection keys are refused at the first (decision 38).
        assert_eq!(
            issues("? [a]\n: 1\n? [b]\n: 2\n"),
            ["test.yml:1:3: syntax error: a mapping key must be a scalar, not a collection"]
        );
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

    /// The pre-pass pads continuation lines for saphyr (decision 28); a
    /// duplicate on a padded line is reported where it is in the file.
    #[test]
    fn a_duplicate_on_a_padded_line_has_its_own_position() {
        assert_eq!(
            issues("a: [\n]\nb: {\nc: 1, c: 2}\n"),
            ["test.yml:4:7: b.c: duplicate key (also at line 4, column 1)"]
        );
    }

    /// So does a syntax error, and every node of the tree.
    #[test]
    fn positions_on_padded_lines_are_the_files() {
        let found = issues("a: [\nb,\nc: d: e]\n");
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].starts_with("test.yml:3:5: syntax error: "),
            "{found:?}"
        );
        let node = root("a: [\nb,\n  c]\nd: 'x\ny'\n");
        let YamlDataOwned::Mapping(map) = &node.data else {
            panic!("mapping");
        };
        let at = |n: &MarkedYamlOwned| {
            (
                n.span.start.line(),
                n.span.start.col(),
                n.span.start.index(),
                n.span.end.line(),
                n.span.end.col(),
            )
        };
        let (list, quoted) = (
            map.values().next().expect("a"),
            map.values().nth(1).expect("d"),
        );
        let YamlDataOwned::Sequence(items) = &list.data else {
            panic!("sequence");
        };
        assert_eq!(at(&items[0]), (2, 0, 5, 2, 1));
        assert_eq!(at(&items[1]), (3, 2, 10, 3, 3));
        assert_eq!(at(quoted), (4, 3, 16, 5, 2));
        let key = map.keys().nth(1).expect("d");
        assert_eq!(at(key), (4, 0, 13, 4, 1));
        // A tag's node too.
        let tagged = root("a: !unsafe [\nb]\n");
        let YamlDataOwned::Mapping(map) = &tagged.data else {
            panic!("mapping");
        };
        let YamlDataOwned::Tagged(_, inner) = &map.values().next().expect("a").data else {
            panic!("tagged");
        };
        let YamlDataOwned::Sequence(items) = &inner.data else {
            panic!("sequence");
        };
        assert_eq!(at(&items[0]), (2, 0, 13, 2, 1));
    }

    /// A tab Ansible's parser refuses is the file's syntax error, at the tab.
    #[test]
    fn a_tab_libyaml_refuses_is_a_syntax_error() {
        assert_eq!(
            issues("a: 1\n\t\nb: 2\n"),
            ["test.yml:2:1: syntax error: a tab in the indentation (YAML indents with spaces)"]
        );
        assert_eq!(keys(&root("a:\t1\n")), ["a"]);
    }

    /// A `:` right after a quoted key in a flow collection is a value
    /// indicator to both parsers: the value's quotes are its own, whatever
    /// they hold (security audit of Task 43).
    #[test]
    fn json_style_flow_mappings_keep_their_values() {
        let node =
            root("x: {\"q\":\"a #b\", \"k\":\"a:\\tb\", 'c':'k:\tv[\t'}\ny: [\"a\":\"b\"]\n");
        let YamlDataOwned::Mapping(map) = &node.data else {
            panic!("mapping");
        };
        let x = map.values().next().expect("x");
        let YamlDataOwned::Mapping(x) = &x.data else {
            panic!("x is a mapping");
        };
        let values: Vec<String> = x
            .values()
            .map(|v| match &v.data {
                YamlDataOwned::Representation(text, ..) => text.clone(),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(values, ["a #b", "a:\tb", "k:\tv[\t"]);
        assert_eq!(keys(&node), ["x", "y"]);
    }

    /// A second document is the issue, not the duplicates in it.
    #[test]
    fn a_second_document_is_refused_before_its_duplicates() {
        assert_eq!(
            issues("a: 1\n---\nb: 1\nb: 2\n"),
            ["test.yml:2:1: syntax error: a second document (Ansible reads one document per file)"]
        );
    }

    /// [`check`] gives [`load`]'s verdict without its tree: the same
    /// issues, or none, for every kind of root, tagged or not,
    /// duplicates, aliases (the loader's budget) and the cases where the
    /// pre-pass rewrites the text. Every text passes the pre-pass, which
    /// refuses a syntax error first.
    #[test]
    fn check_gives_loads_verdict() {
        let mut budget = String::from("a: &a [x, x, x, x, x, x, x, x, x, x]\n");
        for level in 1..8 {
            let prev = if level == 1 {
                "a".to_string()
            } else {
                format!("l{}", level - 1)
            };
            let _ = writeln!(
                budget,
                "l{level}: &l{level} [{}]",
                vec![format!("*{prev}"); 10].join(", ")
            );
        }
        let texts = [
            "",
            "\n",
            "# only a comment\n",
            "---\n",
            "---\n...\n",
            "--- # a comment\n",
            "~\n",
            "null\n",
            "!!null ''\n",
            "''\n",
            "foo\n",
            "&a foo\n",
            "!!str null\n",
            "42\n",
            "|\n  a block\n",
            "|\n  a block",
            "- a\n- b\n",
            "[a, b]\n",
            "{}\n",
            "!!map {}\n",
            "a: 1\nb: [1, 2]\nc: {d: e}\n",
            "a: 1\na: 2\n",
            "a: {b: 1, b: 2}\nc: [x, {d: 1, d: 2}]\n",
            "'a': 1\n\"a\": 2\n",
            "a: &x 1\nb: *x\n",
            "a: &x 1\n*x : 2\n",
            "a: &x [1]\nb: *x\nb: 3\n",
            "a:\tb\n",
            "key:\tvalue\nother: 1\n",
            "? a\n: 1\n? b\n: 2\n",
            "a: 1\n# a trailing comment",
            "!unsafe\na: 1\n",
            "! [a]\n",
            "!vault {a: 1}\n",
            "--- ! \na: 1\n",
            "--- ! \n- a\n",
            "&a !unsafe [a]\n",
            "!!map\na: 1\n",
            "!!seq [a]\n",
            "a: !unsafe [x]\nb: !vault {c: 1}\n",
            "!unsafe foo\n",
            budget.as_str(),
        ];
        for text in texts {
            let prepared =
                prepare("test.yml", text).unwrap_or_else(|issues| panic!("{text:?}: {issues:?}"));
            assert_eq!(
                check("test.yml", &prepared),
                load("test.yml", &prepared).map(|_| ()),
                "{text:?}"
            );
        }
    }
}
