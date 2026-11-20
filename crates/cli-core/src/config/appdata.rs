//! `server_appdata_path` read from the inventory's events, without its
//! tree (the module build's follow-up "the inventory read's memory").
//!
//! saphyr's `YamlLoader` builds the whole document: on a 56 MB inventory
//! that peaked at about 890 MB, every sb start. This receiver decides
//! exactly what the loader decides, which is what Ansible's acceptance
//! and validate-config's agreement rest on (decision 38): the same
//! duplicate-key error at the same key, the same alias budget, and the
//! same value under the root's `server_appdata_path`. It keeps only what
//! those need: each open mapping's keys and pending key, the anchored
//! nodes (an alias copies them), and in full only a collection that is
//! anchored, sits where a key goes, or lies inside one of those.
//!
//! The rules are saphyr 0.3.3's loader (`on_event_impl`,
//! `insert_new_node`, `resolve_alias`, `count_nodes_within_budget` in
//! `src/loader.rs`:
//! <https://docs.rs/crate/saphyr/0.3.3/source/src/loader.rs>), ported
//! because the loader has no mode that keeps less; its own node types
//! and `LoadableYamlNode` methods build what is kept, so values and keys
//! compare as the loader compares them. The tests check the two against
//! each other (`by_the_loader`, on the YAML corpus and crafted cases), so
//! a saphyr upgrade that changes the loader fails them.

use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::HashSet;

use saphyr::{LoadableYamlNode, MappingOwned, ScalarOwned, Yaml, YamlOwned};
use saphyr_parser::{Event, Marker, ScanError, Span, SpannedEventReceiver, Tag};

/// saphyr 0.3.3's `DEFAULT_ALIAS_LIMIT` (private there): the nodes all
/// aliases together may copy.
const ALIAS_LIMIT: usize = 100_000;

/// The key read.
pub(super) const KEY: &str = "server_appdata_path";

/// What the loader would have found.
pub(super) struct Reader<'input> {
    stack: Vec<Frame<'input>>,
    /// Each anchored node, as the loader copies it for an alias.
    anchors: HashMap<usize, YamlOwned>,
    /// The nodes aliases may still copy.
    budget: usize,
    /// The first error; later events are ignored, as the loader does.
    error: Option<ScanError>,
    /// The string the root mapping holds under [`KEY`], once found.
    found: Option<String>,
}

/// One open collection.
struct Frame<'input> {
    /// The collection as the loader builds it, when kept.
    node: Option<YamlOwned>,
    anchor: usize,
    tag: Option<Cow<'input, Tag>>,
    /// A mapping's pending key (`BadValue`: none yet, so the next node is
    /// a key, as in the loader) and where it started. `None`: a sequence.
    key: Option<(YamlOwned, Marker)>,
    /// A mapping's keys so far, when it isn't kept (a kept one holds
    /// them): plain string keys apart, which is nearly all of them, at a
    /// quarter of a node's size each. A string key equals only a string
    /// key, so the split decides as the loader's one map does.
    strings: HashSet<Box<str>>,
    keys: HashSet<YamlOwned>,
}

impl Frame<'_> {
    /// Whether a node starting now takes the key's place.
    fn wants_key(&self) -> bool {
        self.key.as_ref().is_some_and(|(key, _)| key.is_badvalue())
    }
}

impl Default for Reader<'_> {
    fn default() -> Self {
        Self {
            stack: Vec::new(),
            anchors: HashMap::new(),
            budget: ALIAS_LIMIT,
            error: None,
            found: None,
        }
    }
}

impl<'input> Reader<'input> {
    /// The value found, or the loader's first error.
    pub(super) fn finish(self) -> Result<Option<String>, ScanError> {
        self.error.map_or(Ok(self.found), Err)
    }

