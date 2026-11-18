//! The validation run (config spec, "Behaviour"): every Saltbox config
//! file in sb-go's order, checked against its schema, then the credential
//! checks, drawn as term-ui steps. sb's command builds the runner and the
//! settings and calls `run`; nothing is decided twice.

use std::path::{Path, PathBuf};
use std::time::Duration;

use sb_cli_core::config::{read_inventory, read_small, yaml};
use sb_cli_core::exit::Exit;
use sb_term_ui::{Labels, Runner, Step};

use crate::cloudflare::{self, Check};
use crate::dockerhub;
use crate::http::Http;
use crate::issue::{Issue, escape_controls};
use crate::parse;
use crate::rules::{self, Login, Report, Request};
use crate::schema::{self, SchemaError, Validator};
use crate::tree::{self, Tree};
use crate::validators::Saltbox;

/// One config file: its path under the Saltbox root, its schema under
/// `schema/` (none: duplicate keys only), whether it must exist, and
/// whether it is read up to cli-core's 1 MiB cap (`read_small`) or at any
/// length (`read_inventory`: the inventory, hardening plan decision 41).
#[derive(Clone, Copy, Debug)]
pub struct File {
    pub path: &'static str,
    pub schema: Option<&'static str>,
    pub required: bool,
    pub capped: bool,
}

/// providers.yml (Saltbox's DNS providers' credentials, a `vars_files`
/// entry of saltbox.yml): `sb edit providers` checks it, validate-config
/// doesn't (sb-go doesn't), so it is outside [`FILES`]. Saltbox has no
/// schema for it, so it gets localhost.yml's check: the pre-pass, the
/// parse and duplicate keys (config spec, "One file"; tui spec,
/// Difference 38).
pub const PROVIDERS: File = File {
    path: "providers.yml",
    schema: None,
    required: false,
    capped: true,
};

/// The files, in sb-go's order.
pub const FILES: [File; 7] = [
    File {
        path: "accounts.yml",
        schema: Some("accounts.schema.yml"),
        required: true,
        capped: true,
    },
    File {
        path: "adv_settings.yml",
        schema: Some("adv_settings.schema.yml"),
        required: true,
        capped: true,
    },
    File {
        path: "backup_config.yml",
        schema: Some("backup_config.schema.yml"),
        required: true,
        capped: true,
    },
    File {
        path: "hetzner_vlan.yml",
        schema: Some("hetzner_vlan.schema.yml"),
        required: true,
        capped: true,
    },
    File {
        path: "settings.yml",
        schema: Some("settings.schema.yml"),
        required: true,
        capped: true,
    },
    File {
        path: "motd.yml",
        schema: Some("motd.schema.yml"),
        required: false,
        capped: true,
    },
    File {
        path: "inventories/host_vars/localhost.yml",
        schema: None,
        required: false,
        capped: false,
    },
];

/// What the run needs from its caller.
#[derive(Clone)]
pub struct Settings {
    /// The Saltbox root (`SB_SALTBOX_DIR` or `/srv/git/saltbox`).
    pub root: PathBuf,
    /// The zoneinfo places for `timezone` (`validators::Saltbox::zoneinfo_places`).
    pub zoneinfo: Vec<PathBuf>,
    pub http: Http,
    /// Cloudflare's API base (`CLOUDFLARE_BASE_URL` or `cloudflare::BASE_URL`).
    pub cloudflare_url: String,
    /// Docker Hub's login URL (`SB_DOCKERHUB_URL` or `dockerhub::LOGIN_URL`).
    pub dockerhub_url: String,
    /// The Cloudflare retries' backoff (`cloudflare::backoff`).
    pub backoff: fn(u32) -> Duration,
}

/// The URLs as the log has them (`http::loggable_url`, any credentials
/// out); the client is left out (reqwest's lists its proxies, which can
/// carry credentials), as is the backoff.
impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settings")
            .field("root", &self.root)
            .field("zoneinfo", &self.zoneinfo)
            .field(
                "cloudflare_url",
                &crate::http::loggable_url(&self.cloudflare_url),
            )
            .field(
                "dockerhub_url",
                &crate::http::loggable_url(&self.dockerhub_url),
            )
            .finish_non_exhaustive()
    }
}

/// Whether [`validate_file`] runs accounts.yml's credential checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checks {
    /// As concurrent sibling steps (validate-config, `sb update`).
    Run,
    /// None: no request is made, and a file with credentials set says so
    /// in a notice (`sb edit`).
    Skip,
}

/// The notice [`Checks::Skip`] adds under a file with credentials set.
pub const CHECKS_SKIPPED: &str = "credential checks skipped; sb validate-config runs them";

/// One file, checked ([`check_file`]).
pub struct Checked {
    /// The issues, sorted, the warnings, and the credential checks the
    /// file asks for: empty but for the issues when the file couldn't be
    /// parsed, and empty for a file with no schema and no duplicates.
    pub report: Report,
    /// The parsed tree, for a schema-backed file that parsed: a caller
    /// that needs the values reads the file once.
    pub tree: Option<Tree>,
}

impl Checked {
    /// The file's issue count.
    #[must_use]
    pub fn issue_count(&self) -> usize {
        self.report.issues.len()
    }
}

/// The counts only: the report's requests hold credentials, and the tree
/// values.
impl std::fmt::Debug for Checked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Checked")
            .field("issues", &self.report.issues.len())
            .field("warnings", &self.report.warnings.len())
            .field("checks", &self.report.requests.len())
            .field("tree", &self.tree.is_some())
            .finish_non_exhaustive()
    }
}

/// Why a file wasn't checked, worded as validate-config words it.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FileError {
    /// An optional file that isn't there (validate-config's info notice).
    #[error("{0} not found; skipped")]
    Skipped(&'static str),
    /// A required file that isn't there: its path.
    #[error("required config file not found: {0}")]
    Missing(String),
    /// The file couldn't be read (the cap included), or the YAML reader's
    /// deep-stack thread couldn't start.
    #[error("{0}")]
    Read(String),
    /// Its schema couldn't be read or loaded.
    #[error("{0}")]
    Schema(String),
}

/// A file's issues, the error [`validate_file`] fails its step with: one
/// per line, the failure block.
#[derive(Debug, thiserror::Error)]
#[error("{block}")]
pub struct Issues {
    count: usize,
    block: String,
}

impl Issues {
    fn new(issues: &[Issue]) -> Self {
        let lines: Vec<String> = issues.iter().map(ToString::to_string).collect();
        Self {
            count: issues.len(),
            block: lines.join("\n"),
        }
    }

    /// How many issues the file has, for the caller's log.
    #[must_use]
    pub fn count(&self) -> usize {
        self.count
    }
}

