//! Plex (sb-go's `motd/plex.go`): `GET <url>/status/sessions` with
//! `X-Plex-Token`, JSON, the sessions counted by how they play; a paused
//! session isn't active (Difference 14).

use std::sync::Arc;

use serde::Deserialize;

use super::{Answer, DEFAULT_TIMEOUT, Shared, Summary, Texts, send, streams};
use crate::config::{AppSection, Instance};
use crate::{Out, Paint};

const TEXTS: Texts = Texts {
    app: "Plex",
    api: "plex",
    read: "response",
};

/// Plex's section value.
pub async fn plex(section: Option<&AppSection>, shared: &Arc<Shared>, paint: &Paint) -> Out {
    let paint_each = paint.clone();
    let answers = super::ask_each(
        "plex",
        section,
        "Plex",
        shared,
        super::has_token,
        move |shared, instance| {
            let paint = paint_each.clone();
            async move { ask(&shared, &instance, &paint).await }
        },
    )
    .await;
    super::listing(answers, paint)
}

/// The answer of `/status/sessions` (only the fields counted).
#[derive(Debug, Default, Deserialize)]
struct Sessions {
    #[serde(rename = "MediaContainer")]
    container: Option<Container>,
}

#[derive(Debug, Default, Deserialize)]
struct Container {
    size: Option<i64>,
    #[serde(rename = "Metadata")]
    metadata: Option<Vec<Session>>,
}

#[derive(Debug, Default, Deserialize)]
struct Session {
    #[serde(rename = "Media")]
    media: Option<Vec<Media>>,
    #[serde(rename = "TranscodeSession")]
    transcode: Option<Transcode>,
    #[serde(rename = "Player")]
    player: Option<Player>,
}

#[derive(Debug, Default, Deserialize)]
struct Media {
    decision: Option<String>,
    #[serde(rename = "Part")]
    part: Option<Vec<Part>>,
}

#[derive(Debug, Default, Deserialize)]
struct Part {
    decision: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Transcode {
    #[serde(rename = "videoDecision")]
    video: Option<String>,
    #[serde(rename = "audioDecision")]
    audio: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Player {
    state: Option<String>,
}

/// One instance's summary or error.
async fn ask(shared: &Shared, instance: &Instance, paint: &Paint) -> Answer {
    fetch(shared, instance)
        .await
        .map(|counts| Summary {
            text: counts.summary(paint),
            value: counts.value(),
        })
        .map_err(|err| shared.error_text(&err))
}

/// The sessions, counted.
async fn fetch(shared: &Shared, instance: &Instance) -> Result<Counts, String> {
    // sb-go: the base with a `/`, then `status/sessions` resolved
    // against it.
    let mut base = instance.url.clone();
    if !base.ends_with('/') {
        base.push('/');
    }
    let url = reqwest::Url::parse(&base)
        .and_then(|base| base.join("status/sessions"))
        .map_err(|err| format!("invalid URL: {err}"))?;
    let token = super::secret_header(instance.token.as_deref().unwrap_or_default())?;
    let body = send(
        shared,
        |client| {
            client
                .get(url)
                .header("X-Plex-Token", token)
                .header("Accept", "application/json")
        },
        shared.timeout(instance, DEFAULT_TIMEOUT),
        TEXTS,
    )
    .await?;
    let sessions: Sessions = serde_json::from_slice(&body)
        .map_err(|err| format!("failed to parse Plex response: {err}"))?;
    Ok(Counts::of(sessions))
}

/// Active sessions by how they play.
#[derive(Debug, Default, PartialEq, Eq)]
struct Counts {
    active: usize,
    direct_play: usize,
    transcode: usize,
    direct_stream: usize,
    other: usize,
}

impl Counts {
    /// sb-go's counting: the container's `size` as the active count, each
    /// session by its transcode decisions, then its first media's part's
    /// and the media's own decision. A paused session counts nowhere
    /// (Difference 14).
    fn of(sessions: Sessions) -> Self {
        let container = sessions.container.unwrap_or_default();
        let mut counts = Counts::default();
        let mut paused = 0;
        for session in container.metadata.unwrap_or_default() {
            let state = session.player.and_then(|p| p.state);
            if state.as_deref() == Some("paused") {
                paused += 1;
                continue;
            }
            let transcode = session.transcode.unwrap_or_default();
            let is = |value: &Option<String>, want| value.as_deref() == Some(want);
            if is(&transcode.video, "transcode") || is(&transcode.audio, "transcode") {
                counts.transcode += 1;
                continue;
            }
            let Some(media) = session.media.and_then(|m| m.into_iter().next()) else {
                counts.other += 1;
                continue;
            };
            let part = media
                .part
                .and_then(|p| p.into_iter().next())
                .and_then(|p| p.decision);
            if part.as_deref() == Some("directplay") || is(&media.decision, "directplay") {
                counts.direct_play += 1;
            } else if part.as_deref() == Some("directstream") || is(&media.decision, "directstream")
            {
                counts.direct_stream += 1;
            } else {
                counts.other += 1;
            }
        }
        let size = usize::try_from(container.size.unwrap_or(0)).unwrap_or(0);
        counts.active = size.saturating_sub(paused);
        counts
    }

    /// sb-go's `formatPlexStreamSummary`.
    fn summary(&self, paint: &Paint) -> String {
        if self.active == 0 {
            return "No active streams".to_string();
        }
        streams(paint, self.active, &self.kinds())
    }

    /// The kinds the line counts, in its order.
    fn kinds(&self) -> [(usize, &'static str); 4] {
        [
            (self.direct_play, "direct play"),
            (self.transcode, "transcode"),
            (self.direct_stream, "direct stream"),
            (self.other, "other"),
        ]
    }

    /// The `--json` value (motd spec, `--json`).
    fn value(&self) -> serde_json::Value {
        super::streams_value(self.active, &self.kinds())
    }
}

#[cfg(test)]
mod tests {
    use sb_config::testing::{Answer as Reply, Fake};

    use super::*;
    use crate::providers::tests::{painted, refused, section, shared, with_token};

    /// Every way a session plays, sb-go's order of checks, and a paused
    /// one counted nowhere (Difference 14).
    const SESSIONS: &str = r#"{"MediaContainer":{"size":7,"Metadata":[
        {"TranscodeSession":{"videoDecision":"transcode"},"Media":[{"decision":"directplay"}]},
        {"TranscodeSession":{"audioDecision":"transcode"}},
        {"Media":[{"decision":"directstream","Part":[{"decision":"directplay"}]}]},
        {"Media":[{"decision":"directplay","Part":[{"decision":"directstream"}]}]},
        {"Media":[{"Part":[{"decision":"directstream"}]}]},
        {"Media":[{"decision":"copy"}],"Player":{"state":"playing"}},
        {"Media":[{"decision":"directplay"}],"Player":{"state":"paused"}}
    ]}}"#;