    fn on_event_impl(&mut self, event: Event<'input>, span: Span) -> Result<(), ScanError> {
        let mark = span.start;
        match event {
            Event::SequenceStart(anchor, tag) => self.open(false, anchor, tag, mark),
            Event::MappingStart(anchor, tag) => self.open(true, anchor, tag, mark),
            Event::MappingEnd | Event::SequenceEnd => {
                let Some(frame) = self.stack.pop() else {
                    return Ok(());
                };
                let node = frame.node.map(|node| match frame.tag {
                    Some(tag) if !tag.is_yaml_core_schema() => node.into_tagged(tag),
                    _ => node,
                });
                self.insert(node, frame.anchor, mark)?;
            }
            Event::Scalar(text, style, anchor, tag) => {
                // The loader's early parse (its default).
                let node = YamlOwned::from_bare_yaml(Yaml::value_from_cow_and_metadata(
                    text,
                    style,
                    tag.as_ref(),
                ));
                self.insert(Some(node), anchor, mark)?;
            }
            Event::Alias(id) => {
                let node = self.resolve_alias(id, mark);
                self.insert(Some(node), 0, mark)?;
            }
            // A document's end needs nothing: the root was taken when it
            // ended, and an empty document holds no mapping.
            _ => {}
        }
        Ok(())
    }

    /// A collection starts: kept when it is anchored, sits where a key
    /// goes, or lies inside a kept one.
    fn open(&mut self, mapping: bool, anchor: usize, tag: Option<Cow<'input, Tag>>, mark: Marker) {
        let kept = anchor > 0
            || self
                .stack
                .last()
                .is_some_and(|parent| parent.node.is_some() || parent.wants_key());
        let node = kept.then(|| {
            if mapping {
                YamlOwned::Mapping(MappingOwned::new())
            } else {
                YamlOwned::Sequence(Vec::new())
            }
        });
        self.stack.push(Frame {
            node,
            anchor,
            tag,
            key: mapping.then_some((YamlOwned::BadValue, mark)),
            strings: HashSet::new(),
            keys: HashSet::new(),
        });
    }

    /// `node` (`None`: a collection not kept) placed in its parent, as the
    /// loader's `insert_new_node`. The loader's tag step there applies to
    /// a scalar's tag on a collection, which can't happen, so it's left
    /// out.
    fn insert(
        &mut self,
        node: Option<YamlOwned>,
        anchor: usize,
        mark: Marker,
    ) -> Result<(), ScanError> {
        if anchor > 0
            && let Some(node) = &node
        {
            self.anchors.insert(anchor, node.clone());
        }
        let root = self.stack.len() == 1;
        let Some(parent) = self.stack.last_mut() else {
            // The document's root: a kept mapping is looked up whole.
            if let Some(node) = node {
                self.found = string(node.as_mapping_get(KEY));
            }
            return Ok(());
        };
        let Some((pending, key_mark)) = &mut parent.key else {
            // A sequence: kept, it takes the item (its tag comes at its end).
            if let (Some(sequence), Some(node)) = (&mut parent.node, node) {
                sequence.sequence_mut().push(node);
            }
            return Ok(());
        };
        if pending.is_badvalue() {
            // A key: always built (a collection here was kept).
            *pending = node.unwrap_or(YamlOwned::BadValue);
            *key_mark = mark;
            return Ok(());
        }
        let key = pending.take();
        let key_mark = *key_mark;
        // A kept mapping's children are kept: `node` is there.
        let duplicate = if let Some(mapping) = &mut parent.node {
            node.is_some_and(|node| mapping.mapping_mut().insert(key, node).is_some())
        } else {
            // A root under a tag of its own is a `Tagged` node to the
            // loader, which `as_mapping_get` doesn't look into.
            let plain = parent
                .tag
                .as_ref()
                .is_none_or(|tag| tag.is_yaml_core_schema());
            if root && plain && key.as_str() == Some(KEY) {
                self.found = string(node.as_ref());
            }
            if let YamlOwned::Value(ScalarOwned::String(text)) = key {
                !parent.strings.insert(text.into_boxed_str())
            } else {
                !parent.keys.insert(key)
            }
        };
        if duplicate {
            return Err(ScanError::new_str(key_mark, "duplicated key in mapping"));
        }
        Ok(())
    }

    /// An alias's node: a copy of its anchor's, within the budget, as the
    /// loader's `resolve_alias`.
    fn resolve_alias(&mut self, id: usize, mark: Marker) -> YamlOwned {
        let Some(existing) = self.anchors.get_mut(&id) else {
            return YamlOwned::BadValue;
        };
        let mut remaining = self.budget;
        if count_nodes_within_budget(existing, &mut remaining) {
            self.budget = remaining;
            existing.clone()
        } else {
            self.error = Some(ScanError::new_str(
                mark,
                "alias expansion exceeded the maximum node budget (possible billion-laughs attack)",
            ));
            YamlOwned::BadValue
        }
    }
}

