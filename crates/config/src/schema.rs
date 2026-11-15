//! Saltbox's schema files (`schema/*.schema.yml`), loaded strictly into
//! typed rules (config spec, "Schema rules" and decision 5): sb-go's
//! `SchemaRule`, plus `pattern` and `additionalProperties`. Any key, type,
//! format or validator name sb doesn't know, a malformed value or a
//! pattern that doesn't compile fails loading, naming the file and the
//! line and column (and the key or value, where one is at fault).

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::Deserialize;
use serde::de::{self, Deserializer, IntoDeserializer, MapAccess, Visitor};

/// A schema file's rules: its top-level keys, each a config file's key.
pub type Rules = BTreeMap<String, Rule>;

/// One key's rule (sb-go's `SchemaRule`).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// None: no type check (sb-go's empty or missing type).
    #[serde(rename = "type", default, deserialize_with = "name")]
    pub kind: Option<Kind>,
    #[serde(default)]
    pub required: bool,
    #[serde(default, deserialize_with = "name")]
    pub format: Option<Format>,
    /// In bytes, as sb-go's `len`.
    #[serde(default)]
    pub min_length: usize,
    /// In bytes; 0 means no limit, as sb-go.
    #[serde(default)]
    pub max_length: usize,
    pub not_equals: Option<Scalar>,
    #[serde(default)]
    pub required_with: Vec<String>,
    #[serde(default)]
    pub required_when_true: Vec<String>,
    #[serde(default)]
    pub validate_when_true: Vec<String>,
    #[serde(default, deserialize_with = "name")]
    pub custom_validator: Option<Validator>,
    #[serde(default, deserialize_with = "pattern")]
    pub pattern: Option<regex::Regex>,
    pub properties: Option<Rules>,
    pub items: Option<Box<Rule>>,
    #[serde(rename = "additionalProperties")]
    pub additional_properties: Option<Additional>,
    /// For the example a file's generator writes (`sb motd
    /// --generate-config`, motd decision 18); no check reads it.
    pub description: Option<String>,
    /// The example value, as written (any YAML value with string keys);
    /// for the generator, as `description`. A `null` reads as none, as
    /// yaml.v3's `nil` does for sb-go.
    pub example: Option<serde_json::Value>,
}

/// A rule's `type`: sb-go's plain types and its type validators.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Object,
    Array,
    String,
    Boolean,
    Number,
    Float,
    AnsibleBool,
    Subdomain,
    Hostname,
    DirectoryPath,
    Url,
    Timezone,
    CronTime,
    RcloneTemplate,
    SshKeyOrUrl,
    Password,
}

/// A rule's `format` (sb-go's `validateFormat`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    Email,
    Hostname,
    Url,
}

/// A rule's `custom_validator`: sb-go's registry, without
/// `validate_rclone_remote` (decision 6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Validator {
    ValidateSshKeyOrUrl,
    ValidatePasswordStrength,
    ValidateCloudflareConfig,
    ValidateDockerhubConfig,
    ValidateAnsibleBool,
    ValidateTimezone,
    ValidateCronTime,
    ValidateDirectoryPath,
    ValidateRcloneTemplate,
    ValidateWholeNumber,
    ValidateUrl,
    ValidatePositiveNumber,
    ValidateSubdomain,
    ValidateHostname,
}

/// A `not_equals` value: a scalar, as serde-saphyr resolves it
/// (serde-saphyr has no generic value type). Unquoted `yes`, `on`, `y`
/// and leading-zero numbers resolve differently from sb-go's yaml.v3 (the
/// spec's differences); Saltbox quotes its values.
#[derive(Clone, Debug, PartialEq)]
pub enum Scalar {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

/// `additionalProperties`: a rule for every key outside `properties`, or
/// whether such keys are allowed at all.
#[derive(Clone, Debug)]
pub enum Additional {
    Allowed(bool),
    Rule(Box<Rule>),
}

/// A schema file that can't be used.
#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    #[error("schema {file}: {source}")]
    Read {
        file: String,
        #[source]
        source: sb_cli_core::config::ConfigError,
    },
    #[error("schema {file} doesn't exist")]
    Missing { file: String },
    #[error("schema {file}: {message}")]
    Invalid { file: String, message: String },
}

