//! sb-go's schema rules over a config file (config spec, "Schema rules";
//! sb-go's `saltbox/yaml_schema.go`), collecting every issue rather than
//! stopping at the first (decision 2): within one field the first failed
//! check is its issue, as in sb-go; every field and nested object is
//! checked. Messages keep sb-go's wording without its `field '<path>'`
//! prefix, which the issue's path replaces. "is required" points at the
//! parent key (1:1 at the top), "unknown field" at the key, everything
//! else at the value.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use crate::issue::Issue;
use crate::scalar::Value;
use crate::schema::{Additional, Format, Kind, Rule, Rules, Validator};
use crate::secret;
use crate::tree::{Data, Node};

/// The validators the rules call (Task 7's): type validators through the
/// validator they stand for, sync custom validators, the password
/// warning, and the credential sections' structure.
pub trait Validators {
    /// `validator` on `value`, whose parent mapping is `parent`: an error
    /// message, or a warning that doesn't fail the file. A `secret` value
    /// is never echoed in either (`secret::shown`).
    fn validate(&self, validator: Validator, value: &Node, parent: &Node, secret: bool) -> Verdict;

    /// The short-password warning for a `type: password` value, if any.
    fn password_warning(&self, value: &Node) -> Option<String>;

    /// The structure of a credential section (`validate_cloudflare_config`
    /// or `validate_dockerhub_config` on `value`, in `parent`): sb-go's
    /// `…Sync` half, before any network check.
    fn credentials(&self, validator: Validator, value: &Node, parent: &Node) -> Credentials;
}

/// A credential section's structure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Credentials {
    /// None set: the check is skipped.
    Missing,
    /// Set wrongly (a scoped token beside a global key, say): the issue.
    Invalid(String),
    /// Set: the check runs with this login, if the section has no issues
    /// of its own. The login is what the structure check accepted, so the
    /// run checks exactly that and can't find a ready section without one.
    Ready(Login),
}

/// The login a ready credential section holds. Its debug output shows no
/// secret (both credential types redact theirs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Login {
    /// `validate_cloudflare_config`: the credentials and accounts.yml's
    /// `user.domain`, the zone asked about.
    Cloudflare {
        credentials: crate::cloudflare::Credentials,
        domain: String,
    },
    /// `validate_dockerhub_config`.
    Dockerhub(crate::dockerhub::DockerhubLogin),
}

impl Login {
    /// The validator whose check this is.
    #[must_use]
    pub fn validator(&self) -> Validator {
        match self {
            Login::Cloudflare { .. } => Validator::ValidateCloudflareConfig,
            Login::Dockerhub(_) => Validator::ValidateDockerhubConfig,
        }
    }

    /// The key path of a value the login takes from outside its own
    /// section: Cloudflare's zone, accounts.yml's `user.domain`.
    #[must_use]
    pub fn reads(&self) -> Option<&'static str> {
        match self {
            Login::Cloudflare { .. } => Some("user.domain"),
            Login::Dockerhub(_) => None,
        }
    }
}

/// What a validator found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Valid,
    Warning(String),
    Invalid(String),
}

/// A credential check the rules asked for, run after validation (Tasks
/// 8-10). Its debug output names the check and the key, never the values
/// it carries (term-ui decision 8).
#[derive(Clone, PartialEq)]
pub struct Request {
    /// The section's key path (`cloudflare`, `dockerhub`).
    pub path: String,
    pub login: Login,
}

impl Request {
    /// The validator whose check this is.
    #[must_use]
    pub fn validator(&self) -> Validator {
        self.login.validator()
    }
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("validator", &self.validator())
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// Everything the rules found in one file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    /// Sorted by position.
    pub issues: Vec<Issue>,
    pub warnings: Vec<String>,
    pub requests: Vec<Request>,
    /// Credential checks with no credentials set.
    pub skipped: Vec<Validator>,
    /// Credential checks left out because a value they read from another
    /// section has an issue: the check and that value's path.
    pub blocked: Vec<(Validator, &'static str)>,
}

/// `root`, the file shown as `file`, checked against `rules`.
pub fn validate(file: &str, root: &Node, rules: &Rules, validators: &dyn Validators) -> Report {
    let mut walk = Walk {
        file,
        validators,
        report: Report::default(),
    };
    walk.object(root, rules, None, "", (1, 1));
    let report = &mut walk.report;
    report.issues.sort();
    // A check whose login reads another section's value waits for that
    // value's issue to be fixed (config spec, "When the checks run"), as
    // one waits for its own section's.
    let issues = &report.issues;
    let (blocked, requests) = std::mem::take(&mut report.requests)
        .into_iter()
        .partition::<Vec<_>, _>(|request| {
            request
                .login
                .reads()
                .is_some_and(|path| issues.iter().any(|issue| issue.path == path))
        });
    report.requests = requests;
    report.blocked = blocked
        .iter()
        .filter_map(|request| Some((request.validator(), request.login.reads()?)))
        .collect();
    walk.report
}

/// The secret paths of files checked against `rules` (`secret::is_secret`
/// with each path's type), for `tree::build`'s messages before the rules
/// run: through properties, `[n]` items and `additionalProperties`. A path
/// the schema doesn't reach (a key in a structure it doesn't expect, such
/// as a list where a mapping belongs) is secret by its key's name: the
/// name of a credential key or of any `type: password` field (config-L2),
/// or a name the shared key-name rule calls secret, as on the schema.
pub fn secret_paths(rules: &Rules) -> impl Fn(&str) -> bool + '_ {
    let mut names: BTreeSet<&str> = secret::credential_names().collect();
    password_names(rules, &mut names);
    move |path| {
        if let Some(rule) = rule_at(rules, path) {
            secret::is_secret(path, rule.kind)
        } else {
            let key = secret::key_name(path);
            names.contains(key) || crate::facts::secret::is_secret_key(key)
        }
    }
}

/// The names of every `type: password` property in `rules`, at any depth
/// (an array's items or an `additionalProperties` rule has no name).
fn password_names<'a>(rules: &'a Rules, names: &mut BTreeSet<&'a str>) {
    fn walk<'a>(rule: &'a Rule, names: &mut BTreeSet<&'a str>) {
        if let Some(properties) = &rule.properties {
            password_names(properties, names);
        }
        if let Some(items) = &rule.items {
            walk(items, names);
        }
        if let Some(Additional::Rule(other)) = &rule.additional_properties {
            walk(other, names);
        }
    }
    for (name, rule) in rules {
        if rule.kind == Some(Kind::Password) {
            names.insert(name);
        }
        walk(rule, names);
    }
}

/// The rule a dotted path (with `[n]` items) reaches, if the schema
/// reaches it.
fn rule_at<'a>(rules: &'a Rules, path: &str) -> Option<&'a Rule> {
    let mut properties = Some(rules);
    let mut additional: Option<&Additional> = None;
    let mut rule: Option<&Rule> = None;
    for segment in path.split('.') {
        let name = segment.split('[').next().unwrap_or(segment);
        rule = properties.and_then(|p| p.get(name)).or(match additional {
            Some(Additional::Rule(r)) => Some(r),
            _ => None,
        });
        for _ in segment.matches('[') {
            rule = rule.and_then(|r| r.items.as_deref());
            if rule.is_none() {
                break;
            }
        }
        // Past the rules no deeper segment has one: a deep file's paths
        // are read only as far as its schema goes.
        rule?;
        properties = rule.and_then(|r| r.properties.as_ref());
        additional = rule.and_then(|r| r.additional_properties.as_ref());
    }
    rule
}

