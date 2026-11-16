//! The secret canary test (motd spec, Testing, "Secrets"; success
//! criterion 6; CONSTRAINTS.md "Logging"): a motd.yml whose every
//! credential is a distinct canary (short, long, one with `"` and `\`
//! that serde escapes, and some with characters that percent-encode),
//! fakes that echo them back where a provider might show them (a JSON
//! value serde quotes in its error, a lowercase-escaped form, a redirect
//! carrying one), `SABnzbd`'s first instance at a refused port and its
//! second at a fake that echoes its key. No canary, raw or encoded,
//! reaches the report or the run log. The queues, qBittorrent and
//! rTorrent (M45) are covered as the media servers are. A test binary of its own: tracing
//! caches each callsite's interest process-wide
//! (<https://docs.rs/tracing-core/0.1.36/tracing_core/callsite/index.html>).

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sb_cli_core::theme::Theme;
use sb_config::http::{Http, Proxy};
use sb_config::testing::{Answer, Fake};
use sb_motd::{Colours, DockerSettings, Options, Section, Sources};

/// A log writer the test reads back.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("lock")).into_owned()
    }
}

impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("lock").extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The canaries, one per credential.
const PLEX_TOKEN: &str = "PlexCanary\"Token\\-0123456789abcdefghijklmnop";
const EMBY_TOKEN: &str = "Ek7q";
const JELLYFIN_TOKEN: &str = "jf canary/+&=?#%~é";
const SABNZBD_KEY: &str = "sab canary&key=1";
const NZBGET_PASSWORD: &str = "nzb-Canary-Pass:@/";
const SONARR_KEY: &str = "sonarrcanarykey0123456789abcdef";
const QBITTORRENT_PASSWORD: &str = "qb canary=pass&x";
const RTORRENT_PASSWORD: &str = "rt\\canary\"pass";

/// Each canary in the forms a text could carry it: raw, as a query
/// parameter and in a path, each with upper- and lowercase escapes.
fn forms(canary: &str) -> Vec<String> {
    let mut url = reqwest::Url::parse("http://encode.invalid/").expect("url");
    url.query_pairs_mut().append_pair("k", canary);
    let query = url.query().expect("query")["k=".len()..].to_string();
    url.set_path(&format!("/{canary}"));
    let path = url.path()[1..].to_string();
    let mut forms = vec![canary.to_string()];
    for form in [query, path] {
        forms.push(lower_escapes(&form));
        forms.push(form);
    }
    // As serde's errors quote it.
    let debug = format!("{canary:?}");
    forms.push(debug[1..debug.len() - 1].to_string());
    // rTorrent's Basic login.
    if canary == RTORRENT_PASSWORD {
        forms.push(basic_token("rt-user", canary));
    }
    forms.retain(|form| form.len() >= 4);
    forms
}

/// `form` with each `%XX` escape's hex in lowercase.
fn lower_escapes(form: &str) -> String {
    let mut out = String::new();
    let mut left = 0;
    for (at, c) in form.char_indices() {
        if at < left {
            continue;
        }
        if c == '%' && form.len() >= at + 3 {
            out.push('%');
            out.push_str(&form[at + 1..at + 3].to_ascii_lowercase());
            left = at + 3;
        } else {
            out.push(c);
        }
    }
    out
}

/// `text` holds none of the canaries' forms.
fn assert_clean(what: &str, text: &str) {
    for canary in [
        PLEX_TOKEN,
        EMBY_TOKEN,
        JELLYFIN_TOKEN,
        SABNZBD_KEY,
        NZBGET_PASSWORD,
        SONARR_KEY,
        QBITTORRENT_PASSWORD,
        RTORRENT_PASSWORD,
    ] {
        for form in forms(canary) {
            assert!(!text.contains(&form), "{what} shows {form:?}:\n{text}");
        }
    }
}

/// A YAML double-quoted string of `text` (JSON's escapes are YAML's).
fn quoted(text: &str) -> String {
    serde_json::to_string(text).expect("json")
}

/// A Saltbox root with the pinned schema and `motd`.
fn root(motd: &str) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(root.path().join("schema")).expect("schema dir");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../config/tests/fixtures/saltbox-schemas/motd.schema.yml"),
        root.path().join("schema/motd.schema.yml"),
    )
    .expect("schema");
    std::fs::write(root.path().join("motd.yml"), motd).expect("motd.yml");
    root
}

/// A loopback URL nothing listens on: a port bound, read back, freed.
fn refused() -> String {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    let address = listener.local_addr().expect("address");
    drop(listener);
    format!("http://{address}")
}