/// `Scalar` by hand rather than serde's `untagged`, which reports any
/// mismatch as "did not match any variant" at the mapping's start; a
/// visitor keeps serde-saphyr's own error and position
/// (<https://serde.rs/impl-deserialize.html>).
impl<'de> Deserialize<'de> for Scalar {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct ScalarVisitor;
        impl Visitor<'_> for ScalarVisitor {
            type Value = Scalar;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a string, number or boolean")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Scalar, E> {
                Ok(Scalar::Bool(v))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Scalar, E> {
                Ok(Scalar::Int(v))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Scalar, E> {
                i64::try_from(v)
                    .map(Scalar::Int)
                    .map_err(|_| E::custom("number too large"))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Scalar, E> {
                Ok(Scalar::Float(v))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Scalar, E> {
                Ok(Scalar::Str(v.to_string()))
            }
        }
        de.deserialize_any(ScalarVisitor)
    }
}

/// `Additional` by hand, as `Scalar`: a bool, or a mapping read as a
/// `Rule` through `MapAccessDeserializer`, so an error inside the rule
/// keeps its message and position.
impl<'de> Deserialize<'de> for Additional {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct AdditionalVisitor;
        impl<'de> Visitor<'de> for AdditionalVisitor {
            type Value = Additional;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("true, false or a rule")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Additional, E> {
                Ok(Additional::Allowed(v))
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Additional, A::Error> {
                Rule::deserialize(de::value::MapAccessDeserializer::new(map))
                    .map(|rule| Additional::Rule(Box::new(rule)))
            }
        }
        de.deserialize_any(AdditionalVisitor)
    }
}

/// A `type`, `format` or `custom_validator` name; empty means none, as an
/// empty string does in sb-go.
fn name<'de, D, T>(de: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    match Option::<String>::deserialize(de)? {
        Some(text) if !text.is_empty() => {
            let value: de::value::StringDeserializer<D::Error> = text.into_deserializer();
            T::deserialize(value).map(Some)
        }
        _ => Ok(None),
    }
}

/// A `pattern`, compiled when the schema loads, so one that doesn't
/// compile fails loading where it stands (serde's custom error, which
/// serde-saphyr places at the value).
fn pattern<'de, D: Deserializer<'de>>(de: D) -> Result<Option<regex::Regex>, D::Error> {
    Option::<String>::deserialize(de)?
        .map(|source| {
            compile(&source).map_err(|reason| {
                de::Error::custom(format!("pattern `{source}` doesn't compile: {reason}"))
            })
        })
        .transpose()
}

/// `source` as a regex, or why not, in one line: a syntax error's kind
/// from regex-syntax (the parser regex uses, with the same defaults;
/// `regex::Error` only carries its multi-line rendering), or regex's
/// size limit.
fn compile(source: &str) -> Result<regex::Regex, String> {
    if let Err(err) = regex_syntax::parse(source) {
        return Err(match err {
            regex_syntax::Error::Parse(err) => err.kind().to_string(),
            regex_syntax::Error::Translate(err) => err.kind().to_string(),
            other => other.to_string(),
        });
    }
    regex::Regex::new(source).map_err(|err| match err {
        regex::Error::CompiledTooBig(limit) => {
            format!("it compiles to more than the {limit}-byte limit")
        }
        other => other.to_string(),
    })
}

/// serde-saphyr's error in one line, without its source snippet, ending
/// with the line and column; two of its messages advise the programmer
/// (which API to call), so they are worded for the reader here.
fn message(err: &serde_saphyr::Error) -> String {
    match err.without_snippet() {
        serde_saphyr::Error::DuplicateMappingKey { key, location } => format!(
            "duplicate key{} at line {}, column {}",
            key.as_deref().map_or_default(|key| format!(" `{key}`")),
            location.line(),
            location.column()
        ),
        serde_saphyr::Error::MultipleDocuments { location, .. } => format!(
            "more than one YAML document, the second at line {}, column {}",
            location.line(),
            location.column()
        ),
        other => other.to_string(),
    }
}

