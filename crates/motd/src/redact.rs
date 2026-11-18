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
//! `--share`'s filter ([`Share`], decision 7; module build M47) masks IP
//! addresses and the host names sb knows of in the values shown.
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

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::LazyLock;

use regex::Regex;
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

/// sb-go's mask for an IPv4 address (`obscureIP`).
const IPV4_MASK: &str = "xxx.xxx.xxx.xxx";

/// sb-go's mask for an IPv6 address.
const IPV6_MASK: &str = "xxxx:xxxx:xxxx:xxxx";

/// A host name's mask (decision 7).
const HOST_MASK: &str = "[HOST]";

/// The escapes sb's own colours write: anstyle renders a style as SGR,
/// `ESC [ <params> m`
/// (<https://docs.rs/anstyle/1.0.14/anstyle/struct.Style.html#method.render>).
/// Text from outside sb went through `DisplayText`, so no other escape is
/// in a value. The masks run on the text between them, so a colour code
/// next to an address doesn't hide its word boundary.
/// Why not anstream's `strip_str`: it drops the codes, which `mask` keeps
/// in place; anstyle-parse would track offsets through a `Perform` for a
/// grammar this closed. Colons too, as term-ui's `is_sgr` takes them.
static SGR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("\\x1b\\[[0-9;:]*m").expect("the SGR pattern compiles"));

/// sb-go's IPv4 candidates (`obscureIPsInText`), each then confirmed by
/// std's parser (motd spec, Tech Stack), so `192.0.2.300` stays.
static IPV4: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b\d{1,3}(?:\.\d{1,3}){3}\b").expect("valid"));

/// IPv6 candidates: a run of hex digits, colons and dots (an IPv4 tail),
/// with a zone after `%`, each confirmed by std's parser once a colon or
/// dot at either end is let go (`2001:db8::1: refused`, `addr:2001:db8::1`)
/// and no letter or digit touches it. Brackets and a port
/// (`[2001:db8::1]:443`) fall outside the run. Why not sb-go's
/// `\b[0-9a-fA-F:]{3,}\b`: a `\b` can't start before a colon, so `::1`
/// was never masked.
static IPV6: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[0-9A-Fa-f:.]+(?:%[0-9A-Za-z_.-]+)?").expect("valid"));

/// Host-name candidates for error text and Traefik's routers: dotted
/// labels ending in an alphabetic top label, a leading `*.` included
/// (a certificate's `*.example.org`). Why not psl's known suffixes: an
/// internal `radarr.home.lan` would show; sb's own dotted words are
/// left out by name instead.
static NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:\*\.)?(?:[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?\.)+[A-Za-z]{2,63}")
        .expect("valid")
});

/// sb's own dotted words in its error texts: rTorrent's method and the
/// file and unit kinds a path or unit name ends in. They aren't hosts.
const NOT_NAMES: [&str; 1] = ["system.hostname"];
const NOT_TOP_LABELS: [&str; 12] = [
    "service", "timer", "mount", "socket", "target", "slice", "scope", "sock", "yml", "yaml",
    "json", "log",
];

/// `--share`'s filter (decision 7): IP addresses in every value read as
/// sb-go's masks, and host names as `[HOST]`: the hosts sb knows of
/// (motd.yml's instances, Traefik's base, `DOCKER_HOST`) and every name
/// under their domain everywhere, and any host name in an error line or
/// Traefik's routers, whose texts name hosts sb wasn't given (a
/// certificate's names, a router's rule). It runs over the rendered
/// values, never over the run log.
#[derive(Clone, Default)]
pub struct Share {
    /// The known hosts and their domains, whole names in any case,
    /// longest first; `None` when there are none.
    hosts: Option<Regex>,
    /// The known hosts' pattern couldn't be built (its size limit): every
    /// host-shaped name is masked instead, so the filter fails closed.
    every_name: bool,
}

