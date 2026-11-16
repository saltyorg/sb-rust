//! `sb-ops`: Saltbox's workflow commands, which tie the other modules
//! together and own only the order, the prompts and the announcement
//! rules (capability map, `ops`; docs/specs/ops.md). So far the
//! announcement rules and `sb announcements` ([`announcements`], the
//! spec's slice 10), the branch commands ([`branch`], slice 9), and
//! setup's and gha's shared pieces (slice 5): [`prereq`], [`locale`],
//! [`configs`] and [`hooks`], with the `testing` harness they run
//! against; `sb gha` ([`gha`], slice 6); `sb setup` ([`setup`], slice 7).

// sb's lines go through `Write` and cli-core's output errors;
// `print!`/`eprint!` panic on a closed pipe.
#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod announcements;
pub mod branch;
pub mod configs;
pub mod gha;
pub mod hooks;
pub mod locale;
pub mod prereq;
pub mod setup;
#[cfg(feature = "testing")]
pub mod testing;
