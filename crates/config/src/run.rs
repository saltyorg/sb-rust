//! The validation run (config spec, "Behaviour"): every Saltbox config
//! file in sb-go's order, checked against its schema, then the credential
//! checks, drawn as term-ui steps. sb's command builds the runner and the
//! settings and calls `run`; nothing is decided twice.

use std::path::PathBuf;
use std::time::Duration;

use sb_cli_core::config::{read_inventory, read_small, yaml};
use sb_cli_core::exit::Exit;
use sb_term_ui::{Labels, Runner, Step};

use crate::cloudflare::{self, Check};
use crate::dockerhub;
use crate::http::Http;
use crate::issue::{Issue, escape_controls};
use crate::parse;
use crate::rules::{self, Login, Request};
use crate::schema::{self, SchemaError, Validator};
use crate::tree;
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

/// Validate every file under `runner`: `Exit(1)` when any failed (each
/// failed file and check has shown its block).
///
/// # Errors
///
/// `Exit(1)` when a file or credential check failed, or the run was
/// interrupted or stopped.
pub async fn run(runner: &mut Runner, settings: &Settings) -> Result<(), Exit> {
    let labels = Labels::new(
        "Validating Saltbox configuration",
        "Saltbox configuration validated",
    );
    runner
        .step(labels, async |step| {
            let mut failed = false;
            for file in FILES {
                let shown = shown(file);
                let labels =
                    Labels::new(format!("Validating {shown}"), format!("Validated {shown}"))
                        .failure(format!("Validation of {shown}"));
                let result = step
                    .step(labels, async |step| validate(step, settings, file).await)
                    .await;
                failed |= result.is_err();
            }
            if failed { Err(Exit(1).into()) } else { Ok(()) }
        })
        .await
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

/// One file under its `step`: its issues are the step's error (the
/// failure block); a failed credential check fails the step too, after
/// its own block. Each file's outcome, issues and timing reach the run
/// log through term-ui's step records; this adds what they don't say.
async fn validate(step: &mut Step, settings: &Settings, file: File) -> anyhow::Result<()> {
    let path = settings.root.join(file.path);
    let read = if file.capped {
        read_small
    } else {
        read_inventory
    };
    let text = match read(&path) {
        Ok(Some(text)) => text,
        Ok(None) if file.required => {
            anyhow::bail!(
                "required config file not found: {}",
                escape_controls(&path.display().to_string())
            )
        }
        Ok(None) => {
            step.info(format!("{} not found; skipped", file.path));
            return Ok(());
        }
        Err(err) => anyhow::bail!("{}: {err}", escape_controls(&path.display().to_string())),
    };
    let rules = match file.schema {
        Some(schema) => {
            let path = settings.root.join("schema").join(schema);
            let shown = escape_controls(&path.display().to_string());
            // Flattened: a read error's text already carries its causes.
            Some(schema::load(&path, &shown).map_err(|err| match err {
                SchemaError::Missing { .. } => anyhow::anyhow!("schema file not found: {shown}"),
                err => anyhow::anyhow!("{err}"),
            })?)
        }
        None => None,
    };
    let validators = Saltbox::with_zoneinfo(settings.zoneinfo.clone());
    let prepared = parse::prepare(file.path, &text).map_err(|issues| block(&issues))?;
    // The file's tree is loaded, checked and dropped where its depth fits.
    let checked = yaml::with_stack(prepared.depth(), || {
        let root = parse::load(file.path, &prepared)?;
        let Some(rules) = &rules else {
            return Ok(None);
        };
        let tree = tree::build(file.path, &root, &rules::secret_paths(rules))?;
        Ok(Some(rules::validate(file.path, &tree, rules, &validators)))
    })
    .map_err(|err| anyhow::anyhow!("can't start the YAML reader's thread: {err}"))?;
    let Some(report) = checked.map_err(|issues: Vec<Issue>| block(&issues))? else {
        tracing::info!(file = file.path, issues = 0, "config file checked");
        return Ok(());
    };
    tracing::info!(
        file = file.path,
        issues = report.issues.len(),
        warnings = report.warnings.len(),
        checks = report.requests.len(),
        "config file checked"
    );
    for warning in &report.warnings {
        step.warn(warning.as_str());
    }
    for validator in &report.skipped {
        if let Some(name) = check_name(*validator) {
            step.info(format!("no {name} credentials set; skipped"));
        }
    }
    let checks_failed = checks(step, settings, &report.requests).await;
    if !report.issues.is_empty() {
        return Err(block(&report.issues));
    }
    if checks_failed {
        return Err(Exit(1).into());
    }
    Ok(())
}

/// Issues as a failure block, one per line.
fn block(issues: &[Issue]) -> anyhow::Error {
    let lines: Vec<String> = issues.iter().map(ToString::to_string).collect();
    anyhow::anyhow!(lines.join("\n"))
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
async fn checks(step: &mut Step, settings: &Settings, requests: &[Request]) -> bool {
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
            http: Http::new(&Proxy::Off, Duration::from_secs(5)).expect("client"),
            cloudflare_url: format!("{}/client/v4/", fake.url()),
            dockerhub_url: format!("{}/v2/users/login/", fake.url()),
            backoff: no_wait,
        };
        let result = run(&mut runner, &settings).await;
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
