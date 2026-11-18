//! The providers on sb-config's fake HTTP service (motd spec, Testing,
//! "HTTP providers on `sb_config::testing::Fake`").

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use sb_config::http::Proxy;
use sb_config::testing::{Answer as Reply, Fake};
use tokio::time::Instant;

use super::*;
use crate::config::{AppSection, Instance};

/// An instance at `url` with no credentials, on, the default timeout.
pub(crate) fn instance(name: &str, url: &str) -> Instance {
    Instance {
        name: name.to_string(),
        url: url.to_string(),
        apikey: None,
        token: None,
        user: None,
        password: None,
        timeout: None,
        enabled: true,
    }
}

pub(crate) fn with_token(name: &str, url: &str, token: &str) -> Instance {
    Instance {
        token: Some(token.to_string()),
        ..instance(name, url)
    }
}

pub(crate) fn section(instances: Vec<Instance>) -> AppSection {
    AppSection {
        enabled: true,
        instances,
    }
}

/// A run's shared part: no proxy, the sections' 30 s from now, these
/// credentials hidden.
pub(crate) fn shared(secrets: &[&str]) -> Arc<Shared> {
    Shared::new(
        Proxy::Off,
        Instant::now() + crate::run::CAP,
        Secrets::from_values(secrets.iter().copied()),
    )
}

/// A loopback URL nothing listens on: a port bound, read back, freed.
pub(crate) fn refused() -> String {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{address}")
}

pub(crate) fn painted() -> Paint {
    let theme = sb_cli_core::theme::Theme::resolve(
        &sb_cli_core::theme::Palette::default(),
        sb_cli_core::theme::Profile::TrueColor,
    );
    Paint::new(&theme, true)
}

#[test]
fn the_timeout_is_the_instances_or_the_default_cut_to_the_deadline() {
    let shared = shared(&[]);
    let mut one = instance("x", "http://x");
    assert_eq!(shared.timeout(&one, DEFAULT_TIMEOUT), DEFAULT_TIMEOUT);
    one.timeout = Some(Duration::from_millis(2500));
    assert_eq!(
        shared.timeout(&one, DEFAULT_TIMEOUT),
        Duration::from_millis(2500)
    );
    one.timeout = Some(Duration::from_secs(3600));
    let cut = shared.timeout(&one, DEFAULT_TIMEOUT);
    assert!(
        cut <= crate::run::CAP && cut > Duration::from_secs(29),
        "{cut:?}"
    );
    let late = Shared::new(Proxy::Off, Instant::now(), Secrets::default());
    assert_eq!(
        late.timeout(&one, DEFAULT_TIMEOUT),
        Duration::from_millis(1)
    );
    assert_eq!(DEFAULT_TIMEOUT, Duration::from_secs(1));
}

#[test]
fn error_text_hides_credentials_and_keeps_one_line() {
    let shared = shared(&["sekrit-canary-1", "ab"]);
    assert_eq!(
        shared.error_text("bad sekrit-canary-1 x\nsecond"),
        "Error: bad [REDACTED] x"
    );
    // A control character between a credential's parts is dropped
    // first, then the joined credential hidden.
    assert_eq!(
        shared.error_text("k=sekrit-\x07canary-1"),
        "Error: k=[REDACTED]"
    );
    assert_eq!(shared.error_text("ab"), "Error: [REDACTED]");
    assert_eq!(shared.error_text(" \n"), "Error: instance error");
}

#[test]
fn one_instance_shows_alone_and_several_with_their_names() {
    let plain = Paint::plain();
    assert_eq!(listing(Vec::new(), &plain), "");
    assert_eq!(
        listing(
            vec![("Plex".into(), Ok("No active streams".into()))],
            &plain
        ),
        "No active streams"
    );
    assert_eq!(
        listing(vec![("Plex".into(), Err("Error: x".into()))], &plain),
        "Error: x"
    );
    let answers = vec![
        ("Plex 4K".to_string(), Err("Error: x".to_string())),
        ("Plëx".to_string(), Ok("No active streams".to_string())),
        ("Ab".to_string(), Ok("1".to_string())),
    ];
    assert_eq!(
        listing(answers.clone(), &plain),
        "Ab:      1\nPlex 4K: Error: x\nPlëx:   No active streams"
    );
    let paint = painted();
    let text = listing(answers, &paint);
    let first = text.lines().next().unwrap();
    assert_eq!(first, format!("{}1", paint.app_name("Ab:      ")));
    assert!(text.contains(&paint.error("Error: x")), "{text:?}");
}

