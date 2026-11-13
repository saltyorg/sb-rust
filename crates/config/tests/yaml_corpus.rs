//! The YAML corpus (sb-rust hardening plan, decision 3; findings config-H1
//! and config-L1): sb's parser over every case in `yaml-corpus/cases`,
//! against libyaml's verdicts in `yaml-corpus/expected.json`, which
//! `scripts/yaml-corpus.py` generated once through the libyaml loader
//! of `PyYAML` (docs/platform-notes/yaml-acceptance.md). Every
//! difference, in acceptance or in a scalar's path, style, text or
//! resolved value, is listed exactly in `yaml-corpus/mismatches.txt`: a
//! parser change that closes one, or opens another, fails here until the
//! list says so.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use saphyr::{MarkedYamlOwned, ScalarStyle, YamlDataOwned};
use sb_config::parse::parse;
use sb_config::scalar::Value;
use sb_config::tree::{self, Data, Node};
use serde_json::Value as Json;

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/yaml-corpus")
}

/// Every case as (name, text), in name order.
fn cases() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(corpus().join("cases")).expect("cases") {
        let path = entry.expect("an entry").path();
        // The generator reads `*.yml` only.
        if path.extension().is_none_or(|e| e != "yml") {
            continue;
        }
        let name = path
            .file_stem()
            .expect("a name")
            .to_string_lossy()
            .into_owned();
        // sb reads config files as UTF-8 text (cli-core); a BOM is the
        // parser's to handle.
        let text = String::from_utf8(std::fs::read(&path).expect("read")).expect("UTF-8");
        out.push((name, text));
    }
    out.sort();
    out
}

fn expected() -> Json {
    serde_json::from_str(
        &std::fs::read_to_string(corpus().join("expected.json")).expect("expected.json"),
    )
    .expect("JSON")
}

/// One scalar value as both sides record it: key path, style, text as
/// parsed, resolved value as [type, text].
#[derive(Debug, PartialEq)]
struct Scalar {
    path: String,
    style: String,
    text: String,
    value: [String; 2],
}

/// sb's verdict on a case: its scalars, or the issue that refused it.
fn sb(file: &str, text: &str) -> Result<Vec<Scalar>, String> {
    let first = |issues: Vec<sb_config::issue::Issue>| {
        let i = issues.first().expect("a refusal carries an issue");
        format!("{} at line {}, column {}", i.message, i.line, i.column)
    };
    let root = parse(file, text).map_err(first)?;
    let resolved = tree::build(file, &root, &|_| false).map_err(first)?;
    let mut raw = Vec::new();
    texts(&root, "", &mut raw);
    let mut values = Vec::new();
    resolved_values(&resolved, "", &mut values);
    // The two walks pair up only if they visit the same values in order.
    let raw_paths: Vec<_> = raw.iter().map(|(path, ..)| path).collect();
    let value_paths: Vec<_> = values.iter().map(|(path, _)| path).collect();
    assert_eq!(
        raw_paths, value_paths,
        "{file}: the tree and the parse differ"
    );
    Ok(raw
        .into_iter()
        .zip(values)
        .map(|((path, style, text), (_, value))| Scalar {
            path,
            style,
            text,
            value,
        })
        .collect())
}

fn style(style: ScalarStyle) -> &'static str {
    match style {
        ScalarStyle::Plain => "plain",
        ScalarStyle::SingleQuoted => "single",
        ScalarStyle::DoubleQuoted => "double",
        ScalarStyle::Literal => "literal",
        ScalarStyle::Folded => "folded",
    }
}

