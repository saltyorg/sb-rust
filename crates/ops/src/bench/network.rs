//! bench's network rows of the System section (ops spec, "`sb bench`",
//! 1.): IPv4/IPv6 from icanhazip on each family, and Organization,
//! Location and Region from ipinfo, shown as bench.sh's
//! `print_system_info` and `ipv4_info` show them. Requests go through
//! reqwest with the script's 4 s each (`curl -s -m 4`). The addresses in
//! either answer are never kept, printed or logged.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use sb_cli_core::cancel::CancellationToken;
use sb_cli_core::theme::Role;
use sb_term_ui::Cell;

use super::label;

/// Each request's limit (`curl -s -m 4`).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(4);

/// icanhazip, asked over each family, and ipinfo's JSON (one request for
/// the four fields bench.sh asked for one at a time: ops difference 17).
const ICANHAZIP: &str = "https://icanhazip.com";
const IPINFO: &str = "https://ipinfo.io/json";

/// Where the network rows are asked, and the client's `User-Agent`.
#[derive(Debug, Clone)]
pub struct Endpoints {
    /// icanhazip, asked from `0.0.0.0` and from `::`.
    pub v4: String,
    pub v6: String,
    pub ipinfo: String,
    /// Plain `http` allowed (a test's loopback fake), else `https` only.
    pub http: bool,
    pub user_agent: String,
}

impl Endpoints {
    /// The real services.
    #[must_use]
    pub fn production(user_agent: &str) -> Self {
        Self {
            v4: ICANHAZIP.to_owned(),
            v6: ICANHAZIP.to_owned(),
            ipinfo: IPINFO.to_owned(),
            http: false,
            user_agent: user_agent.to_owned(),
        }
    }
}

/// Whether each family reached icanhazip.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Online {
    pub v4: bool,
    pub v6: bool,
}

/// ipinfo's four fields bench.sh shows; the rest of its answer (the
/// address, host name, coordinates) is never read.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct Isp {
    pub org: Option<String>,
    pub city: Option<String>,
    pub country: Option<String>,
    pub region: Option<String>,
}

/// No client could be built: the system's CA certificates are missing
/// (cli-core's `tls`; ops spec, decision 44).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{}", sb_cli_core::tls::MISSING_CA)]
pub struct MissingCa;

/// A client for one request family: bound to `local` (reqwest's
/// `local_address`, so the request goes out on that family;
/// <https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html#method.local_address>),
/// 4 s in all, no redirects (curl's default), `https` only unless
/// `endpoints.http`. None when it can't be built, [`MissingCa`] when that
/// is for the missing CA bundle.
fn client(
    endpoints: &Endpoints,
    local: Option<IpAddr>,
) -> Result<Option<reqwest::Client>, MissingCa> {
    let built = reqwest::Client::builder()
        .user_agent(&endpoints.user_agent)
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .https_only(!endpoints.http)
        .local_address(local)
        .build()
        .map_err(reqwest::Error::without_url);
    match built {
        Ok(client) => Ok(Some(client)),
        Err(err) => {
            // With its causes: reqwest's own text is only "builder error".
            tracing::warn!(error = %sb_config::http::error_chain(&err), "client not built");
            if sb_cli_core::tls::is_missing_ca(&err) {
                Err(MissingCa)
            } else {
                Ok(None)
            }
        }
    }
}

/// The most of an answer read: icanhazip's is an address, ipinfo's a
/// few hundred bytes.
const MAX_ANSWER: usize = 64 << 10;

/// The body of a GET to `url` as text, or none: an answer over 64 KiB is
/// refused, read a chunk at a time as release's downloads are
/// (<https://docs.rs/reqwest/0.13.5/reqwest/struct.Response.html#method.chunk>);
/// the URL is never logged (reqwest's `without_url`).
async fn get(client: Option<reqwest::Client>, url: &str, what: &'static str) -> Option<String> {
    let mut response = client?
        .get(url)
        .send()
        .await
        .map_err(reqwest::Error::without_url)
        .inspect_err(|err| tracing::info!(what, error = %err, "request failed"))
        .ok()?;
    let mut body = Vec::new();
    loop {
        match response.chunk().await.map_err(reqwest::Error::without_url) {
            Ok(Some(chunk)) if body.len() + chunk.len() <= MAX_ANSWER => {
                body.extend_from_slice(&chunk);
            }
            Ok(Some(_)) => {
                tracing::info!(what, limit = MAX_ANSWER, "answer too long");
                return None;
            }
            Ok(None) => break,
            Err(err) => {
                tracing::info!(what, error = %err, "answer not read");
                return None;
            }
        }
    }
    Some(String::from_utf8_lossy(&body).into_owned())
}