/// Instances are asked at once, each as its own task: two held answers
/// end together within one timeout; a disabled or unusable instance
/// and a disabled section ask nothing.
#[tokio::test]
async fn instances_are_asked_at_once_and_only_when_usable() {
    let fake = Fake::start().unwrap();
    fake.script("/a", Reply::new(200, "a").held());
    let shared = shared(&[]);
    let mut off = instance("Off", fake.url());
    off.enabled = false;
    let unusable = instance("Unusable", fake.url());
    let mut slow = instance("Slow", fake.url());
    slow.timeout = Some(Duration::from_millis(300));
    let all = section(vec![
        slow.clone(),
        off,
        unusable,
        Instance {
            name: " ".into(),
            ..slow
        },
    ]);
    let start = Instant::now();
    let answers = ask_each(
        "test",
        Some(&all),
        "Fallback",
        &shared,
        |instance| instance.name != "Unusable",
        |shared, instance| async move {
            let url = format!("{}/a", instance.url);
            send(
                &shared,
                |client| client.get(url),
                shared.timeout(&instance, DEFAULT_TIMEOUT),
                Texts {
                    app: "A",
                    api: "a",
                    read: "body",
                },
            )
            .await
            .map(|_| "ok".to_string())
            .map_err(|err| shared.error_text(&err))
        },
    )
    .await;
    assert!(
        start.elapsed() < Duration::from_millis(1500),
        "{:?}",
        start.elapsed()
    );
    let names: Vec<&str> = answers.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["Slow", "Fallback"]);
    for (_, answer) in &answers {
        let error = answer.as_ref().unwrap_err();
        assert!(
            error.starts_with("Error: failed to connect to A: "),
            "{error}"
        );
    }
    assert_eq!(fake.seen().len(), 2);
    let disabled = AppSection {
        enabled: false,
        ..all.clone()
    };
    let none = ask_each(
        "test",
        Some(&disabled),
        "F",
        &shared,
        |_| true,
        |_, _| async { Ok(String::new()) },
    )
    .await;
    assert_eq!(none.len(), 0);
    let none = ask_each(
        "test",
        None,
        "F",
        &shared,
        |_| true,
        |_, _| async { Ok(String::new()) },
    )
    .await;
    assert_eq!(none.len(), 0);
}

/// A panic in one instance is its error line, hidden; the others still
/// answer.
#[tokio::test]
async fn a_panicking_instance_is_its_own_error_line() {
    let shared = shared(&["panic-canary-123"]);
    let both = section(vec![
        instance("Bad", "http://x"),
        instance("Good", "http://x"),
    ]);
    let answers = ask_each(
        "test",
        Some(&both),
        "F",
        &shared,
        |_| true,
        |_, instance| async move {
            assert!(instance.name != "Bad", "with panic-canary-123 inside");
            Ok("fine".to_string())
        },
    )
    .await;
    assert_eq!(
        answers,
        [
            (
                "Bad".to_string(),
                Err("Error: panic: with [REDACTED] inside".to_string())
            ),
            ("Good".to_string(), Ok("fine".to_string())),
        ]
    );
}

/// A 200 body, a status other than 200 (a redirect not followed), and a
/// refused connection, in sb-go's words.
#[tokio::test]
async fn send_reads_200_and_names_the_rest() {
    let fake = Fake::start().unwrap();
    fake.script("/ok", Reply::json(200, "{}"));
    fake.script(
        "/moved",
        Reply::new(302, "").header("location", "/ok?apikey=x"),
    );
    let shared = shared(&[]);
    let texts = Texts {
        app: "Plex",
        api: "plex",
        read: "response",
    };
    let get = |path: &str| {
        let url = format!("{}{path}", fake.url());
        let shared = Arc::clone(&shared);
        async move { send(&shared, |client| client.get(url), DEFAULT_TIMEOUT, texts).await }
    };
    assert_eq!(get("/ok").await.unwrap(), b"{}");
    assert_eq!(
        get("/moved").await.unwrap_err(),
        "plex API returned status code 302"
    );
    assert_eq!(
        fake.seen()
            .iter()
            .filter(|seen| seen.target.starts_with("/ok"))
            .count(),
        1
    );
    let refused = refused();
    let err = send(
        &shared,
        |client| client.get(refused),
        DEFAULT_TIMEOUT,
        texts,
    )
    .await
    .unwrap_err();
    assert!(err.starts_with("failed to connect to Plex: "), "{err}");
    assert!(!err.contains("127.0.0.1"), "{err}");
}