/// Validate every file under `runner`, motd.yml migrated first from its
/// older format with its backup in `backups` ([`validate_file_migrating`]):
/// `Exit(1)` when any failed (each failed file and check has shown its
/// block).
///
/// # Errors
///
/// `Exit(1)` when a file or credential check failed, or the run was
/// interrupted or stopped.
pub async fn run(runner: &mut Runner, settings: &Settings, backups: &Path) -> Result<(), Exit> {
    let labels = Labels::new(
        "Validating Saltbox configuration",
        "Saltbox configuration validated",
    );
    runner
        .step(labels, async |step| {
            let mut failed = false;
            for file in FILES {
                let result = step
                    .step(file_labels(file), async |step| {
                        validate_file_migrating(step, settings, file, Checks::Run, backups).await
                    })
                    .await;
                failed |= result.is_err();
            }
            if failed { Err(Exit(1).into()) } else { Ok(()) }
        })
        .await
}

/// A file's step labels, as validate-config shows them: "Validating
/// <file>" → "Validated <file>", failing as "Validation of <file>".
#[must_use]
pub fn file_labels(file: File) -> Labels {
    let shown = shown(file);
    Labels::new(format!("Validating {shown}"), format!("Validated {shown}"))
        .failure(format!("Validation of {shown}"))
}

/// A file as its step names it.
fn shown(file: File) -> String {
    if file.schema.is_some() {
        file.path.to_string()
    } else {
        // sb-go's name, the file alone: the whole path doesn't fit
        // term-ui's label column. Issues and notices keep the path.
        let name = file.path.rsplit('/').next().unwrap_or(file.path);
        format!("{name} (no duplicates)")
    }
}

/// `file` read and checked as validate-config checks it, with no network
/// request: cli-core's read (capped, or whole for the inventory), its
/// schema loaded strictly, the YAML pre-pass, the parse, duplicate keys,
/// the schema rules and the warnings. The issue count reaches the run log
/// ("config file checked"); a [`FileError`] doesn't, so a caller other
/// than [`validate_file`] (whose step records it) logs it itself.
///
/// # Errors
///
/// [`FileError`]: the file isn't there, or it or its schema can't be read
/// or loaded.
pub fn check_file(settings: &Settings, file: File) -> Result<Checked, FileError> {
    let path = settings.root.join(file.path);
    let read = if file.capped {
        read_small
    } else {
        read_inventory
    };
    let text = match read(&path) {
        Ok(Some(text)) => text,
        Ok(None) if file.required => {
            return Err(FileError::Missing(escape_controls(
                &path.display().to_string(),
            )));
        }
        Ok(None) => return Err(FileError::Skipped(file.path)),
        Err(err) => {
            return Err(FileError::Read(format!(
                "{}: {err}",
                escape_controls(&path.display().to_string())
            )));
        }
    };
    let rules = match file.schema {
        Some(schema) => {
            let path = settings.root.join("schema").join(schema);
            let shown = escape_controls(&path.display().to_string());
            // Flattened: a read error's text already carries its causes.
            Some(schema::load(&path, &shown).map_err(|err| match err {
                SchemaError::Missing { .. } => {
                    FileError::Schema(format!("schema file not found: {shown}"))
                }
                err => FileError::Schema(err.to_string()),
            })?)
        }
        None => None,
    };
    let checked = parse_and_check(settings, file, &text, rules.as_ref())?;
    tracing::info!(
        file = file.path,
        issues = checked.report.issues.len(),
        warnings = checked.report.warnings.len(),
        checks = checked.report.requests.len(),
        "config file checked"
    );
    Ok(checked)
}

/// `text`, `file`'s, parsed and checked against `rules` (none: duplicate
/// keys only). Its tree is loaded and checked where its depth fits, and
/// handed back as a [`Tree`], which any stack can drop.
pub(crate) fn parse_and_check(
    settings: &Settings,
    file: File,
    text: &str,
    rules: Option<&schema::Rules>,
) -> Result<Checked, FileError> {
    let only = |issues: Vec<Issue>| Checked {
        report: Report {
            issues,
            ..Report::default()
        },
        tree: None,
    };
    let prepared = match parse::prepare(file.path, text) {
        Ok(prepared) => prepared,
        Err(issues) => return Ok(only(issues)),
    };
    let validators = Saltbox::with_zoneinfo(settings.zoneinfo.clone());
    let checked = yaml::with_stack(prepared.depth(), || {
        let root = parse::load(file.path, &prepared)?;
        let Some(rules) = rules else {
            return Ok(None);
        };
        let tree = tree::build(file.path, &root, &rules::secret_paths(rules))?;
        let report = rules::validate(file.path, &tree, rules, &validators);
        Ok(Some((report, Tree::new(tree))))
    })
    .map_err(|err| FileError::Read(format!("can't start the YAML reader's thread: {err}")))?;
    Ok(match checked {
        Ok(Some((report, tree))) => Checked {
            report,
            tree: Some(tree),
        },
        Ok(None) => only(Vec::new()),
        Err(issues) => only(issues),
    })
}

/// `file` checked ([`check_file`]) and drawn under `step`, the file's own
/// step (opened with [`file_labels`]), exactly as validate-config draws it:
/// warnings and skipped checks as notices, the credential checks as
/// concurrent sibling steps with [`Checks::Run`], none with
/// [`Checks::Skip`]. Each file's outcome, issues and timing reach the run
/// log through term-ui's step records and `check_file`'s.
///
/// # Errors
///
/// The step's failure: [`Issues`] (its count for the caller's log, read
/// by `downcast_ref` inside the step's body, so it is returned as the
/// outermost error, never wrapped in context), a
/// [`FileError`] but [`FileError::Skipped`] (a notice), or `Exit(1)` after
/// a failed credential check's own block.
pub async fn validate_file(
    step: &mut Step,
    settings: &Settings,
    file: File,
    checks: Checks,
) -> anyhow::Result<()> {
    let checked = match check_file(settings, file) {
        Ok(checked) => checked,
        Err(skipped @ FileError::Skipped(_)) => {
            step.info(skipped.to_string());
            return Ok(());
        }
        Err(err) => return Err(err.into()),
    };
    let report = &checked.report;
    for warning in &report.warnings {
        step.warn(warning.as_str());
    }
    for validator in &report.skipped {
        if let Some(name) = check_name(*validator) {
            step.info(format!("no {name} credentials set; skipped"));
        }
    }
    let checks_failed = match checks {
        Checks::Run => run_checks(step, settings, &report.requests).await,
        Checks::Skip => {
            if !report.requests.is_empty() {
                step.info(CHECKS_SKIPPED);
            }
            false
        }
    };
    if !report.issues.is_empty() {
        return Err(Issues::new(&report.issues).into());
    }
    if checks_failed {
        return Err(Exit(1).into());
    }
    Ok(())
}

