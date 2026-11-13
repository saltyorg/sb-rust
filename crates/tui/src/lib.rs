//! sb's full-screen screens (capability map, `tui`; docs/specs/tui.md).
//!
//! The groundwork (the tui spec's slice 0): terminal setup and restore,
//! chained with cli-core's restore hook and panic path, and the async
//! event loop every screen runs on ([`Tui`] and [`App`]).

mod term;

pub use term::{App, Ending, Flow, Tui};
