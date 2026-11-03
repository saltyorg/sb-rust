//! A parsed config file as the rules see it: every scalar resolved as
//! sb-go's yaml.v3 decodes it into Go's `interface{}`, every node and key
//! with its 1-based position. A scalar whose tag doesn't fit its text
//! fails the file there, as yaml.v3's decode does.

use saphyr::{MarkedYamlOwned, YamlDataOwned};

use crate::issue::Issue;
use crate::scalar::{Value, resolve};

/// A value in the file, with where it starts.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub line: usize,
    pub column: usize,
    pub data: Data,
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
    pub fn get(&self, key: &str) -> Option<&Node> {
        self.entry(key).map(|(_, value)| value)
    }

    /// Null or an empty string: sb-go's `isEmptyValue`.
    pub fn is_empty(&self) -> bool {
        matches!(&self.data, Data::Scalar(Value::Null))
            || matches!(&self.data, Data::Scalar(Value::Str(text)) if text.is_empty())
    }
}

/// `root`, a parsed file shown as `file`, resolved; or every scalar whose
/// tag doesn't fit its text, each an issue, which never shows the value of
/// a path `secret` names (`rules::secret_paths`).
pub fn build(
    file: &str,
    root: &MarkedYamlOwned,
    secret: &dyn Fn(&str) -> bool,
) -> Result<Node, Vec<Issue>> {
    let mut issues = Vec::new();
    let node = convert(file, root, "", secret, false, &mut issues);
    if issues.is_empty() {
        Ok(node)
    } else {
        issues.sort();
        Err(issues)
    }
}

/// `node` without a custom tag on a collection: saphyr keeps a tagged
/// mapping or sequence as `Tagged`; yaml.v3 decodes the collection itself.
fn untagged(node: &MarkedYamlOwned) -> &MarkedYamlOwned {
    match &node.data {
        YamlDataOwned::Tagged(_, inner) => untagged(inner),
        _ => node,
    }
}

/// `node` at key path `path`, its tag errors added to `issues`.
fn convert(
    file: &str,
    node: &MarkedYamlOwned,
    path: &str,
    secret: &dyn Fn(&str) -> bool,
    under_secret: bool,
    issues: &mut Vec<Issue>,
) -> Node {
    // Everything under a secret key is secret too.
    let hidden = under_secret || secret(path);
    let (line, column) = (node.span.start.line(), node.span.start.col() + 1);
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
                                path: path.to_string(),
                                message: format!("a key must be a scalar, not {kind}"),
                            });
                            "<key>".to_string()
                        }
                    };
                    let child = if path.is_empty() {
                        text.clone()
                    } else {
                        format!("{path}.{text}")
                    };
                    let key = Key {
                        text,
                        line: key.span.start.line(),
                        column: key.span.start.col() + 1,
                    };
                    (key, convert(file, value, &child, secret, hidden, issues))
                })
                .collect(),
        ),
        YamlDataOwned::Sequence(items) => Data::Sequence(
            items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    convert(
                        file,
                        item,
                        &format!("{path}[{index}]"),
                        secret,
                        hidden,
                        issues,
                    )
                })
                .collect(),
        ),
        YamlDataOwned::Representation(text, style, tag) => {
            match resolve(text, *style, tag.as_ref()) {
                Ok(value) => Data::Scalar(value),
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
                        path: path.to_string(),
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
    Node { line, column, data }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse;

    fn tree(text: &str) -> Node {
        let root = parse("t.yml", text).expect("parses");
        build("t.yml", &root, &|_| false).unwrap_or_else(|issues| panic!("{issues:?}"))
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
                    data: Data::Scalar(Value::Str("x".into()))
                },
                Node {
                    line: 4,
                    column: 5,
                    data: Data::Scalar(Value::Str("y".into()))
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

    /// A key that is a collection fails the file there, as yaml.v3's
    /// decode does (`invalid map key`).
    #[test]
    fn a_collection_key_is_an_issue() {
        let root = parse("t.yml", "a: 1\n? [x]\n: three\n").expect("parses");
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

    /// A custom tag on a collection doesn't hide it (yaml.v3 decodes it
    /// as the mapping or sequence it is).
    #[test]
    fn tagged_collections_keep_their_content() {
        let node = tree("o: !foo {a: x}\nl: !foo [y]\n");
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
        let root = parse("t.yml", "? {a: 1}\n: x\n").expect("parses");
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
