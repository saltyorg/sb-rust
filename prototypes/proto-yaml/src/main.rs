//! Prototype: can the saphyr family cover `sb validate-config`?
//! Schema files load typed via serde-saphyr; config files load as saphyr
//! `MarkedYaml` so every error carries file:line:col. Throwaway; see FINDINGS.md.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use saphyr::{LoadableYamlNode, MarkedYaml, Scalar, YamlData};
use serde::Deserialize;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Validate one config file, optionally against a schema (duplicates-only otherwise).
    File { config: PathBuf, schema: Option<PathBuf> },
    /// Validate the standard Saltbox config set: <config_dir> holds the .yml files,
    /// <schema_dir> the *.schema.yml files.
    All { config_dir: PathBuf, schema_dir: PathBuf },
}

/// Mirrors sb-go's SchemaRule. Unknown schema keys are rejected so drift shows up.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    min_length: usize,
    #[serde(default)]
    max_length: usize,
    #[serde(default)]
    not_equals: Option<Forbidden>,
    #[serde(default)]
    required_with: Vec<String>,
    #[serde(default)]
    required_when_true: Vec<String>,
    #[serde(default)]
    validate_when_true: Vec<String>,
    #[serde(default)]
    custom_validator: Option<String>,
    /// Used by motd.schema.yml but absent from sb-go's SchemaRule, so sb-go ignores it.
    #[serde(default)]
    pattern: Option<String>,
    #[serde(default)]
    properties: Option<BTreeMap<String, Rule>>,
    #[serde(default)]
    items: Option<Box<Rule>>,
    /// Used by motd.schema.yml (display_names) but absent from sb-go's SchemaRule.
    #[serde(default, rename = "additionalProperties")]
    additional_properties: Option<Box<Rule>>,
    #[serde(default)]
    #[expect(dead_code, reason = "accepted so deny_unknown_fields allows example-generation keys")]
    description: Option<String>,
    #[serde(default)]
    #[expect(dead_code, reason = "accepted so deny_unknown_fields allows example-generation keys")]
    example: Option<serde::de::IgnoredAny>,
}

/// serde-saphyr has no generic Value type; not_equals only ever holds scalars.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Forbidden {
    Bool(bool),
    Int(i64),
    Str(String),
}

impl Forbidden {
    fn as_string(&self) -> String {
        match self {
            Self::Bool(b) => b.to_string(),
            Self::Int(i) => i.to_string(),
            Self::Str(s) => s.clone(),
        }
    }
}

struct Issue {
    line: usize,
    col: usize,
    path: String,
    msg: String,
}

#[derive(Default)]
struct Report {
    issues: Vec<Issue>,
    /// Types/validators the prototype does not implement, counted for FINDINGS.
    unchecked: BTreeMap<String, usize>,
}

impl Report {
    fn push(&mut self, node: &MarkedYaml, path: &str, msg: impl Into<String>) {
        self.issues.push(Issue {
            line: node.span.start.line(),
            col: node.span.start.col() + 1,
            path: path.to_string(),
            msg: msg.into(),
        });
    }
}

fn main() -> Result<()> {
    let ok = match Cli::parse().action {
        Action::File { config, schema } => validate_file(&config, schema.as_deref())?,
        Action::All { config_dir, schema_dir } => {
            let mut ok = true;
            for (cfg, schema) in [
                ("accounts.yml", Some("accounts")),
                ("adv_settings.yml", Some("adv_settings")),
                ("backup_config.yml", Some("backup_config")),
                ("hetzner_vlan.yml", Some("hetzner_vlan")),
                ("settings.yml", Some("settings")),
                ("motd.yml", Some("motd")),
                ("inventories/host_vars/localhost.yml", None),
            ] {
                let path = config_dir.join(cfg);
                if !path.exists() {
                    println!("skip {cfg} (not present)");
                    continue;
                }
                let schema = schema.map(|s| schema_dir.join(format!("{s}.schema.yml")));
                ok &= validate_file(&path, schema.as_deref())?;
            }
            ok
        }
    };
    std::process::exit(if ok { 0 } else { 1 });
}

