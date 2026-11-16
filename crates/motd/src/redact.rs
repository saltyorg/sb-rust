//! Credential hiding (motd spec, "Secrets"; decision 12): every
//! credential motd.yml configures (`token`, `apikey`, `password`) is taken
//! out of any text motd shows or logs, in each form a text may carry it,
//! and `[REDACTED]` (term-ui's form; Difference 5) put in its place:
//! - raw;
//! - percent-encoded as a query parameter and as a path, each with
//!   upper- and lowercase escapes;
//! - escaped as Rust's `Debug` writes a string, which is how serde's
//!   errors quote a value they can't take (`invalid type: string "…"`).
//!
//! `--share`'s filter joins this module with M47.
//!
//! Library-backed: each occurrence is found and replaced by sb-config's
//! `secret::hide` (whole words only for a value under 8 characters, so a
//! short one doesn't eat other words), and the encoded forms are the
//! `url` crate's, through reqwest's `Url`: its query serializer
//! (`form_urlencoded`, what a query parameter is sent as, `SABnzbd`'s key
//! included) and its path encoder
//! (<https://docs.rs/url/2.5.8/url/struct.Url.html#method.query_pairs_mut>,
//! <https://docs.rs/url/2.5.8/url/struct.Url.html#method.set_path>).
//! Why not `form_urlencoded::byte_serialize` itself: it would be a new
//! direct dependency for the same bytes. Why not url's path set: it is
//! private (url 2.5.8 `src/parser.rs`). `set_path` also drops `.`
//! segments, tabs and newlines, so such a credential gets an extra form
//! no request carries: only more is hidden. Why not one pass of
//! aho-corasick, as sb-exec's `Mask` does: `Mask` has no whole-word rule
//! for short values, and two different credentials overlapping in one
//! text doesn't happen (each service only ever sees its own); a
//! credential that holds another is hidden whole by going longest first.

use reqwest::Url;

use crate::config::Motd;

/// The run's credentials, each in every form a text may carry it,
/// longest first, so a credential that holds another is hidden whole.
#[derive(Clone, Default)]
pub struct Secrets(Vec<String>);

/// Shows how many forms are held, never one of them.
impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Secrets").field(&self.0.len()).finish()
    }
}

impl Secrets {
    /// Every instance's `apikey`, `token` and `password` in `motd`,
    /// whatever app it belongs to and whether it is enabled.
    #[must_use]
    pub fn of(motd: &Motd) -> Self {
        Self::from_values(motd.apps.values().flat_map(|section| {
            section.instances.iter().flat_map(|instance| {
                [&instance.apikey, &instance.token, &instance.password]
                    .into_iter()
                    .flatten()
                    .map(String::as_str)
            })
        }))
    }

    /// `values` and their encoded forms, empty ones left out.
    #[must_use]
    pub fn from_values<'a>(values: impl IntoIterator<Item = &'a str>) -> Self {
        let mut forms: Vec<String> = values
            .into_iter()
            .filter(|value| !value.is_empty())
            .flat_map(|value| {
                let (query, path) = (query_form(value), path_form(value));
                [
                    value.to_string(),
                    lower_escapes(&query),
                    query,
                    lower_escapes(&path),
                    path,
                    debug_form(value),
                ]
            })
            .collect();
        forms.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        forms.dedup();
        Self(forms)
    }

    /// `text` with every form of every credential replaced by
    /// `[REDACTED]`.
    #[must_use]
    pub fn hide(&self, text: &str) -> String {
        self.0.iter().fold(text.to_string(), |text, form| {
            sb_config::secret::hide(&text, form)
        })
    }
}

/// `value` as a query parameter carries it (`application/x-www-form-
/// urlencoded`: a space as `+`, Go's `url.QueryEscape`).
fn query_form(value: &str) -> String {
    let mut url = base();
    url.query_pairs_mut().append_pair("k", value);
    url.query()
        .and_then(|query| query.strip_prefix("k="))
        .unwrap_or(value)
        .to_string()
}

/// `value` as a path carries it (a space as `%20`, near Go's
/// `url.PathEscape`; a `/` stays, as in a path).
fn path_form(value: &str) -> String {
    let mut url = base();
    url.set_path(&format!("/{value}"));
    url.path().strip_prefix('/').unwrap_or(value).to_string()
}

/// `form` with its `%XX` escapes in lowercase hex, as some servers echo
/// them.
fn lower_escapes(form: &str) -> String {
    let mut out = String::with_capacity(form.len());
    let mut rest = form;
    while let Some(at) = rest.find('%') {
        out.push_str(&rest[..at]);
        let escape = rest.get(at..at + 3).unwrap_or(&rest[at..]);
        out.push_str(&escape.to_ascii_lowercase());
        rest = &rest[at + escape.len()..];
    }
    out.push_str(rest);
    out
}

/// `value` as Rust's `Debug` writes a string, without its quotes: `"`
/// and `\` escaped, control characters as `\u{..}` (serde's errors
/// quote a value so).
fn debug_form(value: &str) -> String {
    let quoted = format!("{value:?}");
    quoted
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(&quoted)
        .to_string()
}

/// A URL to encode with.
fn base() -> Url {
    Url::parse("http://encode.invalid/").expect("a valid URL")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_form_is_hidden() {
        let secrets = Secrets::from_values(["a b&c/d#", "short", ""]);
        assert_eq!(query_form("a b&c/d#"), "a+b%26c%2Fd%23");
        assert_eq!(path_form("a b&c/d#"), "a%20b&c/d%23");
        let text =
            "raw a b&c/d# q a+b%26c%2Fd%23 l a+b%26c%2fd%23 p a%20b&c/d%23 w short x shorter";
        assert_eq!(
            secrets.hide(text),
            "raw [REDACTED] q [REDACTED] l [REDACTED] p [REDACTED] w [REDACTED] x shorter"
        );
        assert_eq!(Secrets::default().hide("as is"), "as is");
        // Raw, query, its lowercase, path (no escape to lower) and short.
        assert_eq!(format!("{secrets:?}"), "Secrets(5)");
    }

    /// serde quotes a value it can't take with `Debug`'s escapes.
    #[test]
    fn the_debug_form_is_hidden() {
        let password = "pa\"ss\\word-123456\u{7}";
        assert_eq!(debug_form(password), "pa\\\"ss\\\\word-123456\\u{7}");
        let err = serde_json::from_str::<i64>(&serde_json::to_string(password).unwrap())
            .unwrap_err()
            .to_string();
        assert!(!err.contains(password), "{err}");
        let secrets = Secrets::from_values([password]);
        let hidden = secrets.hide(&err);
        assert!(
            hidden.starts_with("invalid type: string \"[REDACTED]\""),
            "{hidden}"
        );
    }

    #[test]
    fn escapes_are_lowered_alone() {
        assert_eq!(lower_escapes("AB%2FCd%3A"), "AB%2fCd%3a");
        assert_eq!(lower_escapes("x%4"), "x%4");
        assert_eq!(lower_escapes("%"), "%");
        assert_eq!(lower_escapes("plain"), "plain");
    }

    /// A credential that holds another is hidden whole, not in part.
    #[test]
    fn the_longest_form_goes_first() {
        let secrets = Secrets::from_values(["abcdefgh", "abcdefgh-ijklmnop"]);
        assert_eq!(secrets.hide("k=abcdefgh-ijklmnop;"), "k=[REDACTED];");
    }
}
