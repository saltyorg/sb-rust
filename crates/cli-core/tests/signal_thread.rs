//! The signal thread in this process (spec: "Cancellation"): a first
//! SIGTERM, raised at ourselves, reaches the thread and cancels the token.
//! A first signal only cancels, so the test process carries on. Its own
//! test binary, so the handlers stay out of every other test.

use std::time::{Duration, Instant};

use sb_cli_core::cancel::Shutdown;

#[test]
fn a_first_signal_reaches_the_thread_and_cancels() {
    let shutdown = Shutdown::new();
    shutdown.listen_for_signals().expect("listen");
    signal_hook::low_level::raise(signal_hook::consts::SIGTERM).expect("raise");
    let token = shutdown.token();
    let start = Instant::now();
    while !token.is_cancelled() && start.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(token.is_cancelled());
    assert_eq!(shutdown.requested(), Some(143));
    assert!(shutdown.interrupted());
}