fn untagged(node: &MarkedYamlOwned) -> &MarkedYamlOwned {
    match &node.data {
        YamlDataOwned::Tagged(_, inner) => untagged(inner),
        _ => node,
    }
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

/// Every scalar value's (path, style, text) under `node`, in document
/// order, as the generator walks libyaml's nodes; an empty collection
/// as a `seq` or `map` entry.
fn texts(node: &MarkedYamlOwned, path: &str, out: &mut Vec<(String, String, String)>) {
    match &untagged(node).data {
        YamlDataOwned::Representation(text, s, _) => {
            out.push((path.to_string(), style(*s).to_string(), text.clone()));
        }
        YamlDataOwned::Sequence(items) => {
            if items.is_empty() {
                out.push((path.to_string(), "seq".to_string(), String::new()));
            }
            for (index, item) in items.iter().enumerate() {
                texts(item, &format!("{path}[{index}]"), out);
            }
        }
        YamlDataOwned::Mapping(map) => {
            if map.is_empty() {
                out.push((path.to_string(), "map".to_string(), String::new()));
            }
            for (key, value) in map {
                let name = match &untagged(key).data {
                    YamlDataOwned::Representation(text, ..) => text.clone(),
                    _ => "<key>".to_string(),
                };
                texts(value, &join(path, &name), out);
            }
        }
        other => out.push((path.to_string(), "?".to_string(), format!("{other:?}"))),
    }
}

/// Every scalar value's path and resolved [type, text] under `node`, in
/// document order, written as the generator writes Python's; an empty
/// collection as `seq` or `map`.
fn resolved_values(node: &Node, path: &str, out: &mut Vec<(String, [String; 2])>) {
    let mut push = |value: [String; 2]| out.push((path.to_string(), value));
    match &node.data {
        Data::Sequence(items) if items.is_empty() => push(["seq".into(), String::new()]),
        Data::Mapping(entries) if entries.is_empty() => push(["map".into(), String::new()]),
        Data::Scalar(value) => push(match value {
            Value::Null => ["null".into(), String::new()],
            Value::Bool(b) => ["bool".into(), b.to_string()],
            Value::Int(i) => ["int".into(), i.to_string()],
            Value::Float(f) => ["float".into(), format!("{f:?}")],
            Value::Str(s) => ["str".into(), s.clone()],
            Value::Timestamp(t) => ["timestamp".into(), t.clone()],
        }),
        Data::Sequence(items) => {
            for (index, item) in items.iter().enumerate() {
                resolved_values(item, &format!("{path}[{index}]"), out);
            }
        }
        Data::Mapping(entries) => {
            for (key, value) in entries {
                resolved_values(value, &join(path, &key.text), out);
            }
        }
    }
}

/// libyaml's verdict on a case, from expected.json.
fn libyaml(case: &Json) -> Result<Vec<Scalar>, String> {
    if case["accepted"] != Json::Bool(true) {
        return Err(case["error"].as_str().unwrap_or_default().to_string());
    }
    let text = |v: &Json| v.as_str().expect("a string").to_string();
    Ok(case["scalars"]
        .as_array()
        .expect("scalars")
        .iter()
        .map(|s| Scalar {
            path: text(&s["path"]),
            style: text(&s["style"]),
            text: text(&s["text"]),
            value: [text(&s["value"][0]), text(&s["value"][1])],
        })
        .collect())
}

/// Two resolved values are the same: floats by value, since Rust and
/// Python print them differently (`1e20`, `1e+20`).
fn same_value(a: &[String; 2], b: &[String; 2]) -> bool {
    if a[0] == "float" && b[0] == "float" {
        let parse = |t: &str| t.parse::<f64>().ok();
        return match (parse(&a[1]), parse(&b[1])) {
            (Some(x), Some(y)) => x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()),
            _ => a[1] == b[1],
        };
    }
    a == b
}

/// The differences between sb's verdict and libyaml's on case `name`.
fn differences(
    name: &str,
    sb: &Result<Vec<Scalar>, String>,
    lib: &Result<Vec<Scalar>, String>,
) -> Vec<String> {
    match (sb, lib) {
        (Err(sb), Ok(_)) => vec![format!("{name}: libyaml accepts, sb refuses: {sb}")],
        (Ok(_), Err(lib)) => vec![format!("{name}: libyaml refuses ({lib}), sb accepts")],
        (Err(_), Err(_)) => Vec::new(),
        (Ok(ours), Ok(theirs)) => {
            if ours.len() != theirs.len() {
                return vec![format!(
                    "{name}: {} scalars, libyaml {}",
                    ours.len(),
                    theirs.len()
                )];
            }
            let mut out = Vec::new();
            for (a, b) in ours.iter().zip(theirs) {
                let at = if b.path.is_empty() { "(root)" } else { &b.path };
                if a.path != b.path {
                    out.push(format!("{name}: path {:?}, libyaml {:?}", a.path, b.path));
                    continue;
                }
                if (&a.style, &a.text) != (&b.style, &b.text) {
                    out.push(format!(
                        "{name}: {at}: {} {:?}, libyaml {} {:?}",
                        a.style, a.text, b.style, b.text
                    ));
                }
                if !same_value(&a.value, &b.value) {
                    out.push(format!(
                        "{name}: {at}: resolves to {} {:?}, libyaml {} {:?}",
                        a.value[0], a.value[1], b.value[0], b.value[1]
                    ));
                }
            }
            out
        }
    }
}

/// sb's parser over the whole corpus gives exactly the listed differences.
#[test]
fn the_corpus_differs_from_libyaml_exactly_as_listed() {
    let expected = expected();
    let listed_cases = expected["cases"].as_object().expect("cases");
    let mut found = Vec::new();
    let mut seen = BTreeSet::new();
    for (name, text) in cases() {
        seen.insert(name.clone());
        let lib = libyaml(listed_cases.get(&name).unwrap_or_else(|| {
            panic!("{name} isn't in expected.json: run scripts/yaml-corpus.py")
        }));
        found.extend(differences(&name, &sb(&format!("{name}.yml"), &text), &lib));
    }
    assert_eq!(
        seen.iter().collect::<Vec<_>>(),
        listed_cases.keys().collect::<Vec<_>>(),
        "expected.json doesn't match cases/: run scripts/yaml-corpus.py"
    );
    found.sort();
    // The list is grouped under comments; compared in sorted order.
    let mut listed: Vec<String> = std::fs::read_to_string(corpus().join("mismatches.txt"))
        .expect("mismatches.txt")
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect();
    listed.sort();
    let added: Vec<_> = found.iter().filter(|l| !listed.contains(l)).collect();
    let removed: Vec<_> = listed.iter().filter(|l| !found.contains(l)).collect();
    assert!(
        found == listed,
        "the mismatches changed.\nNew: {added:#?}\nGone: {removed:#?}\nThe list is now:\n{}",
        found.join("\n")
    );
}

