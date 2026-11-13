//! sb's full-screen screens (capability map, `tui`; docs/specs/tui.md).
//!
//! The groundwork (the tui spec's slice 0): terminal setup and restore,
//! chained with cli-core's restore hook and panic path, and the async
//! event loop every screen runs on ([`Tui`] and [`App`]).
//!
//! The parts of slice 1 with no screen: the log-source protocol and the
//! journal adapter ([`source`]), and the service status rule
//! ([`status`]).

pub mod source;
pub mod status;
mod term;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use term::{App, Ending, Flow, Tui};
