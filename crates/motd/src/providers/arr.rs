//! Download Queues (sb-go's `motd/queue.go`, through golift.io/starr's
//! `GetQueue(0, 100)`): Sonarr and Radarr `GET <url>/api/v3/queue`,
//! Lidarr and Readarr `/api/v1/queue`, with `X-Api-Key`, 100 records a
//! page until every record is in, each instance a line with its name
//! (always, unlike the other sections) and its records counted by status.
//!
//! The pages are 1, 2, 3 … of 100 until the total is reached or a page
//! comes back empty. starr shrinks the last page's size to what is left
//! (`AdjustPerPage`), which with page numbers fixed by the first size asks
//! for records it already has; sb keeps the size, so each record counts
//! once. The parameters are starr's: `page`, `pageSize`,
//! `sortDirection=ascending`, `sortKey=timeleft` and the app's
//! `includeUnknown…Items=true`, written in Go's sorted order.

use std::sync::Arc;

use serde::Deserialize;

use super::{Answer, DEFAULT_TIMEOUT, Shared, secret_header};
use crate::Paint;
use crate::config::{App, AppSection, Instance};
use crate::text::first_line;

/// One page's size, sb-go's.
const PAGE: usize = 100;

/// At most this many pages are asked for (100,000 records): a server that
/// keeps answering full pages can't hold the section.
const MAX_PAGES: usize = 1000;

/// An Arr app as its queue is asked for.
#[derive(Clone, Copy, Debug)]
struct Arr {
    app: App,
    key: &'static str,
    name: &'static str,
    version: &'static str,
    unknown: &'static str,
}

/// The four apps, in sb-go's order.
const ARRS: [Arr; 4] = [
    Arr {
        app: App::Sonarr,
        key: "sonarr",
        name: "Sonarr",
        version: "v3",
        unknown: "includeUnknownSeriesItems",
    },
    Arr {
        app: App::Radarr,
        key: "radarr",
        name: "Radarr",
        version: "v3",
        unknown: "includeUnknownMovieItems",
    },
    Arr {
        app: App::Lidarr,
        key: "lidarr",
        name: "Lidarr",
        version: "v1",
        unknown: "includeUnknownArtistItems",
    },
    Arr {
        app: App::Readarr,
        key: "readarr",
        name: "Readarr",
        version: "v1",
        unknown: "includeUnknownAuthorItems",
    },
];

/// Download Queues' value: every app's instances, asked at once, listed
/// together by name.
pub async fn queues(
    apps: &std::collections::BTreeMap<App, AppSection>,
    shared: &Arc<Shared>,
    paint: &Paint,
) -> String {
    let asks = ARRS.map(|arr| {
        let paint = paint.clone();
        async move {
            super::ask_each(
                arr.key,
                apps.get(&arr.app),
                arr.name,
                shared,
                super::has_apikey,
                move |shared, instance| {
                    let paint = paint.clone();
                    async move { ask(&shared, &instance, arr, &paint).await }
                },
            )
            .await
        }
    });
    let [a, b, c, d] = asks;
    let (a, b, c, d) = tokio::join!(a, b, c, d);
    // sb-go names every queue line by `providerInstanceName`: trimmed, or
    // the app's name.
    let answers: Vec<(String, Answer)> = ARRS
        .into_iter()
        .zip([a, b, c, d])
        .flat_map(|(arr, answers)| {
            answers.into_iter().map(move |(name, answer)| {
                let trimmed = name.trim();
                let name = if trimmed.is_empty() {
                    arr.name
                } else {
                    trimmed
                };
                (name.to_string(), answer)
            })
        })
        .collect();
    super::named_listing(answers, paint)
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    total_records: Option<i64>,
    records: Option<Vec<Record>>,
}

#[derive(Debug, Default, Deserialize)]
struct Record {
    status: Option<String>,
}

async fn ask(shared: &Shared, instance: &Instance, arr: Arr, paint: &Paint) -> Answer {
    fetch(shared, instance, arr)
        .await
        .map(|queue| summary(&queue, shared, paint))
        .map_err(|err| shared.error_text(&err))
}

/// A queue, counted as its pages came: how many records, and how many
/// of each status (by the app's own text).
#[derive(Debug, Default, PartialEq, Eq)]
struct Queue {
    records: usize,
    statuses: std::collections::BTreeMap<String, usize>,
}

