//! The rules end to end over fixture files, with the real validators
//! (config plan Tasks 6-7): a valid
//! synthetic set passes Saltbox's pinned schemas, and proto-yaml's
//! many-errors case yields every issue, sorted, with positions.

use std::path::{Path, PathBuf};

use sb_config::cloudflare::Credentials;
use sb_config::dockerhub::DockerhubLogin;
use sb_config::rules::{self, Login, Validators};
use sb_config::schema::{self, Validator};
use sb_config::validators::Saltbox;

fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(path)
}

fn report(
    text: &str,
    name: &str,
    schema_path: &Path,
    validators: &dyn Validators,
) -> rules::Report {
    let rules = schema::load(schema_path, "schema").unwrap_or_else(|err| panic!("{err}"));
    let root = sb_config::parse::parse(name, text).unwrap_or_else(|i| panic!("{i:?}"));
    let tree = sb_config::tree::build(name, &root, &rules::secret_paths(&rules))
        .unwrap_or_else(|i| panic!("{i:?}"));
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
            &Saltbox::with_zoneinfo(Vec::new()),
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
        &Saltbox::with_zoneinfo(Vec::new()),
    );
    assert_eq!(
        found,
        [
            "many-errors.yml:1:1: user.domain: is required",
            "many-errors.yml:2:9: user.name: must be at least 1 characters long, got 0",
            "many-errors.yml:3:10: user.email: must not equal its configured default value",
            "many-errors.yml:5:12: backup.enabled: must be a valid Ansible boolean (yes/no, true/false, on/off, 1/0), got: maybe",
            "many-errors.yml:6:12: backup.retries: must be a whole number (integer), got: lots",
            "many-errors.yml:8:7: backup.hosts[0]: must be a valid URL format (e.g., https://example.com)",
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
        &Saltbox::with_zoneinfo(Vec::new()),
    );
    let asked: Vec<(Validator, &str)> = found
        .requests
        .iter()
        .map(|r| (r.validator(), r.path.as_str()))
        .collect();
    assert_eq!(
        asked,
        [
            (Validator::ValidateCloudflareConfig, "cloudflare"),
            (Validator::ValidateDockerhubConfig, "dockerhub"),
        ]
    );
    // The logins are the fixture's values, parsed once by the rules.
    let logins: Vec<&Login> = found.requests.iter().map(|r| &r.login).collect();
    assert_eq!(
        logins,
        [
            &Login::Cloudflare {
                credentials: Credentials::Token("not-a-real-token".into()),
                domain: "example.com".into(),
            },
            &Login::Dockerhub(DockerhubLogin {
                user: "seed".into(),
                token: "not-a-real-token".into(),
            }),
        ]
    );
}