    #[tokio::test]
    async fn plex_asks_status_sessions_with_its_token_header() {
        let fake = Fake::start().unwrap();
        fake.script("/plex/status/sessions", Reply::json(200, SESSIONS));
        let url = format!("{}/plex", fake.url());
        let one = section(vec![with_token("Plex", &url, "plex-token-canary")]);
        let out = plex(Some(&one), &shared(&["plex-token-canary"]), &Paint::plain()).await;
        assert_eq!(
            out.text,
            "6 active streams (2 direct play, 2 transcode, 1 direct stream, 1 other)"
        );
        // `--json` (motd spec): the counts by kind.
        assert_eq!(
            out.json,
            crate::Json::Instances(vec![(
                "Plex".to_string(),
                Ok(serde_json::json!({
                    "active": 6,
                    "direct_play": 2,
                    "transcode": 2,
                    "direct_stream": 1,
                    "other": 1,
                }))
            )])
        );
        let seen = fake.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].method, "GET");
        assert_eq!(seen[0].target, "/plex/status/sessions");
        assert_eq!(seen[0].header("x-plex-token"), Some("plex-token-canary"));
        assert_eq!(seen[0].header("accept"), Some("application/json"));
    }

    #[test]
    fn counts_read_as_sb_gos() {
        let counts = |json: &str| Counts::of(serde_json::from_str(json).unwrap());
        assert_eq!(counts("{}"), Counts::default());
        assert_eq!(counts(r#"{"MediaContainer":null}"#), Counts::default());
        let one = counts(r#"{"MediaContainer":{"size":1,"Metadata":[{"Media":[]}]}}"#);
        assert_eq!((one.active, one.other), (1, 1));
        let plain = Paint::plain();
        assert_eq!(Counts::default().summary(&plain), "No active streams");
        assert_eq!(one.summary(&plain), "1 active stream (1 other)");
        let paint = painted();
        assert_eq!(
            one.summary(&paint),
            format!(
                "{} active stream ({} other)",
                paint.value("1"),
                paint.value("1")
            )
        );
        // All paused: nothing active.
        let paused =
            counts(r#"{"MediaContainer":{"size":1,"Metadata":[{"Player":{"state":"paused"}}]}}"#);
        assert_eq!(paused, Counts::default());
    }

    /// A status other than 200, a body that isn't the answer (its text
    /// hidden where serde echoes it), a refused port, a bad URL.
    #[tokio::test]
    async fn plex_errors_are_sb_gos() {
        let fake = Fake::start().unwrap();
        fake.script(
            "/a/status/sessions",
            Reply::new(401, "token plex-token-canary refused"),
        );
        fake.script(
            "/b/status/sessions",
            Reply::json(200, r#"{"MediaContainer":{"size":"plex-token-canary"}}"#),
        );
        let shared = shared(&["plex-token-canary"]);
        let ask = |url: String| {
            let shared = Arc::clone(&shared);
            async move {
                let one = section(vec![with_token("Plex", &url, "plex-token-canary")]);
                plex(Some(&one), &shared, &Paint::plain()).await.text
            }
        };
        assert_eq!(
            ask(format!("{}/a", fake.url())).await,
            "Error: plex API returned status code 401"
        );
        let text = ask(format!("{}/b", fake.url())).await;
        assert!(
            text.starts_with("Error: failed to parse Plex response: "),
            "{text}"
        );
        assert!(!text.contains("canary"), "{text}");
        assert!(text.contains("[REDACTED]"), "{text}");
        let text = ask(refused()).await;
        assert!(
            text.starts_with("Error: failed to connect to Plex: "),
            "{text}"
        );
        assert!(
            ask("not a url".into())
                .await
                .starts_with("Error: invalid URL: ")
        );
    }

    /// No section, a disabled one, and instances without a URL or token
    /// show nothing and ask nothing.
    #[tokio::test]
    async fn nothing_to_ask_hides_the_section() {
        let shared = shared(&[]);
        assert_eq!(plex(None, &shared, &Paint::plain()).await.text, "");
        let mut off = section(vec![with_token("Plex", "http://x", "t")]);
        off.enabled = false;
        assert_eq!(plex(Some(&off), &shared, &Paint::plain()).await.text, "");
        let unusable = section(vec![
            with_token("Plex", "", "t"),
            with_token("Plex", "http://x", ""),
            crate::providers::tests::instance("Plex", "http://x"),
        ]);
        assert_eq!(
            plex(Some(&unusable), &shared, &Paint::plain()).await.text,
            ""
        );
    }
}