/// The string a value holds, its tag set aside, as `inventory_appdata`
/// has always read it.
fn string(value: Option<&YamlOwned>) -> Option<String> {
    let value = value?;
    value
        .get_tagged_node()
        .unwrap_or(value)
        .as_str()
        .map(str::to_string)
}

/// saphyr's `count_nodes_within_budget`: `node`'s nodes, each mapping
/// entry counted once with its value walked, until `budget` runs out.
fn count_nodes_within_budget(node: &mut YamlOwned, budget: &mut usize) -> bool {
    if *budget == 0 {
        return false;
    }
    *budget -= 1;
    // The trait's methods, as the loader's generic code calls them: they
    // look through a tag, where `YamlOwned`'s own `is_sequence` doesn't.
    if LoadableYamlNode::is_sequence(node) {
        for child in node.sequence_mut() {
            if !count_nodes_within_budget(child, budget) {
                return false;
            }
        }
    } else if LoadableYamlNode::is_mapping(node) {
        for (_, value) in node.mapping_mut().iter_mut() {
            if *budget == 0 {
                return false;
            }
            *budget -= 1;
            if !count_nodes_within_budget(value, budget) {
                return false;
            }
        }
    }
    true
}

impl<'input> SpannedEventReceiver<'input> for Reader<'input> {
    fn on_event(&mut self, event: Event<'input>, span: Span) {
        if self.error.is_some() {
            return;
        }
        if let Err(err) = self.on_event_impl(event, span) {
            self.error = Some(err);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use saphyr_parser::Parser;

    fn at() -> Marker {
        Marker::new(0, 1, 0)
    }

    /// Only what the loader's decisions need is built: a plain nested
    /// collection isn't, one in a key's place or under an anchor is, and
    /// so is everything inside a kept one.
    #[test]
    fn only_what_decides_is_kept() {
        let mut reader = Reader::default();
        reader.open(true, 0, None, at());
        reader.open(true, 0, None, at());
        assert!(reader.stack[1].node.is_some(), "a key's place");
        reader.stack[0].key = Some((YamlOwned::Value(ScalarOwned::Null), at()));
        reader.open(false, 0, None, at());
        assert!(reader.stack[2].node.is_some(), "inside a kept one");
        let mut reader = Reader::default();
        reader.open(true, 0, None, at());
        reader.stack[0].key = Some((YamlOwned::Value(ScalarOwned::Null), at()));
        reader.open(true, 0, None, at());
        assert!(reader.stack[1].node.is_none(), "a value");
        reader.open(false, 3, None, at());
        assert!(reader.stack[2].node.is_some(), "anchored");
        assert!(!reader.stack[2].wants_key(), "a sequence takes no key");
    }

    /// An alias to no anchor is the loader's `BadValue`; the budget runs
    /// out inside a mapping as in a sequence.
    #[test]
    fn aliases_copy_within_the_budget() {
        use std::fmt::Write as _;
        let mut reader = Reader::default();
        assert_eq!(reader.resolve_alias(7, at()), YamlOwned::BadValue);
        assert!(reader.error.is_none());

        let mut text = String::from("a: &m {");
        for i in 0..20_000 {
            write!(text, "k{i}: v, ").expect("a String");
        }
        text.push_str("}\n");
        for i in 0..3 {
            writeln!(text, "c{i}: *m").expect("a String");
        }
        // Only anchored nodes are copied for aliases.
        let mut plain = Reader::default();
        Parser::new_from_str("a: [1, {b: 2}]\n")
            .load(&mut plain, false)
            .expect("parses");
        assert!(plain.anchors.is_empty());

        let mut reader = Reader::default();
        Parser::new_from_str(&text)
            .load(&mut reader, false)
            .expect("parses");
        let err = reader.finish().expect_err("over the budget");
        assert_eq!(err.marker().line(), 4);
        assert!(err.info().contains("alias expansion"), "{}", err.info());
    }
}
