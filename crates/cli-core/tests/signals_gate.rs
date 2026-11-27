//! The signals thread's gate (`testing::signals`), in a test binary of its
//! own, as the signals thread is the process's: with no delivery, a hold
//! records none; a delivery waits at the closed gate, undecided, until
//! the gate's drop lets the thread decide it.

use std::time::{Duration, Instant};

use sb_cli_core::cancel::Shutdown;
use sb_cli_core::testing::signals::decided_at_the_hold;
use signal_hook::consts::SIGINT;

/// How long a decision may take once the gate is open: far more than a
/// woken thread needs.
const WAIT: Duration = Duration::from_secs(10);
/// How long the closed gate is watched holding a delivery back: a gate
/// that doesn't hold lets the thread decide well inside it. The closed
/// gate holds with no limit, so this bounds only how soon a gate that
/// doesn't hold is seen, never the correct code.
const STILL: Duration = Duration::from_millis(200);

#[test]
fn the_gate_holds_a_decision_until_it_opens() {
    let shutdown = Shutdown::new();
    shutdown.listen_for_signals().expect("listen");

    // No delivery: the hold opens the gate and records none.
    let gate = decided_at_the_hold();
    drop(shutdown.hold());
    assert!(
        !gate.held_a_delivery(),
        "a delivery recorded with none sent"
    );
    drop(gate);

    // A delivery waits at the closed gate, undecided.
    let gate = decided_at_the_hold();
    signal_hook::low_level::raise(SIGINT).expect("raise");
    std::thread::sleep(STILL);
    assert!(
        !shutdown.token().is_cancelled(),
        "decided while the gate was closed"
    );
    // The gate's drop opens it: the thread decides.
    drop(gate);
    let deadline = Instant::now() + WAIT;
    while !shutdown.token().is_cancelled() {
        assert!(
            Instant::now() < deadline,
            "not decided once the gate opened"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(shutdown.signal(), Some(SIGINT));
}
