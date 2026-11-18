//! Which values the editors mask (tui spec, "Editors", Secrets; ops spec,
//! "`sb restore` (hidden)", the masking rule): facts and restored files
//! have no schema here, so validate-config's secret paths don't apply and
//! the key's name decides (proto-facts `looks_secret`), or a value that
//! is a URL carrying credentials. Masking too much is harmless; a secret
//! under another name shows until the list grows (Known limits). One
//! rule for the fact editor and restore's viewer (ops decision 32);
//! validate-config masks by the key-name half too, on its schemas' fields
//! and off them (config spec, "Secrets").

/// The words that make a key secret, in any case: the fact editor's
/// five, then `apprise` (its URLs carry tokens), `auth`, `credential` and
/// `hook` (the maintainer, 2026-11-17; validate-config's credential
/// names have the last ones).
const SECRET_WORDS: [&str; 9] = [
    "pass",
    "secret",
    "token",
    "key",
    "api",
    "apprise",
    "auth",
    "credential",
    "hook",
];

/// Whether the key `key` holds a secret: its name contains one of the
/// secret words, in any case.
#[must_use]
pub fn is_secret_key(key: &str) -> bool {
    let key = key.to_lowercase();
    SECRET_WORDS.iter().any(|word| key.contains(word))
}

/// Whether `value` is a secret by its look: its first word (a comment
/// after it ignored; quotes around it too) is a URL with a user or
/// password in it, or with a query parameter whose name is a secret key
/// or `sig` (an Azure SAS signature), parsed with the url crate reqwest
/// uses (<https://docs.rs/url/2.5.8/url/struct.Url.html>).
#[must_use]
pub fn is_secret_value(value: &str) -> bool {
    let Some(word) = value.split_whitespace().next() else {
        return false;
    };
    let Ok(url) = reqwest::Url::parse(word.trim_matches(['"', '\''])) else {
        return false;
    };
    !url.username().is_empty()
        || url.password().is_some()
        || url
            .query_pairs()
            .any(|(name, _)| name == "sig" || is_secret_key(&name))
}

/// Whether the value under `key` is masked: the key's name or the
/// value's look says it is a secret.
#[must_use]
pub fn is_secret(key: &str, value: &str) -> bool {
    is_secret_key(key) || is_secret_value(value)
}

#[cfg(test)]
mod tests {
    use super::{is_secret, is_secret_key, is_secret_value};

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
            "apprise",
            "Apprise",
            "oauth",
            "service_account_credentials",
            "webhook",
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

    /// A URL with credentials in its user part or a secret-named query
    /// parameter is secret, quoted or not; other URLs and plain text are
    /// not.
    #[test]
    fn urls_with_credentials_are_secret() {
        for value in [
            "https://user:pw@example.test/",
            "https://token@example.test/",
            "mailto://someone:pw@example.test",
            "\"https://user:pw@example.test\"",
            "'https://example.test/hook?token=abc'",
            "https://example.test/x?api_key=abc&a=b",
            "https://user:pw@example.test # home",
            "https://account.example.test/c?sv=1&sig=abc",
        ] {
            assert!(is_secret_value(value), "{value}");
        }
        for value in [
            "https://example.test/",
            "https://example.test/x?page=2",
            "/mnt/local",
            "yes",
            "",
            "user: pw",
        ] {
            assert!(!is_secret_value(value), "{value}");
        }
    }

    #[test]
    fn a_key_or_a_value_makes_it_secret() {
        assert!(is_secret("token", "abc"));
        assert!(is_secret("url", "https://u:p@example.test"));
        assert!(!is_secret("url", "https://example.test"));
        assert!(!is_secret("name", "demo"));
    }
}
