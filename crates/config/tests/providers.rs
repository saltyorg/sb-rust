//! providers.yml's check and the default copies (config spec, decision
//! 10; success criteria 13 and 14): sb's draft schema
//! (`fixtures/saltbox-schemas/providers.schema.yml`), the notice while
//! Saltbox has no schema, the copies of a missing `hetzner_vlan.yml` or
//! `providers.yml` from Saltbox's defaults, and the credentials kept out of
//! the screen and `--json`. Without the `testing` feature (an integration
//! test doesn't get it), so the root is built here; accounts.yml has no
//! credentials, so no check makes a request.

use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sb_cli_core::cancel::Shutdown;
use sb_config::http::{Http, Proxy};
use sb_config::run::{
    FILES, File, FileError, Outcome, Results, Settings, check_file, render_json, run_recording,
};
use sb_term_ui::{Options, Runner, Target};

/// Known secrets, one per kind of providers.yml credential.
const TOKEN: &str = "duck-not-a-real-secret-1";
const KEY: &str = "pork-not-a-real-secret-2";
const PASSWORD: &str = "cloudns-not-a-real-secret-3";

/// The valid fixture set and the pinned schemas in a new root, with an
/// accounts.yml without credentials, and no motd.yml.
fn saltbox_root() -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("root");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for entry in std::fs::read_dir(fixtures.join("valid")).expect("fixtures") {
        let path = entry.expect("entry").path();
        let name = path.file_name().expect("name");
        if name != "motd.yml" {
            std::fs::copy(&path, root.path().join(name)).expect("copy");
        }
    }
    let schema = root.path().join("schema");
    std::fs::create_dir(&schema).expect("schema dir");
    for entry in std::fs::read_dir(fixtures.join("saltbox-schemas")).expect("schemas") {
        let path = entry.expect("entry").path();
        if path.extension().is_some_and(|e| e == "yml") {
            std::fs::copy(&path, schema.join(path.file_name().expect("name"))).expect("copy");
        }
    }
    std::fs::write(
        root.path().join("accounts.yml"),
        "user:\n  name: seed\n  email: seed@example.com\n  domain: example.com\n  \
         pass: correct-horse-battery\n",
    )
    .expect("accounts");
    root
}

fn settings(root: &Path) -> Settings {
    Settings {
        root: root.to_path_buf(),
        zoneinfo: Vec::new(),
        http: Http::new(&Proxy::Off, Duration::from_secs(5)).expect("client"),
        // Never asked: accounts.yml has no credentials.
        cloudflare_url: "http://127.0.0.1:9/".to_string(),
        dockerhub_url: "http://127.0.0.1:9/".to_string(),
        backoff: |_| Duration::ZERO,
    }
}

fn entry(path: &str) -> File {
    *FILES
        .iter()
        .find(|file| file.path == path)
        .expect("a listed file")
}

/// A validate-config run over `root`: whether it passed, the screen, and
/// what it found.
async fn run_on(root: &Path) -> (bool, String, Results) {
    let term = indicatif::InMemoryTerm::new(80, 160);
    let options = Options {
        target: Target::InMemory {
            term: term.clone(),
            colour: false,
        },
    };
    let shutdown = Shutdown::new();
    let mut runner = Runner::start("sb validate-config", &shutdown, None, options);
    let (result, results) =
        run_recording(&mut runner, &settings(root), &root.join("backups")).await;
    runner.finish();
    (result.is_ok(), term.contents(), results)
}

/// Whether `shown`, a screen or a `--json` document, has the issue at
/// `path` with `message`.
fn has_issue(shown: &str, path: &str, message: &str) -> bool {
    shown.contains(&format!("{path}: {message}"))
        || shown.contains(&format!(
            r#""path":"{path}","severity":"error","message":"{message}""#
        ))
}

fn providers_result(results: &Results) -> &sb_config::run::FileResult {
    results
        .files
        .iter()
        .find(|file| file.file == "providers.yml")
        .expect("providers.yml checked")
}

