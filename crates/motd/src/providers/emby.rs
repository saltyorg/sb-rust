//! Emby (sb-go's `motd/emby.go`): `GET <url>/emby/Sessions` with
//! `X-Emby-Token`, the sessions playing and not paused counted by
//! `PlayState.PlayMethod`.

use std::sync::Arc;

use serde::Deserialize;

use super::{Answer, DEFAULT_TIMEOUT, Shared, Summary, Texts, send, streams};
use crate::config::{AppSection, Instance};
use crate::{Out, Paint};

const TEXTS: Texts = Texts {
    app: "Emby",
    api: "emby",
    read: "response body",
};

/// Emby's section value.
pub async fn emby(section: Option<&AppSection>, shared: &Arc<Shared>, paint: &Paint) -> Out {
    let paint_each = paint.clone();
    let answers = super::ask_each(
        "emby",
        section,
        "Emby",
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

/// One session (only the fields counted).
#[derive(Debug, Default, Deserialize)]
struct Session {
    #[serde(rename = "PlayState")]
    play_state: Option<PlayState>,
    /// Go's `map[string]any`: absent or `null` is no item.
    #[serde(rename = "NowPlayingItem")]
    now_playing: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Default, Deserialize)]
struct PlayState {
    #[serde(rename = "PlayMethod")]
    method: Option<String>,
    #[serde(rename = "IsPaused")]
    paused: Option<bool>,
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

async fn fetch(shared: &Shared, instance: &Instance) -> Result<Counts, String> {
    let url = format!("{}/emby/Sessions", instance.url.trim_end_matches('/'));
    let url =
        reqwest::Url::parse(&url).map_err(|err| format!("failed to create request: {err}"))?;
    let token = super::secret_header(instance.token.as_deref().unwrap_or_default())?;
    let body = send(
        shared,
        |client| {
            client
                .get(url)
                .header("Accept", "application/json")
                .header("X-Emby-Token", token)
        },
        shared.timeout(instance, DEFAULT_TIMEOUT),
        TEXTS,
    )
    .await?;
    let sessions: Vec<Session> = serde_json::from_slice(&body)
        .map_err(|err| format!("failed to parse Emby response: {err}"))?;
    Ok(Counts::of(sessions))
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Counts {
    active: usize,
    direct_play: usize,
    direct_stream: usize,
    transcode: usize,
}

impl Counts {
    /// sb-go's counting: a session with an item and not paused is
    /// active, by its play method, case ignored.
    fn of(sessions: Vec<Session>) -> Self {
        let mut counts = Counts::default();
        for session in sessions {
            let state = session.play_state.unwrap_or_default();
            if session.now_playing.is_none() || state.paused.unwrap_or(false) {
                continue;
            }
            counts.active += 1;
            match state.method.unwrap_or_default().to_lowercase().as_str() {
                "directplay" => counts.direct_play += 1,
                "directstream" => counts.direct_stream += 1,
                "transcode" => counts.transcode += 1,
                _ => {}
            }
        }
        counts
    }

    /// sb-go's `formatStreamSummary`.
    fn summary(&self, paint: &Paint) -> String {
        if self.active == 0 {
            return "No active streams".to_string();
        }
        streams(paint, self.active, &self.kinds())
    }

    /// The kinds the line counts, in its order.
    fn kinds(&self) -> [(usize, &'static str); 3] {
        [
            (self.direct_play, "direct play"),
            (self.direct_stream, "direct stream"),
            (self.transcode, "transcode"),
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
    use crate::providers::tests::{refused, section, shared, with_token};

    #[tokio::test]
    async fn emby_asks_its_sessions_with_its_token_header() {
        let fake = Fake::start().unwrap();
        fake.script(
            "/emby/Sessions",
            Reply::json(
                200,
                r#"[{"NowPlayingItem":{},"PlayState":{"PlayMethod":"DirectPlay"}},
                    {"NowPlayingItem":{"Name":"x"},"PlayState":{"PlayMethod":"directstream"}},
                    {"NowPlayingItem":{},"PlayState":{"PlayMethod":"Transcode"}},
                    {"NowPlayingItem":{},"PlayState":{"PlayMethod":"Other"}},
                    {"NowPlayingItem":{},"PlayState":{"PlayMethod":"DirectPlay","IsPaused":true}},
                    {"NowPlayingItem":null,"PlayState":{"PlayMethod":"DirectPlay"}},
                    {"PlayState":null}]"#,
            ),
        );
        let url = format!("{}/", fake.url());
        let one = section(vec![with_token("Emby", &url, "emby-token-canary")]);
        let out = emby(Some(&one), &shared(&[]), &Paint::plain()).await;
        let text = out.text.clone();
        // `--json` (motd spec): the same answer, typed.
        assert_eq!(
            crate::providers::tests::instances(&out),
            r#"[{"name":"Emby","value":{"active":4,"direct_play":1,"direct_stream":1,"transcode":1}}]"#
        );
        assert_eq!(
            text,
            "4 active streams (1 direct play, 1 direct stream, 1 transcode)"
        );
        let seen = fake.seen();
        assert_eq!(seen[0].target, "/emby/Sessions");
        assert_eq!(seen[0].header("x-emby-token"), Some("emby-token-canary"));
        assert_eq!(seen[0].header("accept"), Some("application/json"));
    }

    #[tokio::test]
    async fn emby_errors_and_idle_are_sb_gos() {
        let fake = Fake::start().unwrap();
        fake.script("/a/emby/Sessions", Reply::json(200, "[]"));
        fake.script("/b/emby/Sessions", Reply::new(500, "down"));
        fake.script("/c/emby/Sessions", Reply::json(200, "{}"));
        let shared = shared(&[]);
        let ask = |url: String| {
            let shared = Arc::clone(&shared);
            async move {
                let one = section(vec![with_token("Emby", &url, "t")]);
                emby(Some(&one), &shared, &Paint::plain()).await.text
            }
        };
        assert_eq!(ask(format!("{}/a", fake.url())).await, "No active streams");
        assert_eq!(
            ask(format!("{}/b", fake.url())).await,
            "Error: emby API returned status code 500"
        );
        let text = ask(format!("{}/c", fake.url())).await;
        assert!(
            text.starts_with("Error: failed to parse Emby response: "),
            "{text}"
        );
        assert!(
            ask(refused())
                .await
                .starts_with("Error: failed to connect to Emby: ")
        );
        assert!(
            ask("::".into())
                .await
                .starts_with("Error: failed to create request: ")
        );
    }
}
