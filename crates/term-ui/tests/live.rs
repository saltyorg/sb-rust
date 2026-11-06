//! Whether a runner is live, for exec's `attached` (which takes sb's
//! terminal; exec spec, "Attached children"): from its start until it is
//! finished or dropped. A test binary of its own: the count is
//! process-wide, and the unit tests' runners would be live beside it.

use sb_cli_core::cancel::Shutdown;
use sb_term_ui::{Options, Runner, runner_live};

#[tokio::test]
async fn a_runner_is_live_from_start_to_finish() {
    assert!(!runner_live());
    let shutdown = Shutdown::new();
    let run = Runner::start("sb demo", &shutdown, None, Options::default());
    assert!(runner_live());
    run.finish();
    assert!(!runner_live());
    let dropped = Runner::start("sb demo", &shutdown, None, Options::default());
    assert!(runner_live());
    drop(dropped);
    assert!(!runner_live());
}