/// The issues `check_file` gives providers.yml holding `text`.
fn issues_of(text: &str) -> Vec<String> {
    let root = saltbox_root();
    std::fs::write(root.path().join("providers.yml"), text).expect("providers");
    let checked = check_file(&settings(root.path()), entry("providers.yml")).expect("checked");
    checked
        .report
        .issues
        .iter()
        .map(ToString::to_string)
        .collect()
}

/// providers.yml is one of validate-config's files, required, after
/// `hetzner_vlan.yml`, with its schema and Saltbox's default.
#[test]
fn providers_is_checked_after_hetzner_vlan() {
    let order: Vec<&str> = FILES.iter().map(|file| file.path).collect();
    assert_eq!(
        order,
        [
            "accounts.yml",
            "adv_settings.yml",
            "backup_config.yml",
            "hetzner_vlan.yml",
            "providers.yml",
            "settings.yml",
            "motd.yml",
            "inventories/host_vars/localhost.yml",
        ]
    );
    let providers = entry("providers.yml");
    assert_eq!(providers.schema, Some("providers.schema.yml"));
    assert!(providers.required && providers.schema_optional && providers.capped);
    assert_eq!(providers.default, Some("defaults/providers.yml.default"));
    let vlan = entry("hetzner_vlan.yml");
    assert_eq!(vlan.default, Some("defaults/hetzner_vlan.yml.default"));
    assert!(!vlan.schema_optional);
    // Only these two have a default copied in, and only providers.yml's
    // schema may be missing.
    for file in FILES {
        if !matches!(file.path, "providers.yml" | "hetzner_vlan.yml") {
            assert_eq!(file.default, None, "{}", file.path);
            assert!(!file.schema_optional, "{}", file.path);
        }
    }
}

/// The valid set's providers.yml, and Saltbox's default as it ships,
/// pass the draft schema.
#[tokio::test]
async fn a_valid_providers_yml_passes() {
    let root = saltbox_root();
    let (passed, screen, results) = run_on(root.path()).await;
    assert!(passed, "{screen}");
    assert!(screen.contains("✓ Validated providers.yml"), "{screen}");
    let providers = providers_result(&results);
    assert_eq!(providers.outcome, Outcome::Validated);
    assert!(providers.notices.is_empty(), "{:?}", providers.notices);
    let default = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/saltbox-defaults/providers.yml.default"),
    )
    .expect("default");
    assert_eq!(issues_of(&default), Vec::<String>::new());
}

/// A `duck:` section, Duck DNS's name before Saltbox renamed it to
/// `duckdns` (2022, with no migration), is accepted with any keys, so a
/// long-lived host doesn't fail on a section Saltbox no longer reads.
#[test]
fn a_legacy_duck_section_passes() {
    assert_eq!(
        issues_of("duck:\n  token: x\n  anything: y\nduckdns:\n  token: z\n"),
        Vec::<String>::new()
    );
}

/// Each kind of violation the draft schema catches is an issue at its
/// key, worded as validate-config words it, sorted by position.
#[test]
fn each_kind_of_violation_is_an_issue() {
    let text = "\
duckdns:
  token: x
  tokn: y
cloudflare:
  api: z
cloudns:
  email: not-an-email
powerdns:
  api_url: not a url
namedotcom:
  server: not_a_host!
netcup:
  customer_number: abc
porkbun:
  - api_key: x
";
    assert_eq!(
        issues_of(text),
        [
            "providers.yml:3:3: duckdns.tokn: unknown field",
            "providers.yml:4:1: cloudflare: unknown field",
            "providers.yml:7:10: cloudns.email: must be a valid email address",
            "providers.yml:9:12: powerdns.api_url: must be a valid URL",
            "providers.yml:11:11: namedotcom.server: must be a valid hostname",
            "providers.yml:13:20: netcup.customer_number: must be a whole number (integer), got: abc",
            "providers.yml:15:3: porkbun: must be an object, got a list",
        ]
    );
}

