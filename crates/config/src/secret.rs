//! The secret fields (config spec, "Secrets"; term-ui decision 8): no
//! issue or warning shows their values. A message that would echo one
//! shows `[REDACTED]` in its place (term-ui's form); an issue on a secret
//! names the key.

use crate::schema::Kind;

/// Shown in place of a secret value.
pub const REDACTED: &str = "[REDACTED]";

/// The credential keys, by path, besides every `type: password` field.
const SECRET_PATHS: [&str; 3] = [
    "cloudflare.api",
    "cloudflare.scoped_token",
    "dockerhub.token",
];

/// Whether the value at `path`, of type `kind`, is secret.
pub fn is_secret(path: &str, kind: Option<Kind>) -> bool {
    kind == Some(Kind::Password) || SECRET_PATHS.contains(&path)
}

/// `text` as a message may show it: `[REDACTED]` for a secret.
pub fn shown(text: &str, secret: bool) -> String {
    if secret {
        REDACTED.to_string()
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_credentials_and_passwords_are_secret() {
        for path in [
            "cloudflare.api",
            "cloudflare.scoped_token",
            "dockerhub.token",
        ] {
            assert!(is_secret(path, Some(Kind::String)), "{path}");
        }
        assert!(is_secret("user.pass", Some(Kind::Password)));
        assert!(is_secret("anything", Some(Kind::Password)));
        assert!(!is_secret("user.name", Some(Kind::String)));
        assert!(!is_secret("dockerhub.user", Some(Kind::String)));
        assert!(!is_secret("cloudflare.email", None));
    }

    #[test]
    fn a_secret_is_shown_redacted() {
        assert_eq!(shown("hunter2", true), "[REDACTED]");
        assert_eq!(shown("plain", false), "plain");
    }
}
