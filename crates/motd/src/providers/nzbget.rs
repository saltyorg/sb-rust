//! `NZBGet` (sb-go's `motd/nzbget.go`): JSON-RPC `status`, then
//! `listgroups`, each a `POST <url>/jsonrpc` with Basic auth (reqwest's
//! `basic_auth`,
//! <https://docs.rs/reqwest/0.13.5/reqwest/struct.RequestBuilder.html#method.basic_auth>),
//! the base's own user name and password dropped as sb-go drops them.

use std::sync::Arc;

use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::{Answer, DEFAULT_TIMEOUT, Shared, Summary, Texts, bytes, send};
use crate::config::{AppSection, Instance};
use crate::{Out, Paint};

const TEXTS: Texts = Texts {
    app: "NZBGet",
    api: "NZBGet",
    read: "response body",
};

/// `NZBGet`'s section value.
pub async fn nzbget(section: Option<&AppSection>, shared: &Arc<Shared>, paint: &Paint) -> Out {
    let paint_each = paint.clone();
    let answers = super::ask_each(
        "nzbget",
        section,
        "NZBGet",
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

#[derive(Debug, Default, Deserialize)]
struct Status {
    result: Option<StatusResult>,
}

#[derive(Debug, Default, Deserialize)]
struct StatusResult {
    #[serde(rename = "ServerPaused")]
    paused: Option<bool>,
    #[serde(rename = "DownloadRate")]
    rate: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
struct Groups {
    result: Option<Vec<Group>>,
}

/// One queued group. Its sizes are read as numbers of any kind (Go's
/// `int64`, which no fraction of `NZBGet`'s has), so that sizes and the
/// rate are formatted by one `f64` path with no lossy cast.
#[derive(Debug, Default, Deserialize)]
struct Group {
    #[serde(rename = "RemainingSizeMB")]
    remaining_mb: Option<f64>,
    #[serde(rename = "FileSizeMB")]
    size_mb: Option<f64>,
    #[serde(rename = "Status")]
    status: Option<String>,
}

async fn ask(shared: &Shared, instance: &Instance, paint: &Paint) -> Answer {
    fetch(shared, instance)
        .await
        .map(|queue| Summary {
            text: queue.summary(paint),
            value: queue.value(),
        })
        .map_err(|err| shared.error_text(&shared.hide_login(instance, &err)))
}

/// `<base>/jsonrpc` without the base's user name and password.
fn rpc_url(base: &str) -> Result<reqwest::Url, String> {
    let mut url = reqwest::Url::parse(base.trim_end_matches('/'))
        .map_err(|err| format!("invalid URL: {err}"))?;
    // Both fail only for URLs that can't carry them.
    let _ = url.set_username("");
    let _ = url.set_password(None);
    let path = format!("{}/jsonrpc", url.path().trim_end_matches('/'));
    url.set_path(&path);
    Ok(url)
}

/// One JSON-RPC call's answer.
async fn call<T: DeserializeOwned>(
    shared: &Shared,
    instance: &Instance,
    url: &reqwest::Url,
    method: &str,
) -> Result<T, String> {
    let request = serde_json::json!({"method": method, "params": [], "id": 1});
    let user = instance.user.clone().unwrap_or_default();
    let password = instance.password.clone();
    let body = send(
        shared,
        |client| {
            // reqwest's `json` sets `Content-Type: application/json`
            // (<https://docs.rs/reqwest/0.13.5/reqwest/struct.RequestBuilder.html#method.json>).
            client
                .post(url.clone())
                .json(&request)
                .basic_auth(user, password)
        },
        shared.timeout(instance, DEFAULT_TIMEOUT),
        TEXTS,
    )
    .await?;
    // sb-go returns the decoder's own error here, with no prefix.
    serde_json::from_slice(&body).map_err(|err| err.to_string())
}

async fn fetch(shared: &Shared, instance: &Instance) -> Result<Queue, String> {
    let url = rpc_url(&instance.url)?;
    let status: Status = call(shared, instance, &url, "status").await?;
    let groups: Groups = call(shared, instance, &url, "listgroups").await?;
    Ok(Queue::of(status, groups))
}

#[derive(Debug, Default, PartialEq)]
struct Queue {
    paused: bool,
    rate: f64,
    items: usize,
    remaining: f64,
    size: f64,
}

/// One megabyte in bytes, as sb-go converts `NZBGet`'s sizes.
const MEGABYTE: f64 = 1_048_576.0;

impl Queue {
    /// sb-go's counting: the sizes summed in MB, as bytes; paused when
    /// the server is, or when every item is.
    fn of(status: Status, groups: Groups) -> Self {
        let status = status.result.unwrap_or_default();
        let groups = groups.result.unwrap_or_default();
        let paused_items = groups
            .iter()
            .filter(|group| group.status.as_deref() == Some("PAUSED"))
            .count();
        let total = |field: fn(&Group) -> Option<f64>| {
            groups
                .iter()
                .map(|group| field(group).unwrap_or(0.0))
                .sum::<f64>()
                * MEGABYTE
        };
        Queue {
            paused: status.paused.unwrap_or(false)
                || (!groups.is_empty() && paused_items == groups.len()),
            rate: status.rate.unwrap_or(0.0),
            items: groups.len(),
            remaining: total(|group| group.remaining_mb),
            size: total(|group| group.size_mb),
        }
    }

    /// sb-go's `formatNzbgetSummary`.
    fn summary(&self, paint: &Paint) -> String {
        let speed = format!("{}/s", bytes(self.rate));
        super::queue(
            paint,
            i64::try_from(self.items).unwrap_or(i64::MAX),
            &bytes(self.remaining),
            &bytes(self.size),
            (!self.paused).then_some(speed.as_str()),
        )
    }

    /// The `--json` value (motd spec, `--json`).
    fn value(&self) -> serde_json::Value {
        let speed = format!("{}/s", bytes(self.rate));
        super::queue_value(
            i64::try_from(self.items).unwrap_or(i64::MAX),
            &bytes(self.remaining),
            &bytes(self.size),
            (!self.paused).then_some(speed.as_str()),
        )
    }
}

#[cfg(test)]
mod tests {
    use sb_config::testing::{Answer as Reply, Fake};

    use super::*;
    use crate::providers::tests::{instance, painted, refused, section, shared};

    fn with_login(name: &str, url: &str) -> Instance {
        Instance {
            user: Some("nzb-user".to_string()),
            password: Some("nzb-pass-canary".to_string()),
            ..instance(name, url)
        }
    }

    #[test]
    fn bytes_are_sb_gos_format_bytes() {
        for (count, text) in [
            (0.0, "0 B"),
            (-0.5, "0 B"),
            (1023.9, "1023 B"),
            (1024.0, "1.0 KB"),
            (1536.0, "1.5 KB"),
            (1_048_575.0, "1024.0 KB"),
            (1_048_576.0, "1.0 MB"),
            (3.5 * 1_073_741_824.0, "3.5 GB"),
            (1.25 * 1_099_511_627_776.0, "1.2 TB"),
            (1e30, "8.0 EB"),
        ] {
            assert_eq!(bytes(count), text, "{count}");
        }
    }

    #[test]
    fn the_rpc_url_drops_the_bases_login() {
        assert_eq!(
            rpc_url("http://u:p@nzbget:6789/").unwrap().as_str(),
            "http://nzbget:6789/jsonrpc"
        );
        assert_eq!(
            rpc_url("http://n/x/").unwrap().as_str(),
            "http://n/x/jsonrpc"
        );
        assert!(rpc_url("n").unwrap_err().starts_with("invalid URL: "));
    }

    #[tokio::test]
    async fn nzbget_calls_status_then_listgroups_with_basic_auth() {
        let fake = Fake::start().unwrap();
        // The fake answers by path: `status`'s answer first, then
        // `listgroups`', which repeats.
        fake.script(
            "/jsonrpc",
            Reply::json(
                200,
                r#"{"result":{"ServerPaused":false,"DownloadRate":2097152}}"#,
            ),
        );
        fake.script(
            "/jsonrpc",
            Reply::json(
                200,
                r#"{"result":[{"RemainingSizeMB":512,"FileSizeMB":1024,"Status":"DOWNLOADING"},{"RemainingSizeMB":512,"FileSizeMB":1024,"Status":"QUEUED"}]}"#,
            ),
        );
        let one = section(vec![with_login("NZBGet", fake.url())]);
        let out = nzbget(Some(&one), &shared(&[]), &Paint::plain()).await;
        let text = out.text.clone();
        // `--json` (motd spec): the same answer, typed.
        assert_eq!(
            crate::providers::tests::instances(&out),
            r#"[{"name":"NZBGet","value":{"items":2,"left":"1.0 GB","paused":false,"size":"2.0 GB","speed":"2.0 MB/s"}}]"#
        );
        assert_eq!(
            text,
            "Downloading at 2.0 MB/s, 2 items in queue (1.0 GB remaining / 2.0 GB total)"
        );
        let seen = fake.seen();
        assert_eq!(seen.len(), 2);
        for (call, method) in seen.iter().zip(["status", "listgroups"]) {
            assert_eq!(call.method, "POST");
            assert_eq!(call.target, "/jsonrpc");
            assert_eq!(call.header("content-type"), Some("application/json"));
            // base64 of `nzb-user:nzb-pass-canary`.
            assert_eq!(
                call.header("authorization"),
                Some("Basic bnpiLXVzZXI6bnpiLXBhc3MtY2FuYXJ5")
            );
            let body: serde_json::Value = serde_json::from_str(&call.body).unwrap();
            assert_eq!(
                body,
                serde_json::json!({"method": method, "params": [], "id": 1})
            );
        }
    }

    #[test]
    fn the_queue_reads_as_sb_gos() {
        let read = |status: &str, groups: &str| {
            Queue::of(
                serde_json::from_str(status).unwrap(),
                serde_json::from_str(groups).unwrap(),
            )
        };
        let plain = Paint::plain();
        let empty = read("{}", "{}");
        assert_eq!(empty.summary(&plain), "Queue is empty");
        // Every item paused: the queue is paused.
        let paused = read(
            r#"{"result":{"DownloadRate":10}}"#,
            r#"{"result":[{"FileSizeMB":1,"RemainingSizeMB":1,"Status":"PAUSED"}]}"#,
        );
        assert!(paused.paused);
        assert_eq!(
            paused.summary(&plain),
            "Paused, 1 item in queue (1.0 MB remaining / 1.0 MB total)"
        );
        let server = read(
            r#"{"result":{"ServerPaused":true}}"#,
            r#"{"result":[{"Status":"QUEUED"},{"Status":"PAUSED"}]}"#,
        );
        assert!(server.paused);
        let one_paused = read(
            "{}",
            r#"{"result":[{"Status":"QUEUED"},{"Status":"PAUSED"}]}"#,
        );
        assert!(!one_paused.paused);
        assert_eq!(
            one_paused.summary(&plain),
            "Downloading at 0 B/s, 2 items in queue (0 B remaining / 0 B total)"
        );
        let paint = painted();
        assert!(one_paused.summary(&paint).contains(&paint.value("0 B/s")));
    }

    #[tokio::test]
    async fn nzbget_errors_are_sb_gos() {
        let fake = Fake::start().unwrap();
        fake.script("/a/jsonrpc", Reply::new(401, ""));
        fake.script("/b/jsonrpc", Reply::json(200, "1"));
        let shared = shared(&["nzb-pass-canary"]);
        let ask = |url: String| {
            let shared = Arc::clone(&shared);
            async move {
                let one = section(vec![with_login("NZBGet", &url)]);
                nzbget(Some(&one), &shared, &Paint::plain()).await.text
            }
        };
        assert_eq!(
            ask(format!("{}/a", fake.url())).await,
            "Error: NZBGet API returned status code 401"
        );
        // The decoder's own words, no prefix (sb-go's).
        let text = ask(format!("{}/b", fake.url())).await;
        assert!(text.starts_with("Error: invalid type"), "{text}");
        let text = ask(refused()).await;
        assert!(
            text.starts_with("Error: failed to connect to NZBGet: "),
            "{text}"
        );
        let no_password = Instance {
            password: None,
            ..with_login("NZBGet", fake.url())
        };
        let no_user = Instance {
            user: Some(String::new()),
            ..with_login("NZBGet", fake.url())
        };
        let unusable = section(vec![no_password, no_user, with_login("NZBGet", "")]);
        assert_eq!(
            nzbget(Some(&unusable), &shared, &Paint::plain()).await.text,
            ""
        );
    }
}
