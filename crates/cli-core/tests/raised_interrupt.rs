//! A SIGINT sb raises at itself (console does, for Ctrl-C at a prompt)
//! with the signal thread listening (spec: "Cancellation"): the run is
//! interrupted by it as soon as the raise returns, with no wait, and the
//! thread's later read of that delivery doesn't count it again (a second
//! signal would end this test process). Its own test binary, so the
//! handlers stay out of every other test.

use std::time::Duration;

use sb_cli_core::cancel::Shutdown;
use signal_hook::consts::SIGINT;

#[test]
fn a_raised_sigint_interrupts_at_once_and_counts_once() {
    let shutdown = Shutdown::new();
    shutdown.listen_for_signals().expect("listen");
    signal_hook::low_level::raise(SIGINT).expect("raise");
    assert!(shutdown.interrupt_raised());
    assert_eq!(shutdown.signal(), Some(SIGINT));
    assert_eq!(shutdown.requested(), Some(130));
    // Time for the thread to read the delivery: had it counted it as a
    // second signal, it would have ended this process by SIGINT.
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(shutdown.signal(), Some(SIGINT));
}