/// Text as the script's `$(...)` gives it: trailing newlines removed.
fn substituted(text: &str) -> &str {
    text.trim_end_matches('\n')
}

/// icanhazip over IPv4 and over IPv6, at once: a family is online when
/// its answer has any text but newlines, as bench.sh's `$(curl -s ...)`
/// did (its
/// first try, a ping to Google, goes: ops difference 17). The address
/// answered is dropped unread. Cancelling `token` gives offline.
///
/// # Errors
///
/// [`MissingCa`] when no client can be built for the missing CA bundle:
/// nothing is asked.
pub async fn online(endpoints: &Endpoints, token: &CancellationToken) -> Result<Online, MissingCa> {
    let answered = |body: Option<String>| body.is_some_and(|body| !substituted(&body).is_empty());
    let v4 = get(
        client(endpoints, Some(IpAddr::V4(Ipv4Addr::UNSPECIFIED)))?,
        &endpoints.v4,
        "icanhazip over IPv4",
    );
    let v6 = get(
        client(endpoints, Some(IpAddr::V6(Ipv6Addr::UNSPECIFIED)))?,
        &endpoints.v6,
        "icanhazip over IPv6",
    );
    let (v4, v6) = token
        .run_until_cancelled(async { tokio::join!(v4, v6) })
        .await
        .unwrap_or_default();
    let online = Online {
        v4: answered(v4),
        v6: answered(v6),
    };
    tracing::info!(v4 = online.v4, v6 = online.v6, "connectivity checked");
    Ok(online)
}

/// ipinfo's fields, or none of them when it can't be asked or answers
/// something else; a field of nothing but newlines counts as none, as
/// the script's `-n` tests.
///
/// # Errors
///
/// [`MissingCa`] when the client can't be built for the missing CA
/// bundle: nothing is asked.
pub async fn isp(endpoints: &Endpoints, token: &CancellationToken) -> Result<Isp, MissingCa> {
    let client = client(endpoints, None)?;
    let body = token
        .run_until_cancelled(get(client, &endpoints.ipinfo, "ipinfo"))
        .await
        .flatten();
    let isp: Isp = body
        .and_then(|body| {
            // The error's kind and place only: its text can quote the
            // answer (an address).
            serde_json::from_str(&body)
                .inspect_err(|err| {
                    tracing::info!(
                        kind = ?err.classify(),
                        line = err.line(),
                        column = err.column(),
                        "ipinfo's answer not read"
                    );
                })
                .ok()
        })
        .unwrap_or_default();
    // `-n "$(curl ...)"`: any text but newlines counts.
    let known = |field: Option<String>| field.filter(|value| !substituted(value).is_empty());
    Ok(Isp {
        org: known(isp.org),
        city: known(isp.city),
        country: known(isp.country),
        region: known(isp.region),
    })
}

/// The one network row when the CA bundle is missing: `IPv4/IPv6`, then
/// `Not checked:` and cli-core's sentence with the fix, in Problem, in
/// place of the network rows (ops spec, decision 44): neither check could
/// ask, so neither `✗ Offline` nor `No ISP detected` is known.
#[must_use]
pub fn missing_ca_row() -> [Cell; 2] {
    [
        label("IPv4/IPv6"),
        Cell::new(format!("Not checked: {}", sb_cli_core::tls::MISSING_CA)).role(Role::Problem),
    ]
}

/// One family's state: `✓ Online` or `✗ Offline`.
fn state(on: bool) -> &'static str {
    if on { "✓ Online" } else { "✗ Offline" }
}

