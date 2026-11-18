//! Which key names hold a secret (proto-facts `looks_secret`), for text
//! that leaves the screen: validate-config masks such keys' values in its
//! issue lines and the run log, on its schemas' fields and off them
//! (config spec, "Secrets"), and sb-config's logged URLs mask such query
//! values (`http::loggable_url`). The screens no longer mask (the
//! maintainer, 2026-11-18; tui spec, "Keys: one map for every screen",
//! "Secrets: where masking stays"), so the value half of the rule, a URL
//! with credentials, went with restore's viewer, its only user. Masking
//! too much is harmless; a secret under another name is quoted until the
//! list grows (tui spec, Known limits).

/// The words that make a key secret, in any case: the fact editor's
/// five, then `apprise` (its URLs carry tokens), `auth`, `credential` and
/// `hook` (the maintainer, 2026-11-17; validate-config's credential
/// names have the last ones), then `hmac` (`ZeroSSL`'s `hmacencoded`, an
/// ACME EAB HMAC key) and `claim` (Sandbox tracearr's `claim_code`, which
/// claims the first admin account; Plex's claim tokens).
const SECRET_WORDS: [&str; 11] = [
    "pass",
    "secret",
    "token",
    "key",
    "api",
    "apprise",
    "auth",
    "credential",
    "hook",
    "hmac",
    "claim",
];

/// The words that make a key secret only as a whole word of its name
/// (split at every character that isn't a letter or a digit), because
/// they sit inside common words (`kids`, `seabed`): `kid` (`ZeroSSL`'s
/// EAB key id, half of the ACME credential) and `eab`, for an ACME EAB
/// credential however a role spells it (`eab`, `eab_id`).
const SECRET_NAME_WORDS: [&str; 2] = ["kid", "eab"];

/// Whether the key `key` names a secret.
///
/// It does when its name, in any case, contains one of the secret words
/// or has one of the whole-word ones as a word.
#[must_use]
pub fn is_secret_key(key: &str) -> bool {
    let key = key.to_lowercase();
    SECRET_WORDS.iter().any(|word| key.contains(word))
        || key
            .split(|c: char| !c.is_alphanumeric())
            .any(|word| SECRET_NAME_WORDS.contains(&word))
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
            "apprise",
            "Apprise",
            "oauth",
            "service_account_credentials",
            "webhook",
        ] {
            assert!(is_secret_key(key), "{key}");
        }
    }

    /// Every key Saltbox's and Sandbox's roles store through
    /// `saltbox_facts` that holds a secret (researched 2026-11-18 at
    /// Saltbox b3ddcd1 and Sandbox 98b7054), by role.
    #[test]
    fn the_saltbox_and_sandbox_fact_secrets_are_secret() {
        for key in [
            // Saltbox: arcane, authentik, plex, zerossl (traefik).
            "encryption_key",
            "secret_key",
            "api_token",
            "token",
            "kid",
            "hmacencoded",
            // Sandbox: bookstack, dockhand, homebox, n8n, teslamate, tofa,
            // tracearr; the others store `secret_key`.
            "app_key",
            "oidc_client_secret",
            "api_key_pepper",
            "runners_auth_token",
            "postgres_password",
            "setup_access_token",
            "session_signing_key",
            "jwt_secret",
            "cookie_secret",
            "claim_code",
        ] {
            assert!(is_secret_key(key), "{key}");
        }
    }

    /// The new names generalise: ACME EAB credentials however a role
    /// spells them, HMAC keys and claim codes or tokens.
    #[test]
    fn eab_hmac_and_claim_names_are_secret() {
        for key in [
            "eab_kid",
            "EAB_KID",
            "zerossl-kid",
            "acme.eab",
            "eab_hmac",
            "hmac",
            "HMACEncoded",
            "claim",
            "plex_claim",
        ] {
            assert!(is_secret_key(key), "{key}");
        }
    }

    /// `kid` and `eab` count only as a whole word of the name, so words
    /// that merely contain them stay visible.
    #[test]
    fn kid_and_eab_inside_other_words_are_not() {
        for key in [
            "kids",
            "skid",
            "kidney_count",
            "seabed",
            "agreeable",
            "seaboard_id",
        ] {
            assert!(!is_secret_key(key), "{key}");
        }
    }

    /// Plex's `client_identifier` (the one non-secret key those roles
    /// store) and other plausible non-secret names stay visible.
    #[test]
    fn saltbox_non_secret_fact_names_are_not() {
        for key in [
            "client_identifier",
            "host",
            "port",
            "username",
            "user",
            "email",
            "path",
            "name",
            "instance",
            "enabled",
        ] {
            assert!(!is_secret_key(key), "{key}");
        }
    }

    #[test]
    fn other_keys_are_not() {
        for key in ["url", "transcode_path", "enabled", "user", "cookie", ""] {
            assert!(!is_secret_key(key), "{key}");
        }
    }
}
