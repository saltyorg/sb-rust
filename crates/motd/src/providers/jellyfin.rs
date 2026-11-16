//! Jellyfin (sb-go's `motd/jellyfin.go`, through jellyfin-go's
//! `SessionAPI.GetSessions`): `GET <url>/Sessions` with `Authorization:
//! MediaBrowser Token="…"`, the sessions with an item counted by their
//! transcoding info. jellyfin-go's errors read `<code> <reason>` for a
//! status other than 2xx (its generated client's `GenericOpenAPIError`),
//! after sb-go's `failed to get sessions: `.

use std::sync::Arc;

use super::{Answer, DEFAULT_TIMEOUT, Shared, streams};
use crate::Paint;
use crate::config::{AppSection, Instance};

/// Jellyfin's section value.
pub async fn jellyfin(section: Option<&AppSection>, shared: &Arc<Shared>, paint: &Paint) -> String {
    let paint_each = paint.clone();
    let answers = super::ask_each(
        "jellyfin",
        section,
        "Jellyfin",
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

/// One session, as jellyfin-go reads it: a field is there when the JSON
/// names it, even as `null` (its `Nullable*` types set `isSet` in
/// `UnmarshalJSON`, and `HasNowPlayingItem`/`HasTranscodingInfo` ask
/// `IsSet`), so the session is read as a map of its fields.
type Session = serde_json::Map<String, serde_json::Value>;

async fn ask(shared: &Shared, instance: &Instance, paint: &Paint) -> Answer {
    fetch(shared, instance)
        .await
        .map(|counts| counts.summary(paint))
        .map_err(|err| shared.error_text(&format!("failed to get sessions: {err}")))
}

async fn fetch(shared: &Shared, instance: &Instance) -> Result<Counts, String> {
    let url = format!("{}/Sessions", instance.url.trim_end_matches('/'));
    let url = reqwest::Url::parse(&url).map_err(|err| err.to_string())?;
    let token = instance.token.as_deref().unwrap_or_default();
    let authorization = super::secret_header(&format!("MediaBrowser Token=\"{token}\""))?;
    let http = shared.http().await?;
    let response = http
        .client()
        .get(url)
        .header("Accept", "application/json")
        .header("Authorization", authorization)
        .timeout(shared.timeout(instance, DEFAULT_TIMEOUT))
        .send()
        .await
        .map_err(|err| sb_config::http::error_chain(&err.without_url()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(status.to_string());
    }
    let body = sb_config::http::body(response).await?;
    let sessions: Vec<Session> = serde_json::from_slice(&body).map_err(|err| err.to_string())?;
    Ok(Counts::of(sessions))
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Counts {
    active: usize,
    direct_play: usize,
    remux: usize,
    direct_stream: usize,
    transcode: usize,
}

impl Counts {
    /// sb-go's counting: a session with an item (even `null`) is active;
    /// without transcoding info it is direct play; with it (even `null`),
    /// video not direct is a transcode, audio not direct a direct stream,
    /// and both direct a remux.
    fn of(sessions: Vec<Session>) -> Self {
        let mut counts = Counts::default();
        for session in sessions {
            if !session.contains_key("NowPlayingItem") {
                continue;
            }
            counts.active += 1;
            let direct = |info: &serde_json::Value, key: &str| {
                info.get(key)
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
            };
            match session.get("TranscodingInfo") {
                None => counts.direct_play += 1,
                Some(info) if !direct(info, "IsVideoDirect") => counts.transcode += 1,
                Some(info) if !direct(info, "IsAudioDirect") => counts.direct_stream += 1,
                Some(_) => counts.remux += 1,
            }
        }
        counts
    }

    fn summary(&self, paint: &Paint) -> String {
        if self.active == 0 {
            return "No active streams".to_string();
        }
        streams(
            paint,
            self.active,
            &[
                (self.direct_play, "direct play"),
                (self.remux, "remux"),
                (self.direct_stream, "direct stream"),
                (self.transcode, "transcode"),
            ],
        )
    }
}

#[cfg(test)]
mod tests {
    use sb_config::testing::{Answer as Reply, Fake};

    use super::*;
    use crate::providers::tests::{refused, section, shared, with_token};

    #[tokio::test]
    async fn jellyfin_asks_its_sessions_with_mediabrowser_auth() {
        let fake = Fake::start().unwrap();
        fake.script(
            "/Sessions",
            Reply::json(
                200,
                r#"[{"NowPlayingItem":{}},
                    {"NowPlayingItem":{},"TranscodingInfo":{"IsVideoDirect":false,"IsAudioDirect":true}},
                    {"NowPlayingItem":{},"TranscodingInfo":{"IsVideoDirect":true,"IsAudioDirect":false}},
                    {"NowPlayingItem":{},"TranscodingInfo":{"IsVideoDirect":true,"IsAudioDirect":true}},
                    {"NowPlayingItem":{},"TranscodingInfo":{}},
                    {"NowPlayingItem":null},
                    {"NowPlayingItem":{},"TranscodingInfo":null},
                    {}]"#,
            ),
        );
        let one = section(vec![with_token("Jellyfin", fake.url(), "jf-token-canary")]);
        let text = jellyfin(Some(&one), &shared(&[]), &Paint::plain()).await;
        assert_eq!(
            text,
            "7 active streams (2 direct play, 1 remux, 1 direct stream, 3 transcode)"
        );
        let seen = fake.seen();
        assert_eq!(seen[0].target, "/Sessions");
        assert_eq!(
            seen[0].header("authorization"),
            Some(r#"MediaBrowser Token="jf-token-canary""#)
        );
    }

    #[tokio::test]
    async fn jellyfin_errors_are_jellyfin_gos() {
        let fake = Fake::start().unwrap();
        fake.script("/a/Sessions", Reply::new(401, ""));
        fake.script("/b/Sessions", Reply::json(200, "[]"));
        fake.script("/c/Sessions", Reply::json(200, "[1]"));
        let shared = shared(&[]);
        let ask = |url: String| {
            let shared = Arc::clone(&shared);
            async move {
                let one = section(vec![with_token("Jellyfin", &url, "t")]);
                jellyfin(Some(&one), &shared, &Paint::plain()).await
            }
        };
        assert_eq!(
            ask(format!("{}/a/", fake.url())).await,
            "Error: failed to get sessions: 401 Unauthorized"
        );
        assert_eq!(ask(format!("{}/b", fake.url())).await, "No active streams");
        assert!(
            ask(format!("{}/c", fake.url()))
                .await
                .starts_with("Error: failed to get sessions: invalid type")
        );
        assert!(
            ask(refused())
                .await
                .starts_with("Error: failed to get sessions: ")
        );
        assert!(
            ask("x".into())
                .await
                .starts_with("Error: failed to get sessions: ")
        );
    }
}
