//! The secret fields (config spec, "Secrets"; term-ui decision 8): no
//! issue or warning shows their values. A message that would echo one
//! shows `[REDACTED]` in its place (term-ui's form); an issue on a secret
//! names the key.

use crate::schema::Kind;

/// Shown in place of a secret value.
pub const REDACTED: &str = "[REDACTED]";

/// The credential keys, by path (`[]` standing for any item of a list),
/// besides every `type: password` field: accounts.yml's Cloudflare and
/// Docker Hub credentials and its Apprise URL (which carries a token),
/// `backup_config.yml`'s restore password, and motd.yml's tokens, API keys
/// and passwords. The schemas type these `string`; a test checks the list
/// against the pinned schemas (`rules`' tests). Each ends in a key the
/// shared key-name rule masks as well (2026-11-18), so the list documents
/// Saltbox's known credentials and keeps them masked should that rule
/// narrow; masking no longer waits for it to grow.
const SECRET_PATHS: [&str; 16] = [
    "apprise",
    "backup.restore_service.pass",
    "cloudflare.api",
    "cloudflare.scoped_token",
    "dockerhub.token",
    "emby.instances[].token",
    "jellyfin.instances[].token",
    "lidarr.instances[].apikey",
    "nzbget.instances[].password",
    "plex.instances[].token",
    "qbittorrent.instances[].password",
    "radarr.instances[].apikey",
    "readarr.instances[].apikey",
    "rtorrent.instances[].password",
    "sabnzbd.instances[].apikey",
    "sonarr.instances[].apikey",
];

/// Whether the value at `path`, of type `kind`, is secret.
///
/// A password field, a credential path, or a key whose name says it holds
/// a secret (the editors' and restore's rule,
/// `facts::secret::is_secret_key`): a provider field Saltbox's schema adds
/// as a `string` (`apikey`) is masked before the list knows it.
#[must_use]
pub fn is_secret(path: &str, kind: Option<Kind>) -> bool {
    kind == Some(Kind::Password)
        || SECRET_PATHS.contains(&any_item(path).as_ref())
        || crate::facts::secret::is_secret_key(key_name(path))
}

/// The credential paths, as [`SECRET_PATHS`] writes them.
#[cfg(test)]
pub(crate) fn credential_paths() -> impl Iterator<Item = &'static str> {
    SECRET_PATHS.iter().copied()
}

/// The key names of the credential paths (`api`, `token`, `apikey`, …),
/// secret wherever they sit off the schema (`rules::secret_paths`).
pub(crate) fn credential_names() -> impl Iterator<Item = &'static str> {
    SECRET_PATHS.iter().map(|path| key_name(path))
}

/// `path` with each `[n]` item (ASCII digits) written `[]`: `a[].b` for
/// `a[2].b`, borrowed when it has none. Found by hand from the left, as
/// regex's `replace_all` of `\[[0-9]+\]` finds them (a test compares
/// them): why not regex, its compile was part of the fixed patterns'
/// 1.9 M instructions in validate-config (`rules.rs`).
fn any_item(path: &str) -> std::borrow::Cow<'_, str> {
    let b = path.as_bytes();
    let mut out: Option<String> = None;
    let mut last = 0;
    let mut at = 0;
    while at < b.len() {
        if b[at] == b'[' {
            let mut end = at + 1;
            while b.get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
            if end > at + 1 && b.get(end) == Some(&b']') {
                let out = out.get_or_insert_with(|| String::with_capacity(path.len()));
                out.push_str(&path[last..at]);
                out.push_str("[]");
                last = end + 1;
                at = end + 1;
                continue;
            }
        }
        at += 1;
    }
    match out {
        None => std::borrow::Cow::Borrowed(path),
        Some(mut out) => {
            out.push_str(&path[last..]);
            std::borrow::Cow::Owned(out)
        }
    }
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

    /// The items by hand are regex's `\[[0-9]+\]`, each written `[]`, on
    /// every short text of the characters that matter.
    #[test]
    fn the_items_are_the_patterns() {
        let pattern = regex::Regex::new(r"\[[0-9]+\]").expect("compiles");
        for text in crate::testing::texts(&["[", "]", "1", "23", "a", ".", "\u{FF16}"], 6) {
            let (ours, theirs) = (any_item(&text), pattern.replace_all(&text, "[]"));
            assert_eq!(ours, theirs, "{text:?}");
            // Borrowed when there is no item, as regex's.
            assert_eq!(
                matches!(ours, std::borrow::Cow::Borrowed(_)),
                matches!(theirs, std::borrow::Cow::Borrowed(_)),
                "{text:?}"
            );
        }
    }

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
        // A credential's key name, typed `string` and on no list.
        assert!(is_secret("newapp.instances[0].apikey", Some(Kind::String)));
        assert!(is_secret("newapp.client_secret", Some(Kind::String)));
    }

    /// The credentials the schemas type `string`, at any item of a list
    /// (Task 60): motd.yml's tokens, API keys and passwords,
    /// `backup_config.yml`'s restore password, accounts.yml's Apprise URL.
    #[test]
    fn string_typed_credentials_are_secret_by_path() {
        for path in [
            "plex.instances[0].token",
            "emby.instances[3].token",
            "sonarr.instances[12].apikey",
            "qbittorrent.instances[0].password",
            "backup.restore_service.pass",
            "apprise",
        ] {
            assert!(is_secret(path, Some(Kind::String)), "{path}");
        }
        assert!(!is_secret("plex.instances[0].name", Some(Kind::String)));
        assert!(!is_secret("nzbget.instances[0].user", Some(Kind::String)));
        assert!(!is_secret(
            "backup.restore_service.user",
            Some(Kind::String)
        ));
        // Only `[n]` items match the list; such paths are still secret
        // by their key's name.
        for path in ["plex.instances.token[", "plex.instances[x].token"] {
            assert!(!SECRET_PATHS.contains(&any_item(path).as_ref()), "{path}");
            assert!(is_secret(path, Some(Kind::String)), "{path}");
        }
    }

    #[test]
    fn items_are_read_as_any_item() {
        assert_eq!(any_item("a[0].b[12][3]"), "a[].b[][]");
        assert_eq!(any_item("a.b"), "a.b");
        assert_eq!(any_item("a[x]"), "a[x]");
        assert_eq!(any_item("a[1"), "a[1");
    }

    #[test]
    fn the_key_name_is_the_last_key_without_items() {
        assert_eq!(key_name("user[0].pass[1]"), "pass");
        assert_eq!(key_name("pass"), "pass");
        assert_eq!(key_name("a.b"), "b");
        let names: std::collections::BTreeSet<_> = credential_names().collect();
        assert_eq!(
            names.into_iter().collect::<Vec<_>>(),
            [
                "api",
                "apikey",
                "apprise",
                "pass",
                "password",
                "scoped_token",
                "token"
            ]
        );
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