/// `<url>/api/<version>/queue` with page `page`'s parameters.
fn page_url(base: &str, arr: Arr, page: usize) -> Result<reqwest::Url, String> {
    let url = format!("{}/api/{}/queue", base.trim_end_matches('/'), arr.version);
    let mut url = reqwest::Url::parse(&url).map_err(|err| format!("invalid URL: {err}"))?;
    let page = page.to_string();
    let size = PAGE.to_string();
    let mut pairs = [
        (arr.unknown, "true"),
        ("page", page.as_str()),
        ("pageSize", size.as_str()),
        ("sortDirection", "ascending"),
        ("sortKey", "timeleft"),
    ];
    pairs.sort_unstable();
    url.query_pairs_mut().extend_pairs(pairs);
    Ok(url)
}

/// The queue counted page by page, at most [`PAGE`] records taken from
/// each (a page of more is the app's mistake, and counting only a page's
/// worth keeps [`MAX_PAGES`] a bound on what is counted), or starr's text
/// for the first failure.
async fn fetch(shared: &Shared, instance: &Instance, arr: Arr) -> Result<Queue, String> {
    let key = secret_header(instance.apikey.as_deref().unwrap_or_default())?;
    let failed = |why: String| format!("api.Get({}/queue): {why}", arr.version);
    let mut queue = Queue::default();
    for page in 1..=MAX_PAGES {
        let url = page_url(&instance.url, arr, page)?;
        let client = shared.client().await?;
        let response = client
            .get(url)
            .header("X-Api-Key", key.clone())
            .header("Accept", "application/json")
            .timeout(shared.timeout(instance, DEFAULT_TIMEOUT))
            .send()
            .await
            .map_err(|err| failed(sb_config::http::error_chain(&err.without_url())))?;
        let status = response.status();
        let body = sb_config::http::body(response).await.map_err(failed)?;
        if !status.is_success() {
            return Err(failed(invalid_status(status.as_u16(), &body)));
        }
        let page: Page = serde_json::from_slice(&body)
            .map_err(|err| failed(format!("decoding Starr JSON response body: {err}")))?;
        let records = page.records.unwrap_or_default();
        let got = records.len();
        for record in records.into_iter().take(PAGE) {
            queue.records += 1;
            *queue
                .statuses
                .entry(record.status.unwrap_or_default())
                .or_default() += 1;
        }
        let total = usize::try_from(page.total_records.unwrap_or(0)).unwrap_or(0);
        if queue.records >= total || got == 0 {
            break;
        }
    }
    Ok(queue)
}

/// starr's `ReqError` text (`parseNon200`): `invalid status code, N >=
/// 300` (or `< 200`), then the answer's `message`; or its
/// `propertyName: errorMessage`, alone or the first of a list; or its
/// body's first 400 bytes.
fn invalid_status(code: u16, body: &[u8]) -> String {
    #[derive(Deserialize)]
    struct Message {
        message: Option<String>,
    }
    #[derive(Deserialize)]
    struct Property {
        #[serde(rename = "errorMessage")]
        message: Option<String>,
        #[serde(rename = "propertyName")]
        name: Option<String>,
    }
    let base = if code >= 300 {
        format!("invalid status code, {code} >= 300")
    } else {
        format!("invalid status code, {code} < 200")
    };
    if let Ok(Message {
        message: Some(message),
    }) = serde_json::from_slice::<Message>(body)
        && !message.is_empty()
    {
        return format!("{base}, {message}");
    }
    let property = serde_json::from_slice::<Property>(body).ok().or_else(|| {
        serde_json::from_slice::<Vec<Property>>(body)
            .ok()
            .and_then(|list| list.into_iter().next())
    });
    if let Some(Property {
        message: Some(message),
        name,
    }) = property
        && !message.is_empty()
    {
        return match name.filter(|name| !name.is_empty()) {
            Some(name) => format!("{base}, {name}: {message}"),
            None => format!("{base}, {message}"),
        };
    }
    let text = String::from_utf8_lossy(&body[..body.len().min(400)]).into_owned();
    if text.is_empty() {
        base
    } else {
        format!("{base}, {text}")
    }
}

/// The most statuses a line names; the rest are left out (`…`), and the
/// whole line is cut to sb-config's limit for remote text. Real queues
/// have a handful.
const MAX_STATUSES: usize = 20;

