//! The validation run's log records (config plan Task 10): each file, its
//! issues, each check's outcome and timing, and no secret. A test binary
//! of its own: tracing caches each callsite's interest process-wide
//! (https://docs.rs/tracing-core/0.1.36/tracing_core/callsite/index.html),
//! so a log test sharing a process with tests that run without a
//! subscriber misses events. Without the `testing` feature (an
//! integration test doesn't get it), so the root is built here and the
//! checks go to a refused loopback port.

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sb_cli_core::cancel::Shutdown;
use sb_config::http::{Http, Proxy};
use sb_config::run::{Settings, run};
use sb_term_ui::{Options, Runner, Target};

/// Known secrets, one per secret field the run reads.
const PASSWORD: &str = "pass-not-a-real-secret-1";
const TOKEN: &str = "token-not-a-real-secret-2";
const HUB_TOKEN: &str = "hub-not-a-real-secret-3";

/// A log writer the test can read back, through tracing-subscriber's
/// `MakeWriter` for a closure. Why not its `Mutex<W>` or `Arc<W>` writers:
/// the first owns the buffer, the second needs `&W: Write`.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("lock")).into_owned()
    }
}

impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("lock").extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The valid fixture set and the pinned schemas in `root`, with
/// accounts.yml carrying the known secrets and an issue outside the
/// credential sections (so the checks still run), and no motd.yml.
fn saltbox_root(root: &Path) {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for entry in std::fs::read_dir(fixtures.join("valid")).expect("fixtures") {
        let path = entry.expect("entry").path();
        let name = path.file_name().expect("name");
        if name != "motd.yml" {
            std::fs::copy(&path, root.join(name)).expect("copy");
        }
    }
    let schema = root.join("schema");
    std::fs::create_dir(&schema).expect("schema dir");
    for entry in std::fs::read_dir(fixtures.join("saltbox-schemas")).expect("schemas") {
        let path = entry.expect("entry").path();
        if path.extension().is_some_and(|e| e == "yml") {
            std::fs::copy(&path, schema.join(path.file_name().expect("name"))).expect("copy");
        }
    }
    let accounts = format!(
        "user:\n  name: seed\n  email: not-an-email\n  domain: example.com\n  \
         pass: {PASSWORD}\ncloudflare:\n  scoped_token: {TOKEN}\n\
         dockerhub:\n  user: seed\n  token: {HUB_TOKEN}\n"
    );
    std::fs::write(root.join("accounts.yml"), accounts).expect("accounts");
}

/// A loopback URL nothing listens on (bound, then closed: another process
/// could take the port in between, a low risk for a test).
fn refused() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    format!("http://127.0.0.1:{port}/")
}

fn no_wait(_: u32) -> Duration {
    Duration::ZERO
}

/// One run over `settings` on an in-memory screen.
async fn validate(settings: &Settings) -> Result<(), sb_cli_core::exit::Exit> {
    let term = indicatif::InMemoryTerm::new(80, 140);
    let options = Options {
        target: Target::InMemory {
            term,
            colour: false,
        },
    };
    let shutdown = Shutdown::new();
    let mut runner = Runner::start("sb validate-config", &shutdown, None, options).await;
    let result = run(&mut runner, settings).await;
    runner.finish();
    result
}

#[tokio::test]
async fn files_issues_and_checks_are_logged_without_secrets() {
    let log = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer({
            let log = log.clone();
            move || log.clone()
        })
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let root = tempfile::tempdir().expect("root");
    saltbox_root(root.path());
    let url = refused();
    let settings = Settings {
        root: root.path().to_path_buf(),
        zoneinfo: Vec::new(),
        http: Http::new(&Proxy::Off, Duration::from_secs(5)).expect("client"),
        cloudflare_url: url.clone(),
        dockerhub_url: url,
        backoff: no_wait,
    };
    assert!(validate(&settings).await.is_err());
    // A second run: the password as a tag error, an issue on a secret.
    let accounts = std::fs::read_to_string(root.path().join("accounts.yml")).expect("read");
    let tagged = accounts.replace(
        &format!("pass: {PASSWORD}"),
        &format!("pass: !!int {PASSWORD}"),
    );
    std::fs::write(root.path().join("accounts.yml"), tagged).expect("accounts");
    assert!(validate(&settings).await.is_err());

    let text = log.text();
    for expected in [
        // Each file, with its counts.
        "config file checked file=\"accounts.yml\" issues=1",
        "config file checked file=\"settings.yml\" issues=0",
        "step=\"Validating motd.yml\" text=\"motd.yml not found; skipped\"",
        "step=\"Validating localhost.yml (no duplicates)\"",
        // The issue, in its step's record.
        "step=\"Validating accounts.yml\" outcome=\"failed\"",
        "user.email: must be a valid email address",
        // Each check's outcome and timing.
        "step=\"Validating Cloudflare API credentials\" outcome=\"failed\" elapsed_ms=",
        "step=\"Validating Docker Hub credentials\" outcome=\"failed\" elapsed_ms=",
        // The second run's issue on the password, its value withheld.
        "user.pass: cannot decode !!str `[REDACTED]` as a !!int",
    ] {
        assert!(text.contains(expected), "{expected}\n{text}");
    }
    for secret in [PASSWORD, TOKEN, HUB_TOKEN] {
        assert!(!text.contains(secret), "{secret}\n{text}");
    }
}