/// While Saltbox has no providers schema, the file gets the duplicate-key
/// check alone and a notice: the run passes, `--json` carries the notice.
#[tokio::test]
async fn a_missing_providers_schema_is_a_notice() {
    let root = saltbox_root();
    std::fs::remove_file(root.path().join("schema/providers.schema.yml")).expect("remove");
    // Off the schema's rules: no unknown-field issue without it.
    std::fs::write(root.path().join("providers.yml"), "anything:\n  goes: 1\n").expect("write");
    let (passed, screen, results) = run_on(root.path()).await;
    let notice = "schema/providers.schema.yml not found; checked for duplicate keys only";
    assert!(passed, "{screen}");
    assert!(screen.contains(&format!("› {notice}")), "{screen}");
    let providers = providers_result(&results);
    assert_eq!(providers.outcome, Outcome::Validated);
    assert_eq!(providers.notices, [notice]);
    let json = render_json(&results);
    assert!(
        json.contains(&format!(
            r#"{{"file":"providers.yml","outcome":"validated","issues":[],"notices":["{notice}"]}}"#
        )),
        "{json}"
    );
    // A duplicate key is still the file's issue.
    std::fs::write(root.path().join("providers.yml"), "a: 1\na: 2\n").expect("write");
    let (passed, screen, _) = run_on(root.path()).await;
    assert!(!passed, "{screen}");
    assert!(
        screen.contains("providers.yml:2:1: a: duplicate key (also at line 1, column 1)"),
        "{screen}"
    );
}

/// Another file's missing schema stays its issue.
#[test]
fn only_providers_schema_may_be_missing() {
    let root = saltbox_root();
    std::fs::remove_file(root.path().join("schema/hetzner_vlan.schema.yml")).expect("remove");
    let err = check_file(&settings(root.path()), entry("hetzner_vlan.yml")).expect_err("an error");
    assert!(
        matches!(&err, FileError::Schema(text) if text.starts_with("schema file not found: ")),
        "{err:?}"
    );
}

/// A schema that is there but broken is an issue for providers.yml too.
#[test]
fn a_broken_providers_schema_is_an_issue() {
    let root = saltbox_root();
    std::fs::write(
        root.path().join("schema/providers.schema.yml"),
        "duckdns:\n  type: nonsense\n",
    )
    .expect("write");
    let err = check_file(&settings(root.path()), entry("providers.yml")).expect_err("an error");
    assert!(matches!(err, FileError::Schema(_)), "{err:?}");
}

fn default_path(root: &Path, file: &str) -> PathBuf {
    root.join("defaults").join(format!("{file}.default"))
}

fn write_default(root: &Path, file: &str, text: &str) -> PathBuf {
    let path = default_path(root, file);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("defaults");
    std::fs::write(&path, text).expect("default");
    path
}