/// Shows how much it masks, never a host.
impl std::fmt::Debug for Share {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Share")
            .field("hosts", &self.hosts.is_some())
            .field("every_name", &self.every_name)
            .finish()
    }
}

impl Share {
    /// The filter for `urls`' hosts. Only a name with a dot is masked: a
    /// single label (a container on Saltbox's network, `localhost`)
    /// names nothing outside the host, and masking it would hide the
    /// containers' own names in Docker's line. Every name under a host's
    /// registrable domain is masked with it (Saltbox serves its apps as
    /// one domain's subdomains, and a certificate error lists the
    /// others). An IP host is masked as any address is. Why not url's
    /// `Host` for the address check: url isn't a direct dependency, and
    /// reqwest's `Url::host_str` with std's parser answers the same.
    #[must_use]
    pub fn new<'a>(urls: impl IntoIterator<Item = &'a str>) -> Self {
        let names: Vec<String> = urls
            .into_iter()
            .filter_map(|url| {
                let url = Url::parse(url).ok()?;
                let name = url.host_str()?.trim_end_matches('.');
                let bare = name.trim_start_matches('[').trim_end_matches(']');
                (name.contains('.') && bare.parse::<IpAddr>().is_err())
                    .then(|| name.to_ascii_lowercase())
            })
            .filter(|name| !name.is_empty())
            .collect();
        let mut patterns: Vec<String> = names
            .iter()
            .map(|name| {
                // The registrable domain, as sb-config's Cloudflare check
                // takes it (`registrable_domain`, psl's list):
                // `mine.co.uk` stays whole, an unlisted `home.lan` is its
                // own domain.
                let domain = sb_config::cloudflare::registrable_domain(name).unwrap_or(name);
                format!(r"(?:\*\.|\b(?:[a-z0-9-]+\.)*){}\b", regex::escape(domain))
            })
            .collect();
        patterns.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        patterns.dedup();
        if patterns.is_empty() {
            return Self::default();
        }
        match Regex::new(&format!("(?i){}", patterns.join("|"))) {
            Ok(hosts) => Self {
                hosts: Some(hosts),
                every_name: false,
            },
            Err(_) => Self {
                hosts: None,
                every_name: true,
            },
        }
    }

    /// The hosts of `motd`'s instances, Traefik's base and `DOCKER_HOST`.
    #[must_use]
    pub fn of(motd: Option<&Motd>, traefik_base: &str, docker_host: Option<&str>) -> Self {
        let instances = motd.into_iter().flat_map(|motd| {
            motd.apps
                .values()
                .flat_map(|section| section.instances.iter().map(|i| i.url.as_str()))
        });
        Self::new(
            instances
                .chain(std::iter::once(traefik_base))
                .chain(docker_host),
        )
    }

    /// `value` with its addresses and hosts masked, the colour codes kept
    /// as they are. Every host-shaped name is masked in a line holding an
    /// `Error: `, and in all of `value` when `names` (Traefik's routers).
    #[must_use]
    pub fn mask(&self, value: &str, names: bool) -> String {
        value
            .split('\n')
            .map(|line| {
                let names = names || self.every_name || line.contains("Error: ");
                let mut out = String::with_capacity(line.len());
                let mut rest = 0;
                for code in SGR.find_iter(line) {
                    out.push_str(&self.mask_text(&line[rest..code.start()], names));
                    out.push_str(code.as_str());
                    rest = code.end();
                }
                out.push_str(&self.mask_text(&line[rest..], names));
                out
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Text without colour codes: hosts, then IPv6 (an IPv4 tail with
    /// it), then IPv4.
    fn mask_text(&self, text: &str, names: bool) -> String {
        let mut text = match &self.hosts {
            Some(hosts) => hosts.replace_all(text, HOST_MASK).into_owned(),
            None => text.to_string(),
        };
        if names {
            text = mask_names(&text);
        }
        let mut out = String::with_capacity(text.len());
        let mut rest = 0;
        for found in IPV6.find_iter(&text) {
            out.push_str(&text[rest..found.start()]);
            out.push_str(&ipv6_masked(&text, found.start(), found.end()));
            rest = found.end();
        }
        out.push_str(&text[rest..]);
        IPV4.replace_all(&out, |found: &regex::Captures<'_>| {
            let candidate = &found[0];
            if candidate.parse::<Ipv4Addr>().is_ok() {
                IPV4_MASK.to_string()
            } else {
                candidate.to_string()
            }
        })
        .into_owned()
    }
}

/// Last login's source for `--share` (sb-go's `obscureIP`, with a name as
/// `[HOST]`): `local` stays, an address (a zone after `%` allowed) reads
/// as its mask, anything else is a host name.
#[must_use]
pub fn mask_source(from: &str) -> String {
    let address = from.split_once('%').map_or(from, |(address, _)| address);
    match address.parse::<IpAddr>() {
        _ if from == "local" || from.is_empty() => from.to_string(),
        Ok(IpAddr::V4(_)) => IPV4_MASK.to_string(),
        Ok(IpAddr::V6(_)) => IPV6_MASK.to_string(),
        Err(_) => HOST_MASK.to_string(),
    }
}

/// Every host-shaped name in `text` as `[HOST]`, but a path's part (after
/// a `/`), a call (before a `(`, as `api.Get(`), and sb's own words.
fn mask_names(text: &str) -> String {
    NAME.replace_all(text, |found: &regex::Captures<'_>| {
        let whole = found.get(0).expect("the match");
        let name = whole.as_str();
        let before = text[..whole.start()].chars().next_back();
        let after = text[whole.end()..].chars().next();
        let joined = |c: Option<char>, extra: &[char]| {
            c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || extra.contains(&c))
        };
        let top = name
            .rsplit('.')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if joined(before, &['/', '-', '.', '@'])
            || joined(after, &['(', '-'])
            || NOT_NAMES.contains(&name)
            || NOT_TOP_LABELS.contains(&top.as_str())
        {
            name.to_string()
        } else {
            HOST_MASK.to_string()
        }
    })
    .into_owned()
}

