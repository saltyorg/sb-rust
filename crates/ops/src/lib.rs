//! `sb-ops`: Saltbox's workflow commands, which tie the other modules
//! together and own only the order, the prompts and the announcement
//! rules (capability map, `ops`; docs/specs/ops.md). So far the
//! announcement rules and `sb announcements` ([`announcements`], the
//! spec's slice 10), and the branch commands ([`branch`], slice 9).

// sb's lines go through `Write` and cli-core's output errors;
// `print!`/`eprint!` panic on a closed pipe.
#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod announcements;
pub mod branch;