/// sb-go's `formatDetailedQueueOutput` line: `N item(s) in queue`, then
/// each status's count, statuses sorted, lowercased and read with their
/// count ([`counted`]); counts in the value colour. The statuses are the
/// app's text: one line safe to draw, credentials hidden, before they
/// are painted.
fn summary(queue: &Queue, shared: &Shared, paint: &Paint) -> String {
    let noun = if queue.records == 1 { "item" } else { "items" };
    let mut summary = format!(
        "{} {noun} in queue",
        paint.value(&queue.records.to_string())
    );
    let mut parts: Vec<String> = queue
        .statuses
        .iter()
        .take(MAX_STATUSES)
        .map(|(status, n)| {
            // Lowercased after the credentials are hidden (a credential
            // lowercased wouldn't match), the mark kept as it is.
            let shown = shared
                .remote(&first_line(status.as_bytes()))
                .to_lowercase()
                .replace(
                    &sb_config::secret::REDACTED.to_lowercase(),
                    sb_config::secret::REDACTED,
                );
            format!("{} {}", paint.value(&n.to_string()), counted(&shown, *n))
        })
        .collect();
    if queue.statuses.len() > MAX_STATUSES {
        parts.push("…".to_string());
    }
    if !parts.is_empty() {
        summary = format!("{summary}, {}", parts.join(", "));
    }
    crate::text::capped(&summary)
}