/// `text[start..end]` masked if it holds an IPv6 address standing alone,
/// once a colon or dot at either end is let go (an address followed by
/// `: cause`, or glued to `addr:`), its zone kept (sb-go keeps what
/// follows the address).
fn ipv6_masked(text: &str, start: usize, end: usize) -> String {
    let candidate = &text[start..end];
    let (address, zone) = candidate
        .split_once('%')
        .map_or((candidate, ""), |(address, zone)| (address, zone));
    let mut lead = 0;
    let mut tail = address.len();
    if address.starts_with(':') && !address.starts_with("::") {
        lead = 1;
    }
    while tail > lead && address[..tail].ends_with(['.', ':']) && !address[..tail].ends_with("::") {
        tail -= 1;
    }
    let core = &address[lead..tail];
    let touches = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    let before = if lead == 0 {
        text[..start].chars().next_back()
    } else {
        None
    };
    let after = if tail == address.len() && zone.is_empty() {
        text[end..].chars().next()
    } else {
        None
    };
    let parsed = core.matches(':').count() >= 2
        && core.chars().any(|c| c.is_ascii_hexdigit())
        && core.parse::<Ipv6Addr>().is_ok();
    if !parsed || touches(before) || touches(after) {
        return candidate.to_string();
    }
    let zone = if zone.is_empty() {
        String::new()
    } else {
        format!("%{zone}")
    };
    format!("{}{IPV6_MASK}{}{zone}", &address[..lead], &address[tail..])
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

    fn share(urls: &[&str]) -> Share {
        Share::new(urls.iter().copied())
    }

    /// IPv4 and IPv6 in sb-go's masks, brackets, ports and zones kept;
    /// what std's parser refuses stays (decision 7, criterion 7).
    #[test]
    fn addresses_are_masked() {
        let none = Share::default();
        for (text, masked) in [
            ("from 192.0.2.9 now", "from xxx.xxx.xxx.xxx now"),
            ("192.0.2.9:8080", "xxx.xxx.xxx.xxx:8080"),
            ("192.0.2.300 stays", "192.0.2.300 stays"),
            ("at 2001:db8::7 ok", "at xxxx:xxxx:xxxx:xxxx ok"),
            ("[2001:db8::7]:443", "[xxxx:xxxx:xxxx:xxxx]:443"),
            ("::1", "xxxx:xxxx:xxxx:xxxx"),
            ("fe80::1%eth0 up", "xxxx:xxxx:xxxx:xxxx%eth0 up"),
            ("(2001:db8:0:0:0:0:0:1),", "(xxxx:xxxx:xxxx:xxxx),"),
            ("::ffff:192.0.2.9", "xxxx:xxxx:xxxx:xxxx"),
            ("2001:db8::192.0.2.9 up", "xxxx:xxxx:xxxx:xxxx up"),
            (
                "connect to 2001:db8::1: Connection refused",
                "connect to xxxx:xxxx:xxxx:xxxx: Connection refused",
            ),
            (
                "addr:2001:db8::1 at 2001:db8::1.",
                "addr:xxxx:xxxx:xxxx:xxxx at xxxx:xxxx:xxxx:xxxx.",
            ),
            ("fe80:: and Plex :: 2", "xxxx:xxxx:xxxx:xxxx and Plex :: 2"),
            (
                "23:10 12:34:56 aa:bb:cc:dd:ee:ff",
                "23:10 12:34:56 aa:bb:cc:dd:ee:ff",
            ),
            ("g2001:db8::7 2001:db8::7z", "g2001:db8::7 2001:db8::7z"),
            ("Ubuntu 22.04.5 LTS", "Ubuntu 22.04.5 LTS"),
        ] {
            assert_eq!(none.mask(text, false), masked, "{text}");
        }
    }

    /// sb's colour codes stay, and one touching an address doesn't hide
    /// it (`…m192.0.2.9`).
    #[test]
    fn colour_codes_are_kept_and_skipped() {
        let painted = "\x1b[38;2;1;2;3m192.0.2.9\x1b[0m from \x1b[1m2001:db8::7\x1b[0m";
        assert_eq!(
            Share::default().mask(painted, false),
            "\x1b[38;2;1;2;3mxxx.xxx.xxx.xxx\x1b[0m from \x1b[1mxxxx:xxxx:xxxx:xxxx\x1b[0m"
        );
    }

    /// Known hosts with a dot, in any case and as whole words, longest
    /// first; single labels and IP hosts aren't names to hide.
    #[test]
    fn known_hosts_are_masked() {
        let share = share(&[
            "https://plex.example.org:32400",
            "http://media.plex.example.org/",
            "http://sonarr:8989",
            "http://192.0.2.4:8080",
            "http://[2001:db8::4]:80",
            "not a url",
        ]);
        assert_eq!(
            share.mask(
                "PLEX.example.org, media.plex.example.org and plexXexample.org",
                false
            ),
            "[HOST], [HOST] and plexXexample.org"
        );
        assert_eq!(share.mask("sonarr stopped", false), "sonarr stopped");
        assert_eq!(share.mask("at 192.0.2.4", false), "at xxx.xxx.xxx.xxx");
        assert_eq!(
            share.mask("myplex.example.org.uk", false),
            "[HOST].uk",
            "a name under the domain, cut where a label ends"
        );
        // A name of three labels masks its domain's other names too (a
        // certificate's), but not a name of two labels' parent.
        assert_eq!(
            share.mask(
                r#"valid for DnsName("*.example.org") and sonarr.example.org"#,
                false
            ),
            r#"valid for DnsName("[HOST]") and [HOST]"#
        );
        assert_eq!(
            Share::new(["https://example.org"]).mask("example.org and a.org", false),
            "[HOST] and a.org"
        );
        // The registrable domain, from the public suffix list.
        let uk = Share::new(["https://mine.co.uk", "https://media.plex.example.net"]);
        assert_eq!(
            uk.mask("a.mine.co.uk other.co.uk sonarr.example.net", false),
            "[HOST] other.co.uk [HOST]"
        );
        let shown = format!("{share:?}");
        assert_eq!(shown, "Share { hosts: true, every_name: false }");
        assert!(!shown.contains("example"), "{shown}");
    }

    /// motd.yml's instances, enabled or not, Traefik's base and
    /// `DOCKER_HOST`.
    #[test]
    fn the_hosts_come_from_the_config_traefik_and_docker() {
        let instance = |url: &str, enabled: bool| crate::config::Instance {
            name: "x".into(),
            url: url.into(),
            apikey: None,
            token: None,
            user: None,
            password: None,
            timeout: None,
            enabled,
        };
        let motd = Motd {
            apps: [(
                crate::config::App::Plex,
                crate::config::AppSection {
                    enabled: true,
                    instances: vec![
                        instance("https://a.example.org", true),
                        instance("https://b.example.org", false),
                    ],
                },
            )]
            .into(),
            ..Motd::default()
        };
        let share = Share::of(
            Some(&motd),
            "http://traefik.example.org:8080",
            Some("tcp://docker.example.net:2376"),
        );
        assert_eq!(
            share.mask(
                "a.example.org b.example.org traefik.example.org docker.example.net",
                false
            ),
            "[HOST] [HOST] [HOST] [HOST]"
        );
        let bare = Share::of(None, "http://traefik:8080", None);
        assert_eq!(
            bare.mask("traefik a.example.org", false),
            "traefik a.example.org"
        );
    }

    /// In an error line, and in Traefik's routers, every host-shaped
    /// name is masked, configured or not; sb's own dotted words, paths and
    /// calls stay.
    #[test]
    fn names_in_errors_and_routers_are_masked() {
        let none = Share::default();
        assert_eq!(
            none.mask(
                "Error: certificate not valid for name \"media.other.net\"",
                false
            ),
            "Error: certificate not valid for name \"[HOST]\""
        );
        assert_eq!(
            none.mask(
                "Sonarr: 0 items\nRadarr: Error: dns for radarr.home.lan failed",
                false
            ),
            "Sonarr: 0 items\nRadarr: Error: dns for [HOST] failed"
        );
        assert_eq!(
            none.mask("served by media.other.net", false),
            "served by media.other.net"
        );
        assert_eq!(
            none.mask("plex-secure@docker: unable to obtain ACME certificate for domains \"plex.mine.io\"", true),
            "plex-secure@docker: unable to obtain ACME certificate for domains \"[HOST]\""
        );
        for kept in [
            "Error: could not connect to rtorrent: system.hostname XMLRPC call failed",
            "Error: api.Get(v3/queue): invalid status code",
            "Error: couldn't reach Docker at unix:///var/run/docker.sock: gone",
            "Error: systemctl list-units -- saltbox_managed_*.service: failed",
            "Error: motd.yml has 2 issues",
            "Error: d.multicall2 and throttle.global_down.rate failed",
        ] {
            assert_eq!(none.mask(kept, false), kept);
        }
    }

    /// Last login's source: sb-go's masks, a name as `[HOST]`, `local`
    /// as it is.
    #[test]
    fn the_last_login_source_is_masked() {
        assert_eq!(mask_source("192.0.2.9"), "xxx.xxx.xxx.xxx");
        assert_eq!(mask_source("2001:db8::9"), "xxxx:xxxx:xxxx:xxxx");
        assert_eq!(mask_source("laptop.example.org"), "[HOST]");
        assert_eq!(mask_source("local"), "local");
        assert_eq!(mask_source(""), "");
        assert_eq!(mask_source("fe80::1%eth0"), "xxxx:xxxx:xxxx:xxxx");
    }
}