fn validate_file(config: &Path, schema: Option<&Path>) -> Result<bool> {
    let text = std::fs::read_to_string(config).with_context(|| format!("read {}", config.display()))?;
    let name = config.display();
    // Parse errors (including duplicate keys) come straight from saphyr with a marker.
    let docs = match MarkedYaml::load_from_str(&text) {
        Ok(docs) => docs,
        Err(e) => {
            println!("{name}: {e}");
            return Ok(false);
        }
    };
    let Some(root) = docs.first() else {
        println!("{name}: empty document");
        return Ok(false);
    };
    let Some(schema) = schema else {
        println!("{name}: ok (duplicate-key check only)");
        return Ok(true);
    };
    let rules: BTreeMap<String, Rule> = serde_saphyr::from_str(&std::fs::read_to_string(schema)?)
        .with_context(|| format!("schema {}", schema.display()))?;

    let mut report = Report::default();
    validate_object(root, &rules, "", &mut report);
    for i in &report.issues {
        println!("{name}:{}:{}: {}: {}", i.line, i.col, i.path, i.msg);
    }
    if report.issues.is_empty() {
        println!("{name}: ok");
    }
    if !report.unchecked.is_empty() {
        println!("  (not implemented in prototype: {:?})", report.unchecked);
    }
    Ok(report.issues.is_empty())
}

fn mapping<'a>(node: &'a MarkedYaml<'a>) -> Option<Vec<(String, &'a MarkedYaml<'a>)>> {
    match &node.data {
        YamlData::Mapping(m) => Some(
            m.iter()
                .map(|(k, v)| (scalar_string(k).unwrap_or_default(), v))
                .collect(),
        ),
        _ => None,
    }
}

fn scalar_string(node: &MarkedYaml) -> Option<String> {
    match &node.data {
        YamlData::Value(Scalar::String(s)) => Some(s.to_string()),
        YamlData::Value(Scalar::Integer(i)) => Some(i.to_string()),
        YamlData::Value(Scalar::FloatingPoint(f)) => Some(f.to_string()),
        YamlData::Value(Scalar::Boolean(b)) => Some(b.to_string()),
        _ => None,
    }
}

fn is_empty(node: &MarkedYaml) -> bool {
    match &node.data {
        YamlData::Value(Scalar::Null) => true,
        YamlData::Value(Scalar::String(s)) => s.is_empty(),
        YamlData::Sequence(s) => s.is_empty(),
        YamlData::Mapping(m) => m.is_empty(),
        _ => false,
    }
}

/// sb-go's parseAnsibleBool: real booleans plus yes/no/on/off/1/0 strings.
fn ansible_bool(node: &MarkedYaml) -> Option<bool> {
    match &node.data {
        YamlData::Value(Scalar::Boolean(b)) => Some(*b),
        YamlData::Value(Scalar::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "yes" | "true" | "on" | "1" => Some(true),
            "no" | "false" | "off" | "0" => Some(false),
            _ => None,
        },
        YamlData::Value(Scalar::Integer(1)) => Some(true),
        YamlData::Value(Scalar::Integer(0)) => Some(false),
        _ => None,
    }
}

fn any_true(siblings: &[(String, &MarkedYaml)], fields: &[String]) -> bool {
    fields.iter().any(|f| {
        siblings
            .iter()
            .any(|(k, v)| k == f && ansible_bool(v) == Some(true))
    })
}

fn join(base: &str, field: &str) -> String {
    if base.is_empty() { field.to_string() } else { format!("{base}.{field}") }
}