/// The type validators and the validator each stands for (sb-go's
/// `builtInValidators`).
fn type_validator(kind: Kind) -> Option<Validator> {
    Some(match kind {
        Kind::AnsibleBool => Validator::ValidateAnsibleBool,
        Kind::Subdomain => Validator::ValidateSubdomain,
        Kind::Hostname => Validator::ValidateHostname,
        Kind::DirectoryPath => Validator::ValidateDirectoryPath,
        Kind::Url => Validator::ValidateUrl,
        Kind::Timezone => Validator::ValidateTimezone,
        Kind::CronTime => Validator::ValidateCronTime,
        Kind::RcloneTemplate => Validator::ValidateRcloneTemplate,
        Kind::SshKeyOrUrl => Validator::ValidateSshKeyOrUrl,
        Kind::Password => Validator::ValidatePasswordStrength,
        Kind::Object | Kind::Array | Kind::String | Kind::Boolean | Kind::Number | Kind::Float => {
            return None;
        }
    })
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

struct Walk<'a> {
    file: &'a str,
    validators: &'a dyn Validators,
    report: Report,
}

impl Walk<'_> {
    fn issue(&mut self, (line, column): (usize, usize), path: &str, message: String) {
        self.report.issues.push(Issue {
            file: self.file.to_string(),
            line,
            column,
            path: path.to_string(),
            message,
        });
    }

    /// The mapping `obj` at `path` (its key at `key_at`) against
    /// `properties`, its other keys against `additional` (unknown fields
    /// when there is none or it is `false`). sb-go's
    /// `validateObjectWithTypeFlexibility`.
    fn object(
        &mut self,
        obj: &Node,
        properties: &Rules,
        additional: Option<&Additional>,
        path: &str,
        key_at: (usize, usize),
    ) {
        for (name, rule) in properties {
            let field = join(path, name);
            let Some((key, value)) = obj.entry(name) else {
                if required(rule, obj) {
                    self.issue(key_at, &field, "is required".to_string());
                } else if let Some(validator) = credential(rule) {
                    self.report.skipped.push(validator);
                }
                continue;
            };
            if validated(rule, obj) {
                self.field(value, rule, &field, obj, (key.line, key.column));
            }
        }
        let Data::Mapping(entries) = &obj.data else {
            return;
        };
        for (key, value) in entries
            .iter()
            .filter(|(k, _)| !properties.contains_key(&k.text))
        {
            let field = join(path, &key.text);
            match additional {
                Some(Additional::Allowed(true)) => {}
                Some(Additional::Rule(rule)) => {
                    if validated(rule, obj) {
                        self.field(value, rule, &field, obj, (key.line, key.column));
                    }
                }
                Some(Additional::Allowed(false)) | None => {
                    self.issue((key.line, key.column), &field, "unknown field".to_string());
                }
            }
        }
    }

    /// `value` at `path` (its key at `key_at`) against `rule`, `parent`
    /// the mapping it sits in (an array's for its items, as sb-go): the
    /// first failed check is its issue; otherwise its object or items are
    /// walked. sb-go's `validateFieldWithTypeFlexibility`.
    fn field(
        &mut self,
        value: &Node,
        rule: &Rule,
        path: &str,
        parent: &Node,
        key_at: (usize, usize),
    ) {
        if let Some(message) = self.checks(value, rule, parent, secret::is_secret(path, rule.kind))
        {
            self.issue((value.line, value.column), path, message);
            return;
        }
        // A credential section: its structure first (an issue at its key
        // when wrong, the section still walked); a check only when set and
        // the section has no issues of its own; skipped when not set.
        let mut request = None;
        if let Some(validator) = credential(rule) {
            if value.is_empty() {
                self.report.skipped.push(validator);
            } else {
                match self.validators.credentials(validator, value, parent) {
                    Credentials::Missing => self.report.skipped.push(validator),
                    Credentials::Invalid(message) => self.issue(key_at, path, message),
                    Credentials::Ready(login) => {
                        request = Some(Request {
                            path: path.to_string(),
                            login,
                        });
                    }
                }
            }
        }
        let before = self.report.issues.len();
        match (rule.kind, &value.data) {
            (Some(Kind::Object), Data::Mapping(_))
                if rule.properties.is_some() || rule.additional_properties.is_some() =>
            {
                let none = Rules::new();
                let properties = rule.properties.as_ref().unwrap_or(&none);
                self.object(
                    value,
                    properties,
                    rule.additional_properties.as_ref(),
                    path,
                    key_at,
                );
            }
            // A list where the schema describes an object's keys (Task 60;
            // sb-go skips it, a listed difference): its keys would go
            // unchecked, and Ansible can't read them as the object's. An
            // empty list stays as sb-go has it, as an empty value; a
            // credential section has its own structure issue.
            (Some(Kind::Object), Data::Sequence(items))
                if !items.is_empty()
                    && (rule.properties.is_some() || rule.additional_properties.is_some())
                    && credential(rule).is_none() =>
            {
                // motd.yml's older bare lists are migrated before the
                // check (`migrate`, motd decision 4); one left is this
                // issue too.
                let message = "must be an object, got a list".to_string();
                self.issue((value.line, value.column), path, message);
            }
            (Some(Kind::Array), Data::Sequence(items)) => {
                if let Some(item_rule) = &rule.items {
                    for (index, item) in items.iter().enumerate() {
                        let at = (item.line, item.column);
                        self.field(item, item_rule, &format!("{path}[{index}]"), parent, at);
                    }
                }
            }
            _ => {}
        }
        if let Some(request) = request
            && self.report.issues.len() == before
        {
            self.report.requests.push(request);
        }
    }

    /// The checks on one value in sb-go's order; the first that fails.
    fn checks(&mut self, value: &Node, rule: &Rule, parent: &Node, secret: bool) -> Option<String> {
        if rule
            .not_equals
            .as_ref()
            .is_some_and(|forbidden| equals(value, forbidden))
        {
            return Some("must not equal its configured default value".to_string());
        }
        if !rule.required_with.is_empty()
            && rule
                .required_with
                .iter()
                .any(|other| parent.get(other).is_some_and(|v| !v.is_empty()))
            && value.is_empty()
        {
            return Some(format!(
                "is required when any of {} are present",
                rule.required_with.join(", ")
            ));
        }
        if !rule.required && value.is_empty() {
            return None;
        }
        if let Some(format) = rule.format {
            let Some(text) = string(value) else {
                return Some("must be a string".to_string());
            };
            let (valid, name) = match format {
                Format::Email => (EMAIL.is_match(text), "a valid email address"),
                Format::Hostname => (valid_hostname(text), "a valid hostname"),
                Format::Url => (valid_url(text), "a valid URL"),
            };
            if !valid {
                return Some(format!("must be {name}"));
            }
        }
        if rule.min_length > 0 || rule.max_length > 0 {
            let Some(text) = string(value) else {
                return Some("must be a string".to_string());
            };
            let length = text.len();
            if length < rule.min_length {
                return Some(format!(
                    "must be at least {} characters long, got {length}",
                    rule.min_length
                ));
            }
            if rule.max_length > 0 && length > rule.max_length {
                return Some(format!(
                    "must be at most {} characters long, got {length}",
                    rule.max_length
                ));
            }
        }
        if let Some(pattern) = &rule.pattern {
            let Some(text) = string(value) else {
                return Some("must be a string".to_string());
            };
            if !pattern.is_match(text) {
                return Some(format!("must match the pattern `{}`", pattern.as_str()));
            }
        }
        match rule.kind {
            Some(Kind::Number) => {
                if let Some(message) = whole_number(value, secret) {
                    return Some(message);
                }
            }
            Some(Kind::Float) => {
                if let Some(message) = float(value, secret) {
                    return Some(message);
                }
            }
            _ => {}
        }
        if let Some(validator) = rule.kind.and_then(type_validator) {
            if let Some(message) = self.run(validator, value, parent, secret) {
                return Some(message);
            }
            if rule.kind == Some(Kind::Password)
                && let Some(warning) = self.validators.password_warning(value)
            {
                self.report.warnings.push(warning);
            }
        }
        match rule.custom_validator {
            // The credential checks are `field`'s.
            Some(Validator::ValidateCloudflareConfig | Validator::ValidateDockerhubConfig)
            | None => None,
            Some(validator) => self.run(validator, value, parent, secret),
        }
    }

    /// A sync validator's error; its warning is kept.
    fn run(
        &mut self,
        validator: Validator,
        value: &Node,
        parent: &Node,
        secret: bool,
    ) -> Option<String> {
        match self.validators.validate(validator, value, parent, secret) {
            Verdict::Valid => None,
            Verdict::Warning(warning) => {
                self.report.warnings.push(warning);
                None
            }
            Verdict::Invalid(message) => Some(message),
        }
    }
}