/// `text`, the schema file shown as `file` (already escaped for the
/// screen), as rules.
///
/// # Errors
///
/// [`SchemaError::Invalid`] when the text isn't a valid schema.
pub fn parse(file: &str, text: &str) -> Result<Rules, SchemaError> {
    serde_saphyr::from_str(text).map_err(|err| SchemaError::Invalid {
        file: file.to_string(),
        message: message(&err),
    })
}

/// The schema file at `path`, shown as `file` (already escaped), as rules:
/// read as cli-core reads Saltbox's files (a regular file up to 1 MiB, in
/// UTF-8).
///
/// # Errors
///
/// A [`SchemaError`] when the file is missing or can't be read, or isn't a
/// valid schema.
pub fn load(path: &Path, file: &str) -> Result<Rules, SchemaError> {
    let file = file.to_string();
    match sb_cli_core::config::read_small(path) {
        Ok(Some(text)) => parse(&file, &text),
        Ok(None) => Err(SchemaError::Missing { file }),
        Err(source) => Err(SchemaError::Read { file, source }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The error loading `text` as `strict.schema.yml`, as shown.
    fn error(text: &str) -> String {
        parse("strict.schema.yml", text)
            .expect_err("loads")
            .to_string()
    }

    #[test]
    fn a_rule_with_every_key_loads() {
        let rules = parse(
            "full.schema.yml",
            "user:\n  type: object\n  required: true\n  description: the user\n  properties:\n    email:\n      type: string\n      format: email\n      not_equals: your@example.com\n      min_length: 3\n      max_length: 64\n      required_with: [name]\n      example: me@example.com\n    colour:\n      type: string\n      pattern: '^#[0-9a-f]{3}$'\n    flag:\n      type: ansible_bool\n      required_when_true: [token]\n      validate_when_true: [token]\n    token:\n      type: string\n      custom_validator: validate_cloudflare_config\n    names:\n      type: object\n      additionalProperties:\n        type: string\n    list:\n      type: array\n      items:\n        type: number\n",
        )
        .expect("loads");
        let user = &rules["user"];
        assert_eq!(user.kind, Some(Kind::Object));
        assert!(user.required);
        let props = user.properties.as_ref().expect("properties");
        let email = &props["email"];
        assert_eq!(email.format, Some(Format::Email));
        assert_eq!(
            email.not_equals,
            Some(Scalar::Str("your@example.com".into()))
        );
        assert_eq!((email.min_length, email.max_length), (3, 64));
        assert_eq!(email.required_with, ["name"]);
        let pattern = props["colour"].pattern.as_ref().expect("pattern");
        assert!(pattern.is_match("#a1f") && !pattern.is_match("a1f"));
        assert_eq!(props["flag"].kind, Some(Kind::AnsibleBool));
        assert_eq!(props["flag"].required_when_true, ["token"]);
        assert_eq!(
            props["token"].custom_validator,
            Some(Validator::ValidateCloudflareConfig)
        );
        assert!(matches!(
            &props["names"].additional_properties,
            Some(Additional::Rule(rule)) if rule.kind == Some(Kind::String)
        ));
        let items = props["list"].items.as_ref().expect("items");
        assert_eq!(items.kind, Some(Kind::Number));
        assert_eq!(user.description.as_deref(), Some("the user"));
        assert_eq!(email.example, Some("me@example.com".into()));
    }

    /// motd's schema writes its examples as booleans, numbers, strings
    /// and empty collections; each is kept as written, and `null` is none.
    #[test]
    fn examples_keep_their_values() {
        let rules = parse(
            "examples.schema.yml",
            "a:
  example: false
b:
  example: 0
c:
  example: 1.5
d:
  example: \"x\"
\
             e:
  example: []
f:
  example: {}
g:
  example: null
h:
  example: [1, {k: v}]
",
        )
        .expect("loads");
        let example = |key: &str| rules[key].example.clone();
        assert_eq!(example("a"), Some(false.into()));
        assert_eq!(example("b"), Some(0.into()));
        assert_eq!(example("c"), Some(1.5.into()));
        assert_eq!(example("d"), Some("x".into()));
        assert_eq!(example("e"), Some(serde_json::json!([])));
        assert_eq!(example("f"), Some(serde_json::json!({})));
        assert_eq!(example("g"), None);
        assert_eq!(example("h"), Some(serde_json::json!([1, {"k": "v"}])));
    }

    #[test]
    fn additional_properties_can_be_a_bool() {
        let rules = parse(
            "bool.schema.yml",
            "a:\n  type: object\n  additionalProperties: false\nb:\n  type: object\n  additionalProperties: true\n",
        )
        .expect("loads");
        assert!(matches!(
            rules["a"].additional_properties,
            Some(Additional::Allowed(false))
        ));
        assert!(matches!(
            rules["b"].additional_properties,
            Some(Additional::Allowed(true))
        ));
    }

    #[test]
    fn an_unknown_key_names_the_file_and_key() {
        let message = error("user:\n  type: string\n  requird: true\n");
        assert!(message.contains("strict.schema.yml"), "{message}");
        assert!(message.contains("requird"), "{message}");
        assert!(message.contains("line 3"), "{message}");
    }

    #[test]
    fn an_unknown_type_is_named() {
        let message = error("user:\n  type: strng\n");
        assert!(message.contains("strict.schema.yml"), "{message}");
        assert!(message.contains("strng"), "{message}");
        assert!(message.contains("line 2"), "{message}");
    }

    #[test]
    fn an_unknown_format_is_named() {
        let message = error("user:\n  type: string\n  format: ipv4\n");
        assert!(
            message.contains("ipv4") && message.contains("line 3"),
            "{message}"
        );
    }

    /// Decision 6: no Saltbox schema uses it, so sb doesn't carry it.
    #[test]
    fn validate_rclone_remote_is_refused() {
        let message =
            error("remote:\n  type: string\n  custom_validator: validate_rclone_remote\n");
        assert!(message.contains("validate_rclone_remote"), "{message}");
    }

    #[test]
    fn a_pattern_that_does_not_compile_is_named() {
        let message = error("colour:\n  type: string\n  pattern: '^#(['\n");
        assert_eq!(
            message,
            "schema strict.schema.yml: pattern `^#([` doesn't compile: \
             unclosed character class at line 3, column 12"
        );
    }

    /// Lookaround isn't Rust regex syntax: refused, not misread.
    #[test]
    fn a_lookaround_pattern_is_refused() {
        let message = error("colour:\n  type: string\n  pattern: '^(?=a)'\n");
        assert!(message.contains("^(?=a)"), "{message}");
    }

    #[test]
    fn an_unknown_validator_is_named() {
        let message = error("user:\n  type: string\n  custom_validator: validate_nothing\n");
        assert!(message.contains("strict.schema.yml"), "{message}");
        assert!(
            message.contains("validate_nothing") && message.contains("line 3"),
            "{message}"
        );
    }

    #[test]
    fn a_malformed_value_is_named() {
        let message = error("user:\n  type: string\n  required: maybe\n");
        assert_eq!(
            message,
            "schema strict.schema.yml: invalid boolean at line 3, column 13"
        );
        let message = error("user:\n  type: string\n  min_length: -1\n");
        assert!(
            message.contains("strict.schema.yml") && message.contains("line 3"),
            "{message}"
        );
    }

    /// An empty name means none, as in sb-go; a missing one too.
    #[test]
    fn empty_names_mean_none() {
        let rules = parse(
            "empty.schema.yml",
            "a:\n  type: ''\n  format: ''\n  custom_validator: ''\nb:\n  type:\n",
        )
        .expect("loads");
        assert_eq!(rules["a"].kind, None);
        assert_eq!(rules["a"].format, None);
        assert_eq!(rules["a"].custom_validator, None);
        assert_eq!(rules["b"].kind, None);
    }

    /// An error inside an `additionalProperties` rule keeps its message and
    /// position (not "did not match any variant").
    #[test]
    fn an_error_inside_additional_properties_is_named() {
        let message = error("names:\n  type: object\n  additionalProperties:\n    type: strng\n");
        assert!(
            message.contains("strng") && message.contains("line 4"),
            "{message}"
        );
        let message = error("names:\n  type: object\n  additionalProperties:\n    typ: string\n");
        assert!(
            message.contains("`typ`") && message.contains("line 4"),
            "{message}"
        );
        let message = error("names:\n  type: object\n  additionalProperties: maybe\n");
        assert!(message.contains("true, false or a rule"), "{message}");
    }

    #[test]
    fn not_equals_takes_any_scalar() {
        let rules = parse(
            "scalars.schema.yml",
            "a:\n  not_equals: true\nb:\n  not_equals: -3\nc:\n  not_equals: 1.5\nd:\n  not_equals: x\ne:\n  not_equals: 7\n",
        )
        .expect("loads");
        assert_eq!(rules["a"].not_equals, Some(Scalar::Bool(true)));
        assert_eq!(rules["b"].not_equals, Some(Scalar::Int(-3)));
        assert_eq!(rules["c"].not_equals, Some(Scalar::Float(1.5)));
        assert_eq!(rules["d"].not_equals, Some(Scalar::Str("x".into())));
        assert_eq!(rules["e"].not_equals, Some(Scalar::Int(7)));
        let message = error("a:\n  not_equals: [1]\n");
        assert!(
            message.contains("a string, number or boolean") && message.contains("line 2"),
            "{message}"
        );
        let message = error("a:\n  not_equals: 18446744073709551615\n");
        assert!(message.contains("number too large"), "{message}");
    }

    #[test]
    fn a_duplicate_key_is_named_for_the_reader() {
        let message = error("user:\n  type: string\n  type: number\n");
        assert!(
            message.contains("duplicate key `type`") && message.contains("line 3"),
            "{message}"
        );
        assert!(!message.contains("DuplicateKeyPolicy"), "{message}");
    }

    #[test]
    fn a_second_document_is_named_for_the_reader() {
        let message = error("a:\n  type: string\n---\nb:\n  type: string\n");
        assert!(message.contains("more than one YAML document"), "{message}");
        assert!(!message.contains("from_str_multiple"), "{message}");
    }

    /// A pattern too big for regex's size limit says so, on one line.
    #[test]
    fn a_pattern_too_big_says_so() {
        let message = error("colour:\n  type: string\n  pattern: 'a{1000}{1000}{1000}'\n");
        assert!(
            message.contains("byte limit") && message.contains("line 3"),
            "{message}"
        );
        assert!(!message.contains('\n'), "{message}");
    }

    /// A pattern regex-syntax refuses after parsing (an unknown Unicode
    /// property) gives its reason too.
    #[test]
    fn a_pattern_refused_in_translation_says_why() {
        let message = error("colour:\n  type: string\n  pattern: '\\p{Nope}'\n");
        assert!(message.contains("Unicode property not found"), "{message}");
    }

    #[test]
    fn a_missing_file_says_so() {
        let dir = tempfile::tempdir().expect("temp dir");
        let message = load(&dir.path().join("gone.schema.yml"), "gone.schema.yml")
            .expect_err("missing")
            .to_string();
        assert_eq!(message, "schema gone.schema.yml doesn't exist");
    }

    /// A valid schema padded past 1 MiB: only the size can refuse it.
    #[test]
    fn a_file_over_a_mebibyte_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("huge.schema.yml");
        let schema = format!("user:\n  type: string\n# {}\n", "x".repeat(1 << 20));
        assert!(parse("huge.schema.yml", &schema).is_ok());
        std::fs::write(&path, schema).expect("write");
        let message = load(&path, "huge.schema.yml")
            .expect_err("too big")
            .to_string();
        assert!(message.starts_with("schema huge.schema.yml: "), "{message}");
        assert!(message.contains("bigger than"), "{message}");
    }

    #[test]
    fn load_reads_and_parses_the_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("one.schema.yml");
        std::fs::write(&path, "user:\n  type: string\n").expect("write");
        let rules = load(&path, "one.schema.yml").expect("loads");
        assert_eq!(rules["user"].kind, Some(Kind::String));
        std::fs::write(&path, "user:\n  typ: string\n").expect("write");
        let message = load(&path, "one.schema.yml")
            .expect_err("strict")
            .to_string();
        assert!(
            message.contains("one.schema.yml") && message.contains("`typ`"),
            "{message}"
        );
    }
}
