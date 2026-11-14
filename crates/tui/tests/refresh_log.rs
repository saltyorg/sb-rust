//! The run log of the picker's 5 s service refresh (tui spec,
//! "Verification notes": the refresh's run log, maintainer 2026-11-14): a
//! refresh that changes nothing writes nothing, not even the records the
//! read itself writes (sb-exec's child records, sb-host's list record);
//! a failed refresh and each service whose state changed, or that was
//! added or went, are logged. A failure is logged when it starts or its
//! cause changes, and the first good read after it logs one recovery
//! record with the count of reads that failed (maintainer, 2026-11-14). A
//! test binary of its own: tracing caches each callsite's interest
//! process-wide
//! (<https://docs.rs/tracing-core/0.1.36/tracing_core/callsite/index.html>),
//! so a log test sharing a process with tests that run without a
//! subscriber misses events (as sb-exec's `tests/capture_log.rs`).

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jiff::Timestamp;
use sb_cli_core::theme::Theme;
use sb_host::systemd::Service;
use sb_tui::App as _;
use sb_tui::picker::{ListServices, Picker};
use tokio::sync::Notify;

/// A log writer the test can read back (sb-exec's `tests/capture_log.rs`).
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    /// What was written since the last call.
    fn take(&self) -> String {
        let mut bytes = self.0.lock().expect("lock");
        String::from_utf8_lossy(&std::mem::take(&mut *bytes)).into_owned()
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

fn service(name: &str, active: &str, sub: &str) -> Service {
    Service {
        name: name.into(),
        active: active.into(),
        sub: sub.into(),
        unit_file_state: Some("enabled".into()),
        active_enter: None,
        state_change: None,
        timer: None,
    }
}

/// A read that writes records of its own, as sb-exec's children and
/// sb-host's list do, then gives the next of `results`.
fn lister(results: Vec<Result<Vec<Service>, String>>) -> ListServices {
    let results = Arc::new(Mutex::new(results.into_iter()));
    Box::new(move || {
        let next = results.lock().expect("results").next();
        Box::pin(async move {
            tracing::info!(command = "systemctl list-units", "child started");
            tracing::warn!(cause = "no timers", "systemd timers not read");
            tracing::info!(count = 2, "systemd services listed");
            next.unwrap_or_else(|| Err("no more".into()))
        })
    })
}

/// One test, with phases: a test binary has one global subscriber.
#[tokio::test(start_paused = true)]
async fn a_quiet_refresh_writes_nothing_and_failures_and_changes_are_logged() {
    let log = Captured::default();
    let writer = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("subscriber");
    let first = vec![
        service("docker", "active", "running"),
        service("saltbox_managed_web", "active", "running"),
    ];
    let mut changed = first.clone();
    changed[1].active = "failed".into();
    changed[1].sub = "failed".into();
    let mut swapped = changed.clone();
    swapped[0] = service("saltbox_managed_new", "active", "running");
    let waker = Arc::new(Notify::new());
    let mut picker = Picker::new(&first, Timestamp::now(), Theme::plain()).with_refresh(
        lister(vec![
            Ok(first.clone()),
            Ok(changed),
            Err("systemctl timed out".into()),
            Ok(swapped),
        ]),
        Arc::clone(&waker),
    );
    // The test's own records reach the capture: it is listening.
    tracing::info!("listening");
    assert!(log.take().contains("listening"));
    let mut refresh = async || {
        tokio::time::advance(Duration::from_secs(5)).await;
        picker.tick();
        waker.notified().await;
        picker.wake();
        log.take()
    };
    // The same list: nothing at all.
    assert_eq!(refresh().await, "");
    // A changed state: that service, and nothing of the read's.
    let text = refresh().await;
    assert!(text.contains("service state changed"), "{text}");
    assert!(text.contains("saltbox_managed_web"), "{text}");
    assert!(text.contains("failed"), "{text}");
    assert!(!text.contains("docker"), "{text}");
    for read in ["child started", "timers not read", "services listed"] {
        assert!(!text.contains(read), "{text}");
    }
    // A failed read: its cause.
    let text = refresh().await;
    assert!(text.contains("service refresh failed"), "{text}");
    assert!(text.contains("systemctl timed out"), "{text}");
    assert!(!text.contains("child started"), "{text}");
    // A service added and one gone: each named; the one unchanged since
    // the last list read isn't. The read works again: one recovery
    // record, with the one read that failed.
    let text = refresh().await;
    assert!(
        text.contains("service refresh recovered failed_reads=1"),
        "{text}"
    );
    assert!(
        text.contains("service listed service=saltbox_managed_new"),
        "{text}"
    );
    assert!(
        text.contains("service no longer listed service=docker"),
        "{text}"
    );
    assert!(!text.contains("saltbox_managed_web"), "{text}");
    drop(refresh);
    each_field_is_a_change_and_a_missing_time_is_not(&log).await;
    a_failure_is_logged_when_it_starts_or_changes(&log).await;
}

/// A read that keeps failing logs its failure once, again only when the
/// cause changes; the first good read logs one recovery record with how
/// many reads failed in between, and a good read after that logs none.
/// Called from the one test: the binary has one global subscriber.
async fn a_failure_is_logged_when_it_starts_or_changes(log: &Captured) {
    let list = vec![service("docker", "active", "running")];
    let waker = Arc::new(Notify::new());
    let mut picker = Picker::new(&list, Timestamp::now(), Theme::plain()).with_refresh(
        lister(vec![
            Err("systemctl timed out".into()),
            Err("systemctl timed out".into()),
            Err("systemctl timed out".into()),
            Err("systemctl exited with 1".into()),
            Err("systemctl exited with 1".into()),
            Ok(list.clone()),
            Ok(list.clone()),
        ]),
        Arc::clone(&waker),
    );
    let mut refresh = async || {
        tokio::time::advance(Duration::from_secs(5)).await;
        picker.tick();
        waker.notified().await;
        picker.wake();
        log.take()
    };
    let text = refresh().await;
    assert_eq!(text.matches("service refresh failed").count(), 1, "{text}");
    assert!(text.contains("systemctl timed out"), "{text}");
    assert_eq!(refresh().await, "", "the same cause again: nothing");
    assert_eq!(refresh().await, "", "and again");
    let text = refresh().await;
    assert_eq!(text.matches("service refresh failed").count(), 1, "{text}");
    assert!(
        text.contains("systemctl exited with 1"),
        "a new cause: {text}"
    );
    assert_eq!(refresh().await, "", "the new cause again: nothing");
    let text = refresh().await;
    assert_eq!(
        text.matches("service refresh recovered").count(),
        1,
        "{text}"
    );
    assert!(text.contains("failed_reads=5"), "{text}");
    assert!(!text.contains("refresh failed"), "{text}");
    assert_eq!(refresh().await, "", "a good read after: nothing");
}

/// The other fields of a state, each alone, are a change; a time that a
/// read couldn't get (sb-host's `show` failed or timed out) isn't one,
/// in either direction, so one failed `show` doesn't log every service.
/// Called from the one test: the binary has one global subscriber.
async fn each_field_is_a_change_and_a_missing_time_is_not(log: &Captured) {
    let at = |secs| Some(Timestamp::from_second(secs).expect("a time"));
    let mut timed = service("saltbox_managed_timed", "active", "running");
    timed.state_change = at(1_700_000_000);
    let mut untimed = timed.clone();
    untimed.state_change = None;
    let mut restarted = timed.clone();
    restarted.state_change = at(1_700_000_060);
    let mut disabled = restarted.clone();
    disabled.unit_file_state = Some("disabled".into());
    let mut exited = disabled.clone();
    exited.sub = "exited".into();
    let waker = Arc::new(Notify::new());
    let mut picker = Picker::new(
        std::slice::from_ref(&timed),
        Timestamp::now(),
        Theme::plain(),
    )
    .with_refresh(
        lister(vec![
            Ok(vec![untimed.clone()]),
            Ok(vec![timed.clone()]),
            Ok(vec![restarted]),
            Ok(vec![disabled]),
            Ok(vec![exited]),
        ]),
        Arc::clone(&waker),
    );
    let mut refresh = async || {
        tokio::time::advance(Duration::from_secs(5)).await;
        picker.tick();
        waker.notified().await;
        picker.wake();
        log.take()
    };
    assert_eq!(refresh().await, "", "the time went missing: no change");
    assert_eq!(refresh().await, "", "and came back: no change");
    let text = refresh().await;
    assert!(text.contains("service state changed"), "{text}");
    assert!(text.contains("since=2023-11-14T22:14:20Z"), "{text}");
    let text = refresh().await;
    assert!(text.contains("unit_file_state=disabled"), "{text}");
    let text = refresh().await;
    assert!(text.contains("active=active sub=exited"), "{text}");
}