/// [`validate_file`], with motd.yml first migrated from its older format
/// when it is in it and both its checks pass (`migrate::motd`), the
/// original copied into `backups`: validate-config's and `sb update`'s
/// way (motd spec, decision 4). A migration shows one notice under the
/// file's step; a failed write a warning, the file then checked as it
/// is.
///
/// # Errors
///
/// As [`validate_file`].
pub async fn validate_file_migrating(
    step: &mut Step,
    settings: &Settings,
    file: File,
    checks: Checks,
    backups: &Path,
) -> anyhow::Result<()> {
    if file.path == crate::migrate::MOTD.path {
        match crate::migrate::motd(settings, backups) {
            Ok(Some(backup)) => step.info(format!(
                "Migrated motd.yml to the current format (backup: {})",
                escape_controls(&backup.display().to_string())
            )),
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(error = %err, "motd.yml not migrated");
                step.warn(format!("motd.yml not migrated: {err}"));
            }
        }
    }
    validate_file(step, settings, file, checks).await
}

/// A check under `step`, ended early when the step is cancelled (Ctrl-C,
/// the command's request), as term-ui asks of a body
/// (<https://docs.rs/tokio-util/0.7.20/tokio_util/sync/struct.CancellationToken.html#method.run_until_cancelled>).
async fn stoppable(
    step: &Step,
    check: impl Future<Output = Result<(), String>>,
) -> anyhow::Result<()> {
    match step.token().run_until_cancelled(check).await {
        Some(result) => result.map_err(anyhow::Error::msg),
        None => anyhow::bail!("stopped before the service answered"),
    }
}

/// The service a credential validator checks, as notices name it.
fn check_name(validator: Validator) -> Option<&'static str> {
    match validator {
        Validator::ValidateCloudflareConfig => Some("Cloudflare"),
        Validator::ValidateDockerhubConfig => Some("Docker Hub"),
        _ => None,
    }
}

