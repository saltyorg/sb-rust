//! `SABnzbd` (sb-go's `motd/sabnzbd.go`): `GET <url>/api?mode=queue&
//! output=json&apikey=…`, the one API here that takes its key in the
//! query (motd spec, "Secrets"); the queue's status, speed, sizes and
//! item count, its texts `SABnzbd`'s own.

use std::sync::Arc;

use serde::Deserialize;

use super::{Answer, DEFAULT_TIMEOUT, Shared, Summary, Texts, send};
use crate::config::{AppSection, Instance};
use crate::text::first_line;
use crate::{Out, Paint};

const TEXTS: Texts = Texts {
    app: "SABnzbd",
    api: "SABnzbd",
    read: "response body",
};

/// `SABnzbd`'s section value.
pub async fn sabnzbd(section: Option<&AppSection>, shared: &Arc<Shared>, paint: &Paint) -> Out {
    let paint_each = paint.clone();
    let answers = super::ask_each(
        "sabnzbd",
        section,
        "SABnzbd",
        shared,
        super::has_apikey,
        move |shared, instance| {
            let paint = paint_each.clone();
            async move { ask(&shared, &instance, &paint).await }
        },
    )
    .await;
    super::listing(answers, paint)
}

#[derive(Debug, Default, Deserialize)]
struct Answered {
    queue: Option<Queue>,
}

#[derive(Debug, Default, Deserialize)]
struct Queue {
    status: Option<String>,
    speed: Option<String>,
    size: Option<String>,
    sizeleft: Option<String>,
    noofslots_total: Option<i64>,
}

async fn ask(shared: &Shared, instance: &Instance, paint: &Paint) -> Answer {
    fetch(shared, instance)
        .await
        .map(|queue| {
            let fields = Fields::of(&queue, shared);
            Summary {
                text: fields.summary(paint),
                value: fields.value(),
            }
        })
        .map_err(|err| shared.error_text(&err))
}

/// The API's URL: the base without a trailing `/`, `/api` added to its
/// path, the three parameters set over any the base had (Go's
/// `Query().Set`), and every key written in order, as Go's `Encode`
/// sorts them, through the `url` crate's query serializer
/// (<https://docs.rs/url/2.5.8/url/struct.Url.html#method.query_pairs_mut>).
fn api_url(base: &str, apikey: &str) -> Result<reqwest::Url, String> {
    let mut url = reqwest::Url::parse(base.trim_end_matches('/'))
        .map_err(|err| format!("invalid SABnzbd URL: {err}"))?;
    let path = format!("{}/api", url.path().trim_end_matches('/'));
    url.set_path(&path);
    let set = [("apikey", apikey), ("mode", "queue"), ("output", "json")];
    let mut pairs: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| !set.iter().any(|(name, _)| name == key))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .chain(
            set.iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string())),
        )
        .collect();
    // Stable: a key's values keep their order, as Go's do.
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    url.query_pairs_mut().clear().extend_pairs(pairs);
    Ok(url)
}

async fn fetch(shared: &Shared, instance: &Instance) -> Result<Queue, String> {
    let url = api_url(
        &instance.url,
        instance.apikey.as_deref().unwrap_or_default(),
    )?;
    let body = send(
        shared,
        |client| client.get(url),
        shared.timeout(instance, DEFAULT_TIMEOUT),
        TEXTS,
    )
    .await?;
    let answered: Answered = serde_json::from_slice(&body)
        .map_err(|err| format!("failed to parse SABnzbd response: {err}"))?;
    Ok(answered.queue.unwrap_or_default())
}

/// The queue's fields as the line and `--json` show them (sb-go's
/// `formatSabnzbdSummary`): `Paused, …` when the status says so, else
/// the speed with `/s` unless it has a `/`. They are `SABnzbd`'s text,
/// shown as they came: one line safe to draw, credentials hidden, cut to
/// sb-config's limit, before they are painted. Read once, for both.
struct Fields {
    items: i64,
    left: String,
    size: String,
    /// `None` when paused.
    speed: Option<String>,
}

impl Fields {
    fn of(queue: &Queue, shared: &Shared) -> Self {
        let text = |value: &Option<String>| {
            shared.remote(&first_line(value.as_deref().unwrap_or_default().as_bytes()))
        };
        let paused = text(&queue.status).to_lowercase() == "paused";
        let mut speed = text(&queue.speed);
        if !speed.contains('/') {
            speed.push_str("/s");
        }
        Self {
            items: queue.noofslots_total.unwrap_or(0),
            left: text(&queue.sizeleft),
            size: text(&queue.size),
            speed: (!paused).then_some(speed),
        }
    }

    fn summary(&self, paint: &Paint) -> String {
        super::queue(
            paint,
            self.items,
            &self.left,
            &self.size,
            self.speed.as_deref(),
        )
    }