/// The credential check a rule names, if any (`field` handles these).
fn credential(rule: &Rule) -> Option<Validator> {
    rule.custom_validator.filter(|validator| {
        matches!(
            validator,
            Validator::ValidateCloudflareConfig | Validator::ValidateDockerhubConfig
        )
    })
}

/// sb-go's `isFieldRequired`: `required`, or an Ansible true in one of
/// `required_when_true`'s keys.
fn required(rule: &Rule, parent: &Node) -> bool {
    rule.required || any_ansible_true(&rule.required_when_true, parent)
}

/// sb-go's `shouldValidateField`: no `validate_when_true`, or an Ansible
/// true in one of its keys.
fn validated(rule: &Rule, parent: &Node) -> bool {
    rule.validate_when_true.is_empty() || any_ansible_true(&rule.validate_when_true, parent)
}

fn any_ansible_true(keys: &[String], parent: &Node) -> bool {
    keys.iter()
        .any(|key| parent.get(key).is_some_and(ansible_true))
}

/// sb-go's `parseAnsibleBool` giving true: a YAML `true`, or yes, true,
/// on or 1 as a string, trimmed, any case (an integer 1 isn't one).
fn ansible_true(value: &Node) -> bool {
    match &value.data {
        Data::Scalar(Value::Bool(b)) => *b,
        Data::Scalar(Value::Str(text)) => {
            matches!(
                text.trim().to_lowercase().as_str(),
                "yes" | "true" | "on" | "1"
            )
        }
        _ => false,
    }
}

/// Go's `reflect.DeepEqual` of a decoded value and a `not_equals`: the
/// same kind and value.
fn equals(value: &Node, forbidden: &crate::schema::Scalar) -> bool {
    use crate::schema::Scalar;
    match (&value.data, forbidden) {
        (Data::Scalar(Value::Bool(a)), Scalar::Bool(b)) => a == b,
        (Data::Scalar(Value::Int(a)), Scalar::Int(b)) => *a == i128::from(*b),
        // Exactly `==` (Go's), NaN unequal and -0 equal to 0; clippy's
        // `float_cmp` wants the comparison spelled out.
        (Data::Scalar(Value::Float(a)), Scalar::Float(b)) => {
            a.partial_cmp(b) == Some(std::cmp::Ordering::Equal)
        }
        (Data::Scalar(Value::Str(a)), Scalar::Str(b)) => a == b,
        _ => false,
    }
}

fn string(value: &Node) -> Option<&str> {
    match &value.data {
        Data::Scalar(Value::Str(text)) => Some(text),
        _ => None,
    }
}

/// sb-go's email pattern.
static EMAIL: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$").expect("a valid pattern")
});

/// sb-go's hostname pattern.
static HOSTNAME: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"^[a-zA-Z0-9]([a-zA-Z0-9\-]{0,61}[a-zA-Z0-9])?(\.[a-zA-Z0-9]([a-zA-Z0-9\-]{0,61}[a-zA-Z0-9])?)*$",
    )
    .expect("a valid pattern")
});

/// sb-go's URL pattern; Go's `\s` is ASCII whitespace only (Rust's is
/// Unicode), so it is spelled out.
static URL: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^https?://[^\t\n\f\r ]+$").expect("a valid pattern"));

/// sb-go's `isValidURL`.
pub(crate) fn valid_url(text: &str) -> bool {
    URL.is_match(text)
}

/// sb-go's `isValidHostname`: 1 to 253 bytes, then the pattern.
pub(crate) fn valid_hostname(text: &str) -> bool {
    (1..=253).contains(&text.len()) && HOSTNAME.is_match(text)
}

/// sb-go's `validateNumberValue`: an integer, or a string Go's `Atoi`
/// reads (as `i64`'s parse: a sign, digits, no underscores; Go's `int`
/// is 64 bits where sb runs). A secret isn't echoed.
fn whole_number(value: &Node, secret: bool) -> Option<String> {
    let got = match &value.data {
        Data::Scalar(Value::Str(text)) if text.parse::<i64>().is_ok() => return None,
        Data::Scalar(Value::Str(text)) => secret::shown(text, secret),
        Data::Scalar(Value::Int(_)) => return None,
        Data::Scalar(Value::Float(f)) => secret::shown(&go_float(*f), secret),
        _ => go_type(value).to_string(),
    };
    Some(format!("must be a whole number (integer), got: {got}"))
}

/// sb-go's `validateFloatValue`: a float, or a string Go's `ParseFloat`
/// reads. A secret isn't echoed.
fn float(value: &Node, secret: bool) -> Option<String> {
    let got = match &value.data {
        Data::Scalar(Value::Str(text)) if go_parses_float(text) => return None,
        Data::Scalar(Value::Str(text)) => secret::shown(text, secret),
        Data::Scalar(Value::Float(_)) => return None,
        Data::Scalar(Value::Int(i)) => secret::shown(&i.to_string(), secret),
        _ => go_type(value).to_string(),
    };
    Some(format!("must be a float, got: {got}"))
}

/// Whether Go's `ParseFloat` reads `text`: decimal (underscores only
/// between digits, Go's `underscoreOK`), or `inf`, `infinity` and `nan`
/// in any case, a sign only on the infinities; within f64's range. Rust's
/// parse reads the same decimal and named forms but no underscores, takes
/// a signed nan, and turns an overflow into infinity, so those are
/// handled here. Go's hex floats aren't read (a listed difference).
pub(crate) fn go_parses_float(text: &str) -> bool {
    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    match unsigned.to_ascii_lowercase().as_str() {
        "nan" => return unsigned.len() == text.len(),
        "inf" | "infinity" => return true,
        _ => {}
    }
    let b = text.as_bytes();
    let digit_at = |at: Option<usize>| at.and_then(|at| b.get(at)).is_some_and(u8::is_ascii_digit);
    let between_digits = b
        .iter()
        .enumerate()
        .all(|(at, &c)| c != b'_' || (digit_at(at.checked_sub(1)) && digit_at(Some(at + 1))));
    between_digits
        && text
            .replace('_', "")
            .parse::<f64>()
            .is_ok_and(f64::is_finite)
}

