//! `sb restore`'s core (ops spec, "`sb restore` (hidden)", slice 1): the
//! restore service's user key ([`user_key`]), OpenSSL's `enc
//! -aes-256-cbc -pbkdf2` format as Saltbox's backup role writes it
//! ([`decrypt`]), and the placement of each plaintext in the Saltbox
//! root ([`place`]); the answers ([`answers`]) and the command
//! ([`restore`], slice 2).

pub mod answers;
mod command;
mod crypto;
mod key;
mod place;

pub use command::{MAX_BODY, RestoreArgs, SERVICE, restore};

pub use crypto::{DecryptError, decrypt};
pub use key::{UserKey, user_key};
pub use place::{FILES, place};