/// The credential checks `requests` asks for, as concurrent steps under
/// `step` that all run to their end (decision 3): whether any failed.
async fn run_checks(step: &mut Step, settings: &Settings, requests: &[Request]) -> bool {
    if requests.is_empty() {
        return false;
    }
    let mut group = step.group_all::<()>();
    for request in requests {
        let http = settings.http.clone();
        // The login the rules accepted, carried by the request: nothing to
        // parse again, so no ready section can go unchecked.
        match request.login.clone() {
            Login::Cloudflare {
                credentials,
                domain,
            } => {
                let (base_url, backoff) = (settings.cloudflare_url.clone(), settings.backoff);
                let labels = Labels::new(
                    "Validating Cloudflare API credentials",
                    "Cloudflare API credentials validated",
                )
                .failure("Cloudflare API credentials validation");
                group.spawn(labels, move |step| async move {
                    let check = Check {
                        http: &http,
                        base_url: &base_url,
                        backoff: &backoff,
                    };
                    stoppable(&step, cloudflare::check(&check, &credentials, &domain)).await
                });
            }
            Login::Dockerhub(login) => {
                let url = settings.dockerhub_url.clone();
                let labels = Labels::new(
                    "Validating Docker Hub credentials",
                    "Docker Hub credentials validated",
                )
                .failure("Docker Hub credentials validation");
                group.spawn(labels, move |step| async move {
                    stoppable(&step, dockerhub::login(&http, &url, &login)).await
                });
            }
        }
    }
    group.join().await.iter().any(Result::is_err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::Proxy;
    use crate::testing::{Answer, Fake, saltbox_root};
    use sb_cli_core::cancel::Shutdown;
    use sb_term_ui::{Options, Target};

    const ZONES: &str = r#"{"success":true,"result":[{"id":"z1","name":"example.com"}]}"#;
    const SSL_FULL: &str = r#"{"success":true,"result":{"id":"ssl","value":"full"}}"#;

    /// The fake answering both checks well.
    fn healthy() -> Fake {
        let fake = Fake::start().expect("fake");
        fake.script("/client/v4/zones", Answer::json(200, ZONES));
        fake.script(
            "/client/v4/zones/z1/settings/ssl",
            Answer::json(200, SSL_FULL),
        );
        fake.script("/v2/users/login/", Answer::json(200, "{}"));
        fake
    }

    fn no_wait(_: u32) -> Duration {
        Duration::ZERO
    }

    /// The run over `root` against `fake`: its result and the screen.
    async fn run_on(root: &std::path::Path, fake: &Fake) -> (Result<(), Exit>, String) {
        run_until(root, fake, &Shutdown::new()).await
    }

    /// `run_on`, with the run's `shutdown`.
    async fn run_until(
        root: &std::path::Path,
        fake: &Fake,
        shutdown: &Shutdown,
    ) -> (Result<(), Exit>, String) {
        let http = Http::new(&Proxy::Off, Duration::from_secs(5)).expect("client");
        run_with(root, fake, shutdown, http).await
    }

    /// `run_until`, with this client (a clone shares what it builds).
    async fn run_with(
        root: &std::path::Path,
        fake: &Fake,
        shutdown: &Shutdown,
        http: Http,
    ) -> (Result<(), Exit>, String) {
        let term = indicatif::InMemoryTerm::new(80, 140);
        let options = Options {
            target: Target::InMemory {
                term: term.clone(),
                colour: false,
            },
        };
        let mut runner = Runner::start("sb validate-config", shutdown, None, options);
        let settings = Settings {
            root: root.to_path_buf(),
            zoneinfo: Vec::new(),
            http,
            cloudflare_url: format!("{}/client/v4/", fake.url()),
            dockerhub_url: format!("{}/v2/users/login/", fake.url()),
            backoff: no_wait,
        };
        let result = run(&mut runner, &settings, &root.join("backups")).await;
        runner.finish();
        (result, term.contents())
    }

    /// `Settings`' `Debug` shows the service URLs as the log does, with
    /// any credentials in them taken out (`http::loggable_url`).
    #[test]
    fn settings_debug_keeps_url_credentials_out() {
        let settings = Settings {
            root: "/srv/git/saltbox".into(),
            zoneinfo: Vec::new(),
            http: Http::new(&Proxy::Off, Duration::from_secs(5)).expect("client"),
            cloudflare_url: "https://u:cf-pass-zz@cf.example.com/client/v4/".to_string(),
            dockerhub_url: "https://u:hub-pass-zz@hub.example.com/v2/users/login/".to_string(),
            backoff: no_wait,
        };
        let shown = format!("{settings:?}");
        for hidden in ["cf-pass-zz", "hub-pass-zz"] {
            assert!(!shown.contains(hidden), "{hidden}: {shown}");
        }
        assert!(shown.contains("cf.example.com"), "{shown}");
    }

    fn write(root: &std::path::Path, file: &str, text: &str) {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(path, text).expect("write");
    }

    #[tokio::test]
    async fn the_valid_set_passes_with_both_checks() {
        let root = saltbox_root("valid").expect("root");
        let fake = healthy();
        let (result, screen) = run_on(root.path(), &fake).await;
        assert!(result.is_ok(), "{screen}");
        for line in [
            "✓ Saltbox configuration validated",
            "✓ Validated accounts.yml",
            "✓ Validated settings.yml",
            "✓ Validated motd.yml",
            "✓ Cloudflare API credentials validated",
            "✓ Docker Hub credentials validated",
            "inventories/host_vars/localhost.yml not found; skipped",
        ] {
            assert!(screen.contains(line), "{line}\n{screen}");
        }
        // The fixture's values, each in its field.
        let seen = fake.seen();
        assert_eq!(seen[0].target, "/client/v4/zones?name=example.com");
        assert_eq!(
            seen[0].header("authorization"),
            Some("Bearer not-a-real-token")
        );
        let login = seen
            .iter()
            .find(|s| s.target == "/v2/users/login/")
            .expect("login");
        let body: serde_json::Value = serde_json::from_str(&login.body).expect("json");
        assert_eq!(
            body,
            serde_json::json!({"username": "seed", "password": "not-a-real-token"})
        );
        assert_eq!(seen.len(), 3);
    }

    /// An old-format motd.yml is migrated before it is checked: one
    /// notice under its step naming the backup, then it validates (motd
    /// decision 4).
    #[tokio::test]
    async fn an_old_motd_yml_is_migrated_then_validated() {
        let root = saltbox_root("valid").expect("root");
        let old = "# my motd\nplex:\n  - name: Plex\n    url: http://plex:32400\n    token: plex-token-zz\n";
        write(root.path(), "motd.yml", old);
        let fake = healthy();
        let (result, screen) = run_on(root.path(), &fake).await;
        assert!(result.is_ok(), "{screen}");
        let notice = format!(
            "› Migrated motd.yml to the current format (backup: {}/backups/motd.yml.",
            root.path().display()
        );
        assert!(screen.contains(&notice), "{screen}");
        assert!(screen.contains("✓ Validated motd.yml"), "{screen}");
        let new = std::fs::read_to_string(root.path().join("motd.yml")).expect("read");
        assert_eq!(
            new,
            "# my motd\nplex:\n  instances:\n    - name: Plex\n      url: http://plex:32400\n      token: plex-token-zz\n"
        );
        // A file that can't be migrated is checked as it is: its issues as
        // today, nothing written, no backup.
        let root = saltbox_root("valid").expect("root");
        let urlless = "plex:\n  - name: Plex\n    token: plex-token-zz\n";
        write(root.path(), "motd.yml", urlless);
        let (result, screen) = run_on(root.path(), &fake).await;
        assert!(result.is_err(), "{screen}");
        assert!(
            screen.contains("plex: must be an object, got a list"),
            "{screen}"
        );
        assert!(!screen.contains("Migrated"), "{screen}");
        assert!(!screen.contains("plex-token-zz"), "{screen}");
        assert_eq!(
            std::fs::read_to_string(root.path().join("motd.yml")).expect("read"),
            urlless
        );
        assert!(!root.path().join("backups").exists());
    }

    /// A migration that can't write its backup is a warning; the file is
    /// then checked as it is.
    #[tokio::test]
    async fn a_failed_migration_is_a_warning() {
        let root = saltbox_root("valid").expect("root");
        let old = "plex:\n  - name: Plex\n    url: http://plex:32400\n    token: plex-token-zz\n";
        write(root.path(), "motd.yml", old);
        // A file where the backups folder belongs.
        write(root.path(), "backups", "");
        let fake = healthy();
        let (result, screen) = run_on(root.path(), &fake).await;
        assert!(result.is_err(), "{screen}");
        assert!(screen.contains("motd.yml not migrated: "), "{screen}");
        assert_eq!(
            std::fs::read_to_string(root.path().join("motd.yml")).expect("read"),
            old
        );
    }

    /// A global key: `/user` first, with the key and email in their
    /// headers.
    #[tokio::test]
    async fn a_global_key_is_sent_as_its_headers() {
        let root = saltbox_root("valid").expect("root");
        let accounts = std::fs::read_to_string(root.path().join("accounts.yml")).expect("read");
        write(
            root.path(),
            "accounts.yml",
            &accounts.replace(
                "  scoped_token: not-a-real-token\n",
                "  api: not-a-real-key\n  email: owner@example.com\n",
            ),
        );
        let fake = healthy();
        fake.script(
            "/client/v4/user",
            Answer::json(200, r#"{"success":true,"result":{}}"#),
        );
        let (result, screen) = run_on(root.path(), &fake).await;
        assert!(result.is_ok(), "{screen}");
        let seen = fake.seen();
        let user = seen
            .iter()
            .find(|s| s.target == "/client/v4/user")
            .expect("user");
        assert_eq!(user.header("x-auth-key"), Some("not-a-real-key"));
        assert_eq!(user.header("x-auth-email"), Some("owner@example.com"));
        assert!(
            seen.iter()
                .any(|s| s.target == "/client/v4/zones?name=example.com")
        );
    }

    #[tokio::test]
    async fn a_missing_required_file_is_its_issue() {
        let root = saltbox_root("valid").expect("root");
        std::fs::remove_file(root.path().join("hetzner_vlan.yml")).expect("remove");
        let (result, screen) = run_on(root.path(), &healthy()).await;
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert!(
            screen.contains("✗ Validation of hetzner_vlan.yml"),
            "{screen}"
        );
        assert!(
            screen.contains("required config file not found:"),
            "{screen}"
        );
        assert!(screen.contains("sub-step failed"), "{screen}");
        // The other files are still validated (decision 2).
        assert!(screen.contains("✓ Validated settings.yml"), "{screen}");
    }

    #[tokio::test]
    async fn a_missing_optional_file_is_skipped() {
        let root = saltbox_root("valid").expect("root");
        std::fs::remove_file(root.path().join("motd.yml")).expect("remove");
        let (result, screen) = run_on(root.path(), &healthy()).await;
        assert!(result.is_ok(), "{screen}");
        assert!(screen.contains("motd.yml not found; skipped"), "{screen}");
    }

    #[tokio::test]
    async fn a_missing_schema_is_the_files_issue() {
        let root = saltbox_root("valid").expect("root");
        std::fs::remove_file(root.path().join("schema/settings.schema.yml")).expect("remove");
        let (result, screen) = run_on(root.path(), &healthy()).await;
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert!(screen.contains("✗ Validation of settings.yml"), "{screen}");
        assert!(screen.contains("schema file not found:"), "{screen}");
    }

    #[tokio::test]
    async fn a_file_over_a_mebibyte_is_its_issue() {
        let root = saltbox_root("valid").expect("root");
        write(
            root.path(),
            "hetzner_vlan.yml",
            &format!("# {}\n", "x".repeat(1 << 20)),
        );
        let (result, screen) = run_on(root.path(), &healthy()).await;
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert!(
            screen.contains("✗ Validation of hetzner_vlan.yml"),
            "{screen}"
        );
        assert!(screen.contains("is bigger than"), "{screen}");
    }

    /// The inventory has no length cap (decision 41): localhost.yml well
    /// over a mebibyte is read and checked whole, its duplicates found at
    /// its end.
    #[tokio::test]
    async fn localhost_has_no_length_cap() {
        use std::fmt::Write as _;
        let root = saltbox_root("valid").expect("root");
        let mut long = String::new();
        for i in 0..200_000 {
            writeln!(long, "key_{i}: value").expect("a String");
        }
        assert!(long.len() > 3 << 20, "{}", long.len());
        write(root.path(), "inventories/host_vars/localhost.yml", &long);
        let (result, screen) = run_on(root.path(), &healthy()).await;
        assert!(result.is_ok(), "{screen}");
        assert!(
            screen.contains("✓ Validated localhost.yml (no duplicates)"),
            "{screen}"
        );
        write(
            root.path(),
            "inventories/host_vars/localhost.yml",
            &format!("{long}key_0: again\n"),
        );
        let (result, screen) = run_on(root.path(), &healthy()).await;
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert!(
            screen.contains("inventories/host_vars/localhost.yml:200001:1: key_0: duplicate key (also at line 1, column 1)"),
            "{screen}"
        );
    }

    /// localhost.yml: duplicate keys only, labelled so, whole (sb-go's
    /// name, which fits the label column).
    #[tokio::test]
    async fn localhost_is_checked_for_duplicates_only() {
        let root = saltbox_root("valid").expect("root");
        write(
            root.path(),
            "inventories/host_vars/localhost.yml",
            "anything: 1\nunknown: [x]\n",
        );
        let (result, screen) = run_on(root.path(), &healthy()).await;
        assert!(result.is_ok(), "{screen}");
        assert!(
            screen.contains("✓ Validated localhost.yml (no duplicates)"),
            "{screen}"
        );
        write(
            root.path(),
            "inventories/host_vars/localhost.yml",
            "a: 1\na: 2\n",
        );
        let (result, screen) = run_on(root.path(), &healthy()).await;
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert!(
            screen.contains("✗ Validation of localhost.yml (no duplicates)"),
            "{screen}"
        );
        assert!(
            screen.contains("inventories/host_vars/localhost.yml:2:1: a: duplicate key (also at line 1, column 1)"),
            "{screen}"
        );
    }

    /// Depth is limited, never length (decision 38): a file nested as deep
    /// as the limit is parsed, built and checked on the deep stack, with
    /// no overflow on this test's small one, and one level more is the
    /// file's issue where it goes too deep.
    #[tokio::test]
    async fn a_deep_file_is_checked_or_refused_without_overflow() {
        let root = saltbox_root("valid").expect("root");
        let deep = |n: usize| format!("x:\n  {}v\n", "- ".repeat(n));
        write(
            root.path(),
            "inventories/host_vars/localhost.yml",
            &deep(yaml::MAX_DEPTH - 1),
        );
        let accounts = std::fs::read_to_string(root.path().join("accounts.yml")).expect("read");
        write(
            root.path(),
            "accounts.yml",
            &format!("{accounts}deep:\n  {}v\n", "- ".repeat(yaml::MAX_DEPTH - 1)),
        );
        let (result, screen) = run_on(root.path(), &healthy()).await;
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert!(
            screen.contains("✓ Validated localhost.yml (no duplicates)"),
            "{screen}"
        );
        assert!(screen.contains("deep: unknown field"), "{screen}");
        write(
            root.path(),
            "inventories/host_vars/localhost.yml",
            &deep(yaml::MAX_DEPTH),
        );
        let (_, screen) = run_on(root.path(), &healthy()).await;
        assert!(
            screen.contains(&format!(
                "inventories/host_vars/localhost.yml:2:{}: syntax error: nested deeper than {} levels",
                3 + 2 * (yaml::MAX_DEPTH - 1),
                yaml::MAX_DEPTH
            )),
            "{screen}"
        );
    }

    /// An issue elsewhere in accounts.yml doesn't skip the checks; both
    /// blocks are shown.
    #[tokio::test]
    async fn an_issue_elsewhere_still_runs_the_checks() {
        let root = saltbox_root("valid").expect("root");
        let accounts = std::fs::read_to_string(root.path().join("accounts.yml")).expect("read");
        write(
            root.path(),
            "accounts.yml",
            &accounts.replace("name: seed", "name: ''"),
        );
        let fake = healthy();
        let (result, screen) = run_on(root.path(), &fake).await;
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert!(
            screen
                .contains("accounts.yml:3:9: user.name: must be at least 1 characters long, got 0"),
            "{screen}"
        );
        assert!(
            screen.contains("✓ Cloudflare API credentials validated"),
            "{screen}"
        );
        assert_eq!(fake.seen().len(), 3);
    }

    /// An issue inside a credential section skips that check only.
    #[tokio::test]
    async fn an_issue_in_a_section_skips_its_check() {
        let root = saltbox_root("valid").expect("root");
        let accounts = std::fs::read_to_string(root.path().join("accounts.yml")).expect("read");
        write(
            root.path(),
            "accounts.yml",
            &accounts.replace(
                "  scoped_token: not-a-real-token\n",
                "  scoped_token: not-a-real-token\n  extra: 1\n",
            ),
        );
        let fake = healthy();
        let (result, screen) = run_on(root.path(), &fake).await;
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert!(
            screen.contains("cloudflare.extra: unknown field"),
            "{screen}"
        );
        assert!(!screen.contains("Cloudflare API credentials"), "{screen}");
        assert!(
            screen.contains("✓ Docker Hub credentials validated"),
            "{screen}"
        );
        assert_eq!(fake.seen().len(), 1);
    }

    #[tokio::test]
    async fn no_credentials_are_skipped_with_a_notice() {
        let root = saltbox_root("valid").expect("root");
        let accounts = std::fs::read_to_string(root.path().join("accounts.yml")).expect("read");
        let user_only = accounts.split("cloudflare:").next().expect("user");
        write(root.path(), "accounts.yml", user_only);
        let fake = healthy();
        let (result, screen) = run_on(root.path(), &fake).await;
        assert!(result.is_ok(), "{screen}");
        assert!(
            screen.contains("no Cloudflare credentials set; skipped"),
            "{screen}"
        );
        assert!(
            screen.contains("no Docker Hub credentials set; skipped"),
            "{screen}"
        );
        assert_eq!(fake.seen(), [] as [crate::testing::Seen; 0]);
    }

    /// The client, and the platform's roots it loads, is built only when a
    /// credential check makes a request: a run with none set builds none.
    #[tokio::test]
    async fn a_run_without_credentials_builds_no_client() {
        let root = saltbox_root("valid").expect("root");
        let accounts = std::fs::read_to_string(root.path().join("accounts.yml")).expect("read");
        let user_only = accounts.split("cloudflare:").next().expect("user");
        write(root.path(), "accounts.yml", user_only);
        let fake = healthy();
        let http = Http::new(&Proxy::Off, Duration::from_secs(5)).expect("client");
        let (result, screen) = run_with(root.path(), &fake, &Shutdown::new(), http.clone()).await;
        assert!(result.is_ok(), "{screen}");
        assert!(!http.built(), "{screen}");

        let root = saltbox_root("valid").expect("root");
        let (result, screen) = run_with(root.path(), &fake, &Shutdown::new(), http.clone()).await;
        assert!(result.is_ok(), "{screen}");
        assert!(http.built(), "{screen}");
    }

    /// A failed check shows its own block, and accounts.yml fails with it.
    #[tokio::test]
    async fn a_failed_check_shows_its_block() {
        let root = saltbox_root("valid").expect("root");
        let fake = Fake::start().expect("fake");
        fake.script("/client/v4/zones", Answer::json(200, ZONES));
        fake.script(
            "/client/v4/zones/z1/settings/ssl",
            Answer::json(200, SSL_FULL),
        );
        fake.script(
            "/v2/users/login/",
            Answer::json(401, r#"{"message":"Incorrect authentication credentials"}"#),
        );
        let (result, screen) = run_on(root.path(), &fake).await;
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert!(
            screen.contains("✗ Docker Hub credentials validation"),
            "{screen}"
        );
        assert!(
            screen.contains(
                "docker hub authentication failed (HTTP 401): Incorrect authentication credentials"
            ),
            "{screen}"
        );
        assert!(
            screen.contains("✓ Cloudflare API credentials validated"),
            "{screen}"
        );
        assert!(screen.contains("✗ Validating accounts.yml"), "{screen}");
    }

    #[tokio::test]
    async fn a_short_password_warns() {
        let root = saltbox_root("valid").expect("root");
        let accounts = std::fs::read_to_string(root.path().join("accounts.yml")).expect("read");
        write(
            root.path(),
            "accounts.yml",
            &accounts.replace("pass: correct-horse-battery", "pass: short1"),
        );
        let (result, screen) = run_on(root.path(), &healthy()).await;
        assert!(result.is_ok(), "{screen}");
        assert!(
            screen.contains("WARNING: Password is shorter than 12 characters (6)."),
            "{screen}"
        );
    }

    /// A tag error on a secret doesn't show its value (`secret_paths`), and
    /// stops the file before its rules and checks.
    #[tokio::test]
    async fn a_secret_tag_error_is_redacted() {
        let root = saltbox_root("valid").expect("root");
        let accounts = std::fs::read_to_string(root.path().join("accounts.yml")).expect("read");
        write(
            root.path(),
            "accounts.yml",
            &accounts.replace("pass: correct-horse-battery", "pass: !!int s3cret-pass"),
        );
        let fake = healthy();
        let (result, screen) = run_on(root.path(), &fake).await;
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert!(!screen.contains("s3cret-pass"), "{screen}");
        assert!(screen.contains("`[REDACTED]`"), "{screen}");
        // A decode error stops the file: its checks don't run.
        assert!(fake.seen().is_empty(), "{screen}");
    }

    fn settings_on(root: &std::path::Path, fake: &Fake) -> Settings {
        Settings {
            root: root.to_path_buf(),
            zoneinfo: Vec::new(),
            http: Http::new(&Proxy::Off, Duration::from_secs(5)).expect("client"),
            cloudflare_url: format!("{}/client/v4/", fake.url()),
            dockerhub_url: format!("{}/v2/users/login/", fake.url()),
            backoff: no_wait,
        }
    }

    fn file(path: &str) -> File {
        *FILES
            .iter()
            .find(|file| file.path == path)
            .expect("a listed file")
    }

    /// The valid set with an issue of every kind: a rule, a warning, a
    /// parse error, a duplicate key, a missing required file and a
    /// missing schema.
    fn broken_set() -> tempfile::TempDir {
        let root = saltbox_root("valid").expect("root");
        let accounts = std::fs::read_to_string(root.path().join("accounts.yml")).expect("read");
        write(
            root.path(),
            "accounts.yml",
            &accounts
                .replace("name: seed", "name: ''")
                .replace("pass: correct-horse-battery", "pass: short1"),
        );
        write(root.path(), "motd.yml", "a: [unclosed\n");
        write(
            root.path(),
            "inventories/host_vars/localhost.yml",
            "a: 1\nb: 2\na: 3\n",
        );
        std::fs::remove_file(root.path().join("hetzner_vlan.yml")).expect("remove");
        std::fs::remove_file(root.path().join("schema/settings.schema.yml")).expect("remove");
        root
    }

    /// `check_file` gives each file the issues `run` shows for it (config
    /// criterion 12), over the valid set and a broken one: every issue
    /// on screen, and nothing on screen that isn't one of them.
    #[tokio::test]
    async fn check_file_gives_the_issues_run_shows() {
        for root in [saltbox_root("valid").expect("root"), broken_set()] {
            let fake = healthy();
            let (_, screen) = run_on(root.path(), &fake).await;
            let settings = settings_on(root.path(), &fake);
            let mut expected = Vec::new();
            for file in FILES {
                match check_file(&settings, file) {
                    Ok(checked) => {
                        assert_eq!(checked.issue_count(), checked.report.issues.len());
                        expected.extend(checked.report.issues.iter().map(ToString::to_string));
                    }
                    Err(FileError::Skipped(_)) => {}
                    Err(err) => expected.push(err.to_string()),
                }
            }
            for line in &expected {
                assert!(screen.contains(line.as_str()), "{line}\n{screen}");
            }
            let shown = screen
                .lines()
                .map(|line| line.trim_start_matches([' ', '│']))
                .filter(|line| {
                    FILES
                        .iter()
                        .any(|file| line.starts_with(&format!("{}:", file.path)))
                        || line.contains("not found:")
                })
                .count();
            assert_eq!(shown, expected.len(), "{expected:?}\n{screen}");
        }
    }

    /// The broken set's file errors, each worded as validate-config words
    /// it; a file over the cap is a read error.
    #[test]
    fn check_file_names_what_it_could_not_check() {
        let root = broken_set();
        let fake = Fake::start().expect("fake");
        let settings = settings_on(root.path(), &fake);
        let missing = check_file(&settings, file("hetzner_vlan.yml")).expect_err("missing");
        assert!(
            matches!(&missing, FileError::Missing(path) if path.ends_with("/hetzner_vlan.yml")),
            "{missing:?}"
        );
        assert!(
            missing
                .to_string()
                .starts_with("required config file not found: ")
        );
        let schema = check_file(&settings, file("settings.yml")).expect_err("schema");
        assert!(
            matches!(&schema, FileError::Schema(text) if text.starts_with("schema file not found: ") && text.ends_with("/settings.schema.yml")),
            "{schema:?}"
        );
        write(root.path(), "schema/motd.schema.yml", "[not, a, mapping]\n");
        let invalid = check_file(&settings, file("motd.yml")).expect_err("invalid schema");
        assert!(
            matches!(&invalid, FileError::Schema(text) if text.contains("motd.schema.yml")),
            "{invalid:?}"
        );
        std::fs::remove_file(root.path().join("motd.yml")).expect("remove");
        assert_eq!(
            check_file(&settings, file("motd.yml")).expect_err("skipped"),
            FileError::Skipped("motd.yml")
        );
        assert_eq!(
            FileError::Skipped("motd.yml").to_string(),
            "motd.yml not found; skipped"
        );
        write(
            root.path(),
            "backup_config.yml",
            &format!("# {}\n", "x".repeat(1 << 20)),
        );
        let big = check_file(&settings, file("backup_config.yml")).expect_err("too big");
        assert!(
            matches!(&big, FileError::Read(text) if text.contains("is bigger than")),
            "{big:?}"
        );
        assert!(big.to_string().contains("/backup_config.yml: "), "{big}");
        assert_eq!(fake.seen(), [] as [crate::testing::Seen; 0]);
    }

    /// A schema-backed file hands back its tree and its requests; the
    /// inventory, with no schema, neither; a file that doesn't parse, its
    /// issues alone. The debug output shows counts, never a value.
    #[test]
    fn check_file_hands_back_the_tree() {
        let root = saltbox_root("valid").expect("root");
        let fake = Fake::start().expect("fake");
        let settings = settings_on(root.path(), &fake);
        let checked = check_file(&settings, file("accounts.yml")).expect("checked");
        let tree = checked.tree.as_ref().expect("a tree");
        let name = tree.get("user").and_then(|user| user.get("name"));
        assert_eq!(
            name.map(|node| &node.data),
            Some(&crate::tree::Data::Scalar(crate::scalar::Value::Str(
                "seed".into()
            )))
        );
        assert_eq!(checked.issue_count(), 0);
        assert_eq!(checked.report.requests.len(), 2);
        let shown = format!("{checked:?}");
        assert_eq!(
            shown,
            "Checked { issues: 0, warnings: 0, checks: 2, tree: true, .. }"
        );
        assert!(!shown.contains("not-a-real-token"), "{shown}");

        write(root.path(), "inventories/host_vars/localhost.yml", "a: 1\n");
        let inventory =
            check_file(&settings, file("inventories/host_vars/localhost.yml")).expect("checked");
        assert!(inventory.tree.is_none());
        assert_eq!(inventory.report, Report::default());

        write(root.path(), "settings.yml", "a: [unclosed\n");
        let broken = check_file(&settings, file("settings.yml")).expect("checked");
        assert!(broken.tree.is_none());
        assert_eq!(broken.issue_count(), 1, "{:?}", broken.report.issues);
        assert!(broken.report.warnings.is_empty() && broken.report.requests.is_empty());
        assert_eq!(fake.seen(), [] as [crate::testing::Seen; 0]);
    }

    /// `file` through `validate_file` with `checks`, under its own
    /// top-level step as `sb edit` draws it: the screen, and the issue
    /// count when the step failed with them.
    async fn validate_one(
        root: &std::path::Path,
        fake: &Fake,
        path: &str,
        checks: Checks,
    ) -> (Result<(), Exit>, Option<usize>, String) {
        let term = indicatif::InMemoryTerm::new(40, 140);
        let options = Options {
            target: Target::InMemory {
                term: term.clone(),
                colour: false,
            },
        };
        let mut runner = Runner::start("sb edit", &Shutdown::new(), None, options);
        let settings = settings_on(root, fake);
        let mut count = None;
        let file = file(path);
        let result = runner
            .step(file_labels(file), async |step| {
                let result = validate_file(step, &settings, file, checks).await;
                count = result
                    .as_ref()
                    .err()
                    .and_then(|err| err.downcast_ref::<Issues>())
                    .map(Issues::count);
                result
            })
            .await;
        runner.finish();
        (result, count, term.contents())
    }

    /// `Checks::Skip` makes no request, says so under a file with
    /// credentials set, and still reports the file's issues.
    #[tokio::test]
    async fn skipped_checks_make_no_request() {
        let root = saltbox_root("valid").expect("root");
        let fake = healthy();
        let (result, count, screen) =
            validate_one(root.path(), &fake, "accounts.yml", Checks::Skip).await;
        assert!(result.is_ok(), "{screen}");
        assert_eq!(count, None);
        assert!(screen.contains("✓ Validated accounts.yml"), "{screen}");
        assert!(screen.contains(CHECKS_SKIPPED), "{screen}");
        assert!(!screen.contains("credentials validated"), "{screen}");
        assert_eq!(fake.seen(), [] as [crate::testing::Seen; 0]);

        // The same file with `Checks::Run`: both checks, no such notice.
        let (result, _, screen) =
            validate_one(root.path(), &fake, "accounts.yml", Checks::Run).await;
        assert!(result.is_ok(), "{screen}");
        assert!(!screen.contains(CHECKS_SKIPPED), "{screen}");
        assert_eq!(fake.seen().len(), 3, "{screen}");

        // Credentials and an issue elsewhere (an `sb edit` typo): the
        // notice, the issues, and still no request.
        let accounts = std::fs::read_to_string(root.path().join("accounts.yml")).expect("read");
        write(
            root.path(),
            "accounts.yml",
            &format!("{}extra: 1\n", accounts.replace("name: seed", "name: ''")),
        );
        let fake = healthy();
        let (result, count, screen) =
            validate_one(root.path(), &fake, "accounts.yml", Checks::Skip).await;
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert_eq!(count, Some(2), "{screen}");
        assert!(screen.contains(CHECKS_SKIPPED), "{screen}");
        assert!(screen.contains("user.name: must be at least"), "{screen}");
        assert_eq!(fake.seen(), [] as [crate::testing::Seen; 0]);

        // No credentials: nothing skipped to mention but the services.
        let fake = healthy();
        let accounts = std::fs::read_to_string(root.path().join("accounts.yml")).expect("read");
        let user_only = accounts.split("cloudflare:").next().expect("user");
        write(
            root.path(),
            "accounts.yml",
            &user_only.replace("name: seed", "name: ''"),
        );
        let (result, count, screen) =
            validate_one(root.path(), &fake, "accounts.yml", Checks::Skip).await;
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert_eq!(count, Some(1), "{screen}");
        assert!(!screen.contains(CHECKS_SKIPPED), "{screen}");
        assert!(
            screen.contains("no Cloudflare credentials set; skipped"),
            "{screen}"
        );
        assert!(screen.contains("✗ Validation of accounts.yml"), "{screen}");
        assert!(screen.contains("user.name: must be at least"), "{screen}");
        assert_eq!(fake.seen(), [] as [crate::testing::Seen; 0]);
    }

    /// A missing optional file through `validate_file`: its notice, and
    /// the step passes; a missing required one fails it.
    #[tokio::test]
    async fn validate_file_skips_or_fails_a_missing_file() {
        let root = saltbox_root("valid").expect("root");
        let fake = healthy();
        let (result, count, screen) = validate_one(
            root.path(),
            &fake,
            "inventories/host_vars/localhost.yml",
            Checks::Skip,
        )
        .await;
        assert!(result.is_ok(), "{screen}");
        assert_eq!(count, None);
        assert!(
            screen.contains("inventories/host_vars/localhost.yml not found; skipped"),
            "{screen}"
        );
        std::fs::remove_file(root.path().join("settings.yml")).expect("remove");
        let (result, count, screen) =
            validate_one(root.path(), &fake, "settings.yml", Checks::Skip).await;
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert_eq!(count, None);
        assert!(
            screen.contains("required config file not found:"),
            "{screen}"
        );
    }

    /// The files in sb-go's order, on screen too.
    #[tokio::test]
    async fn the_files_run_in_sb_gos_order() {
        assert_eq!(
            FILES.map(|file| file.path),
            [
                "accounts.yml",
                "adv_settings.yml",
                "backup_config.yml",
                "hetzner_vlan.yml",
                "settings.yml",
                "motd.yml",
                "inventories/host_vars/localhost.yml",
            ]
        );
        let root = saltbox_root("valid").expect("root");
        let (_, screen) = run_on(root.path(), &healthy()).await;
        let at = |text: &str| {
            screen
                .find(text)
                .unwrap_or_else(|| panic!("{text}\n{screen}"))
        };
        let order = FILES.map(|file| at(&format!("Validated {}", shown(file))));
        assert!(order.is_sorted(), "{screen}");
    }

    /// An issue inside `dockerhub:` skips only the Docker Hub check.
    #[tokio::test]
    async fn an_issue_in_dockerhub_skips_its_check() {
        let root = saltbox_root("valid").expect("root");
        let accounts = std::fs::read_to_string(root.path().join("accounts.yml")).expect("read");
        write(
            root.path(),
            "accounts.yml",
            &format!("{accounts}  extra: 1\n"),
        );
        let fake = healthy();
        let (result, screen) = run_on(root.path(), &fake).await;
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert!(
            screen.contains("dockerhub.extra: unknown field"),
            "{screen}"
        );
        assert!(!screen.contains("Docker Hub credentials"), "{screen}");
        assert!(
            screen.contains("✓ Cloudflare API credentials validated"),
            "{screen}"
        );
        assert_eq!(fake.seen().len(), 2);
    }

    /// A structure error is an accounts.yml issue; that check is skipped.
    #[tokio::test]
    async fn a_mixed_cloudflare_login_is_an_issue() {
        let root = saltbox_root("valid").expect("root");
        let accounts = std::fs::read_to_string(root.path().join("accounts.yml")).expect("read");
        write(
            root.path(),
            "accounts.yml",
            &accounts.replace(
                "  scoped_token: not-a-real-token\n",
                "  scoped_token: not-a-real-token\n  api: not-a-real-key\n",
            ),
        );
        let fake = healthy();
        let (result, screen) = run_on(root.path(), &fake).await;
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert!(
            screen.contains("'scoped_token' cannot be combined with 'api' or 'email'"),
            "{screen}"
        );
        assert!(!screen.contains("Cloudflare API credentials"), "{screen}");
        assert!(!screen.contains("not-a-real-key"), "{screen}");
        assert_eq!(fake.seen().len(), 1);
    }

    /// Duplicate keys stop a schema-backed file: no rules, no checks.
    #[tokio::test]
    async fn duplicates_stop_a_schema_backed_file() {
        let root = saltbox_root("valid").expect("root");
        let accounts = std::fs::read_to_string(root.path().join("accounts.yml")).expect("read");
        write(
            root.path(),
            "accounts.yml",
            &format!("{accounts}user: {{}}\n"),
        );
        let fake = healthy();
        let (result, screen) = run_on(root.path(), &fake).await;
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert!(
            screen.contains("accounts.yml:12:1: user: duplicate key"),
            "{screen}"
        );
        assert!(fake.seen().is_empty(), "{screen}");
    }

    /// A schema that can't be used fails its file, once, without the
    /// cause repeated.
    #[tokio::test]
    async fn an_invalid_schema_fails_its_file() {
        let root = saltbox_root("valid").expect("root");
        write(root.path(), "schema/motd.schema.yml", "[not, a, mapping]\n");
        let (result, screen) = run_on(root.path(), &healthy()).await;
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert!(screen.contains("✗ Validation of motd.yml"), "{screen}");
        assert_eq!(screen.matches("motd.schema.yml").count(), 1, "{screen}");
        let big = format!("# {}\n", "x".repeat(1 << 20));
        write(root.path(), "schema/motd.schema.yml", &big);
        let (result, screen) = run_on(root.path(), &healthy()).await;
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        assert_eq!(screen.matches("is bigger than").count(), 1, "{screen}");
    }

    /// Ctrl-C during the checks: each check reads interrupted with its
    /// whole label (term-ui decision 11), the times in one column.
    #[tokio::test]
    async fn an_interrupt_keeps_the_checks_labels_whole() {
        let root = saltbox_root("valid").expect("root");
        let fake = Fake::start().expect("fake");
        fake.script("/client/v4/zones", Answer::json(200, ZONES).held());
        fake.script("/v2/users/login/", Answer::json(200, "{}").held());
        let shutdown = Shutdown::new();
        let signal = async {
            while fake.seen().len() < 2 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            shutdown.signalled(2);
        };
        let ((result, screen), ()) = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(run_until(root.path(), &fake, &shutdown), signal)
        })
        .await
        .expect("the run ends");
        fake.release();
        // term-ui's interrupted ending; cli-core's finish_run turns it
        // into ending by SIGINT from the shutdown.
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        for label in [
            "✗ Validating Cloudflare API credentials",
            "✗ Validating Docker Hub credentials",
        ] {
            let line = screen.lines().find(|l| l.contains(label));
            assert!(
                line.is_some_and(|l| l.contains("interrupted")),
                "{label}\n{screen}"
            );
        }
        let steps = screen
            .lines()
            .filter(|l| l.contains('✗') || l.contains('✓'));
        assert!(steps.clone().all(|l| !l.contains('…')), "{screen}");
        assert!(steps.count() > 2, "{screen}");
    }

    /// Ctrl-C or the command's own request stops checks still waiting on
    /// the services.
    #[tokio::test]
    async fn a_stop_ends_waiting_checks() {
        let root = saltbox_root("valid").expect("root");
        let fake = Fake::start().expect("fake");
        fake.script("/client/v4/zones", Answer::json(200, ZONES).held());
        fake.script("/v2/users/login/", Answer::json(200, "{}").held());
        let shutdown = Shutdown::new();
        let stop = async {
            while fake.seen().len() < 2 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let _ = shutdown.request(1);
        };
        let ((result, screen), ()) = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(run_until(root.path(), &fake, &shutdown), stop)
        })
        .await
        .expect("the run ends");
        fake.release();
        assert!(matches!(result, Err(Exit(1))), "{screen}");
        let line = |label: &str| {
            screen
                .lines()
                .find(|l| l.contains(label))
                .unwrap_or_default()
                .to_string()
        };
        assert!(
            line("✗ Validating Cloudflare API credentials").contains("stopped"),
            "{screen}"
        );
        assert!(
            line("✗ Validating Docker Hub credentials").contains("stopped"),
            "{screen}"
        );
    }
}
