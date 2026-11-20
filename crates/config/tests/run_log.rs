//! The validation run's log records (config plan Task 10): each file, its
//! issues, each check's outcome and timing, and no secret. A test binary
//! of its own: tracing caches each callsite's interest process-wide
//! (<https://docs.rs/tracing-core/0.1.36/tracing_core/callsite/index.html>),
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
const DNS_KEY: &str = "dns-not-a-real-secret-4";

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
/// credential sections (so the checks still run), providers.yml a DNS
/// key and an issue beside it, no motd.yml, and `hetzner_vlan.yml`
/// missing with Saltbox's default to copy.
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
    let providers = format!("porkbun:\n  api_key: {DNS_KEY}\n  secret_kye: {DNS_KEY}\n");
    std::fs::write(root.join("providers.yml"), providers).expect("providers");
    let vlan = root.join("hetzner_vlan.yml");
    std::fs::create_dir(root.join("defaults")).expect("defaults");
    std::fs::rename(&vlan, root.join("defaults/hetzner_vlan.yml.default")).expect("default");
}

/// A loopback URL nothing listens on, with the socket that holds its
/// port: bound and never listening, so a connect to it is refused
/// (connect(2), `ECONNREFUSED`: "no one listening on the remote
/// address", <https://man7.org/linux/man-pages/man2/connect.2.html>) and,
/// while the caller keeps the socket, no other test's server can be given
/// the port. A port bound and freed again could be (seen under the gate's
/// parallel load: a 404 from another test's fake). tokio's `TcpSocket`
/// binds without listening
/// (<https://docs.rs/tokio/1.54.1/tokio/net/struct.TcpSocket.html#method.bind>);
/// std's `TcpListener::bind` always listens. The port stays held because
/// the socket has no `SO_REUSEADDR` (tokio's `TcpSocket` leaves it off),
/// and Linux lets a second socket share the address only when both set it
/// (socket(7), <https://man7.org/linux/man-pages/man7/socket.7.html>);
/// the assertion below checks it.
fn refused() -> (tokio::net::TcpSocket, String) {
    let socket = tokio::net::TcpSocket::new_v4().expect("socket");
    socket
        .bind(std::net::SocketAddr::from((
            std::net::Ipv4Addr::LOCALHOST,
            0,
        )))
        .expect("bind");
    let addr = socket.local_addr().expect("addr");
    // The port is held: nothing else can bind it while the socket lives.
    let second = std::net::TcpListener::bind(addr).map(|_| ());
    assert_eq!(
        second.map_err(|e| e.kind()),
        Err(std::io::ErrorKind::AddrInUse),
        "{addr} not held"
    );
    (socket, format!("http://{addr}/"))
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
    let mut runner = Runner::start("sb validate-config", &shutdown, None, options);
    let backups = settings.root.join("backups");
    let result = run(&mut runner, settings, &backups).await;
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
    let (_held, url) = refused();
    let settings = Settings {
        root: root.path().to_path_buf(),
        zoneinfo: Vec::new(),
        http: Http::new(&Proxy::Off, Duration::from_secs(5)).expect("client"),
        cloudflare_url: url.clone(),
        dockerhub_url: url,
        backoff: no_wait,
    };
    assert!(validate(&settings).await.is_err());
    // The copy is logged once, in the first run.
    assert_eq!(
        log.text()
            .matches("missing config file copied from Saltbox's default")
            .count(),
        1
    );
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
        "config file checked file=\"providers.yml\" issues=1",
        "porkbun.secret_kye: unknown field",
        // The default copied, named in the step's notice and its record.
        "missing config file copied from Saltbox's default file=\"hetzner_vlan.yml\" default=\"defaults/hetzner_vlan.yml.default\"",
        "text=\"hetzner_vlan.yml was missing; copied Saltbox's default\"",
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
    for secret in [PASSWORD, TOKEN, HUB_TOKEN, DNS_KEY] {
        assert!(!text.contains(secret), "{secret}\n{text}");
    }
}