fn validate_object(node: &MarkedYaml, rules: &BTreeMap<String, Rule>, path: &str, r: &mut Report) {
    let Some(entries) = mapping(node) else {
        r.push(node, path, "must be a mapping");
        return;
    };
    for (field, rule) in rules {
        let fpath = join(path, field);
        let value = entries.iter().find(|(k, _)| k == field).map(|(_, v)| *v);
        let required = rule.required || any_true(&entries, &rule.required_when_true);
        match value {
            None if required => r.push(node, &fpath, "is required"),
            None => {}
            Some(_) if !rule.validate_when_true.is_empty() && !any_true(&entries, &rule.validate_when_true) => {}
            Some(v) => validate_field(v, rule, &fpath, &entries, r),
        }
    }
    for (field, v) in &entries {
        if !rules.contains_key(field) {
            r.push(v, &join(path, field), "unknown field");
        }
    }
}

fn validate_field(v: &MarkedYaml, rule: &Rule, path: &str, siblings: &[(String, &MarkedYaml)], r: &mut Report) {
    if let Some(forbidden) = &rule.not_equals
        && scalar_string(v) == Some(forbidden.as_string())
    {
        r.push(v, path, "must not equal its configured default value");
    }
    if !rule.required_with.is_empty() && is_empty(v) {
        let present = rule
            .required_with
            .iter()
            .any(|f| siblings.iter().any(|(k, sv)| k == f && !is_empty(sv)));
        if present {
            r.push(v, path, format!("is required when any of {:?} are present", rule.required_with));
        }
    }
    if !rule.required && is_empty(v) {
        return;
    }
    if rule.min_length > 0 || rule.max_length > 0 {
        match &v.data {
            YamlData::Value(Scalar::String(s)) => {
                let n = s.chars().count();
                if rule.min_length > 0 && n < rule.min_length {
                    r.push(v, path, format!("must be at least {} characters long, got {n}", rule.min_length));
                }
                if rule.max_length > 0 && n > rule.max_length {
                    r.push(v, path, format!("must be at most {} characters long, got {n}", rule.max_length));
                }
            }
            _ => r.push(v, path, "must be a string"),
        }
    }
    if rule.format.as_deref() == Some("email") {
        let ok = scalar_string(v).is_some_and(|s| s.split_once('@').is_some_and(|(a, d)| !a.is_empty() && d.contains('.')));
        if !ok {
            r.push(v, path, "must be a valid email address");
        }
    }
    match rule.kind.as_str() {
        "object" => {
            if let Some(props) = &rule.properties {
                validate_object(v, props, path, r);
            } else if let Some(value_rule) = &rule.additional_properties {
                match mapping(v) {
                    Some(entries) => {
                        for (k, item) in &entries {
                            validate_field(item, value_rule, &join(path, k), &entries, r);
                        }
                    }
                    None => r.push(v, path, "must be a mapping"),
                }
            }
        }
        "array" => match &v.data {
            YamlData::Sequence(items) => {
                if let Some(item_rule) = &rule.items {
                    for (i, item) in items.iter().enumerate() {
                        validate_field(item, item_rule, &format!("{path}[{i}]"), siblings, r);
                    }
                }
            }
            _ => r.push(v, path, "must be a list"),
        },
        "number" => {
            let ok = matches!(v.data, YamlData::Value(Scalar::Integer(_) | Scalar::FloatingPoint(_)))
                || scalar_string(v).is_some_and(|s| s.parse::<f64>().is_ok());
            if !ok {
                r.push(v, path, "must be a number");
            }
        }
        "boolean" | "ansible_bool" => {
            if ansible_bool(v).is_none() {
                r.push(v, path, "must be a boolean (true/false/yes/no)");
            }
        }
        "url" => {
            if !scalar_string(v).is_some_and(|s| s.starts_with("http://") || s.starts_with("https://")) {
                r.push(v, path, "must be an http(s) URL");
            }
        }
        "string" | "" => {}
        other => *r.unchecked.entry(other.to_string()).or_default() += 1,
    }
    if rule.pattern.is_some() {
        *r.unchecked.entry("pattern".to_string()).or_default() += 1;
    }
    if let Some(name) = &rule.custom_validator {
        *r.unchecked.entry(format!("custom:{name}")).or_default() += 1;
    }
}