/// A provider that takes the connection and never answers stays inside
/// its budget: sb-go's 1 s by default, its `timeout` when set, and never
/// past the section's deadline (criterion 10).
#[tokio::test]
async fn a_hung_instance_ends_at_its_timeout_or_the_deadline() {
    let fake = Fake::start().unwrap();
    fake.script("/status/sessions", Reply::json(200, "{}").held());
    let hung = section(vec![with_token("Plex", fake.url(), "t")]);
    let start = Instant::now();
    let text = plex::plex(Some(&hung), &shared(&[]), &Paint::plain()).await;
    let took = start.elapsed();
    assert!(
        text.starts_with("Error: failed to connect to Plex: "),
        "{text}"
    );
    assert!(
        took >= Duration::from_millis(900) && took < Duration::from_secs(3),
        "{took:?}"
    );
    // A long `timeout`, cut to a deadline 300 ms away.
    let mut slow = with_token("Plex", fake.url(), "t");
    slow.timeout = Some(Duration::from_secs(20));
    let near = Shared::new(
        Proxy::Off,
        Instant::now() + Duration::from_millis(300),
        Secrets::default(),
    );
    let start = Instant::now();
    let text = plex::plex(Some(&section(vec![slow])), &near, &Paint::plain()).await;
    let took = start.elapsed();
    assert!(
        text.starts_with("Error: failed to connect to Plex: "),
        "{text}"
    );
    assert!(took < Duration::from_millis(900), "{took:?}");
    // A refused port ends at once.
    let start = Instant::now();
    let gone = section(vec![with_token("Plex", &refused(), "t")]);
    let text = plex::plex(Some(&gone), &shared(&[]), &Paint::plain()).await;
    assert!(
        text.starts_with("Error: failed to connect to Plex: "),
        "{text}"
    );
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "{:?}",
        start.elapsed()
    );
}

/// The client is built once per run, whichever providers ask.
#[tokio::test]
async fn the_client_is_built_once() {
    let shared = shared(&[]);
    let first: *const reqwest::Client = shared.client().await.unwrap();
    let second: *const reqwest::Client = shared.client().await.unwrap();
    assert_eq!(first, second);
    let broken = Shared::new(
        Proxy::Via("not a proxy url \u{0}".to_string()),
        Instant::now() + crate::run::CAP,
        Secrets::default(),
    );
    let why = broken.client().await.unwrap_err();
    let one = section(vec![with_token("Plex", "http://x", "t")]);
    let text = plex::plex(Some(&one), &broken, &Paint::plain()).await;
    assert_eq!(text, format!("Error: {why}"));
}

/// A client build begun by `start` runs on the blocking pool: it begins
/// before any provider asks, a slow one holds up none of the runtime's
/// other tasks, and the providers asking meanwhile share the one client
/// it builds.
#[tokio::test]
async fn a_started_build_runs_beside_the_other_tasks() {
    static BUILDS: AtomicUsize = AtomicUsize::new(0);
    fn slow(proxy: &Proxy) -> Result<Http, String> {
        BUILDS.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(300));
        build(proxy)
    }
    let shared = Shared::with_build(
        Proxy::Off,
        Instant::now() + crate::run::CAP,
        Secrets::default(),
        slow,
    );
    let started = std::time::Instant::now();
    shared.start();
    let ticker = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        started.elapsed()
    });
    let begun = tokio::time::timeout(Duration::from_secs(5), async {
        while BUILDS.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });
    assert!(begun.await.is_ok(), "start began no build");
    let (first, second) = tokio::join!(shared.client(), shared.client());
    let other = ticker.await.unwrap();
    assert!(other < Duration::from_millis(200), "{other:?}");
    let first: *const reqwest::Client = first.unwrap();
    let second: *const reqwest::Client = second.unwrap();
    assert_eq!(first, second);
    assert_eq!(BUILDS.load(Ordering::SeqCst), 1);
}

/// A client that can't be built fails each request that needs it, with
/// the build's text, as before the build moved off the runtime.
#[tokio::test]
async fn a_client_that_cant_be_built_fails_the_requests() {
    fn broken(_: &Proxy) -> Result<Http, String> {
        Err("no roots".to_string())
    }
    let shared = Shared::with_build(
        Proxy::Off,
        Instant::now() + crate::run::CAP,
        Secrets::default(),
        broken,
    );
    shared.start();
    assert_eq!(shared.client().await.unwrap_err(), "no roots");
    let one = section(vec![with_token("Plex", "http://x", "t")]);
    let text = plex::plex(Some(&one), &shared, &Paint::plain()).await;
    assert_eq!(text, "Error: no roots");
}

/// A build that panics is each asking instance's error line, as a panic
/// in the instance's own task is, from one build.
#[tokio::test]
async fn a_build_that_panics_is_the_instances_error() {
    static BUILDS: AtomicUsize = AtomicUsize::new(0);
    fn panics(_: &Proxy) -> Result<Http, String> {
        BUILDS.fetch_add(1, Ordering::SeqCst);
        panic!("build broke")
    }
    let shared = Shared::with_build(
        Proxy::Off,
        Instant::now() + crate::run::CAP,
        Secrets::default(),
        panics,
    );
    shared.start();
    let one = section(vec![with_token("Plex", "http://x", "t")]);
    let text = plex::plex(Some(&one), &shared, &Paint::plain()).await;
    assert_eq!(text, "Error: panic: build broke");
    let two = section(vec![with_token("Emby", "http://y", "t")]);
    let text = emby::emby(Some(&two), &shared, &Paint::plain()).await;
    assert_eq!(text, "Error: panic: build broke");
    assert_eq!(BUILDS.load(Ordering::SeqCst), 1);
}
