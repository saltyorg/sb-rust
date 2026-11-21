//! qBittorrent (sb-go's `motd/qbittorrent.go`, through go-qbittorrent's
//! `LoginCtx` and `SyncMainDataCtx`): `POST <url>/api/v2/auth/login` with
//! the user and password as a form (1 s), its session cookies sent with
//! `GET <url>/api/v2/sync/maindata?rid=0` (the instance's `timeout`, or
//! sb-go's 20 s), every torrent counted by its state (Difference 12: the
//! states sb-go counted nowhere go to their nearest group). The login's
//! session is the only state a request creates, as in sb-go. No retry and
//! no new login after a refused main data, where go-qbittorrent retries
//! (Difference 27).
//!
//! Library-backed: the form body is the `url` project's
//! `form_urlencoded` serializer
//! (<https://docs.rs/form_urlencoded/1.2.2/form_urlencoded/struct.Serializer.html>;
//! why not reqwest's `form`: a feature off in sb's build, for one body);
//! the main data's torrents are counted as serde reads them, only the
//! counts kept, on tokio's blocking pool through cli-core's `spawn::blocking`
//! (the run's span carried), as the answer can be large
//! (<https://docs.rs/serde/1.0.228/serde/de/trait.Visitor.html#method.visit_map>,
//! <https://docs.rs/tokio/1.54.1/tokio/task/fn.spawn_blocking.html>).
//! The session's cookies are read from the login's `Set-Cookie` headers
//! and every `name=value` sent back, as a cookie jar does for one host
//! (RFC 6265 §5.4): qBittorrent's name is `SID` up to 5.1 and
//! `QBT_SID_<port>` from 5.2. Why not reqwest's `cookies` feature and its
//! store: it adds `cookie_store`, and one store on the run's one client
//! would mix two qBittorrents on one host (cookies aren't kept apart by
//! port).

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

use super::{Answer, DEFAULT_TIMEOUT, Shared, Summary, bytes, rate_bytes, torrents_summary};
use crate::config::{AppSection, Instance};
use crate::{Out, Paint};

/// sb-go's limit for the main data, after the 1 s login.
pub const DATA_TIMEOUT: Duration = Duration::from_secs(20);

/// The main data lists every torrent (about 1.5 KB each): it is read up to
/// this many bytes (about 40,000 torrents), not sb-config's 1 MiB.
pub const MAX_DATA: usize = 64 << 20;

/// qBittorrent's section value.
pub async fn qbittorrent(section: Option<&AppSection>, shared: &Arc<Shared>, paint: &Paint) -> Out {
    let paint_each = paint.clone();
    let answers = super::ask_each(
        "qbittorrent",
        section,
        "qBittorrent",
        shared,
        |instance| {
            !instance.url.is_empty()
                && super::is_set(instance.user.as_ref())
                && super::is_set(instance.password.as_ref())
        },
        move |shared, instance| {
            let paint = paint_each.clone();
            async move { ask(&shared, &instance, &paint).await }
        },
    )
    .await;
    super::listing(answers, paint)
}

async fn ask(shared: &Shared, instance: &Instance, paint: &Paint) -> Answer {
    fetch(shared, instance)
        .await
        .map(|counts| Summary {
            text: counts.summary(paint),
            value: counts.value(),
        })
        .map_err(|err| shared.error_text(&err))
}

#[derive(Debug, Default, Deserialize)]
struct MainData {
    server_state: Option<ServerState>,
    torrents: Option<Groups>,
}

#[derive(Debug, Default, Deserialize)]
struct ServerState {
    dl_info_speed: Option<i64>,
    up_info_speed: Option<i64>,
}

/// One torrent, its state alone read.
#[derive(Debug, Default, Deserialize)]
struct Torrent {
    state: Option<String>,
}

/// The torrents counted by group as they are read (downloading, seeding,
/// stopped, errored): nothing of a torrent is kept.
#[derive(Debug, Default, PartialEq, Eq)]
struct Groups([usize; 4]);

impl<'de> Deserialize<'de> for Groups {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Counting;
        impl<'de> Visitor<'de> for Counting {
            type Value = Groups;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a map of torrents")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Groups, A::Error> {
                let mut groups = [0; 4];
                while let Some((IgnoredAny, torrent)) = map.next_entry::<IgnoredAny, Torrent>()? {
                    groups[group(torrent.state.as_deref().unwrap_or_default())] += 1;
                }
                Ok(Groups(groups))
            }
        }
        deserializer.deserialize_map(Counting)
    }
}