/// The fakes' answers: each echoes a credential where a provider could
/// show it.
fn script(fake: &Fake) {
    // serde quotes a string it can't take as the type it wants. Plex's
    // URL below carries a query, so sb-go's resolution of
    // `status/sessions` against it drops `/plex`, as here.
    fake.script(
        "/status/sessions",
        Answer::json(
            200,
            format!(r#"{{"MediaContainer":{{"size":{}}}}}"#, quoted(PLEX_TOKEN)),
        ),
    );
    fake.script(
        "/emby/emby/Sessions",
        Answer::json(
            200,
            format!(r#"[{{"PlayState":{{"IsPaused":{}}}}}]"#, quoted(EMBY_TOKEN)),
        ),
    );
    // A redirect carrying the token (never followed) and a body
    // echoing it.
    let mut location = reqwest::Url::parse(fake.url()).expect("url");
    location.set_path("/elsewhere");
    location
        .query_pairs_mut()
        .append_pair("api_key", JELLYFIN_TOKEN);
    fake.script(
        "/jellyfin/Sessions",
        Answer::new(302, format!("moved, token {JELLYFIN_TOKEN}"))
            .header("location", location.as_str()),
    );
    // The password as a server might echo it from a URL, escapes in
    // lowercase.
    let lowered = forms(NZBGET_PASSWORD)
        .into_iter()
        .find(|form| form.contains("%3a") && form.contains("Canary"))
        .expect("a lowercase form");
    fake.script(
        "/nzbget/jsonrpc",
        Answer::json(
            200,
            format!(r#"{{"result":{{"ServerPaused":{}}}}}"#, quoted(&lowered)),
        ),
    );
    fake.script(
        "/sabnzbd/api",
        Answer::json(
            200,
            format!(
                r#"{{"queue":{{"noofslots_total":{}}}}}"#,
                quoted(SABNZBD_KEY)
            ),
        ),
    );
    // Sonarr's error body echoes its key, as starr's text shows a body.
    fake.script(
        "/sonarr/api/v3/queue",
        Answer::new(401, format!("bad key {SONARR_KEY}")),
    );
    // qBittorrent logs in, then answers its main data with the password
    // where serde quotes it.
    fake.script(
        "/qbittorrent/api/v2/auth/login",
        Answer::new(200, "Ok.").header("set-cookie", "SID=s; path=/"),
    );
    fake.script(
        "/qbittorrent/api/v2/sync/maindata",
        Answer::json(
            200,
            format!(
                r#"{{"server_state":{{"dl_info_speed":{}}}}}"#,
                quoted(QBITTORRENT_PASSWORD)
            ),
        ),
    );
    // rTorrent answers with a fault that echoes its password and the
    // request's Basic `Authorization` header.
    fake.script(
        "/rtorrent/RPC2",
        Answer::new(
            200,
            format!(
                "<methodResponse><fault><value><struct><member><name>faultCode</name><value><i4>-1</i4></value></member><member><name>faultString</name><value><string>{} Authorization: Basic {}</string></value></member></struct></value></fault></methodResponse>",
                RTORRENT_PASSWORD.replace('&', "&amp;").replace('"', "&quot;"),
                basic_token("rt-user", RTORRENT_PASSWORD)
            ),
        ),
    );
}

/// The Basic token of `user` and `password`, as reqwest encodes it.
fn basic_token(user: &str, password: &str) -> String {
    let request = reqwest::Client::new()
        .get("http://login.invalid/")
        .basic_auth(user, Some(password))
        .build()
        .expect("request");
    let value = request.headers()[reqwest::header::AUTHORIZATION]
        .to_str()
        .expect("text")
        .to_string();
    value["Basic ".len()..].to_string()
}

/// motd.yml with one instance per provider, each with its canary.
fn motd_yml(fake: &Fake) -> String {
    let base = fake.url();
    // NZBGet's URL carries its login too, as a user might write it; the
    // log must drop it.
    let nzbget_url = {
        let mut url = reqwest::Url::parse(&format!("{base}/nzbget")).expect("url");
        url.set_username("nzb-user").expect("user");
        url.set_password(Some(NZBGET_PASSWORD)).expect("password");
        url.to_string()
    };
    format!(
        "plex:\n  instances:\n    - name: Plex\n      url: {plex}\n      token: {plex_token}\n\
         emby:\n  instances:\n    - name: Emby\n      url: {base}/emby\n      token: {emby_token}\n\
         jellyfin:\n  instances:\n    - name: Jellyfin\n      url: {base}/jellyfin\n      token: {jellyfin_token}\n\
         sabnzbd:\n  instances:\n    - name: SABnzbd\n      url: {sabnzbd}\n      apikey: {sabnzbd_key}\n    - name: SABnzbd 2\n      url: {base}/sabnzbd\n      apikey: {sabnzbd_key}\n\
         nzbget:\n  instances:\n    - name: NZBGet\n      url: {nzbget}\n      user: nzb-user\n      password: {nzbget_password}\n\
         sonarr:\n  instances:\n    - name: Sonarr\n      url: {base}/sonarr\n      apikey: {sonarr_key}\n\
         qbittorrent:\n  instances:\n    - name: qBittorrent\n      url: {base}/qbittorrent\n      user: qb-user\n      password: {qbittorrent_password}\n\
         rtorrent:\n  instances:\n    - name: rTorrent\n      url: {base}/rtorrent/RPC2\n      user: rt-user\n      password: {rtorrent_password}\n",
        plex = quoted(&{
            let mut url = reqwest::Url::parse(&format!("{base}/plex")).expect("url");
            url.query_pairs_mut()
                .append_pair("X-Plex-Token", PLEX_TOKEN);
            url.to_string()
        }),
        plex_token = quoted(PLEX_TOKEN),
        emby_token = quoted(EMBY_TOKEN),
        jellyfin_token = quoted(JELLYFIN_TOKEN),
        sabnzbd = quoted(&refused()),
        sabnzbd_key = quoted(SABNZBD_KEY),
        nzbget = quoted(&nzbget_url),
        nzbget_password = quoted(NZBGET_PASSWORD),
        sonarr_key = quoted(SONARR_KEY),
        qbittorrent_password = quoted(QBITTORRENT_PASSWORD),
        rtorrent_password = quoted(RTORRENT_PASSWORD),
    )
}

/// The report of every HTTP section for the Saltbox root `root`, and the
/// log it wrote at every level of every target (stricter than the run
/// log's filter).
fn report_and_log(root: &Path) -> (String, String) {
    let settings = sb_config::run::Settings {
        root: root.to_path_buf(),
        zoneinfo: Vec::new(),
        http: Http::new(&Proxy::Off, Duration::from_secs(1)).expect("client"),
        cloudflare_url: String::new(),
        dockerhub_url: String::new(),
        backoff: sb_config::cloudflare::backoff,
    };
    let theme = Theme::plain();
    let options = Options {
        all: false,
        sections: vec![
            Section::Plex,
            Section::Emby,
            Section::Jellyfin,
            Section::Sabnzbd,
            Section::Nzbget,
            Section::Queues,
            Section::Qbittorrent,
            Section::Rtorrent,
        ],
        ..Options::default()
    };
    let sources = Sources {
        docker: DockerSettings::default(),
        systemd: sb_host::systemd::Systemd::new().with_program("/nonexistent/systemctl"),
        banner: sb_motd::banner::Tools::system(),
    };
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .finish();
    let report = tracing::subscriber::with_default(subscriber, || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(sb_motd::report_from(
                &options,
                Colours {
                    theme: &theme,
                    on: false,
                },
                sources,
                || sb_motd::config::load(&settings),
            ))
    })
    .expect("a report");
    (report.text, captured.text())
}

#[test]
fn no_credential_reaches_the_report_or_the_run_log() {
    let fake = Fake::start().expect("fake");
    script(&fake);
    let motd = motd_yml(&fake);
    let root = root(&motd);
    let (report, log) = report_and_log(root.path());
    // Each provider answered with an error line, so each path ran.
    for key in [
        "SABnzbd:",
        "NZBGet:",
        "Plex:",
        "Emby:",
        "Jellyfin:",
        "Download Queues:",
        "qBittorrent:",
        "rTorrent:",
    ] {
        let line = report
            .lines()
            .find(|line| line.starts_with(key))
            .unwrap_or_else(|| panic!("no {key} line:\n{report}"));
        assert!(line.contains("Error: "), "{line}");
    }
    assert!(report.contains("[REDACTED]"), "{report}");
    // Each new provider's line had its canary to hide.
    for key in ["Download Queues:", "qBittorrent:", "rTorrent:"] {
        let line = report
            .lines()
            .find(|line| line.starts_with(key))
            .unwrap_or_default();
        assert!(line.contains("[REDACTED]"), "{key} {line}");
    }
    assert!(report.contains("302 Found"), "{report}");
    assert!(report.contains("SABnzbd 2:"), "{report}");
    // SABnzbd's second instance got its key in the query.
    assert!(
        fake.seen()
            .iter()
            .any(|seen| seen.target.starts_with("/sabnzbd/api?apikey=")),
        "{:?}",
        fake.seen()
    );
    for app in [
        "plex",
        "emby",
        "jellyfin",
        "sabnzbd",
        "nzbget",
        "sonarr",
        "qbittorrent",
        "rtorrent",
    ] {
        assert!(
            log.lines()
                .any(|line| line.contains("motd instance")
                    && line.contains(&format!("app=\"{app}\""))),
            "{app}: {log}"
        );
    }
    assert_clean("the report", &report);
    assert_clean("the run log", &log);
    // The redirect wasn't followed.
    assert!(
        fake.seen()
            .iter()
            .all(|seen| !seen.target.starts_with("/elsewhere"))
    );
}
