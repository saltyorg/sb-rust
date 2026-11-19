//! `sb-ops`: Saltbox's workflow commands, which tie the other modules
//! together and own only the order, the prompts and the announcement
//! rules (capability map, `ops`; docs/specs/ops.md). So far the
//! announcement rules and `sb announcements` ([`announcements`], the
//! spec's slice 10), the branch commands ([`branch`], slice 9),
//! setup's and gha's shared pieces (slice 5): [`prereq`], [`locale`],
//! [`configs`] and [`hooks`], with the `testing` harness they run
//! against; `sb gha` ([`gha`], slice 6); `sb setup` ([`setup`], slice
//! 7); `sb update` ([`update`], slice 11), on one [`Checkout`] type for
//! the two checkouts; `sb restore` ([`restore`], slices 1 and 2); and
//! `sb bench` ([`bench`], slices 3 and 4). update and setup end with
//! completion's refresh ([`completions`], slice 13). Before a checkout is
//! reset, its local changes are asked about ([`discard`], decision 40).

// sb's lines go through `Write` and cli-core's output errors;
// `print!`/`eprint!` panic on a closed pipe.
#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod announcements;
pub mod bench;
pub mod branch;
mod checkout;
pub mod completions;
pub mod configs;
pub mod discard;
pub mod gha;
pub mod hooks;
pub mod locale;
mod moved;
pub mod prereq;
pub mod restore;
pub mod setup;
#[cfg(feature = "testing")]
pub mod testing;
pub mod update;

pub use checkout::Checkout;
