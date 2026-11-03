//! The rules end to end over fixture files (config plan Task 6): a valid
//! synthetic set passes Saltbox's pinned schemas, and proto-yaml's
//! many-errors case yields every issue, sorted, with positions.

use std::path::{Path, PathBuf};

use sb_config::rules::{self, Credentials, Validators, Verdict};
use sb_config::scalar::Value;
use sb_config::schema::{self, Validator};
use sb_config::tree::{Data, Node};

fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(path)
}

/// Validators that accept everything (Task 7 brings the real ones).
struct AcceptAll;

impl Validators for AcceptAll {
    fn validate(&self, _: Validator, _: &Node, _: &Node) -> Verdict {
        Verdict::Valid
    }
    fn password_warning(&self, _: &Node) -> Option<String> {
        None
    }
    fn credentials(&self, _: Validator, _: &Node, _: &Node) -> Credentials {
        Credentials::Ready
    }
}

/// Just enough of `validate_ansible_bool` and `validate_url` for the
/// many-errors case.
struct Minimal;

impl Validators for Minimal {
    fn validate(&self, validator: Validator, value: &Node, _: &Node) -> Verdict {
        let Data::Scalar(Value::Str(text)) = &value.data else {
            return Verdict::Valid;
        };
        match validator {
            Validator::ValidateAnsibleBool if text == "maybe" => {
                Verdict::Invalid("must be a valid Ansible boolean".into())
            }
            Validator::ValidateUrl if !text.starts_with("http") => {
                Verdict::Invalid("must be a valid URL".into())
            }
            _ => Verdict::Valid,
        }
    }
    fn password_warning(&self, _: &Node) -> Option<String> {
        None
    }
    fn credentials(&self, _: Validator, _: &Node, _: &Node) -> Credentials {
        Credentials::Ready
    }
}

fn report(
    text: &str,
    name: &str,
    schema_path: &Path,
    validators: &dyn Validators,
) -> rules::Report {
    let rules = schema::load(schema_path, "schema").unwrap_or_else(|err| panic!("{err}"));
    let root = sb_config::parse::parse(name, text).unwrap_or_else(|i| panic!("{i:?}"));
    let tree = sb_config::tree::build(name, &root).unwrap_or_else(|i| panic!("{i:?}"));
    rules::validate(name, &tree, &rules, validators)
}

fn issues(
    config: &Path,
    name: &str,
    schema_path: &Path,
    validators: &dyn Validators,
) -> Vec<String> {
    let text = std::fs::read_to_string(config).expect("read config");
    report(&text, name, schema_path, validators)
        .issues
        .iter()
        .map(ToString::to_string)
        .collect()
}

#[test]
fn the_valid_set_passes_the_pinned_schemas() {
    for name in [
        "accounts",
        "adv_settings",
        "backup_config",
        "hetzner_vlan",
        "motd",
        "settings",
    ] {
        let found = issues(
            &fixture(&format!("valid/{name}.yml")),
            &format!("{name}.yml"),
            &fixture(&format!("saltbox-schemas/{name}.schema.yml")),
            &AcceptAll,
        );
        assert_eq!(found, Vec::<String>::new(), "{name}");
    }
}

#[test]
fn many_errors_yields_every_issue_in_order() {
    let found = issues(
        &fixture("many-errors/many-errors.yml"),
        "many-errors.yml",
        &fixture("many-errors/schema.yml"),
        &Minimal,
    );
    assert_eq!(
        found,
        [
            "many-errors.yml:1:1: user.domain: is required",
            "many-errors.yml:2:9: user.name: must be at least 1 characters long, got 0",
            "many-errors.yml:3:10: user.email: must not equal its configured default value",
            "many-errors.yml:5:12: backup.enabled: must be a valid Ansible boolean",
            "many-errors.yml:6:12: backup.retries: must be a whole number (integer), got: lots",
            "many-errors.yml:8:7: backup.hosts[0]: must be a valid URL",
            "many-errors.yml:9:3: backup.colour: unknown field",
            "many-errors.yml:11:8: labels.web: must be at least 2 characters long, got 1",
        ]
    );
}

/// The valid accounts.yml's credential sections are both requests.
#[test]
fn the_valid_accounts_asks_for_both_checks() {
    let text = std::fs::read_to_string(fixture("valid/accounts.yml")).expect("read");
    let found = report(
        &text,
        "accounts.yml",
        &fixture("saltbox-schemas/accounts.schema.yml"),
        &AcceptAll,
    );
    let asked: Vec<(Validator, &str)> = found
        .requests
        .iter()
        .map(|r| (r.validator, r.path.as_str()))
        .collect();
    assert_eq!(
        asked,
        [
            (Validator::ValidateCloudflareConfig, "cloudflare"),
            (Validator::ValidateDockerhubConfig, "dockerhub"),
        ]
    );
}

/// The pinned motd schema's colour pattern and display_names rule, end to
/// end (decision 4: a miss is an issue).
#[test]
fn the_pinned_motd_schema_checks_colours_and_names() {
    let found: Vec<String> = report(
        "colors:\n  text:\n    label: \"#abcd\"\nsystemd:\n  display_names:\n    plex: 5\n",
        "motd.yml",
        &fixture("saltbox-schemas/motd.schema.yml"),
        &AcceptAll,
    )
    .issues
    .iter()
    .map(ToString::to_string)
    .collect();
    assert_eq!(
        found,
        [
            "motd.yml:3:12: colors.text.label: must match the pattern `^#([A-Fa-f0-9]{6}|[A-Fa-f0-9]{3})$`"
        ]
    );
}