/// A state's group, sb-go's (Difference 12): `moving` and `allocating`
/// downloading, `checkingResumeData` stopped, `unknown` and any state
/// qBittorrent adds later an error.
fn group(state: &str) -> usize {
    match state {
        "downloading" | "forcedDL" | "metaDL" | "checkingDL" | "queuedDL" | "stalledDL"
        | "moving" | "allocating" => 0,
        "uploading" | "forcedUP" | "checkingUP" | "queuedUP" | "stalledUP" => 1,
        "pausedDL" | "pausedUP" | "stoppedUP" | "stoppedDL" | "checkingResumeData" => 2,
        _ => 3,
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Counts {
    downloading: usize,
    seeding: usize,
    stopped: usize,
    errored: usize,
    down: i64,
    up: i64,
}

impl Counts {
    fn of(data: MainData) -> Self {
        let state = data.server_state.unwrap_or_default();
        let Groups([downloading, seeding, stopped, errored]) = data.torrents.unwrap_or_default();
        Counts {
            downloading,
            seeding,
            stopped,
            errored,
            down: state.dl_info_speed.unwrap_or(0),
            up: state.up_info_speed.unwrap_or(0),
        }
    }

    fn summary(&self, paint: &Paint) -> String {
        let speed = |rate: i64| format!("{}/s", bytes(rate_bytes(rate)));
        torrents_summary(
            paint,
            [self.downloading, self.seeding, self.stopped, self.errored],
            &speed(self.down),
            &speed(self.up),
        )
    }

    /// The `--json` value (motd spec, `--json`).
    fn value(&self) -> serde_json::Value {
        let speed = |rate: i64| format!("{}/s", bytes(rate_bytes(rate)));
        super::torrents_value(
            [self.downloading, self.seeding, self.stopped, self.errored],
            &speed(self.down),
            &speed(self.up),
        )
    }
}

/// `<url>/api/v2/<endpoint>`, as go-qbittorrent's `url.JoinPath` builds
/// it.
fn api_url(base: &str, endpoint: &str) -> Result<reqwest::Url, String> {
    let mut url = reqwest::Url::parse(base).map_err(|err| format!("invalid URL: {err}"))?;
    let path = format!("{}/api/v2/{endpoint}", url.path().trim_end_matches('/'));
    url.set_path(&path);
    Ok(url)
}

/// A `Set-Cookie` header's `name=value`: what comes before its first
/// `;`, when it has a name.
fn cookie(header: &str) -> Option<&str> {
    let pair = header.split(';').next()?.trim();
    let (key, _) = pair.split_once('=')?;
    (!key.trim().is_empty()).then_some(pair)
}

/// The session's cookies after a login (`a=1; b=2`), none when the login
/// set none, or sb-go's text for a refused login.
async fn login(shared: &Shared, instance: &Instance) -> Result<Option<String>, String> {
    let failed = |why: String| format!("failed to login to qbittorrent: {why}");
    let url = api_url(&instance.url, "auth/login").map_err(failed)?;
    let body = form_urlencoded::Serializer::new(String::new())
        .append_pair("username", instance.user.as_deref().unwrap_or_default())
        .append_pair("password", instance.password.as_deref().unwrap_or_default())
        .finish();
    let client = shared.client().await.map_err(failed)?;
    let response = client
        .post(url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .timeout(shared.within(DEFAULT_TIMEOUT))
        .send()
        .await
        .map_err(|err| {
            failed(format!(
                "login error: {}",
                sb_config::http::error_chain(&err.without_url())
            ))
        })?;
    match response.status().as_u16() {
        403 => {
            return Err(failed(
                "User's IP is banned for too many failed login attempts".to_string(),
            ));
        }
        200 | 204 => {}
        code => {
            return Err(failed(format!(
                "login error; status code: {code}: unexpected status code"
            )));
        }
    }
    let pairs: Vec<String> = response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(cookie)
        .map(ToString::to_string)
        .collect();
    let text = sb_config::http::body(response).await.map_err(failed)?;
    let text = String::from_utf8_lossy(&text);
    if text == "Fails." || (pairs.is_empty() && text != "Ok.") {
        return Err(failed("bad credentials".to_string()));
    }
    Ok((!pairs.is_empty()).then(|| pairs.join("; ")))
}

async fn fetch(shared: &Shared, instance: &Instance) -> Result<Counts, String> {
    let session = login(shared, instance).await?;
    let failed = |why: String| format!("could not sync main data: {why}");
    let mut url = api_url(&instance.url, "sync/maindata").map_err(failed)?;
    url.query_pairs_mut().append_pair("rid", "0");
    let client = shared.client().await.map_err(failed)?;
    let mut request = client
        .get(url)
        .timeout(shared.timeout(instance, DATA_TIMEOUT));
    if let Some(session) = session {
        request = request.header(reqwest::header::COOKIE, super::secret_header(&session)?);
    }
    let response = request.send().await.map_err(|err| {
        failed(format!(
            "could not get main data: {}",
            sb_config::http::error_chain(&err.without_url())
        ))
    })?;
    let status = response.status().as_u16();
    if !matches!(status, 200 | 204) {
        return Err(failed(format!(
            "could not get main data; status code: {status}: unexpected status code"
        )));
    }
    let body = sb_config::http::body_within(response, MAX_DATA)
        .await
        .map_err(failed)?;
    let data = sb_cli_core::spawn::blocking(move || serde_json::from_slice::<MainData>(&body))
        .await
        .map_err(|err| failed(err.to_string()))?
        .map_err(|err| failed(format!("could not unmarshal body: {err}")))?;
    Ok(Counts::of(data))
}

#[cfg(test)]
mod tests {
    use sb_config::testing::{Answer as Reply, Fake};

    use super::*;
    use crate::providers::tests::{instance, painted, refused, section, shared};

    fn with_login(url: &str) -> Instance {
        Instance {
            user: Some("qb user".into()),
            password: Some("qb-pass&canary".into()),
            ..instance("qBittorrent", url)
        }
    }

    /// Every state qBittorrent has, sb-go's groups and Difference 12's.
    const STATES: [(&str, usize); 21] = [
        ("downloading", 0),
        ("forcedDL", 0),
        ("metaDL", 0),
        ("checkingDL", 0),
        ("queuedDL", 0),
        ("stalledDL", 0),
        ("moving", 0),
        ("allocating", 0),
        ("uploading", 1),
        ("forcedUP", 1),
        ("checkingUP", 1),
        ("queuedUP", 1),
        ("stalledUP", 1),
        ("pausedDL", 2),
        ("pausedUP", 2),
        ("stoppedUP", 2),
        ("stoppedDL", 2),
        ("checkingResumeData", 2),
        ("error", 3),
        ("missingFiles", 3),
        ("unknown", 3),
    ];

    fn maindata() -> String {
        let torrents: Vec<String> = STATES
            .iter()
            .enumerate()
            .map(|(n, (state, _))| format!(r#""h{n}":{{"state":"{state}","name":"t{n}"}}"#))
            .collect();
        format!(
            r#"{{"rid":1,"full_update":true,"server_state":{{"dl_info_speed":3145728,"up_info_speed":512}},"torrents":{{{}}}}}"#,
            torrents.join(",")
        )
    }

    /// The login's form and cookie, then main data with the cookie, every
    /// state counted.
    #[tokio::test]
    async fn qbittorrent_logs_in_then_reads_main_data() {
        let fake = Fake::start().unwrap();
        fake.script(
            "/qb/api/v2/auth/login",
            Reply::new(200, "Ok.").header("set-cookie", "SID=qb-session-canary; HttpOnly; path=/"),
        );
        fake.script("/qb/api/v2/sync/maindata", Reply::json(200, maindata()));
        let url = format!("{}/qb/", fake.url());
        let one = section(vec![with_login(&url)]);
        let out = qbittorrent(Some(&one), &shared(&[]), &Paint::plain()).await;
        let text = out.text.clone();
        // `--json` (motd spec): the same answer, typed.
        assert_eq!(
            crate::providers::tests::instances(&out),
            r#"[{"name":"qBittorrent","value":{"down":"3.0 MB/s","downloading":8,"errored":3,"seeding":5,"stopped":5,"up":"512 B/s"}}]"#
        );
        assert_eq!(
            text,
            "↓3.0 MB/s ↑512 B/s | 8 Downloading | 5 Seeding | 5 Stopped | 3 Error"
        );
        let seen = fake.seen();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].method, "POST");
        assert_eq!(seen[0].target, "/qb/api/v2/auth/login");
        assert_eq!(
            seen[0].header("content-type"),
            Some("application/x-www-form-urlencoded")
        );
        assert_eq!(seen[0].body, "username=qb+user&password=qb-pass%26canary");
        assert_eq!(seen[1].method, "GET");
        assert_eq!(seen[1].target, "/qb/api/v2/sync/maindata?rid=0");
        assert_eq!(seen[1].header("cookie"), Some("SID=qb-session-canary"));
    }

    /// qBittorrent 5.2 names its cookie `QBT_SID_<port>`: every cookie
    /// the login sets goes back.
    #[tokio::test]
    async fn every_cookie_the_login_sets_goes_back() {
        let fake = Fake::start().unwrap();
        fake.script(
            "/api/v2/auth/login",
            Reply::new(200, "Ok.").header("set-cookie", "QBT_SID_8080=qb52-session; HttpOnly"),
        );
        fake.script("/api/v2/sync/maindata", Reply::json(200, "{}"));
        let one = section(vec![with_login(fake.url())]);
        assert_eq!(
            qbittorrent(Some(&one), &shared(&[]), &Paint::plain())
                .await
                .text,
            "No torrents present"
        );
        let seen = fake.seen();
        assert_eq!(seen[1].header("cookie"), Some("QBT_SID_8080=qb52-session"));
    }

    /// A login that takes the connection and never answers ends at the
    /// fixed 1 s, whatever the instance's `timeout`; main data at the
    /// deadline when the `timeout` is longer.
    #[tokio::test]
    async fn a_hung_login_ends_at_one_second() {
        let fake = Fake::start().unwrap();
        fake.script("/api/v2/auth/login", Reply::new(200, "Ok.").held());
        let mut slow = with_login(fake.url());
        slow.timeout = Some(Duration::from_secs(5));
        let start = tokio::time::Instant::now();
        let text = qbittorrent(
            Some(&section(vec![slow.clone()])),
            &shared(&[]),
            &Paint::plain(),
        )
        .await
        .text;
        let took = start.elapsed();
        assert!(
            text.starts_with("Error: failed to login to qbittorrent: login error: "),
            "{text}"
        );
        assert!(
            took >= Duration::from_millis(900) && took < Duration::from_secs(3),
            "{took:?}"
        );
        let data = Fake::start().unwrap();
        data.script("/api/v2/auth/login", Reply::new(200, "Ok."));
        data.script("/api/v2/sync/maindata", Reply::json(200, "{}").held());
        slow.url = data.url().to_string();
        // The fake is plain http: a client without the roots asks it.
        let near = Shared::without_roots(
            sb_config::http::Proxy::Off,
            tokio::time::Instant::now() + Duration::from_millis(400),
            crate::redact::Secrets::default(),
        );
        let start = tokio::time::Instant::now();
        let text = qbittorrent(Some(&section(vec![slow])), &near, &Paint::plain())
            .await
            .text;
        assert!(
            text.starts_with("Error: could not sync main data: "),
            "{text}"
        );
        assert!(
            start.elapsed() < Duration::from_millis(1500),
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn every_state_is_counted_in_its_group() {
        for (state, group) in STATES {
            let data: MainData =
                serde_json::from_str(&format!(r#"{{"torrents":{{"h":{{"state":"{state}"}}}}}}"#))
                    .unwrap();
            let counts = Counts::of(data);
            let groups = [
                counts.downloading,
                counts.seeding,
                counts.stopped,
                counts.errored,
            ];
            let mut want = [0; 4];
            want[group] = 1;
            assert_eq!(groups, want, "{state}");
        }
        // A state qBittorrent adds later, or none, is an error.
        let data: MainData =
            serde_json::from_str(r#"{"torrents":{"a":{"state":"newState"},"b":{}}}"#).unwrap();
        assert_eq!(Counts::of(data).errored, 2);
        assert_eq!(Counts::of(MainData::default()), Counts::default());
        let paint = painted();
        let counts = Counts {
            seeding: 1,
            ..Counts::default()
        };
        assert_eq!(
            counts.summary(&paint),
            format!(
                "↓{} ↑{} | {} Downloading | {} Seeding",
                paint.value("0 B/s"),
                paint.value("0 B/s"),
                paint.value("0"),
                paint.value("1")
            )
        );
        assert_eq!(
            Counts::default().summary(&Paint::plain()),
            "No torrents present"
        );
    }

    #[test]
    fn the_cookie_and_the_urls() {
        assert_eq!(cookie("SID=abc; HttpOnly"), Some("SID=abc"));
        assert_eq!(cookie(" QBT_SID_8080=abc "), Some("QBT_SID_8080=abc"));
        assert_eq!(cookie("other=1; SID=x"), Some("other=1"));
        assert_eq!(cookie("SID"), None);
        assert_eq!(cookie("=x"), None);
        assert_eq!(
            api_url("http://qb:8080", "auth/login").unwrap().as_str(),
            "http://qb:8080/api/v2/auth/login"
        );
        assert_eq!(
            api_url("http://qb/x/", "sync/maindata").unwrap().as_str(),
            "http://qb/x/api/v2/sync/maindata"
        );
        assert!(api_url("qb", "x").unwrap_err().starts_with("invalid URL: "));
    }

    /// go-qbittorrent's refusals and errors, in sb-go's words, the
    /// password never shown.
    #[tokio::test]
    async fn qbittorrent_errors_are_go_qbittorrents() {
        let fake = Fake::start().unwrap();
        let shared = shared(&["qb-pass&canary"]);
        let ask = |path: &str| {
            let url = format!("{}{path}", fake.url());
            let shared = Arc::clone(&shared);
            async move {
                let one = section(vec![with_login(&url)]);
                qbittorrent(Some(&one), &shared, &Paint::plain()).await.text
            }
        };
        fake.script("/a/api/v2/auth/login", Reply::new(403, "banned"));
        assert_eq!(
            ask("/a").await,
            "Error: failed to login to qbittorrent: User's IP is banned for too many failed login attempts"
        );
        fake.script("/b/api/v2/auth/login", Reply::new(200, "Fails."));
        assert_eq!(
            ask("/b").await,
            "Error: failed to login to qbittorrent: bad credentials"
        );
        fake.script("/c/api/v2/auth/login", Reply::new(200, "odd"));
        assert_eq!(
            ask("/c").await,
            "Error: failed to login to qbittorrent: bad credentials"
        );
        fake.script("/d/api/v2/auth/login", Reply::new(500, ""));
        assert_eq!(
            ask("/d").await,
            "Error: failed to login to qbittorrent: login error; status code: 500: unexpected status code"
        );
        // "Ok." without a cookie (no auth needed): main data asked without.
        fake.script("/e/api/v2/auth/login", Reply::new(200, "Ok."));
        fake.script("/e/api/v2/sync/maindata", Reply::new(409, ""));
        assert_eq!(
            ask("/e").await,
            "Error: could not sync main data: could not get main data; status code: 409: unexpected status code"
        );
        let e = fake
            .seen()
            .into_iter()
            .rfind(|s| s.target.starts_with("/e/api/v2/sync"))
            .unwrap();
        assert_eq!(e.header("cookie"), None);
        fake.script("/f/api/v2/auth/login", Reply::new(200, "Ok."));
        fake.script(
            "/f/api/v2/sync/maindata",
            Reply::json(200, r#"{"torrents":["qb-pass&canary"]}"#),
        );
        let text = ask("/f").await;
        assert!(
            text.starts_with("Error: could not sync main data: could not unmarshal body: "),
            "{text}"
        );
        assert!(!text.contains("canary"), "{text}");
        let gone = section(vec![with_login(&refused())]);
        let text = qbittorrent(Some(&gone), &shared, &Paint::plain())
            .await
            .text;
        assert!(
            text.starts_with("Error: failed to login to qbittorrent: login error: "),
            "{text}"
        );
        let partial = section(vec![Instance {
            password: None,
            ..with_login("http://qb")
        }]);
        assert_eq!(
            qbittorrent(Some(&partial), &shared, &Paint::plain())
                .await
                .text,
            ""
        );
        let bad = section(vec![with_login("qb")]);
        assert!(
            qbittorrent(Some(&bad), &shared, &Paint::plain())
                .await
                .text
                .starts_with("Error: failed to login to qbittorrent: invalid URL: ")
        );
    }
}
