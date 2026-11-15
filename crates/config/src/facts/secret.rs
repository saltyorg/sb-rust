//! Which facts the editor masks (tui spec, "Editors", Secrets): facts
//! have no schema, so validate-config's secret paths don't apply and the
//! key's name decides (proto-facts `looks_secret`). Masking too much is
//! harmless; a secret under another name shows until the list grows
//! (Known limits).

/// The words that make a key secret, in any case.
const SECRET_WORDS: [&str; 5] = ["pass", "secret", "token", "key", "api"];

/// Whether the fact `key` holds a secret: its name contains `pass`,
/// `secret`, `token`, `key` or `api`, in any case.
#[must_use]
pub fn is_secret_key(key: &str) -> bool {
    let key = key.to_lowercase();
    SECRET_WORDS.iter().any(|word| key.contains(word))
}

#[cfg(test)]
mod tests {
    use super::is_secret_key;

    #[test]
    fn keys_naming_a_secret_word_are_secret_in_any_case() {
        for key in [
            "password",
            "PASS",
            "client_secret",
            "claim_token",
            "api_key",
            "ApiKey",
            "monkey",
            "rapid",
        ] {
            assert!(is_secret_key(key), "{key}");
        }
    }

    #[test]
    fn other_keys_are_not() {
        for key in ["url", "transcode_path", "enabled", "user", "cookie", ""] {
            assert!(!is_secret_key(key), "{key}");
        }
    }
}