/// Go's `%T` for a decoded value, as sb-go's messages print it.
pub(crate) fn go_type(value: &Node) -> &'static str {
    match &value.data {
        Data::Mapping(_) => "map[string]interface {}",
        Data::Sequence(_) => "[]interface {}",
        Data::Scalar(Value::Null) => "<nil>",
        Data::Scalar(Value::Bool(_)) => "bool",
        // yaml.v3 decodes an integer past i64 as a `uint64`.
        Data::Scalar(Value::Int(i)) if i64::try_from(*i).is_err() => "uint64",
        Data::Scalar(Value::Int(_)) => "int",
        Data::Scalar(Value::Float(_)) => "float64",
        Data::Scalar(Value::Str(_)) => "string",
        Data::Scalar(Value::Timestamp(_)) => "time.Time",
    }
}

/// Go's `%v` for a float64: `%g` with the shortest digits, in exponent
/// form when the exponent is below -4 or at least 6 (Go's
/// `internal/strconv/ftoa.go`, `fmtEFG`: `eprec = 6` when shortest), with
/// a sign and at least two exponent digits; `NaN`, `+Inf`, `-Inf`. No
/// crate reproduces Go's `%g` (ryu has its own cutoffs); Rust's `{:e}`
/// gives the same shortest digits. Also motd's `--generate-config`'s, for
/// a schema example.
#[must_use]
pub fn go_float(f: f64) -> String {
    if f.is_nan() {
        return "NaN".to_string();
    }
    if f.is_infinite() {
        return if f.is_sign_positive() { "+Inf" } else { "-Inf" }.to_string();
    }
    let shortest = format!("{f:e}");
    let (mantissa, exponent) = shortest.split_once('e').unwrap_or((&shortest, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    if (-4..6).contains(&exponent) {
        format!("{f}")
    } else {
        // A sign and at least two digits: `e+06`, `e-05`.
        format!("{mantissa}e{exponent:+03}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dockerhub::DockerhubLogin;
    use crate::schema;
    use std::cell::RefCell;

    /// Validators for tests: a value `bad` is invalid, `warn` warns, and a
    /// password `short` warns; every call is recorded.
    #[derive(Default)]
    struct Fake {
        calls: RefCell<Vec<(Validator, String)>>,
    }

    impl Validators for Fake {
        fn validate(&self, validator: Validator, value: &Node, parent: &Node, _: bool) -> Verdict {
            let parent_keys = match &parent.data {
                crate::tree::Data::Mapping(entries) => entries
                    .iter()
                    .map(|(k, _)| k.text.clone())
                    .collect::<Vec<_>>()
                    .join(","),
                _ => String::new(),
            };
            self.calls.borrow_mut().push((validator, parent_keys));
            match &value.data {
                crate::tree::Data::Scalar(crate::scalar::Value::Str(s)) if s == "bad" => {
                    Verdict::Invalid(format!("{validator:?} refused it"))
                }
                crate::tree::Data::Scalar(crate::scalar::Value::Str(s)) if s == "warn" => {
                    Verdict::Warning("a warning".into())
                }
                _ => Verdict::Valid,
            }
        }

        fn password_warning(&self, value: &Node) -> Option<String> {
            matches!(&value.data, crate::tree::Data::Scalar(crate::scalar::Value::Str(s)) if s == "short")
                .then(|| "password is short".to_string())
        }

        /// Missing when the section is empty, invalid with a `bad` key,
        /// ready otherwise, with a login made of the section's own `user`
        /// or `api` text and its line, so a test can tell which section a
        /// request carries.
        fn credentials(&self, validator: Validator, value: &Node, _: &Node) -> Credentials {
            self.calls
                .borrow_mut()
                .push((validator, "credentials".into()));
            match &value.data {
                crate::tree::Data::Mapping(entries) if entries.is_empty() => Credentials::Missing,
                _ if value.get("bad").is_some() => {
                    Credentials::Invalid("structure is wrong".into())
                }
                _ if validator == Validator::ValidateDockerhubConfig => {
                    Credentials::Ready(Login::Dockerhub(DockerhubLogin {
                        user: value.get("user").and_then(string).unwrap_or("").into(),
                        token: format!("line {}", value.line),
                    }))
                }
                _ => Credentials::Ready(Login::Cloudflare {
                    credentials: crate::cloudflare::Credentials::Token(
                        value.get("api").and_then(string).unwrap_or("").into(),
                    ),
                    domain: format!("line {}", value.line),
                }),
            }
        }
    }

    /// The report for `config` under `schema_text`, with `fake`.
    fn run_with(schema_text: &str, config: &str, fake: &Fake) -> Report {
        run_in("c.yml", schema_text, config, fake)
    }

    /// The report for `config`, named `file`, under `schema_text`.
    fn run_in(file: &str, schema_text: &str, config: &str, fake: &Fake) -> Report {
        let rules = schema::parse("s.schema.yml", schema_text).expect("schema");
        let root = crate::parse::parse(file, config).expect("parses");
        let tree = crate::tree::build(file, &root, &|_| false).expect("builds");
        validate(file, &tree, &rules, fake)
    }

    /// The issues for `config` under `schema_text`, as shown.
    fn issues(schema_text: &str, config: &str) -> Vec<String> {
        run_with(schema_text, config, &Fake::default())
            .issues
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    fn clean(schema_text: &str, config: &str) {
        assert_eq!(
            issues(schema_text, config),
            Vec::<String>::new(),
            "{config}"
        );
    }

    #[test]
    fn a_missing_required_key_points_at_its_parent() {
        let schema = "user:\n  type: object\n  required: true\n  properties:\n    name: {type: string, required: true}\n";
        assert_eq!(
            issues(schema, "other: 1\n"),
            [
                "c.yml:1:1: other: unknown field",
                "c.yml:1:1: user: is required"
            ]
        );
        assert_eq!(
            issues(schema, "\n\nuser:\n  name: x\n  x: 1\n"),
            ["c.yml:5:3: user.x: unknown field"]
        );
        assert_eq!(
            issues(schema, "\n\nuser:\n  x: 1\n"),
            [
                "c.yml:3:1: user.name: is required",
                "c.yml:4:3: user.x: unknown field"
            ]
        );
    }

    /// sb-go: `required: true` with a null value passes.
    #[test]
    fn a_required_null_passes() {
        clean("a: {type: string, required: true}\n", "a:\n");
    }

    #[test]
    fn an_unknown_key_points_at_the_key() {
        assert_eq!(
            issues("a: {type: string}\n", "a: x\nzz: 1\n"),
            ["c.yml:2:1: zz: unknown field"]
        );
        let schema = "o:\n  type: object\n  properties:\n    a: {type: string}\n";
        assert_eq!(
            issues(schema, "o:\n  a: x\n  b: y\n"),
            ["c.yml:3:3: o.b: unknown field"]
        );
    }

    /// sb-go doesn't type-check string, boolean, array or object values.
    #[test]
    fn plain_types_are_not_checked() {
        clean(
            "s: {type: string}\nb: {type: boolean}\na: {type: array}\no: {type: object}\n",
            "s: 5\nb: maybe\na: x\no: [1]\n",
        );
    }

    #[test]
    fn empty_optional_values_are_skipped() {
        let schema =
            "e: {type: string, format: email, min_length: 5, pattern: '^x'}\nn: {type: number}\n";
        clean(schema, "e: ''\nn:\n");
        // A required empty value is checked.
        let schema = "e: {type: string, required: true, format: email}\n";
        assert_eq!(
            issues(schema, "e: ''\n"),
            ["c.yml:1:4: e: must be a valid email address"]
        );
    }

    #[test]
    fn not_equals_compares_the_resolved_value() {
        let schema =
            "e: {type: string, not_equals: 'your@example.com'}\nn: {type: number, not_equals: 0}\n";
        assert_eq!(
            issues(schema, "e: your@example.com\nn: 0\n"),
            [
                "c.yml:1:4: e: must not equal its configured default value",
                "c.yml:2:4: n: must not equal its configured default value",
            ]
        );
        // A string `0` isn't the number 0 (Go's DeepEqual).
        clean(schema, "e: x\nn: '0'\n");
    }

    /// sb-go: `required_with` fires only for a present-but-empty key.
    #[test]
    fn required_with_fires_for_a_present_empty_key() {
        let schema = "token: {type: string}\nemail: {type: string}\nkey: {type: string, required_with: [token, email]}\n";
        assert_eq!(
            issues(schema, "token: t\nkey: ''\n"),
            ["c.yml:2:6: key: is required when any of token, email are present"]
        );
        clean(schema, "token: t\n");
        clean(schema, "token: ''\nkey: ''\n");
    }

    #[test]
    fn formats_need_strings_and_valid_text() {
        let schema = "e: {type: string, format: email}\nh: {type: string, format: hostname}\nu: {type: string, format: url}\n";
        clean(
            schema,
            "e: me@example.com\nh: a-b.example.com\nu: https://example.com/x\n",
        );
        assert_eq!(
            issues(schema, "e: me@\nh: -bad\nu: ftp://x\n"),
            [
                "c.yml:1:4: e: must be a valid email address",
                "c.yml:2:4: h: must be a valid hostname",
                "c.yml:3:4: u: must be a valid URL",
            ]
        );
        assert_eq!(issues(schema, "e: 5\n"), ["c.yml:1:4: e: must be a string"]);
        let long = format!("h: {}\n", "a".repeat(254));
        assert_eq!(
            issues(schema, &long),
            ["c.yml:1:4: h: must be a valid hostname"]
        );
    }

    /// Lengths count bytes, as Go's `len`.
    #[test]
    fn lengths_count_bytes() {
        let schema = "p: {type: string, min_length: 3, max_length: 4}\n";
        clean(schema, "p: é1\n");
        assert_eq!(
            issues(schema, "p: ab\n"),
            ["c.yml:1:4: p: must be at least 3 characters long, got 2"]
        );
        assert_eq!(
            issues(schema, "p: éé1\n"),
            ["c.yml:1:4: p: must be at most 4 characters long, got 5"]
        );
        assert_eq!(
            issues(schema, "p: 12\n"),
            ["c.yml:1:4: p: must be a string"]
        );
    }

    #[test]
    fn numbers_are_whole() {
        let schema = "n: {type: number}\n";
        clean(schema, "n: '8080'\n");
        clean(schema, "n: 8080\n");
        clean(schema, "n: 18446744073709551615\n");
        assert_eq!(
            issues(schema, "n: abc\n"),
            ["c.yml:1:4: n: must be a whole number (integer), got: abc"]
        );
        assert_eq!(
            issues(schema, "n: 1.5\n"),
            ["c.yml:1:4: n: must be a whole number (integer), got: 1.5"]
        );
        assert_eq!(
            issues(schema, "n: 1e21\n"),
            ["c.yml:1:4: n: must be a whole number (integer), got: 1e+21"]
        );
        assert_eq!(
            issues(schema, "n: true\n"),
            ["c.yml:1:4: n: must be a whole number (integer), got: bool"]
        );
        assert_eq!(
            issues(schema, "n: [1]\n"),
            ["c.yml:1:4: n: must be a whole number (integer), got: []interface {}"]
        );
        assert_eq!(
            issues(schema, "n: 2001-01-01\n"),
            ["c.yml:1:4: n: must be a whole number (integer), got: time.Time"]
        );
        // Go's Atoi: a string past int64 isn't a whole number.
        assert_eq!(
            issues(schema, "n: '9223372036854775808'\n"),
            ["c.yml:1:4: n: must be a whole number (integer), got: 9223372036854775808"]
        );
        let required = "n: {type: number, required: true}\n";
        // (saphyr places an empty value just after its key's colon.)
        assert_eq!(
            issues(required, "n:\n"),
            ["c.yml:1:2: n: must be a whole number (integer), got: <nil>"]
        );
    }

    #[test]
    fn floats_are_floats() {
        let schema = "f: {type: float}\n";
        clean(schema, "f: '1.25'\n");
        clean(schema, "f: 1.25\n");
        clean(schema, "f: 'inf'\n");
        assert_eq!(
            issues(schema, "f: 5\n"),
            ["c.yml:1:4: f: must be a float, got: 5"]
        );
        assert_eq!(
            issues(schema, "f: abc\n"),
            ["c.yml:1:4: f: must be a float, got: abc"]
        );
        assert_eq!(
            issues(schema, "f: '1e400'\n"),
            ["c.yml:1:4: f: must be a float, got: 1e400"]
        );
        assert_eq!(
            issues(schema, "f: {a: 1}\n"),
            ["c.yml:1:4: f: must be a float, got: map[string]interface {}"]
        );
    }

    #[test]
    fn required_when_true_follows_ansible_booleans() {
        let schema = "enabled: {type: ansible_bool}\nremotes: {type: string, required_when_true: [enabled]}\n";
        clean(schema, "enabled: false\n");
        clean(schema, "enabled: 'false'\n");
        clean(schema, "enabled: true\nremotes: r\n");
        assert_eq!(
            issues(schema, "enabled: true\n"),
            ["c.yml:1:1: remotes: is required"]
        );
        assert_eq!(
            issues(schema, "enabled: ' Yes '\n"),
            ["c.yml:1:1: remotes: is required"]
        );
        // sb-go: an unquoted 1 is an integer, not an Ansible true.
        clean(schema, "enabled: 1\n");
    }

    #[test]
    fn validate_when_true_skips_or_checks() {
        let schema =
            "enabled: {type: ansible_bool}\nport: {type: number, validate_when_true: [enabled]}\n";
        clean(schema, "enabled: no\nport: abc\n");
        clean(schema, "port: abc\n");
        assert_eq!(
            issues(schema, "enabled: on\nport: abc\n"),
            ["c.yml:2:7: port: must be a whole number (integer), got: abc"]
        );
    }

    /// An unanchored search, as JSON Schema's; decision 4: a miss is an
    /// issue.
    #[test]
    fn patterns_search_strings() {
        let schema = "c: {type: string, pattern: 'b+'}\n";
        clean(schema, "c: abbc\n");
        assert_eq!(
            issues(schema, "c: xyz\n"),
            ["c.yml:1:4: c: must match the pattern `b+`"]
        );
        assert_eq!(issues(schema, "c: 5\n"), ["c.yml:1:4: c: must be a string"]);
    }

    #[test]
    fn objects_without_properties_take_any_keys() {
        clean("o: {type: object}\n", "o:\n  anything: 1\n  else: [2]\n");
    }

    #[test]
    fn additional_properties_check_the_other_keys() {
        let rule = "o:\n  type: object\n  additionalProperties: {type: string, min_length: 2}\n";
        clean(rule, "o:\n  a: xy\n  b: zz\n");
        assert_eq!(
            issues(rule, "o:\n  a: x\n"),
            ["c.yml:2:6: o.a: must be at least 2 characters long, got 1"]
        );
        let closed = "o:\n  type: object\n  properties:\n    a: {type: string}\n  additionalProperties: false\n";
        assert_eq!(
            issues(closed, "o:\n  a: x\n  b: y\n"),
            ["c.yml:3:3: o.b: unknown field"]
        );
        let open = "o:\n  type: object\n  properties:\n    a: {type: string}\n  additionalProperties: true\n";
        clean(open, "o:\n  a: x\n  b: y\n");
        let both = "o:\n  type: object\n  properties:\n    a: {type: number}\n  additionalProperties: {type: number}\n";
        assert_eq!(
            issues(both, "o:\n  a: x\n  b: y\n"),
            [
                "c.yml:2:6: o.a: must be a whole number (integer), got: x",
                "c.yml:3:6: o.b: must be a whole number (integer), got: y",
            ]
        );
        // A scalar skips them (type flexibility); a list is an issue.
        clean(rule, "o: 1\n");
        assert_eq!(
            issues(rule, "o: [1]\n"),
            ["c.yml:1:4: o: must be an object, got a list"]
        );
    }

    /// A list where the schema describes an object's keys is an issue at
    /// the list (Task 60; sb-go skips it); an empty list, a scalar and a
    /// plain `type: object` aren't (a credential section: tests/rules.rs).
    #[test]
    fn a_list_where_an_object_belongs_is_an_issue() {
        let schema = "user:\n  type: object\n  required: true\n  properties:\n    name: {type: string, required: true}\n    pass: {type: password}\n";
        assert_eq!(
            issues(schema, "user:\n  - pass: x\n"),
            ["c.yml:2:3: user: must be an object, got a list"]
        );
        let nested = "a:\n  type: object\n  properties:\n    b:\n      type: object\n      properties:\n        c: {type: string}\n";
        assert_eq!(
            issues(nested, "a:\n  b:\n    - c: x\n    - d\n"),
            ["c.yml:3:5: a.b: must be an object, got a list"]
        );
        clean(
            schema
                .replace("required: true\n  properties", "properties")
                .as_str(),
            "user: []\n",
        );
        clean(schema, "user: x\n");
        clean("o: {type: object}\n", "o: [1]\n");
    }

    /// motd.yml's older form, a bare list of instances, is the plain
    /// issue when the check meets it: validate-config migrates such a
    /// file first (`migrate`, motd decision 4), and one it can't migrate
    /// is reported as it is.
    #[test]
    fn an_older_list_of_instances_is_the_plain_issue() {
        let schema = "plex:\n  type: object\n  properties:\n    enabled: {type: boolean}\n    instances:\n      type: array\n      items:\n        type: object\n        properties:\n          name: {type: string, required: true}\n";
        let config = "plex:\n  - name: a\n";
        assert_eq!(
            issues(schema, config),
            ["c.yml:2:3: plex: must be an object, got a list"]
        );
        let shown: Vec<String> = run_in("motd.yml", schema, config, &Fake::default())
            .issues
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(shown, ["motd.yml:2:3: plex: must be an object, got a list"]);
        clean(
            schema,
            "plex:\n  enabled: true\n  instances:\n    - name: a\n",
        );
    }

    /// Items get the rules of `items`; sb-go hands them the array's parent.
    #[test]
    fn items_take_the_arrays_parent() {
        let schema = "token: {type: string}\nlist:\n  type: array\n  items: {type: string, required_with: [token], custom_validator: validate_url}\n";
        assert_eq!(
            issues(schema, "token: t\nlist:\n  - a\n  - ''\n"),
            ["c.yml:4:5: list[1]: is required when any of token are present"]
        );
        let fake = Fake::default();
        let _ = run_with(schema, "token: t\nlist: [a]\n", &fake);
        assert_eq!(
            *fake.calls.borrow(),
            [(Validator::ValidateUrl, "token,list".to_string())]
        );
        // A value that isn't a sequence skips its items.
        clean(schema, "token: t\nlist: x\n");
    }

    #[test]
    fn nested_objects_are_walked() {
        let schema = "o:\n  type: object\n  properties:\n    i:\n      type: object\n      properties:\n        n: {type: number}\n";
        assert_eq!(
            issues(schema, "o:\n  i:\n    n: x\n"),
            ["c.yml:3:8: o.i.n: must be a whole number (integer), got: x"]
        );
        clean(schema, "o: x\n");
    }

    #[test]
    fn type_validators_and_custom_validators_are_called() {
        let schema = "h: {type: hostname}\nc: {type: string, custom_validator: validate_cron_time}\nw: {type: url}\n";
        let fake = Fake::default();
        let report = run_with(schema, "h: bad\nc: bad\nw: warn\n", &fake);
        let shown: Vec<String> = report.issues.iter().map(ToString::to_string).collect();
        assert_eq!(
            shown,
            [
                "c.yml:1:4: h: ValidateHostname refused it",
                "c.yml:2:4: c: ValidateCronTime refused it",
            ]
        );
        assert_eq!(report.warnings, ["a warning"]);
        let mut called: Vec<String> = fake
            .calls
            .borrow()
            .iter()
            .map(|(v, _)| format!("{v:?}"))
            .collect();
        called.sort();
        assert_eq!(
            called,
            ["ValidateCronTime", "ValidateHostname", "ValidateUrl"]
        );
    }

    /// Every type validator stands for its `validate_*` (sb-go's
    /// `builtInValidators`).
    #[test]
    fn each_type_calls_its_validator() {
        let pairs = [
            ("ansible_bool", Validator::ValidateAnsibleBool),
            ("subdomain", Validator::ValidateSubdomain),
            ("hostname", Validator::ValidateHostname),
            ("directory_path", Validator::ValidateDirectoryPath),
            ("url", Validator::ValidateUrl),
            ("timezone", Validator::ValidateTimezone),
            ("cron_time", Validator::ValidateCronTime),
            ("rclone_template", Validator::ValidateRcloneTemplate),
            ("ssh_key_or_url", Validator::ValidateSshKeyOrUrl),
            ("password", Validator::ValidatePasswordStrength),
        ];
        for (kind, validator) in pairs {
            let fake = Fake::default();
            let _ = run_with(&format!("v: {{type: {kind}}}\n"), "v: x\n", &fake);
            let called: Vec<Validator> = fake.calls.borrow().iter().map(|(v, _)| *v).collect();
            assert_eq!(called, [validator], "{kind}");
        }
    }

    #[test]
    fn passwords_warn_through_their_type() {
        let report = run_with("p: {type: password}\n", "p: short\n", &Fake::default());
        assert_eq!(report.warnings, ["password is short"]);
        let report = run_with(
            "p: {type: string, custom_validator: validate_password_strength}\n",
            "p: short\n",
            &Fake::default(),
        );
        assert_eq!(report.warnings, [] as [String; 0]);
    }

    /// One issue per field (sb-go's first), every field reported, sorted.
    #[test]
    fn every_field_reports_its_first_issue() {
        let schema = "a: {type: string, format: email, not_equals: 'your@example.com'}\nb: {type: number}\nc: {type: string, min_length: 3}\n";
        assert_eq!(
            issues(schema, "c: x\nb: y\na: your@example.com\n"),
            [
                "c.yml:1:4: c: must be at least 3 characters long, got 1",
                "c.yml:2:4: b: must be a whole number (integer), got: y",
                "c.yml:3:4: a: must not equal its configured default value",
            ]
        );
    }

    /// Go's %v for floats: exponent form from 1e+06 up and below 1e-04.
    #[test]
    fn float_values_print_as_go_does() {
        let schema = "n: {type: number}\n";
        let got = |config: &str| issues(schema, config).remove(0);
        assert!(got("n: 1e6\n").ends_with("got: 1e+06"));
        assert!(got("n: 1234567.5\n").ends_with("got: 1.2345675e+06"));
        assert!(got("n: 123456.7\n").ends_with("got: 123456.7"));
        assert!(got("n: 0.0001\n").ends_with("got: 0.0001"));
        assert!(got("n: 0.00001\n").ends_with("got: 1e-05"));
        assert!(got("n: 1e20\n").ends_with("got: 1e+20"));
        assert!(got("n: .nan\n").ends_with("got: NaN"));
        assert!(got("n: .inf\n").ends_with("got: +Inf"));
        assert!(got("n: -.inf\n").ends_with("got: -Inf"));
    }

    /// Go's `ParseFloat`: a sign goes with inf only, not nan; underscores
    /// sit between digits.
    #[test]
    fn float_strings_parse_as_go_does() {
        let schema = "f: {type: float}\n";
        clean(schema, "f: 'NaN'\n");
        clean(schema, "f: '-Infinity'\n");
        clean(schema, "f: '1_000.5'\n");
        assert_eq!(
            issues(schema, "f: '+nan'\n"),
            ["c.yml:1:4: f: must be a float, got: +nan"]
        );
        assert_eq!(
            issues(schema, "f: '1__0'\n"),
            ["c.yml:1:4: f: must be a float, got: 1__0"]
        );
        assert_eq!(
            issues(schema, "f: '_1'\n"),
            ["c.yml:1:4: f: must be a float, got: _1"]
        );
        assert_eq!(
            issues(schema, "f: '1_'\n"),
            ["c.yml:1:4: f: must be a float, got: 1_"]
        );
    }

    const CREDENTIALS: &str = "cloudflare:\n  type: object\n  custom_validator: validate_cloudflare_config\n  properties:\n    api: {type: string}\n    bad: {type: string}\ndockerhub:\n  type: object\n  custom_validator: validate_dockerhub_config\n";

    /// A section whose credentials are ready and that has no issues of its
    /// own is a request for the check, run later (Tasks 8-10).
    #[test]
    fn ready_credentials_are_requests() {
        let report = run_with(
            CREDENTIALS,
            "cloudflare:\n  api: k\ndockerhub:\n  user: u\n",
            &Fake::default(),
        );
        assert!(report.issues.is_empty(), "{:?}", report.issues);
        let asked: Vec<(Validator, &str, &Login)> = report
            .requests
            .iter()
            .map(|r| (r.validator(), r.path.as_str(), &r.login))
            .collect();
        // Each request carries the login its own section gave, at its line.
        assert_eq!(
            asked,
            [
                (
                    Validator::ValidateCloudflareConfig,
                    "cloudflare",
                    &Login::Cloudflare {
                        credentials: crate::cloudflare::Credentials::Token("k".into()),
                        domain: "line 2".into(),
                    }
                ),
                (
                    Validator::ValidateDockerhubConfig,
                    "dockerhub",
                    &Login::Dockerhub(DockerhubLogin {
                        user: "u".into(),
                        token: "line 4".into(),
                    })
                ),
            ]
        );
    }

    /// No credentials (an empty section, or none): the check is skipped,
    /// which the run shows.
    #[test]
    fn missing_credentials_are_skipped() {
        let report = run_with(CREDENTIALS, "cloudflare: {}\n", &Fake::default());
        assert_eq!(report.requests, [] as [Request; 0]);
        assert_eq!(
            report.skipped,
            [
                Validator::ValidateCloudflareConfig,
                Validator::ValidateDockerhubConfig
            ]
        );
    }

    /// A structure error is the file's issue, at the section's key; the
    /// check isn't run, and the section's other issues are still found
    /// (decision 2).
    #[test]
    fn invalid_credentials_are_issues() {
        let schema = "cloudflare:\n  type: object\n  custom_validator: validate_cloudflare_config\n  properties:\n    bad: {type: string}\n    email: {type: string, format: email}\n";
        let report = run_with(
            schema,
            "cloudflare:\n  bad: x\n  email: nope\n  bogus: 1\n",
            &Fake::default(),
        );
        let shown: Vec<String> = report.issues.iter().map(ToString::to_string).collect();
        assert_eq!(
            shown,
            [
                "c.yml:1:1: cloudflare: structure is wrong",
                "c.yml:3:10: cloudflare.email: must be a valid email address",
                "c.yml:4:3: cloudflare.bogus: unknown field",
            ]
        );
        assert_eq!(report.requests, [] as [Request; 0]);
    }

    /// No credentials, the section absent or null as well as empty: the
    /// check is skipped, and the validator isn't asked.
    #[test]
    fn absent_or_null_credentials_are_skipped() {
        for config in ["other: 1\n", "cloudflare:\n", "cloudflare: ''\n"] {
            let fake = Fake::default();
            let report = run_with(CREDENTIALS, &format!("{config}dockerhub: {{}}\n"), &fake);
            assert!(report.requests.is_empty(), "{config}");
            assert!(
                report
                    .skipped
                    .contains(&Validator::ValidateCloudflareConfig),
                "{config}"
            );
            assert!(
                fake.calls
                    .borrow()
                    .iter()
                    .all(|(v, _)| *v != Validator::ValidateCloudflareConfig),
                "{config}"
            );
        }
    }

    /// An issue inside the section (an unknown key) skips its check too.
    #[test]
    fn a_section_with_issues_is_not_checked() {
        let report = run_with(
            CREDENTIALS,
            "cloudflare:\n  api: k\n  extra: 1\n",
            &Fake::default(),
        );
        let shown: Vec<String> = report.issues.iter().map(ToString::to_string).collect();
        assert_eq!(shown, ["c.yml:3:3: cloudflare.extra: unknown field"]);
        assert_eq!(report.requests, [] as [Request; 0]);
    }

    /// A request's debug output never shows the credentials it carries.
    #[test]
    fn a_request_never_prints_its_values() {
        let report = run_with(
            CREDENTIALS,
            "cloudflare:\n  api: not-a-real-key\n",
            &Fake::default(),
        );
        let shown = format!("{report:?}");
        assert!(shown.contains("ValidateCloudflareConfig"), "{shown}");
        assert!(!shown.contains("not-a-real-key"), "{shown}");
    }

    /// The rule's `validate_when_true` applies to other keys as to
    /// properties, looking in the same mapping.
    #[test]
    fn additional_properties_honour_validate_when_true() {
        let gated = "o:\n  type: object\n  properties:\n    gate: {type: ansible_bool}\n  additionalProperties: {type: number, validate_when_true: [gate]}\n";
        clean(gated, "o:\n  a: x\n");
        assert_eq!(
            issues(gated, "o:\n  gate: yes\n  a: x\n"),
            ["c.yml:3:6: o.a: must be a whole number (integer), got: x"]
        );
    }

    /// sb-go: a required null still reaches the type's validator (its
    /// `validate_when_true` table: an rclone template left empty).
    #[test]
    fn a_required_null_reaches_the_validator() {
        let fake = Fake::default();
        let _ = run_with(
            "t: {type: rclone_template, required: true}\n",
            "t:\n",
            &fake,
        );
        assert_eq!(fake.calls.borrow().len(), 1);
        let fake = Fake::default();
        let _ = run_with("t: {type: rclone_template}\n", "t:\n", &fake);
        assert!(fake.calls.borrow().is_empty());
    }

    /// A missing required key is reported even when `validate_when_true`
    /// is false: the required check comes first.
    #[test]
    fn required_comes_before_validate_when_true() {
        let schema = "on: {type: ansible_bool}\nx: {type: string, required: true, validate_when_true: [on]}\n";
        assert_eq!(issues(schema, "on: no\n"), ["c.yml:1:1: x: is required"]);
    }

    /// Any of several keys being true is enough; an absent one is skipped.
    #[test]
    fn conditions_take_any_of_their_keys() {
        let schema = "a: {type: ansible_bool}\nb: {type: ansible_bool}\nx: {type: string, required_when_true: [a, b]}\n";
        assert_eq!(issues(schema, "b: true\n"), ["c.yml:1:1: x: is required"]);
        clean(schema, "a: false\n");
    }

    #[test]
    fn not_equals_takes_booleans_and_floats() {
        let schema = "b: {type: boolean, not_equals: true}\nf: {type: float, not_equals: 1.5}\n";
        assert_eq!(
            issues(schema, "b: true\nf: 1.5\n"),
            [
                "c.yml:1:4: b: must not equal its configured default value",
                "c.yml:2:4: f: must not equal its configured default value",
            ]
        );
        clean(schema, "b: false\nf: 2.5\n");
        // A null `not_equals` checks nothing.
        clean("b: {type: string, not_equals: ~}\n", "b:\n");
    }

    /// "is required" inside an item points at the item.
    #[test]
    fn required_inside_an_item_points_at_it() {
        let schema = "l:\n  type: array\n  items:\n    type: object\n    properties:\n      r: {type: string, required: true}\n";
        assert_eq!(
            issues(schema, "l:\n  - {}\n"),
            ["c.yml:2:5: l[0].r: is required"]
        );
    }

    /// yaml.v3 decodes a nested mapping with a non-string key into a map
    /// sb-go's rules skip; sb checks it like any mapping (a listed
    /// difference).
    #[test]
    fn nested_non_string_keys_are_checked() {
        let schema = "o:\n  type: object\n  properties:\n    a: {type: string}\n";
        assert_eq!(
            issues(schema, "o:\n  1: x\n"),
            ["c.yml:2:3: o.1: unknown field"]
        );
    }

    /// Each length bound works on its own.
    #[test]
    fn each_length_bound_stands_alone() {
        clean("p: {type: string, min_length: 2}\n", "p: a-long-value\n");
        // Exactly at a bound is fine.
        clean(
            "p: {type: string, min_length: 3, max_length: 3}\n",
            "p: abc\n",
        );
        assert_eq!(
            issues("p: {type: string, max_length: 2}\n", "p: abc\n"),
            ["c.yml:1:4: p: must be at most 2 characters long, got 3"]
        );
    }

    /// The secret fields by schema: the credential paths and every
    /// password-typed field, through properties, items and other keys.
    #[test]
    fn secret_paths_follow_the_schema() {
        let rules = schema::parse(
            "s.schema.yml",
            "user:\n  type: object\n  properties:\n    pass: {type: password}\n    name: {type: string}\nlist:\n  type: array\n  items:\n    type: object\n    properties:\n      key: {type: password}\nmore:\n  type: object\n  additionalProperties: {type: password}\nother:\n  type: object\n  properties:\n    pass: {type: string}\nopen:\n  type: object\n  additionalProperties:\n    type: object\n    properties:\n      deep: {type: password}\n",
        )
        .expect("schema");
        let secret = secret_paths(&rules);
        assert!(secret("user.pass"));
        assert!(!secret("user.name"));
        assert!(secret("list[3].key"));
        assert!(secret("more.anything"));
        assert!(secret("dockerhub.token"));
        assert!(!secret("nothing.here"));
        // Off the schema, by the key's name (config-L2).
        assert!(secret("user[0].pass"));
        assert!(secret("user.extra.key"));
        assert!(secret("anywhere[2].token"));
        assert!(secret("pass"));
        assert!(!secret("user[0].name"));
        assert!(secret("elsewhere[0].deep"));
        // On the schema too, a credential's key name is secret whatever
        // its type: a string named `pass` doesn't show.
        assert!(secret("other.pass"));
    }

    /// A field the pinned list doesn't know, typed `string` (a new
    /// provider's `apikey`), is secret by its key's name, on the schema as
    /// off it: the shared key-name rule (`facts::secret::is_secret_key`).
    #[test]
    fn a_new_credential_field_is_secret_by_its_name() {
        let rules = schema::parse(
            "s.schema.yml",
            "newapp:\n  type: object\n  properties:\n    apikey: {type: string}\n    api_key: {type: string}\n    url: {type: string}\n",
        )
        .expect("schema");
        let secret = secret_paths(&rules);
        assert!(secret("newapp.apikey"));
        assert!(secret("newapp.api_key"));
        assert!(!secret("newapp.url"));
        // Off the schema, the same names.
        assert!(secret("newapp[0].api_key"));
        assert!(secret("elsewhere.client_secret"));
        assert!(!secret("elsewhere.url"));
    }

    /// The pinned Saltbox schemas (tests/fixtures/saltbox-schemas).
    fn pinned_schemas() -> Vec<(&'static str, Rules)> {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/saltbox-schemas");
        [
            "accounts",
            "adv_settings",
            "backup_config",
            "hetzner_vlan",
            "motd",
            "settings",
        ]
        .into_iter()
        .map(|name| {
            let file = format!("{name}.schema.yml");
            let rules = schema::load(&dir.join(&file), &file).expect("pinned schema");
            (name, rules)
        })
        .collect()
    }

    /// Every property's path (`[]` for an array's items) and rule.
    fn property_paths<'a>(rules: &'a Rules, at: &str, out: &mut Vec<(String, &'a Rule)>) {
        fn walk<'a>(rule: &'a Rule, path: String, out: &mut Vec<(String, &'a Rule)>) {
            if let Some(properties) = &rule.properties {
                property_paths(properties, &path, out);
            }
            if let Some(items) = &rule.items {
                walk(items, format!("{path}[]"), out);
            }
            if let Some(Additional::Rule(other)) = &rule.additional_properties {
                walk(other, format!("{path}.<any>"), out);
            }
            out.push((path, rule));
        }
        for (name, rule) in rules {
            walk(rule, join(at, name), out);
        }
    }

    /// The credential paths (`secret`'s list) each reach a `type: string`
    /// field of exactly one pinned schema, and every `string` or `url`
    /// field of a pinned schema whose name looks like a credential's (the
    /// shared key-name rule, `facts::secret::is_secret_key`) is on the
    /// list (Task 60). Such a field is masked by its name already
    /// (2026-11-18); a schema pin that moves one, or adds one, fails here
    /// so the list, which documents Saltbox's credentials, keeps up.
    #[test]
    fn the_credential_paths_match_the_pinned_schemas() {
        let schemas = pinned_schemas();
        for path in secret::credential_paths() {
            let found: Vec<_> = schemas
                .iter()
                .filter_map(|(name, rules)| {
                    rule_at(rules, &path.replace("[]", "[0]")).map(|rule| (name, rule.kind))
                })
                .collect();
            assert_eq!(found.len(), 1, "{path}: {found:?}");
            assert_eq!(found[0].1, Some(Kind::String), "{path}");
        }
        let credential_like = crate::facts::secret::is_secret_key;
        let listed: BTreeSet<_> = secret::credential_paths().collect();
        let mut checked = 0;
        for (name, rules) in &schemas {
            let mut paths = Vec::new();
            property_paths(rules, "", &mut paths);
            for (path, rule) in paths {
                if matches!(rule.kind, Some(Kind::String | Kind::Url))
                    && credential_like(secret::key_name(&path))
                {
                    assert!(listed.contains(path.as_str()), "{name}: {path}");
                    checked += 1;
                }
            }
        }
        assert_eq!(
            checked,
            listed.len(),
            "every listed path is credential-like"
        );
    }
}