/// A status as it reads after its count (the maintainer, 2026-11-17;
/// Difference 32): a status that is a noun or a state reads oddly there
/// (`7 warning`), so `warning` and `downloadClientUnavailable` (Sonarr's
/// and Radarr's `QueueStatus`) read `with a warning` or `with warnings`
/// and `with the download client unavailable`, and `error` (not in
/// `QueueStatus`; kept, as the maintainer named it, for an app that
/// sends it: Lidarr's and Readarr's `status` is a free string in their
/// `OpenAPI` documents) reads `with an error` or `with errors`. The rest of
/// `QueueStatus` (`unknown`, `queued`, `paused`, `downloading`,
/// `completed`, `failed`, `delay`, `fallback`: the maintainer kept
/// `delay` as named, and a `fallback` item is the fallback) and any
/// status sb doesn't know stay as the app names them, lowercased.
fn counted(status: &str, count: usize) -> String {
    match (status, count) {
        ("warning", 1) => "with a warning".to_string(),
        ("warning", _) => "with warnings".to_string(),
        ("error", 1) => "with an error".to_string(),
        ("error", _) => "with errors".to_string(),
        ("downloadclientunavailable", _) => "with the download client unavailable".to_string(),
        _ => status.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use sb_config::testing::{Answer as Reply, Fake};

    use super::*;
    use crate::providers::tests::{instance, painted, refused, section, shared};

    fn with_key(name: &str, url: &str, key: &str) -> Instance {
        Instance {
            apikey: Some(key.to_string()),
            ..instance(name, url)
        }
    }

    fn page(total: usize, statuses: &[&str]) -> Reply {
        let records: Vec<String> = statuses
            .iter()
            .map(|status| format!(r#"{{"status":"{status}","title":"x"}}"#))
            .collect();
        Reply::json(
            200,
            format!(
                r#"{{"page":1,"pageSize":100,"totalRecords":{total},"records":[{}]}}"#,
                records.join(",")
            ),
        )
    }

    /// Each app's path, version and parameters; pages of 100 until the
    /// total is in, each record once; every line named, sorted by name.
    #[tokio::test]
    async fn the_queues_are_paged_and_named() {
        let fake = Fake::start().unwrap();
        let downloading: Vec<&str> = vec!["downloading"; 100];
        fake.script("/sonarr/api/v3/queue", page(150, &downloading));
        let mut rest = vec!["completed"; 49];
        rest.push("Warning");
        fake.script("/sonarr/api/v3/queue", page(150, &rest));
        fake.script("/radarr/api/v3/queue", page(0, &[]));
        fake.script("/lidarr/api/v1/queue", page(1, &["queued"]));
        fake.script(
            "/readarr/api/v1/queue",
            Reply::new(401, r#"{"message":"Unauthorized"}"#),
        );
        let base = fake.url();
        let mut apps = BTreeMap::new();
        apps.insert(
            App::Sonarr,
            section(vec![with_key(
                "Sonarr",
                &format!("{base}/sonarr/"),
                "sonarr-key-canary",
            )]),
        );
        apps.insert(
            App::Radarr,
            section(vec![with_key("", &format!("{base}/radarr"), "k")]),
        );
        apps.insert(
            App::Lidarr,
            section(vec![with_key("Lidarr", &format!("{base}/lidarr"), "k")]),
        );
        apps.insert(
            App::Readarr,
            section(vec![with_key("Books", &format!("{base}/readarr"), "k")]),
        );
        let text = queues(&apps, &shared(&[]), &Paint::plain()).await;
        assert_eq!(
            text,
            "Books:  Error: api.Get(v1/queue): invalid status code, 401 >= 300, Unauthorized\n\
             Lidarr: 1 item in queue, 1 queued\n\
             Radarr: 0 items in queue\n\
             Sonarr: 150 items in queue, 1 with a warning, 49 completed, 100 downloading"
        );
        let seen = fake.seen();
        let sonarr: Vec<&str> = seen
            .iter()
            .filter(|s| s.target.starts_with("/sonarr"))
            .map(|s| s.target.as_str())
            .collect();
        assert_eq!(
            sonarr,
            [
                "/sonarr/api/v3/queue?includeUnknownSeriesItems=true&page=1&pageSize=100&sortDirection=ascending&sortKey=timeleft",
                "/sonarr/api/v3/queue?includeUnknownSeriesItems=true&page=2&pageSize=100&sortDirection=ascending&sortKey=timeleft",
            ]
        );
        let first = seen
            .iter()
            .find(|s| s.target.starts_with("/sonarr"))
            .unwrap();
        assert_eq!(first.header("x-api-key"), Some("sonarr-key-canary"));
        assert!(!first.target.contains("canary"));
        for (prefix, unknown) in [
            (
                "/radarr/api/v3/queue?includeUnknownMovieItems=true&page=1",
                "",
            ),
            (
                "/lidarr/api/v1/queue?includeUnknownArtistItems=true&page=1",
                "",
            ),
            (
                "/readarr/api/v1/queue?includeUnknownAuthorItems=true&page=1",
                "",
            ),
        ] {
            assert!(
                seen.iter().any(|s| s.target.starts_with(prefix)),
                "{prefix}{unknown}"
            );
        }
    }

    /// A page of more than 100 records counts 100; a name is trimmed, as
    /// sb-go's `providerInstanceName`; a line of many statuses names 20.
    #[tokio::test]
    async fn a_page_counts_a_pages_worth_and_a_line_stays_short() {
        let fake = Fake::start().unwrap();
        let many: Vec<String> = (0..150).map(|n| format!("s{n:03}")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        fake.script("/api/v3/queue", page(150, &many));
        fake.script("/api/v3/queue", page(150, &[]));
        let mut apps = BTreeMap::new();
        apps.insert(
            App::Radarr,
            section(vec![with_key("  Radarr 4K  ", fake.url(), "k")]),
        );
        let text = queues(&apps, &shared(&[]), &Paint::plain()).await;
        assert!(
            text.starts_with("Radarr 4K: 100 items in queue, 1 s000, "),
            "{text}"
        );
        assert!(text.ends_with(", 1 s019, …"), "{text}");
        assert_eq!(fake.seen().len(), 2);
        let mut spaces = BTreeMap::new();
        spaces.insert(App::Lidarr, section(vec![with_key(" ", fake.url(), "k")]));
        let fake2 = Fake::start().unwrap();
        fake2.script("/api/v1/queue", page(0, &[]));
        spaces.insert(App::Lidarr, section(vec![with_key(" ", fake2.url(), "k")]));
        assert_eq!(
            queues(&spaces, &shared(&[]), &Paint::plain()).await,
            "Lidarr: 0 items in queue"
        );
    }

    /// One instance is named too; an empty page ends the paging; no
    /// section, a disabled one, or no key shows nothing.
    #[tokio::test]
    async fn one_queue_is_named_and_an_empty_page_ends_it() {
        let fake = Fake::start().unwrap();
        fake.script("/api/v3/queue", page(500, &["delay"]));
        fake.script("/api/v3/queue", page(500, &[]));
        let mut apps = BTreeMap::new();
        apps.insert(
            App::Radarr,
            section(vec![with_key("Radarr", fake.url(), "k")]),
        );
        let paint = painted();
        let text = queues(&apps, &shared(&[]), &paint).await;
        assert_eq!(
            text,
            format!(
                "{}{} item in queue, {} delay",
                paint.app_name("Radarr: "),
                paint.value("1"),
                paint.value("1")
            )
        );
        assert_eq!(fake.seen().len(), 2);
        let none = BTreeMap::new();
        assert_eq!(queues(&none, &shared(&[]), &Paint::plain()).await, "");
        let mut unusable = BTreeMap::new();
        unusable.insert(App::Sonarr, section(vec![instance("S", fake.url())]));
        assert_eq!(queues(&unusable, &shared(&[]), &Paint::plain()).await, "");
    }

    /// starr's error texts: a body without a message, an empty one, a
    /// status below 200; a body that isn't the page; a refused port; a
    /// status that is remote text, made safe and hidden.
    #[tokio::test]
    async fn queue_errors_are_starrs() {
        assert_eq!(invalid_status(500, b""), "invalid status code, 500 >= 300");
        assert_eq!(
            invalid_status(500, b"oops"),
            "invalid status code, 500 >= 300, oops"
        );
        assert_eq!(
            invalid_status(199, b"{}"),
            "invalid status code, 199 < 200, {}"
        );
        assert_eq!(
            invalid_status(404, &[b'x'; 500]),
            format!("invalid status code, 404 >= 300, {}", "x".repeat(400))
        );
        let fake = Fake::start().unwrap();
        fake.script("/a/api/v3/queue", Reply::json(200, "1"));
        fake.script(
            "/b/api/v3/queue",
            page(2, &["arr-key-canary-12345", "Down\\u001b[2Jloading"]),
        );
        let shared = shared(&["arr-key-canary-12345"]);
        let ask = |path: &str| {
            let url = format!("{}{path}", fake.url());
            let shared = Arc::clone(&shared);
            async move {
                let mut apps = BTreeMap::new();
                apps.insert(
                    App::Sonarr,
                    section(vec![with_key("S", &url, "arr-key-canary-12345")]),
                );
                queues(&apps, &shared, &Paint::plain()).await
            }
        };
        assert!(ask("/a").await.starts_with(
            "S: Error: api.Get(v3/queue): decoding Starr JSON response body: invalid type"
        ));
        assert_eq!(
            ask("/b").await,
            "S: 2 items in queue, 1 downloading, 1 [REDACTED]"
        );
        let gone = {
            let mut apps = BTreeMap::new();
            apps.insert(App::Sonarr, section(vec![with_key("S", &refused(), "k")]));
            queues(&apps, &shared, &Paint::plain()).await
        };
        assert!(
            gone.starts_with("S: Error: api.Get(v3/queue): error sending request"),
            "{gone}"
        );
        let bad = {
            let mut apps = BTreeMap::new();
            apps.insert(App::Sonarr, section(vec![with_key("S", "::", "k")]));
            queues(&apps, &shared, &Paint::plain()).await
        };
        assert!(bad.starts_with("S: Error: invalid URL: "), "{bad}");
    }

    /// The statuses that read oddly after a count are reworded (the
    /// maintainer, 2026-11-17; Difference 32): `warning`, `error` and
    /// `downloadClientUnavailable`. The rest stay as the app names them,
    /// lowercased, `delay` and an unknown one included.
    #[test]
    fn odd_statuses_read_with_their_count() {
        for (status, one, many) in [
            ("warning", "with a warning", "with warnings"),
            ("error", "with an error", "with errors"),
            (
                "downloadclientunavailable",
                "with the download client unavailable",
                "with the download client unavailable",
            ),
        ] {
            assert_eq!(counted(status, 1), one, "{status}");
            assert_eq!(counted(status, 2), many, "{status}");
        }
        for status in [
            "unknown",
            "queued",
            "paused",
            "downloading",
            "completed",
            "failed",
            "delay",
            "fallback",
            "s000",
            "[REDACTED]",
        ] {
            assert_eq!(counted(status, 3), status);
        }
        let queue = Queue {
            records: 3,
            statuses: [("Downloading".to_string(), 2), ("warning".to_string(), 1)].into(),
        };
        assert_eq!(
            summary(&queue, &shared(&[]), &Paint::plain()),
            "3 items in queue, 2 downloading, 1 with a warning"
        );
    }
}
