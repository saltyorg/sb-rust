//! `sb-cli-core`: the frame every sb-rust command runs in (spec:
//! docs/specs/cli-core.md).

// Output goes through `Write`; `print!`/`eprint!` panic on a closed pipe.
#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod build_info;
pub mod cancel;
pub mod colour;
pub mod exit;
pub mod log;
pub mod root;
pub mod theme;