/// The network rows, in bench.sh's order: `IPv4/IPv6` (`<v4> / <v6>`; in
/// the Ok role when both are online, Problem when neither is, Warn
/// otherwise: a cell has one colour where the script coloured each
/// half), then `Organization` when there is one, `Location` (`city /
/// country`) with both, `Region` when there is one, and `Region` saying
/// `No ISP detected` without an organization.
#[must_use]
pub fn rows(online: Online, isp: &Isp) -> Vec<[Cell; 2]> {
    let role = match (online.v4, online.v6) {
        (true, true) => Role::Ok,
        (false, false) => Role::Problem,
        _ => Role::Warn,
    };
    let mut rows = vec![[
        label("IPv4/IPv6"),
        Cell::new(format!("{} / {}", state(online.v4), state(online.v6))).role(role),
    ]];
    if let Some(org) = &isp.org {
        rows.push([label("Organization"), Cell::new(org.as_str())]);
    }
    if let (Some(city), Some(country)) = (&isp.city, &isp.country) {
        rows.push([label("Location"), Cell::new(format!("{city} / {country}"))]);
    }
    if let Some(region) = &isp.region {
        rows.push([label("Region"), Cell::new(region.as_str())]);
    }
    if isp.org.is_none() {
        rows.push([
            label("Region"),
            Cell::new("No ISP detected").role(Role::Problem),
        ]);
    }
    rows
}

#[cfg(test)]
mod tests {
    use sb_term_ui::Table;

    use super::*;
    use crate::testing::NetworkFake;

    /// [`super::online`] where its clients build (every test but the one
    /// without CA certificates).
    async fn online(endpoints: &Endpoints, token: &CancellationToken) -> Online {
        super::online(endpoints, token)
            .await
            .expect("the clients built")
    }

    /// [`super::isp`] where its client builds.
    async fn isp(endpoints: &Endpoints, token: &CancellationToken) -> Isp {
        super::isp(endpoints, token)
            .await
            .expect("the client built")
    }

    /// Set in this test binary's child that runs without CA certificates:
    /// the file it writes when its check ran.
    const NO_CA_CHILD: &str = "SB_OPS_TEST_NO_CA_CHILD";

    /// Without the system's CA certificates no request is made: both
    /// checks say the bundle is missing (ops spec, decision 44), and the
    /// row says so with the fix, in Problem, instead of `✗ Offline` and
    /// `No ISP detected`. In a child of this test binary with
    /// `SSL_CERT_FILE` an empty file, which rustls-native-certs reads
    /// instead of the system's store
    /// (<https://docs.rs/rustls-native-certs/0.8.5/rustls_native_certs/fn.load_native_certs.html>).
    #[tokio::test]
    async fn without_ca_certificates_the_checks_say_so() {
        if let Some(ran) = std::env::var_os(NO_CA_CHILD) {
            let fake = NetworkFake::start("ADDRESS\n", r#"{"org":"AS1 Example"}"#).expect("fake");
            let token = CancellationToken::new();
            assert_eq!(
                super::online(&fake.endpoints(), &token).await,
                Err(MissingCa)
            );
            assert_eq!(super::isp(&fake.endpoints(), &token).await, Err(MissingCa));
            assert!(fake.seen().is_empty(), "{:?}", fake.seen());
            std::fs::write(ran, "ran").expect("mark");
            return;
        }
        let dir = tempfile::tempdir().expect("dir");
        let (empty, ran) = (dir.path().join("empty.pem"), dir.path().join("ran"));
        std::fs::write(&empty, "").expect("empty bundle");
        let child = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "bench::network::tests::without_ca_certificates_the_checks_say_so",
            ])
            .env(NO_CA_CHILD, &ran)
            .env("SSL_CERT_FILE", &empty)
            .env_remove("SSL_CERT_DIR")
            .output()
            .expect("child");
        assert!(
            child.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        assert!(ran.exists(), "the child's check didn't run");
    }

