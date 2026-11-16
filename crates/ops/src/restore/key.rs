//! The restore service's user key (ops spec, "Protocol"): the lowercase
//! hex SHA-1 of the user name's bytes, as sb-go's
//! `fmt.Sprintf("%x", sha1.Sum([]byte(user)))` (cmd/restore.go). The key
//! is a credential (Saltbox's `wipe_restore_service.yml` deletes a backup
//! by it), so it is held in a buffer cleared on drop, and its `Debug`
//! never shows it (decision 10: never in output, errors or the run log).

use std::fmt;

use sha1::{Digest as _, Sha1};
use zeroize::Zeroizing;

/// A user's key on the restore service: only [`UserKey::as_str`] gives
/// the text (for the request's path); `Debug` prints `[REDACTED]`, so a
/// `?key` in a log record can't leak it. Cleared on drop.
pub struct UserKey(Zeroizing<String>);

impl UserKey {
    /// The key's lowercase hex, for the request's path only.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for UserKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UserKey([REDACTED])")
    }
}

/// `name`'s user key: SHA-1 through `RustCrypto`'s `Digest`
/// (<https://docs.rs/sha1/0.11.0/sha1/>), finalized straight into a
/// buffer cleared on drop (`Digest::finalize_into`,
/// <https://docs.rs/digest/0.11.3/digest/trait.Digest.html#tymethod.finalize_into>),
/// written as lowercase hex
/// (<https://docs.rs/hex/0.4.3/hex/fn.encode.html>). The hasher's own
/// state is cleared too (sha1's `zeroize` feature).
#[must_use]
pub fn user_key(name: &str) -> UserKey {
    let mut hasher = Sha1::new();
    hasher.update(name.as_bytes());
    let mut digest = Zeroizing::new([0u8; 20]);
    let out: &mut [u8; 20] = &mut digest;
    hasher.finalize_into(out.into());
    UserKey(Zeroizing::new(hex::encode(digest.as_slice())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Known SHA-1s (`printf <name> | sha1sum`): the empty name, ASCII,
    /// and UTF-8 hashed as its bytes.
    #[test]
    fn the_key_is_the_names_lowercase_sha1_hex() {
        for (name, key) in [
            ("", "da39a3ee5e6b4b0d3255bfef95601890afd80709"),
            ("saltbox", "1a54fdb5d4243613b47942b76811eeff127f25e7"),
            ("Søren", "815df487277c3defdd86a6874a3a724411fb4f63"),
        ] {
            assert_eq!(user_key(name).as_str(), key, "{name:?}");
        }
    }

    /// The key never shows through `Debug` (a log record's `?key`).
    #[test]
    fn debug_hides_the_key() {
        let key = user_key("saltbox");
        let shown = format!("{key:?}");
        assert_eq!(shown, "UserKey([REDACTED])");
        assert!(!shown.contains(key.as_str()));
    }
}
