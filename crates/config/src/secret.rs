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
#[must_use]
pub fn is_secret(path: &str, kind: Option<Kind>) -> bool {
    kind == Some(Kind::Password) || SECRET_PATHS.contains(&path)
}

/// The key names of the credential paths (`api`, `scoped_token`,
/// `token`), secret wherever they sit off the schema (`rules::secret_paths`).
pub(crate) fn credential_names() -> impl Iterator<Item = &'static str> {
    SECRET_PATHS
        .iter()
        .filter_map(|path| path.rsplit('.').next())
}

/// The key a dotted path ends in, without its `[n]` items: `pass` for
/// `user[0].pass[1]`.
#[must_use]
pub(crate) fn key_name(path: &str) -> &str {
    let last = path.rsplit('.').next().unwrap_or(path);
    last.split('[').next().unwrap_or(last)
}

/// `text` as a message may show it: `[REDACTED]` for a secret.
#[must_use]
pub fn shown(text: &str, secret: bool) -> String {
    if secret {
        REDACTED.to_string()
    } else {
        text.to_string()
    }
}

/// `text` with `value` (a secret) taken out wherever it appears: every
/// occurrence for a value of 8 characters or more; for a shorter one only
/// where it stands as a whole word (no letter, digit, `-` or `_` beside
/// it), so it doesn't eat the letters of other words. Run on raw text,
/// before it is escaped or cut. Why not `regex`: it has no lookaround, so
/// word edges would be consumed and adjacent matches missed.
#[must_use]
pub fn hide(text: &str, value: &str) -> String {
    if value.is_empty() {
        return text.to_string();
    }
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_');
    let short = value.chars().count() < 8;
    let mut out = String::new();
    let mut rest = 0;
    for (at, _) in text.match_indices(value) {
        let end = at + value.len();
        if at < rest
            || (short && (word(text[..at].chars().next_back()) || word(text[end..].chars().next())))
        {
            continue;
        }
        out.push_str(&text[rest..at]);
        out.push_str(REDACTED);
        rest = end;
    }
    out.push_str(&text[rest..]);
    out
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
    fn the_key_name_is_the_last_key_without_items() {
        assert_eq!(key_name("user[0].pass[1]"), "pass");
        assert_eq!(key_name("pass"), "pass");
        assert_eq!(key_name("a.b"), "b");
        let names: Vec<_> = credential_names().collect();
        assert_eq!(names, ["api", "scoped_token", "token"]);
    }

    #[test]
    fn a_secret_is_shown_redacted() {
        assert_eq!(shown("hunter2", true), "[REDACTED]");
        assert_eq!(shown("plain", false), "plain");
    }

    #[test]
    fn hide_takes_out_every_long_secret() {
        assert_eq!(
            hide("bad token s3cret-token.", "s3cret-token"),
            "bad token [REDACTED]."
        );
        assert_eq!(
            hide("xs3cret-tokenx and s3cret-token", "s3cret-token"),
            "x[REDACTED]x and [REDACTED]"
        );
        assert_eq!(hide("dckr_pat_abcd", "dckr_pat_abc"), "[REDACTED]d");
        // Eight characters is long: taken out inside a word too.
        assert_eq!(hide("xabcdefghx", "abcdefgh"), "x[REDACTED]x");
        assert_eq!(hide("xabcdefgx", "abcdefg"), "xabcdefgx");
    }

    #[test]
    fn hide_takes_out_a_short_secret_only_as_a_word() {
        assert_eq!(
            hide("Incorrect token t. here", "t"),
            "Incorrect token [REDACTED]. here"
        );
        assert_eq!(hide("aa aa", "aa"), "[REDACTED] [REDACTED]");
        assert_eq!(hide("a-aa_aa", "aa"), "a-aa_aa");
        assert_eq!(hide("x", ""), "x");
    }
}