    /// The row for a missing bundle: the IPv4/IPv6 label, the sentence
    /// with the fix, in Problem; nothing about being offline.
    #[test]
    fn the_missing_bundle_is_one_problem_row() {
        use sb_cli_core::theme::{Palette, Profile, Theme};
        let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
        let mut table = Table::ruled(Vec::<String>::new());
        table.row(missing_ca_row());
        let text = table.render(&theme, true, None);
        let said = format!("Not checked: {}", sb_cli_core::tls::MISSING_CA);
        assert!(
            text.contains(&sb_term_ui::style::paint(
                &theme,
                Role::Problem,
                true,
                &said
            )),
            "{text:?}"
        );
        assert!(text.contains("IPv4/IPv6"), "{text:?}");
        assert!(
            !text.contains("Offline") && !text.contains("No ISP"),
            "{text:?}"
        );
    }

    fn rendered(rows: Vec<[Cell; 2]>) -> String {
        let mut table = Table::spanning("System");
        for row in rows {
            table.row(row);
        }
        table.render(&sb_cli_core::theme::Theme::plain(), false, None)
    }

    fn fields(
        org: Option<&str>,
        city: Option<&str>,
        country: Option<&str>,
        region: Option<&str>,
    ) -> Isp {
        let text = |value: Option<&str>| value.map(str::to_owned);
        Isp {
            org: text(org),
            city: text(city),
            country: text(country),
            region: text(region),
        }
    }

    #[test]
    fn the_rows_follow_bench_sh() {
        let text = rendered(rows(
            Online {
                v4: true,
                v6: false,
            },
            &fields(
                Some("AS1 Example"),
                Some("Falkenstein"),
                Some("DE"),
                Some("Saxony"),
            ),
        ));
        for line in [
            "│ IPv4/IPv6    │ ✓ Online / ✗ Offline │",
            "│ Organization │ AS1 Example          │",
            "│ Location     │ Falkenstein / DE     │",
            "│ Region       │ Saxony               │",
        ] {
            assert!(text.contains(line), "{line}\n{text}");
        }
        assert!(!text.contains("No ISP detected"), "{text}");
    }

    #[test]
    fn without_an_organization_region_says_no_isp() {
        let text = rendered(rows(
            Online::default(),
            &fields(None, Some("x"), None, Some("y")),
        ));
        assert!(text.contains("✗ Offline / ✗ Offline"), "{text}");
        assert!(!text.contains("Organization"), "{text}");
        // Location needs both the city and the country.
        assert!(!text.contains("Location"), "{text}");
        assert!(text.contains("│ Region    │ y "), "{text}");
        assert!(text.contains("│ Region    │ No ISP detected "), "{text}");
    }

    #[test]
    fn the_connectivity_cell_is_coloured_by_both_families() {
        let role = |online| rows(online, &Isp::default())[0][1].clone();
        let colour = |online| {
            let mut table = Table::new(["a", "b"]);
            table.row([Cell::new("x"), role(online)]);
            let theme = sb_cli_core::theme::Theme::resolve(
                &sb_cli_core::theme::Palette::default(),
                sb_cli_core::theme::Profile::TrueColor,
            );
            (table.render(&theme, true, None), theme)
        };
        for (online, role) in [
            (Online { v4: true, v6: true }, Role::Ok),
            (
                Online {
                    v4: true,
                    v6: false,
                },
                Role::Warn,
            ),
            (
                Online {
                    v4: false,
                    v6: false,
                },
                Role::Problem,
            ),
        ] {
            let (text, theme) = colour(online);
            let expected = format!("{} / {}", state(online.v4), state(online.v6));
            assert!(
                text.contains(&sb_term_ui::style::paint(&theme, role, true, &expected)),
                "{role:?}: {text:?}"
            );
        }
    }

    #[test]
    fn production_asks_the_real_services_over_https() {
        let endpoints = Endpoints::production("sb/test");
        assert_eq!(endpoints.v4, "https://icanhazip.com");
        assert_eq!(endpoints.v6, "https://icanhazip.com");
        assert_eq!(endpoints.ipinfo, "https://ipinfo.io/json");
        assert!(!endpoints.http);
        assert_eq!(endpoints.user_agent, "sb/test");
    }

