//! A parsed config file as the rules see it: every scalar resolved as
//! sb-go's yaml.v3 decodes it into Go's `interface{}`, every node and key
//! with its 1-based position. A scalar whose tag doesn't fit its text
//! fails the file there, as yaml.v3's decode does.

use std::fmt::Write as _;

use saphyr::{MarkedYamlOwned, YamlDataOwned};

use crate::issue::Issue;
use crate::scalar::{Value, resolve};

/// A value in the file, with where it starts.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub line: usize,
    pub column: usize,
    pub data: Data,
    /// A scalar's text as written (its quotes taken off), kept when it
    /// resolved to something other than a string or null (`0123`,
    /// `true`, `1.50`): what yaml.v3 puts in a Go `string` field. None
    /// otherwise, and for a collection.
    pub source: Option<String>,
}

/// A mapping (its keys in file order), a sequence or a scalar.
#[derive(Clone, Debug, PartialEq)]
pub enum Data {
    Mapping(Vec<(Key, Node)>),
    Sequence(Vec<Node>),
    Scalar(Value),
}

/// A mapping key: its text, and where it starts. (yaml.v3 decodes the
/// root's keys into Go strings this way; deeper mappings with other keys
/// sb-go skips, while sb checks them by their text: a listed difference.)
#[derive(Clone, Debug, PartialEq)]
pub struct Key {
    pub text: String,
    pub line: usize,
    pub column: usize,
}

impl Node {
    /// The key and value under `key`, when this is a mapping holding it.
    #[must_use]
    pub fn entry(&self, key: &str) -> Option<(&Key, &Node)> {
        match &self.data {
            Data::Mapping(entries) => entries
                .iter()
                .find(|(k, _)| k.text == key)
                .map(|(k, v)| (k, v)),
            _ => None,
        }
    }

    /// The value under `key`, when this is a mapping holding it.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Node> {
        self.entry(key).map(|(_, value)| value)
    }

    /// The scalar as yaml.v3 decodes it into a Go `string` field
    /// (go.yaml.in yaml v3.0.5, `decode.go`, `scalar`): a string as it
    /// is, any other scalar as its text as written (`password: 0123`
    /// reads `0123`, not the int 83); none for null or a collection.
    #[must_use]
    pub fn go_string(&self) -> Option<&str> {
        match &self.data {
            Data::Scalar(Value::Str(text)) => Some(text),
            Data::Scalar(Value::Null) | Data::Mapping(_) | Data::Sequence(_) => None,
            Data::Scalar(_) => self.source.as_deref(),
        }
    }

    /// Null or an empty string: sb-go's `isEmptyValue`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        matches!(&self.data, Data::Scalar(Value::Null))
            || matches!(&self.data, Data::Scalar(Value::Str(text)) if text.is_empty())
    }
}

/// `root`, a parsed file shown as `file`, resolved; or every scalar whose
/// tag doesn't fit its text, each an issue, which never shows the value of
/// a path `secret` names (`rules::secret_paths`).
///
/// # Errors
///
/// The issues above.
pub fn build(
    file: &str,
    root: &MarkedYamlOwned,
    secret: &dyn Fn(&str) -> bool,
) -> Result<Node, Vec<Issue>> {
    let mut issues = Vec::new();
    let node = convert(file, root, &mut String::new(), secret, false, &mut issues);
    if issues.is_empty() {
        Ok(node)
    } else {
        issues.sort();
        Err(issues)
    }
}

/// A whole file's tree, handed to a caller (`run::check_file`) that may
/// keep it on any stack. A file is nested up to `yaml::MAX_DEPTH` levels,
/// and `Node`'s own drop recurses once per level, more than a tokio
/// worker's stack holds; this drops it level by level from a list on the
/// heap instead, so its depth costs no stack. Its debug output names no
/// value (secrets) and doesn't recurse. Only its own drop is flat: read
/// it by known paths, and only when its file has no issues; a clone of a
/// subtree or a recursive walk still costs a frame per level.
pub struct Tree(Node);

impl Tree {
    /// `root`, to be dropped level by level.
    #[must_use]
    pub fn new(root: Node) -> Self {
        Self(root)
    }
}

impl std::ops::Deref for Tree {
    type Target = Node;

    fn deref(&self) -> &Node {
        &self.0
    }
}