/// A missing `hetzner_vlan.yml` or `providers.yml` is copied from Saltbox's
/// default, 0600 whatever the umask lets through, with the default's
/// owner, then checked; the notice is on the screen and in `--json`.
#[tokio::test]
async fn a_missing_file_with_a_default_is_copied_then_checked() {
    let root = saltbox_root();
    let vlan = "hetzner_vlan:\n  client_number: 1\n  vlan_id: 4000\n";
    let providers = "duckdns:\n  token:\n";
    for (file, text) in [("hetzner_vlan.yml", vlan), ("providers.yml", providers)] {
        std::fs::remove_file(root.path().join(file)).expect("remove");
        let default = write_default(root.path(), file, text);
        std::fs::set_permissions(&default, std::fs::Permissions::from_mode(0o644)).expect("mode");
    }
    let (passed, screen, results) = run_on(root.path()).await;
    assert!(passed, "{screen}");
    for (file, text) in [("hetzner_vlan.yml", vlan), ("providers.yml", providers)] {
        let notice = format!("{file} was missing; copied Saltbox's default");
        assert!(screen.contains(&format!("› {notice}")), "{screen}");
        let path = root.path().join(file);
        assert_eq!(std::fs::read_to_string(&path).expect("copied"), text);
        let meta = std::fs::symlink_metadata(&path).expect("meta");
        assert!(meta.file_type().is_file());
        assert_eq!(meta.mode() & 0o7777, 0o600, "{file}");
        let default = std::fs::metadata(default_path(root.path(), file)).expect("default");
        assert_eq!((meta.uid(), meta.gid()), (default.uid(), default.gid()));
        let result = results
            .files
            .iter()
            .find(|result| result.file == file)
            .expect("checked");
        assert_eq!(result.outcome, Outcome::Validated, "{file}");
        assert_eq!(result.notices, std::slice::from_ref(&notice));
        assert!(
            render_json(&results).contains(&format!(r#""notices":["{notice}"]"#)),
            "{file}"
        );
    }
    // No temporary file is left beside them.
    let names: Vec<String> = std::fs::read_dir(root.path())
        .expect("root")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.starts_with('.'))
        .collect();
    assert_eq!(names, Vec::<String>::new());
    // A second run finds the files and copies nothing.
    let (passed, screen, _) = run_on(root.path()).await;
    assert!(passed && !screen.contains("was missing"), "{screen}");
}

/// The copy keeps the default's owner and group: as root, a default
/// owned by another user gives a copy owned by that user.
#[tokio::test]
async fn the_copy_has_the_defaults_owner() {
    if !rustix::process::geteuid().is_root() {
        return;
    }
    let root = saltbox_root();
    std::fs::remove_file(root.path().join("providers.yml")).expect("remove");
    let default = write_default(root.path(), "providers.yml", "duckdns:\n  token:\n");
    std::os::unix::fs::chown(&default, Some(4321), Some(4322)).expect("chown");
    let (passed, screen, _) = run_on(root.path()).await;
    assert!(passed, "{screen}");
    let meta = std::fs::symlink_metadata(root.path().join("providers.yml")).expect("meta");
    assert_eq!(
        (meta.uid(), meta.gid(), meta.mode() & 0o7777),
        (4321, 4322, 0o600)
    );
}

/// Without a default, a missing file keeps today's issue, with no notice.
#[tokio::test]
async fn a_missing_file_without_a_default_is_its_issue() {
    let root = saltbox_root();
    std::fs::remove_file(root.path().join("providers.yml")).expect("remove");
    let (passed, screen, results) = run_on(root.path()).await;
    let path = root.path().join("providers.yml");
    assert!(!passed, "{screen}");
    assert!(
        screen.contains(&format!(
            "required config file not found: {}",
            path.display()
        )),
        "{screen}"
    );
    assert!(!screen.contains("copied"), "{screen}");
    let providers = providers_result(&results);
    assert_eq!(providers.outcome, Outcome::Failed);
    assert_eq!(providers.notices, Vec::<String>::new());
    assert!(!path.exists());
}

/// A default that can't be read (a directory) leaves the issue, after a
/// warning naming why, and no copy.
#[tokio::test]
async fn an_unreadable_default_is_a_warning_and_the_issue() {
    let root = saltbox_root();
    std::fs::remove_file(root.path().join("providers.yml")).expect("remove");
    std::fs::create_dir_all(default_path(root.path(), "providers.yml")).expect("dir");
    let (passed, screen, results) = run_on(root.path()).await;
    assert!(!passed, "{screen}");
    assert!(
        screen.contains("! providers.yml not copied from Saltbox's default: "),
        "{screen}"
    );
    assert!(
        screen.contains("required config file not found: "),
        "{screen}"
    );
    assert!(!root.path().join("providers.yml").exists());
    // `--json` has the warning too, beside the error.
    let json = render_json(&results);
    assert!(
        json.contains(
            r#""severity":"warning","message":"providers.yml not copied from Saltbox's default: "#
        ),
        "{json}"
    );
}

/// A Saltbox root reached through a link isn't written in: the warning
/// says why, and the missing file stays its issue.
#[tokio::test]
async fn a_root_through_a_link_is_a_warning() {
    let real = saltbox_root();
    std::fs::remove_file(real.path().join("providers.yml")).expect("remove");
    write_default(real.path(), "providers.yml", "duckdns:\n  token:\n");
    let links = tempfile::tempdir().expect("links");
    let linked = links.path().join("saltbox");
    std::os::unix::fs::symlink(real.path(), &linked).expect("link");
    let (passed, screen, _) = run_on(&linked).await;
    assert!(!passed, "{screen}");
    assert!(
        screen.contains(
            "providers.yml not copied from Saltbox's default: the Saltbox root can't be opened without following a link"
        ),
        "{screen}"
    );
    assert!(!real.path().join("providers.yml").exists());
}

/// A link in the way, even a broken one, is never followed or replaced:
/// nothing is copied, its target isn't made, and the file's check
/// reports it.
#[tokio::test]
async fn a_link_in_the_way_is_left_alone() {
    let root = saltbox_root();
    let path = root.path().join("providers.yml");
    std::fs::remove_file(&path).expect("remove");
    write_default(root.path(), "providers.yml", "duckdns:\n  token:\n");
    let target = root.path().join("elsewhere.yml");
    std::os::unix::fs::symlink(&target, &path).expect("link");
    let (passed, screen, _) = run_on(root.path()).await;
    assert!(!passed, "{screen}");
    assert!(!screen.contains("copied Saltbox's default"), "{screen}");
    assert!(!target.exists(), "the link's target was made");
    assert!(
        std::fs::symlink_metadata(&path)
            .expect("link")
            .file_type()
            .is_symlink()
    );
}

/// providers.yml's credentials never show: an issue on one names its key
/// (a tag error's value redacted), on the screen and in `--json`, with
/// the schema and without it.
#[tokio::test]
async fn providers_credentials_never_show() {
    let root = saltbox_root();
    // A tag error stops the file's rules (config spec, Duplicate keys),
    // so it has a run of its own.
    let tagged = format!("duckdns:\n  token: !!int {TOKEN}\n");
    std::fs::write(root.path().join("providers.yml"), &tagged).expect("write");
    let (passed, screen, results) = run_on(root.path()).await;
    assert!(!passed, "{screen}");
    let json = render_json(&results);
    for shown in [&screen, &json] {
        assert!(
            has_issue(
                shown,
                "duckdns.token",
                "cannot decode !!str `[REDACTED]` as a !!int"
            ),
            "{shown}"
        );
        assert!(!shown.contains(TOKEN), "{shown}");
    }
    let text = format!(
        "duckdns:\n  token: {TOKEN}\nporkbun:\n  api_key: {KEY}\n  secret_kye: {KEY}\n\
         cloudns:\n  password: {PASSWORD}\n  email: {PASSWORD}\n"
    );
    std::fs::write(root.path().join("providers.yml"), &text).expect("write");
    let (passed, screen, results) = run_on(root.path()).await;
    assert!(!passed, "{screen}");
    let json = render_json(&results);
    for shown in [&screen, &json] {
        assert!(
            has_issue(shown, "porkbun.secret_kye", "unknown field"),
            "{shown}"
        );
        assert!(
            has_issue(shown, "cloudns.email", "must be a valid email address"),
            "{shown}"
        );
        for secret in [TOKEN, KEY, PASSWORD] {
            assert!(!shown.contains(secret), "{secret}\n{shown}");
        }
    }
    // Without the schema: duplicates alone, and a duplicated credential
    // names its key only.
    std::fs::remove_file(root.path().join("schema/providers.schema.yml")).expect("remove");
    let duplicated = format!("duckdns:\n  token: {TOKEN}\n  token: {TOKEN}\n");
    std::fs::write(root.path().join("providers.yml"), duplicated).expect("write");
    let (passed, screen, results) = run_on(root.path()).await;
    assert!(!passed, "{screen}");
    let json = render_json(&results);
    for shown in [&screen, &json] {
        assert!(
            has_issue(
                shown,
                "duckdns.token",
                "duplicate key (also at line 2, column 3)"
            ),
            "{shown}"
        );
        assert!(!shown.contains(TOKEN), "{shown}");
    }
}