    /// The `--json` value (motd spec, `--json`).
    fn value(&self) -> serde_json::Value {
        super::queue_value(self.items, &self.left, &self.size, self.speed.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use sb_config::testing::{Answer as Reply, Fake};

    use super::*;
    use crate::providers::tests::{instance, painted, refused, section, shared};

    /// The line for `queue`, as the job draws it.
    fn summary(queue: &Queue, shared: &Shared, paint: &Paint) -> String {
        Fields::of(queue, shared).summary(paint)
    }

    fn with_key(name: &str, url: &str, key: &str) -> Instance {
        Instance {
            apikey: Some(key.to_string()),
            ..instance(name, url)
        }
    }

    #[test]
    fn the_key_goes_in_the_query_over_the_bases_own() {
        assert_eq!(
            api_url("http://sab:8080/", "k y&").unwrap().as_str(),
            "http://sab:8080/api?apikey=k+y%26&mode=queue&output=json"
        );
        assert_eq!(
            api_url("http://sab/sabnzbd/?mode=x&keep=1&b=2&b=1", "k")
                .unwrap()
                .as_str(),
            "http://sab/sabnzbd/api?apikey=k&b=2&b=1&keep=1&mode=queue&output=json"
        );
        assert!(
            api_url("sab", "k")
                .unwrap_err()
                .starts_with("invalid SABnzbd URL: ")
        );
    }

    #[tokio::test]
    async fn sabnzbd_asks_its_queue() {
        let fake = Fake::start().unwrap();
        fake.script(
            "/api",
            Reply::json(
                200,
                r#"{"queue":{"status":"Downloading","speed":"1.2 M","size":"3.0 GB","sizeleft":"1.0 GB","noofslots_total":2}}"#,
            ),
        );
        let one = section(vec![with_key("SABnzbd", fake.url(), "sab-key-canary")]);
        let out = sabnzbd(Some(&one), &shared(&[]), &Paint::plain()).await;
        let text = out.text.clone();
        // `--json` (motd spec): the same answer, typed.
        assert_eq!(
            crate::providers::tests::instances(&out),
            r#"[{"name":"SABnzbd","value":{"items":2,"left":"1.0 GB","paused":false,"size":"3.0 GB","speed":"1.2 M/s"}}]"#
        );
        assert_eq!(
            text,
            "Downloading at 1.2 M/s, 2 items in queue (1.0 GB remaining / 3.0 GB total)"
        );
        let seen = fake.seen();
        assert_eq!(seen[0].method, "GET");
        assert_eq!(
            seen[0].target,
            "/api?apikey=sab-key-canary&mode=queue&output=json"
        );
    }

    #[test]
    fn the_summary_reads_as_sb_gos() {
        let queue = |status: &str, speed: &str, items| Queue {
            status: Some(status.into()),
            speed: Some(speed.into()),
            size: Some("2 GB".into()),
            sizeleft: Some("1 GB".into()),
            noofslots_total: Some(items),
        };
        let plain = Paint::plain();
        let shared = shared(&["sab-key-canary"]);
        assert_eq!(
            summary(&queue("Idle", "0", 0), &shared, &plain),
            "Queue is empty"
        );
        assert_eq!(
            summary(&Queue::default(), &shared, &plain),
            "Queue is empty"
        );
        assert_eq!(
            summary(&queue("PAUSED", "0", 1), &shared, &plain),
            "Paused, 1 item in queue (1 GB remaining / 2 GB total)"
        );
        assert_eq!(
            summary(&queue("Downloading", "5 MB/s", 3), &shared, &plain),
            "Downloading at 5 MB/s, 3 items in queue (1 GB remaining / 2 GB total)"
        );
        let paint = painted();
        assert_eq!(
            summary(&queue("x", "1", 1), &shared, &paint),
            format!(
                "Downloading at {}, {} item in queue ({} remaining / {} total)",
                paint.value("1/s"),
                paint.value("1"),
                paint.value("1 GB"),
                paint.value("2 GB")
            )
        );
        // Remote text is drawn safe, as it came (not trimmed, as sb-go
        // shows it), a credential in it hidden before it is painted, and a
        // long field cut.
        assert_eq!(
            summary(&queue("x", "1\x1b[2J", 1), &shared, &plain),
            "Downloading at 1/s, 1 item in queue (1 GB remaining / 2 GB total)"
        );
        assert_eq!(
            summary(&queue("x", "0  ", 1), &shared, &plain),
            "Downloading at 0  /s, 1 item in queue (1 GB remaining / 2 GB total)"
        );
        assert_eq!(
            summary(&queue("x", "sab-key-canary", 1), &shared, &paint),
            format!(
                "Downloading at {}, {} item in queue ({} remaining / {} total)",
                paint.value("[REDACTED]/s"),
                paint.value("1"),
                paint.value("1 GB"),
                paint.value("2 GB")
            )
        );
        let long = "9".repeat(sb_config::http::MAX_TEXT + 5);
        let text = summary(&queue("x", &long, 1), &shared, &plain);
        assert!(
            text.contains(&format!("{}…/s", &long[..sb_config::http::MAX_TEXT])),
            "{text}"
        );
    }

    /// The key is in the URL sent, never in what is shown: a status
    /// error, a refused port.
    #[tokio::test]
    async fn sabnzbd_errors_hide_the_key() {
        let fake = Fake::start().unwrap();
        fake.script("/api", Reply::new(403, "sab-key-canary"));
        let shared = shared(&["sab-key-canary"]);
        let one = section(vec![with_key("SABnzbd", fake.url(), "sab-key-canary")]);
        assert_eq!(
            sabnzbd(Some(&one), &shared, &Paint::plain()).await.text,
            "Error: SABnzbd API returned status code 403"
        );
        let gone = section(vec![with_key("SABnzbd", &refused(), "sab-key-canary")]);
        let text = sabnzbd(Some(&gone), &shared, &Paint::plain()).await.text;
        assert!(
            text.starts_with("Error: failed to connect to SABnzbd: "),
            "{text}"
        );
        assert!(!text.contains("canary"), "{text}");
        let unusable = section(vec![
            instance("SABnzbd", fake.url()),
            with_key("S", "", "k"),
        ]);
        assert_eq!(
            sabnzbd(Some(&unusable), &shared, &Paint::plain())
                .await
                .text,
            ""
        );
        fake.script("/x/api", Reply::json(200, "nope"));
        let bad = section(vec![with_key("S", &format!("{}/x", fake.url()), "k")]);
        let text = sabnzbd(Some(&bad), &shared, &Paint::plain()).await.text;
        assert!(
            text.starts_with("Error: failed to parse SABnzbd response: "),
            "{text}"
        );
    }
}