    #[tokio::test]
    async fn each_family_asks_its_own_address() {
        let fake = NetworkFake::start("ADDRESS\n", r#"{"org":"AS1 Example"}"#).expect("fake");
        let online = online(&fake.endpoints(), &CancellationToken::new()).await;
        assert_eq!(online, Online { v4: true, v6: true });
        let seen = fake.seen();
        assert!(
            seen.iter().any(|seen| seen.path == "/" && seen.v6),
            "{seen:?}"
        );
        assert!(
            seen.iter().any(|seen| seen.path == "/" && !seen.v6),
            "{seen:?}"
        );
        assert!(
            seen.iter()
                .all(|seen| seen.user_agent.as_deref() == Some("sb/test"))
        );
    }

    #[tokio::test]
    async fn an_empty_answer_or_no_server_is_offline() {
        let fake = NetworkFake::start("\n\n", "{}").expect("fake");
        assert_eq!(
            online(&fake.endpoints(), &CancellationToken::new()).await,
            Online::default()
        );
        let mut endpoints = fake.endpoints();
        drop(fake);
        endpoints.v4 = endpoints.ipinfo.clone();
        assert_eq!(
            online(&endpoints, &CancellationToken::new()).await,
            Online::default()
        );
        assert_eq!(
            isp(&endpoints, &CancellationToken::new()).await,
            Isp::default()
        );
    }

    #[tokio::test]
    async fn https_only_refuses_a_plain_http_service() {
        let fake = NetworkFake::start("ADDRESS\n", r#"{"org":"AS1"}"#).expect("fake");
        let endpoints = Endpoints {
            http: false,
            ..fake.endpoints()
        };
        assert_eq!(
            online(&endpoints, &CancellationToken::new()).await,
            Online::default()
        );
        assert_eq!(fake.seen(), Vec::new());
    }

    #[tokio::test]
    async fn ipinfo_gives_its_four_fields_and_nothing_else() {
        let fake = NetworkFake::start(
            "x",
            r#"{"ip":"ADDRESS","hostname":"h","city":"Falkenstein","region":"Saxony",
                "country":"DE","loc":"1,2","org":"AS1 Example","postal":"1","timezone":"t"}"#,
        )
        .expect("fake");
        assert_eq!(
            isp(&fake.endpoints(), &CancellationToken::new()).await,
            Isp {
                org: Some("AS1 Example".to_owned()),
                city: Some("Falkenstein".to_owned()),
                country: Some("DE".to_owned()),
                region: Some("Saxony".to_owned()),
            }
        );
        // Newlines alone are nothing; spaces are text, as `-n` has it.
        let fake =
            NetworkFake::start("x", r#"{"org":"\n","city":"c","region":"  "}"#).expect("fake");
        assert_eq!(
            isp(&fake.endpoints(), &CancellationToken::new()).await,
            Isp {
                city: Some("c".to_owned()),
                region: Some("  ".to_owned()),
                ..Isp::default()
            }
        );
        let fake = NetworkFake::start("x", "rate limited").expect("fake");
        assert_eq!(
            isp(&fake.endpoints(), &CancellationToken::new()).await,
            Isp::default()
        );
    }

    #[tokio::test]
    async fn spaces_answer_online_and_a_long_answer_is_refused() {
        let fake = NetworkFake::start("  \n", "{}").expect("fake");
        assert_eq!(
            online(&fake.endpoints(), &CancellationToken::new()).await,
            Online { v4: true, v6: true }
        );
        let long = "x".repeat(MAX_ANSWER + 1);
        let fake = NetworkFake::start(&long, &format!(r#"{{"org":"{long}"}}"#)).expect("fake");
        assert_eq!(
            online(&fake.endpoints(), &CancellationToken::new()).await,
            Online::default()
        );
        assert_eq!(
            isp(&fake.endpoints(), &CancellationToken::new()).await,
            Isp::default()
        );
        let fits = "x".repeat(MAX_ANSWER);
        let fake = NetworkFake::start(&fits, "{}").expect("fake");
        assert_eq!(
            online(&fake.endpoints(), &CancellationToken::new()).await,
            Online { v4: true, v6: true }
        );
    }

    #[tokio::test]
    async fn a_cancelled_check_asks_nothing() {
        let fake = NetworkFake::start("ADDRESS\n", r#"{"org":"AS1"}"#).expect("fake");
        let token = CancellationToken::new();
        token.cancel();
        assert_eq!(online(&fake.endpoints(), &token).await, Online::default());
        assert_eq!(isp(&fake.endpoints(), &token).await, Isp::default());
    }
}