impl std::fmt::Debug for Tree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tree")
            .field("line", &self.0.line)
            .field("column", &self.0.column)
            .finish_non_exhaustive()
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let mut pending = vec![std::mem::replace(
            &mut self.0.data,
            Data::Scalar(Value::Null),
        )];
        // Each collection's children are moved out before it is dropped,
        // so no drop below goes more than one level down.
        while let Some(data) = pending.pop() {
            match data {
                Data::Mapping(entries) => {
                    pending.extend(entries.into_iter().map(|(_, node)| node.data));
                }
                Data::Sequence(items) => pending.extend(items.into_iter().map(|node| node.data)),
                Data::Scalar(_) => {}
            }
        }
    }
}

/// `node` without a tag on a collection: saphyr keeps a tagged
/// mapping or sequence as `Tagged`; yaml.v3 decodes the collection itself.
fn untagged(node: &MarkedYamlOwned) -> &MarkedYamlOwned {
    match &node.data {
        YamlDataOwned::Tagged(_, inner) => untagged(inner),
        _ => node,
    }
}

/// `node` at key path `path`, its tag errors added to `issues`. Each child
/// adds its segment to `path` and takes it off again, so a tree's paths
/// take its depth in memory, not its depth squared.
fn convert(
    file: &str,
    node: &MarkedYamlOwned,
    path: &mut String,
    secret: &dyn Fn(&str) -> bool,
    under_secret: bool,
    issues: &mut Vec<Issue>,
) -> Node {
    // Everything under a secret key is secret too.
    let hidden = under_secret || secret(path);
    let (line, column) = (node.span.start.line(), node.span.start.col() + 1);
    let mut source = None;
    let data = match &untagged(node).data {
        YamlDataOwned::Mapping(map) => Data::Mapping(
            map.iter()
                .map(|(key, value)| {
                    let text = match &untagged(key).data {
                        YamlDataOwned::Representation(text, ..) => text.clone(),
                        other => {
                            // The loader leaves a mapping or a sequence here.
                            let kind = if matches!(other, YamlDataOwned::Sequence(_)) {
                                "a sequence"
                            } else {
                                "a mapping"
                            };
                            issues.push(Issue {
                                file: file.to_string(),
                                line: key.span.start.line(),
                                column: key.span.start.col() + 1,
                                path: path.clone(),
                                message: format!("a key must be a scalar, not {kind}"),
                            });
                            "<key>".to_string()
                        }
                    };
                    let parent = path.len();
                    if parent > 0 {
                        path.push('.');
                    }
                    path.push_str(&text);
                    let key = Key {
                        text,
                        line: key.span.start.line(),
                        column: key.span.start.col() + 1,
                    };
                    let child = convert(file, value, path, secret, hidden, issues);
                    path.truncate(parent);
                    (key, child)
                })
                .collect(),
        ),
        YamlDataOwned::Sequence(items) => Data::Sequence(
            items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    let parent = path.len();
                    let _ = write!(path, "[{index}]");
                    let child = convert(file, item, path, secret, hidden, issues);
                    path.truncate(parent);
                    child
                })
                .collect(),
        ),
        YamlDataOwned::Representation(text, style, tag) => {
            match resolve(text, *style, tag.as_ref()) {
                Ok(value) => {
                    if !matches!(value, Value::Str(_) | Value::Null) {
                        source = Some(text.clone());
                    }
                    Data::Scalar(value)
                }
                Err(message) => {
                    // yaml.v3's message quotes the text: not a secret's.
                    let message = if hidden {
                        message.replacen(
                            &format!("`{text}`"),
                            &format!("`{}`", crate::secret::REDACTED),
                            1,
                        )
                    } else {
                        message
                    };
                    issues.push(Issue {
                        file: file.to_string(),
                        line,
                        column,
                        path: path.clone(),
                        message,
                    });
                    Data::Scalar(Value::Null)
                }
            }
        }
        // `early_parse(false)` keeps every scalar a representation, and the
        // loader resolves aliases and drops bad values as errors (parse).
        _ => Data::Scalar(Value::Null),
    };
    Node {
        line,
        column,
        data,
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse;

    fn tree(text: &str) -> Node {
        let root = parse("t.yml", text).expect("parses");
        build("t.yml", &root, &|_| false).unwrap_or_else(|issues| panic!("{issues:?}"))
    }

    /// A tree nested as deep as a file may be is dropped on a small stack
    /// (a tokio worker's is 2 MiB), mappings and sequences alike, and its
    /// debug output shows neither its values nor its depth.
    #[test]
    fn a_deep_tree_drops_without_recursion() {
        let mut node = Node {
            line: 1,
            column: 1,
            data: Data::Scalar(Value::Str("secret-leaf".into())),
            source: None,
        };
        for level in 0..sb_cli_core::config::yaml::MAX_DEPTH {
            let data = if level % 2 == 0 {
                Data::Sequence(vec![node])
            } else {
                let key = Key {
                    text: "k".into(),
                    line: 1,
                    column: 1,
                };
                Data::Mapping(vec![(key, node)])
            };
            node = Node {
                line: 2,
                column: 3,
                data,
                source: None,
            };
        }
        let tree = Tree::new(node);
        assert!(matches!(tree.data, Data::Mapping(_)));
        let shown = format!("{tree:?}");
        assert_eq!(shown, "Tree { line: 2, column: 3, .. }");
        std::thread::Builder::new()
            .stack_size(64 * 1024)
            .spawn(move || drop(tree))
            .expect("thread")
            .join()
            .expect("dropped without overflow");
    }

    /// A Go `string` field gets a scalar's text as written: yaml.v3's
    /// `decode.go` sets `n.Value` for any scalar that resolved to
    /// something other than a string or null (M44: an unquoted
    /// `password: 0123`).
    #[test]
    fn go_string_is_the_text_as_written() {
        let node = tree(
            "octal: 0123\nhex: 0x1F\nfloat: 1.50\nbool: true\ntagged: !!int \"017\"\n\
             quoted: '0123'\nplain: abc\nnull: ~\nempty: ''\ndate: 2001-12-14\nlist: [1]\n",
        );
        let read = |key: &str| node.get(key).expect(key).go_string();
        assert_eq!(
            node.get("octal").unwrap().data,
            Data::Scalar(Value::Int(83))
        );
        assert_eq!(read("octal"), Some("0123"));
        assert_eq!(read("hex"), Some("0x1F"));
        assert_eq!(read("float"), Some("1.50"));
        assert_eq!(read("bool"), Some("true"));
        assert_eq!(read("tagged"), Some("017"));
        assert_eq!(read("quoted"), Some("0123"));
        assert_eq!(read("plain"), Some("abc"));
        assert_eq!(read("null"), None);
        assert_eq!(read("empty"), Some(""));
        assert_eq!(read("date"), Some("2001-12-14"));
        assert_eq!(read("list"), None);
        assert_eq!(node.go_string(), None);
        // Strings and null keep no second copy.
        assert_eq!(node.get("plain").unwrap().source, None);
        assert_eq!(node.get("null").unwrap().source, None);
    }

    #[test]
    fn scalars_are_resolved_and_positions_one_based() {
        let node = tree("a: 017\nb:\n  - x\n  - 'y'\nc: ~\n");
        let Data::Mapping(entries) = &node.data else {
            panic!("mapping");
        };
        let (key, a) = &entries[0];
        assert_eq!((key.text.as_str(), key.line, key.column), ("a", 1, 1));
        assert_eq!((a.line, a.column), (1, 4));
        assert_eq!(a.data, Data::Scalar(Value::Int(15)));
        let b = node.get("b").expect("b");
        assert_eq!(
            b.data,
            Data::Sequence(vec![
                Node {
                    line: 3,
                    column: 5,
                    data: Data::Scalar(Value::Str("x".into())),
                    source: None,
                },
                Node {
                    line: 4,
                    column: 5,
                    data: Data::Scalar(Value::Str("y".into())),
                    source: None,
                },
            ])
        );
        assert!(node.get("c").expect("c").is_empty());
        assert!(node.get("nope").is_none());
    }

    /// Keys are their text, as yaml.v3 decodes a key into a Go string.
    #[test]
    fn keys_are_their_text() {
        let node = tree("1: one\n'q': two\n");
        let Data::Mapping(entries) = &node.data else {
            panic!("mapping");
        };
        let keys: Vec<_> = entries.iter().map(|(k, _)| k.text.as_str()).collect();
        assert_eq!(keys, ["1", "q"]);
    }

    /// saphyr's loader, as the pre-pass would never hand it over: parse
    /// refuses a collection key first (decision 38).
    fn loaded(text: &str) -> MarkedYamlOwned {
        let mut loader = saphyr::YamlLoader::<MarkedYamlOwned>::default();
        loader.early_parse(false);
        saphyr_parser::Parser::new_from_str(text)
            .load(&mut loader, false)
            .expect("loads");
        loader.into_documents().remove(0)
    }

    /// A key that is a collection fails the file there, as yaml.v3's
    /// decode does (`invalid map key`); the pre-pass refuses it before.
    #[test]
    fn a_collection_key_is_an_issue() {
        let root = loaded("a: 1\n? [x]\n: three\n");
        let issues: Vec<String> = build("t.yml", &root, &|_| false)
            .expect_err("fails")
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            issues,
            ["t.yml:2:3: a key must be a scalar, not a sequence"]
        );
    }

    /// A tag on a collection that isn't the core schema's (Ansible's
    /// `!unsafe`) doesn't hide it (yaml.v3 decodes it as the mapping or
    /// sequence it is).
    #[test]
    fn tagged_collections_keep_their_content() {
        let node = tree("o: !unsafe {a: x}\nl: !unsafe [y]\n");
        assert_eq!(
            node.get("o").expect("o").get("a").expect("a").data,
            Data::Scalar(Value::Str("x".into()))
        );
        assert!(
            matches!(&node.get("l").expect("l").data, Data::Sequence(items) if items.len() == 1)
        );
    }

    /// A value from an alias is placed at the alias.
    #[test]
    fn an_alias_value_sits_at_the_alias() {
        let node = tree("a: &x abc\nb: *x\n");
        let b = node.get("b").expect("b");
        assert_eq!((b.line, b.column), (2, 4));
    }

    #[test]
    fn emptiness_is_null_or_an_empty_string() {
        let node = tree("a: ''\nb: null\nc: 0\nd: []\n");
        assert!(node.get("a").expect("a").is_empty());
        assert!(node.get("b").expect("b").is_empty());
        assert!(!node.get("c").expect("c").is_empty());
        assert!(!node.get("d").expect("d").is_empty());
    }

    /// yaml.v3 fails the decode at every scalar whose tag doesn't fit.
    #[test]
    fn a_tag_that_does_not_fit_is_an_issue() {
        let root = parse("t.yml", "a: !!int x\nb:\n  c: !!bool maybe\n").expect("parses");
        let issues: Vec<String> = build("t.yml", &root, &|_| false)
            .expect_err("fails")
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            issues,
            [
                "t.yml:1:4: a: cannot decode !!str `x` as a !!int",
                "t.yml:3:6: b.c: cannot decode !!str `maybe` as a !!bool",
            ]
        );
    }

    /// An alias resolves to its anchor's value (saphyr's loader copies it).
    #[test]
    fn aliases_hold_their_anchors_value() {
        let node = tree("a: &n 5\nb: *n\n");
        assert_eq!(node.get("b").expect("b").data, Data::Scalar(Value::Int(5)));
    }

    /// Only a mapping holds keys.
    #[test]
    fn get_needs_a_mapping() {
        let node = tree("l: [a]\n");
        assert!(node.get("l").expect("l").get("a").is_none());
        assert!(node.get("l").expect("l").entry("a").is_none());
    }

    #[test]
    fn a_mapping_key_is_an_issue_too() {
        let root = loaded("? {a: 1}\n: x\n");
        let issues: Vec<String> = build("t.yml", &root, &|_| false)
            .expect_err("fails")
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(issues, ["t.yml:1:3: a key must be a scalar, not a mapping"]);
    }

    /// A secret's value isn't echoed in a tag error (term-ui decision 8).
    #[test]
    fn a_secret_tag_error_hides_the_value() {
        let root = parse("t.yml", "dockerhub:\n  token: !!int s3cret\n").expect("parses");
        let issues: Vec<String> = build("t.yml", &root, &|path| path == "dockerhub.token")
            .expect_err("fails")
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            issues,
            ["t.yml:2:10: dockerhub.token: cannot decode !!str `[REDACTED]` as a !!int"]
        );
    }

    /// Everything under a secret key is secret too: a list or a mapping
    /// under one hides its values as well.
    #[test]
    fn values_under_a_secret_are_hidden() {
        let secret = |path: &str| path == "dockerhub.token" || path == "cloudflare.api";
        let root = parse(
            "t.yml",
            "dockerhub:\n  token: [!!int s3cret]\ncloudflare:\n  api: {x: !!int s3cret}\n",
        )
        .expect("parses");
        let issues: Vec<String> = build("t.yml", &root, &secret)
            .expect_err("fails")
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            issues,
            [
                "t.yml:2:11: dockerhub.token[0]: cannot decode !!str `[REDACTED]` as a !!int",
                "t.yml:4:12: cloudflare.api.x: cannot decode !!str `[REDACTED]` as a !!int",
            ]
        );
    }
}