/// The pinned motd schema's colour pattern and `display_names` rule, end to
/// end (decision 4: a miss is an issue).
#[test]
fn the_pinned_motd_schema_checks_colours_and_names() {
    let found: Vec<String> = report(
        "colors:\n  text:\n    label: \"#abcd\"\nsystemd:\n  display_names:\n    plex: 5\n",
        "motd.yml",
        &fixture("saltbox-schemas/motd.schema.yml"),
        &Saltbox::with_zoneinfo(Vec::new()),
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

/// No secret value in any issue or warning (config spec, "Secrets"): each
/// secret field given a rule that echoes what it checks.
#[test]
fn secrets_are_never_shown() {
    let schema = r"
cloudflare:
  type: object
  properties:
    api: {type: number}
    scoped_token: {type: cron_time}
dockerhub:
  type: object
  properties:
    token: {type: ansible_bool}
user:
  type: object
  properties:
    pass: {type: password, min_length: 30}
";
    let dir = tempfile::tempdir().expect("temp dir");
    let schema_path = dir.path().join("secrets.schema.yml");
    std::fs::write(&schema_path, schema).expect("write schema");
    let config = "cloudflare:\n  api: s3cret-one\n  scoped_token: s3cret-two\ndockerhub:\n  token: s3cret-three\nuser:\n  pass: s3cret-4\n";
    let found = report(
        config,
        "accounts.yml",
        &schema_path,
        &Saltbox::with_zoneinfo(Vec::new()),
    );
    let shown: Vec<String> = found.issues.iter().map(ToString::to_string).collect();
    assert_eq!(
        shown,
        [
            "accounts.yml:2:8: cloudflare.api: must be a whole number (integer), got: [REDACTED]",
            "accounts.yml:3:17: cloudflare.scoped_token: must be a valid Ansible cron special time (annually, daily, hourly, monthly, reboot, weekly, yearly), got: [REDACTED]",
            "accounts.yml:5:10: dockerhub.token: must be a valid Ansible boolean (yes/no, true/false, on/off, 1/0), got: [REDACTED]",
            "accounts.yml:7:9: user.pass: must be at least 30 characters long, got 8",
        ]
    );
    for line in shown.iter().chain(&found.warnings) {
        assert!(!line.contains("s3cret"), "{line}");
    }
}

/// A tag error on a password-typed field doesn't show the value either.
#[test]
fn a_secret_tag_error_is_redacted() {
    let rules =
        schema::load(&fixture("saltbox-schemas/accounts.schema.yml"), "schema").expect("schema");
    let root =
        sb_config::parse::parse("accounts.yml", "user:\n  pass: !!int s3cret\n").expect("parses");
    let issues: Vec<String> =
        sb_config::tree::build("accounts.yml", &root, &rules::secret_paths(&rules))
            .expect_err("fails")
            .iter()
            .map(ToString::to_string)
            .collect();
    assert_eq!(
        issues,
        ["accounts.yml:2:9: user.pass: cannot decode !!str `[REDACTED]` as a !!int"]
    );
}

/// A list under a password-typed field hides its values too.
#[test]
fn values_under_a_secret_field_are_redacted() {
    let rules =
        schema::load(&fixture("saltbox-schemas/accounts.schema.yml"), "schema").expect("schema");
    let root = sb_config::parse::parse("accounts.yml", "user:\n  pass:\n    - !!bool s3cret\n")
        .expect("parses");
    let issues: Vec<String> =
        sb_config::tree::build("accounts.yml", &root, &rules::secret_paths(&rules))
            .expect_err("fails")
            .iter()
            .map(ToString::to_string)
            .collect();
    assert_eq!(
        issues,
        ["accounts.yml:3:7: user.pass[0]: cannot decode !!str `[REDACTED]` as a !!bool"]
    );
}

/// A secret key in a structure the schema doesn't expect (a list where
/// `user` is a mapping) hides its value too (config-L2).
#[test]
fn a_secret_key_in_a_misplaced_list_is_redacted() {
    let rules =
        schema::load(&fixture("saltbox-schemas/accounts.schema.yml"), "schema").expect("schema");
    let root = sb_config::parse::parse(
        "accounts.yml",
        "user:\n  - pass: !!int s3cret\ncloudflare:\n  extra:\n    api: !!int s3cret-key\n",
    )
    .expect("parses");
    let issues: Vec<String> =
        sb_config::tree::build("accounts.yml", &root, &rules::secret_paths(&rules))
            .expect_err("fails")
            .iter()
            .map(ToString::to_string)
            .collect();
    assert_eq!(
        issues,
        [
            "accounts.yml:2:11: user[0].pass: cannot decode !!str `[REDACTED]` as a !!int",
            "accounts.yml:5:10: cloudflare.extra.api: cannot decode !!str `[REDACTED]` as a !!int",
        ]
    );
}

/// A credential the schema types `string` hides its value too, at any
/// item of a list (Task 60): motd.yml's tokens and `backup_config.yml`'s
/// restore password.
#[test]
fn a_string_typed_credential_is_redacted() {
    for (schema_file, file, text, expected) in [
        (
            "motd.schema.yml",
            "motd.yml",
            "plex:\n  instances:\n    - name: a\n    - token: !!int s3cret-token\n",
            "motd.yml:4:14: plex.instances[1].token: cannot decode !!str `[REDACTED]` as a !!int",
        ),
        (
            "backup_config.schema.yml",
            "backup_config.yml",
            "backup:\n  restore_service:\n    pass: !!bool s3cret\n",
            "backup_config.yml:3:11: backup.restore_service.pass: cannot decode !!str `[REDACTED]` as a !!bool",
        ),
    ] {
        let rules = schema::load(
            &fixture(&format!("saltbox-schemas/{schema_file}")),
            "schema",
        )
        .expect("schema");
        let root = sb_config::parse::parse(file, text).expect("parses");
        let issues: Vec<String> = sb_config::tree::build(file, &root, &rules::secret_paths(&rules))
            .expect_err("fails")
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(issues, [expected]);
    }
}

/// A list where accounts.yml's schema wants an object is one issue at the
/// list, with the real validators (Task 60); a credential section keeps
/// its own structure issue, at its key, and nothing more.
#[test]
fn a_list_where_an_object_belongs_is_an_issue() {
    let text = "user:\n  - name: a\n    pass: s3cret-pass\ncloudflare:\n  - api: s3cret-key\n";
    let found: Vec<String> = report(
        text,
        "accounts.yml",
        &fixture("saltbox-schemas/accounts.schema.yml"),
        &Saltbox::with_zoneinfo(Vec::new()),
    )
    .issues
    .iter()
    .map(ToString::to_string)
    .collect();
    assert_eq!(
        found,
        [
            "accounts.yml:2:3: user: must be an object, got a list",
            "accounts.yml:4:1: cloudflare: cloudflare config must be an object",
        ]
    );
}