/// cli-core's inventory read accepts exactly the cases validate-config's
/// parser accepts, and refuses the others at the same line and column:
/// both go through the same pre-pass (decisions 28 and 38), so
/// `appdata_path` falls back to `/opt` only on a file Ansible refuses too.
/// A root that isn't a mapping is read by both; only validate-config then
/// refuses it as a config file, which is no YAML refusal.
#[test]
fn the_inventory_read_agrees_with_validate_config() {
    let mut differ = Vec::new();
    for (name, text) in cases() {
        let parsed = match parse(&format!("{name}.yml"), &text) {
            Err(issues) if issues[0].message.starts_with("the file must be a mapping") => Ok(()),
            parsed => parsed
                .map(|_| ())
                .map_err(|issues| (issues[0].line, issues[0].column)),
        };
        let read = sb_cli_core::config::inventory_appdata(&text)
            .map(|_| ())
            .map_err(|refusal| (refusal.line, refusal.column));
        if parsed != read {
            differ.push(format!(
                "{name}: validate-config {parsed:?}, inventory {read:?}"
            ));
        }
    }
    assert!(differ.is_empty(), "{differ:#?}");
}

/// What saphyr reads and libyaml refuses (decision 38: a second document,
/// a mapping dedented past its first line, a `:` with no key, a key
/// split over lines, content after `...`, an end marker alone, and what
/// differential fuzzing found: an anchor defined twice or ending in a
/// character libyaml stops at, a flow value's `:` before a bracket, `>`
/// starting a flow value, a tab after `...`; and what the code review
/// found: a control character, a directive libyaml doesn't read, a tag
/// before a bracket, a top-level block scalar at column 0, a flow key
/// split over lines, a collection as a key; and decision 41's tag
/// Ansible's loader has no constructor for) is refused at the line and
/// column libyaml and Ansible's constructor name. A NEL is refused where
/// it is, which libyaml takes as a line break.
#[test]
fn what_libyaml_refuses_is_refused_where_it_says() {
    let expected = expected();
    let cases = cases();
    let sb_at = |name: &str| {
        let (_, text) = cases
            .iter()
            .find(|(case, _)| case == name)
            .expect("the case");
        let issues = parse(&format!("{name}.yml"), text).expect_err("refused");
        format!(" at line {}, column {}", issues[0].line, issues[0].column)
    };
    for name in [
        "multi-document",
        "mapping-indented-then-dedented",
        "colon-no-key-at-top",
        "colon-no-key-after-entry",
        "colon-no-key-in-flow",
        "explicit-key-two-values",
        "flow-map-multi-line-quoted-key",
        "content-after-end-marker",
        "end-marker-alone",
        "anchor-defined-twice",
        "anchor-name-then-dot",
        "flow-plain-colon-before-brace",
        "flow-block-indicator",
        "tab-after-end-marker",
        "control-character",
        "directive-yaml-1-3",
        "tag-before-bracket",
        "top-level-block-scalar-at-column-0",
        "flow-key-across-lines",
        "collection-as-key",
        "tag-custom",
    ] {
        let lib = expected["cases"][name]["error"].as_str().expect("an error");
        let at = sb_at(name);
        assert!(lib.ends_with(&at), "{name}: sb{at}, libyaml {lib}");
    }
    // Where libyaml names a later point than sb: the line after a NEL,
    // the directive's name, the token after a flow `?`.
    assert_eq!(sb_at("nel-line-break"), " at line 1, column 5");
    assert_eq!(sb_at("directive-unknown"), " at line 1, column 1");
    assert_eq!(sb_at("flow-question-without-space"), " at line 1, column 5");
}

/// The tab layouts libyaml refuses (config-L1) are refused at the line and
/// column libyaml names, in the original file.
#[test]
fn tabs_libyaml_refuses_are_refused_where_it_says() {
    let expected = expected();
    let cases = cases();
    for name in ["tab-after-dash", "tab-indented-top-level", "tab-only-line"] {
        let (_, text) = cases
            .iter()
            .find(|(case, _)| case == name)
            .expect("the case");
        let issues = parse(&format!("{name}.yml"), text).expect_err("refused");
        let lib = expected["cases"][name]["error"].as_str().expect("an error");
        let at = format!(" at line {}, column {}", issues[0].line, issues[0].column);
        assert!(lib.ends_with(&at), "{name}: sb{at}, libyaml {lib}");
        assert!(
            issues[0].message.starts_with("syntax error: a tab "),
            "{name}: {}",
            issues[0].message
        );
    }
}
