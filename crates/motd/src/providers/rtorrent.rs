//! rTorrent (sb-go's `motd/rtorrent.go`, through go-rtorrent): XML-RPC
//! `POST <url>`, Basic auth when both `user` and `password` are set.
//! `system.hostname` first, as the quick check (1 s), then the data (the
//! instance's `timeout`, or sb-go's 20 s): one `d.multicall2` over the
//! main view with the three fields counted, and the two global rates.
//! sb-go made a `d.is_active` call per torrent; the multicall has it
//! (Difference 13), so the data is exactly three calls, sent as one
//! `system.multicall` request, so one round trip where sb-go made three
//! in a row (each call's answer and error as when asked alone).

use std::sync::Arc;
use std::time::Duration;

use super::xmlrpc::{self, Value};
use super::{Answer, DEFAULT_TIMEOUT, Shared, Summary, bytes, rate_bytes, torrents_summary};
use crate::config::{AppSection, Instance};
use crate::{Out, Paint};

/// sb-go's limit for the data, after the 1 s check.
pub const DATA_TIMEOUT: Duration = Duration::from_secs(20);

/// The data's answers may list every torrent: they are read up to this
/// many bytes (about 130,000 torrents: each row is about 120 bytes here),
/// not sb-config's 1 MiB.
pub const MAX_DATA: usize = 16 << 20;

/// The two global rates' methods.
const DOWN_RATE: &str = "throttle.global_down.rate";
const UP_RATE: &str = "throttle.global_up.rate";

/// Which limit a call has: the fixed 1 s check, or the data's (the
/// instance's `timeout` or 20 s), each cut to what is left of the
/// section's 30 s when the call starts.
#[derive(Clone, Copy, Debug)]
enum Limit {
    Check,
    Data,
}

/// rTorrent's section value.
pub async fn rtorrent(section: Option<&AppSection>, shared: &Arc<Shared>, paint: &Paint) -> Out {
    let paint_each = paint.clone();
    let answers = super::ask_each(
        "rtorrent",
        section,
        "rTorrent",
        shared,
        |instance| !instance.url.is_empty(),
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
        .map_err(|err| shared.error_text(&shared.hide_login(instance, &err)))
}

/// The torrents, counted, and the two rates.
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

/// One XML-RPC call's first parameter.
async fn call(
    shared: &Shared,
    instance: &Instance,
    method: &str,
    params: &[&str],
    limit: Limit,
) -> Result<Value, String> {
    let body = xmlrpc::method_call(method, params).map_err(|err| err.to_string())?;
    first_param(post(shared, instance, body, limit).await?)
}

/// A call's first parameter, of those its answer holds.
fn first_param(params: Vec<Value>) -> Result<Value, String> {
    params
        .into_iter()
        .next()
        .ok_or_else(|| "an answer without a value".to_string())
}

/// `calls` as one `system.multicall`: each call's first parameter or its
/// error, in order, as [`call`] would give it alone (fewer when the
/// answer has fewer entries). A failed request is the whole call's
/// error.
async fn multicall(
    shared: &Shared,
    instance: &Instance,
    calls: &[(&str, &[&str])],
    limit: Limit,
) -> Result<Vec<Result<Value, String>>, String> {
    let body = xmlrpc::multicall(calls).map_err(|err| err.to_string())?;
    let answer = first_param(post(shared, instance, body, limit).await?)?;
    Ok(xmlrpc::multicall_results(answer)
        .map_err(|err| err.to_string())?
        .into_iter()
        .map(|result| result.map_err(|err| err.to_string()).and_then(first_param))
        .collect())
}

/// One request's answer, read within its cap and parsed on tokio's
/// blocking pool through cli-core's `spawn::blocking` (the run's span
/// carried), as a multicall can be large
/// (<https://docs.rs/tokio/1.54.1/tokio/task/fn.spawn_blocking.html>).
async fn post(
    shared: &Shared,
    instance: &Instance,
    body: Vec<u8>,
    limit: Limit,
) -> Result<Vec<Value>, String> {
    let (timeout, cap) = match limit {
        Limit::Check => (shared.within(DEFAULT_TIMEOUT), sb_config::http::MAX_BODY),
        Limit::Data => (shared.timeout(instance, DATA_TIMEOUT), MAX_DATA),
    };
    let client = shared.client().await?;
    let mut request = client
        .post(instance.url.as_str())
        .header("Content-Type", "text/xml")
        .body(body)
        .timeout(timeout);
    let user = instance.user.as_deref().unwrap_or_default();
    let password = instance.password.as_deref().unwrap_or_default();
    if !user.is_empty() && !password.is_empty() {
        request = request.basic_auth(user, Some(password));
    }
    let response = request.send().await.map_err(|err| {
        format!(
            "POST failed: {}",
            sb_config::http::error_chain(&err.without_url())
        )
    })?;
    let status = response.status();
    if status != reqwest::StatusCode::OK {
        return Err(format!("rTorrent returned status code {}", status.as_u16()));
    }
    let body = sb_config::http::body_within(response, cap).await?;
    sb_cli_core::spawn::blocking(move || {
        xmlrpc::response(&String::from_utf8_lossy(&body)).map_err(|err| err.to_string())
    })
    .await
    .map_err(|err| err.to_string())?
}

async fn fetch(shared: &Shared, instance: &Instance) -> Result<Counts, String> {
    let hostname = call(shared, instance, "system.hostname", &[], Limit::Check)
        .await
        .map_err(|err| {
            format!("could not connect to rtorrent: system.hostname XMLRPC call failed: {err}")
        })?;
    if first(&hostname).str().is_none() {
        return Err("could not connect to rtorrent: result isn't string".to_string());
    }
    let fields = ["", "main", "d.is_active=", "d.complete=", "d.message="];
    let calls: [(&str, &[&str]); 3] = [("d.multicall2", &fields), (DOWN_RATE, &[]), (UP_RATE, &[])];
    let rows_failed =
        |err: String| format!("could not get torrents: d.multicall2 XMLRPC call failed: {err}");
    let mut answers = multicall(shared, instance, &calls, Limit::Data)
        .await
        .map_err(rows_failed)?
        .into_iter();
    let mut next = || {
        answers
            .next()
            .unwrap_or_else(|| Err("an answer without a value".to_string()))
    };
    // The calls' errors in the order sb-go met them: the rows, their
    // count, then each rate.
    let rows = next().map_err(rows_failed)?;
    let mut counts = count(&rows)?;
    drop(rows);
    let rate = |answer: Result<Value, String>, method: &str, what: &str| {
        let value = answer.map_err(|err| {
            format!("could not get {what} rate: {method} XMLRPC call failed: {err}")
        })?;
        first(&value)
            .int()
            .ok_or_else(|| format!("could not get {what} rate: result isn't int"))
    };
    counts.down = rate(next(), DOWN_RATE, "download")?;
    counts.up = rate(next(), UP_RATE, "upload")?;
    Ok(counts)
}

/// A single value, or the first of a list (go-rtorrent's reading of a
/// scalar answer).
fn first(value: &Value) -> &Value {
    match value {
        Value::Array(items) => items.first().unwrap_or(value),
        other => other,
    }
}

/// sb-go's counting of the multicall's rows (`[is_active, complete,
/// message]` each): a message is an error; else active and complete is
/// seeding, active is downloading, inactive is stopped.
fn count(rows: &Value) -> Result<Counts, String> {
    let bad = || "could not get torrents: unexpected answer from rTorrent".to_string();
    let Value::Array(rows) = rows else {
        return Err(bad());
    };
    let mut counts = Counts::default();
    for row in rows {
        let Value::Array(fields) = row else {
            return Err(bad());
        };
        let [active, complete, message] = fields.as_slice() else {
            return Err(bad());
        };
        let (Some(active), Some(complete), Some(message)) =
            (active.int(), complete.int(), message.str())
        else {
            return Err(bad());
        };
        if !message.is_empty() {
            counts.errored += 1;
        } else if active == 1 {
            if complete > 0 {
                counts.seeding += 1;
            } else {
                counts.downloading += 1;
            }
        } else {
            counts.stopped += 1;
        }
    }
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use sb_config::testing::{Answer as Reply, Fake};

    use super::*;
    use crate::providers::tests::{instance, painted, refused, section, shared};

    fn answer(value: &str) -> Reply {
        Reply::new(
            200,
            format!("<?xml version=\"1.0\"?><methodResponse><params><param><value>{value}</value></param></params></methodResponse>"),
        )
        .header("content-type", "text/xml")
    }

    fn row(active: i64, complete: i64, message: &str) -> String {
        format!(
            "<value><array><data><value><i8>{active}</i8></value><value><i8>{complete}</i8></value><value><string>{message}</string></value></data></array></value>"
        )
    }

    /// A `system.multicall` answer: each entry one call's values (an
    /// array) or its fault (a struct), as written.
    fn multi(entries: &[&str]) -> Reply {
        let entries = entries.join("</value><value>");
        answer(&format!(
            "<array><data><value>{entries}</value></data></array>"
        ))
    }

    /// One call's values in a multicall answer.
    fn ok(value: &str) -> String {
        format!("<array><data><value>{value}</value></data></array>")
    }

    /// One call's fault in a multicall answer.
    fn fault(code: i64, message: &str) -> String {
        format!(
            "<struct><member><name>faultCode</name><value><i4>{code}</i4></value></member><member><name>faultString</name><value><string>{message}</string></value></member></struct>"
        )
    }

    fn script(fake: &Fake, rows: &str) {
        fake.script("/RPC2", answer("<string>seedbox</string>"));
        fake.script(
            "/RPC2",
            multi(&[
                &ok(&format!("<array><data>{rows}</data></array>")),
                &ok("<i8>2097152</i8>"),
                &ok("<i8>1536</i8>"),
            ]),
        );
    }

    /// `with_login`'s user and password as HTTP Basic sends them: the
    /// base64 of `rt-user:rt-pass-canary`, a test canary.
    const CANARY_BASIC: &str = "cnQtdXNlcjpydC1wYXNzLWNhbmFyeQ==";

    fn with_login(url: &str) -> Instance {
        Instance {
            user: Some("rt-user".into()),
            password: Some("rt-pass-canary".into()),
            ..instance("rTorrent", url)
        }
    }

    /// The check, then exactly three calls in one `system.multicall`
    /// request: the rows' multicall with the three fields, the two rates;
    /// Basic auth; every state counted.
    #[tokio::test]
    async fn rtorrent_makes_one_multicall_after_the_check() {
        let fake = Fake::start().unwrap();
        let rows = [
            row(1, 0, ""),
            row(1, 1, ""),
            row(1, 1, ""),
            row(0, 1, ""),
            row(0, 0, "Tracker: [Failure]"),
        ]
        .concat();
        script(&fake, &rows);
        let url = format!("{}/RPC2", fake.url());
        let one = section(vec![with_login(&url)]);
        let out = rtorrent(Some(&one), &shared(&[]), &Paint::plain()).await;
        let text = out.text.clone();
        // `--json` (motd spec): the same answer, typed.
        assert_eq!(
            crate::providers::tests::instances(&out),
            r#"[{"name":"rTorrent","value":{"down":"2.0 MB/s","downloading":1,"errored":1,"seeding":2,"stopped":1,"up":"1.5 KB/s"}}]"#
        );
        assert_eq!(
            text,
            "↓2.0 MB/s ↑1.5 KB/s | 1 Downloading | 2 Seeding | 1 Stopped | 1 Error"
        );
        let seen = fake.seen();
        let methods: Vec<String> = seen
            .iter()
            .map(|call| {
                let body = &call.body;
                let start = body.find("<methodName>").unwrap() + "<methodName>".len();
                body[start..start + body[start..].find('<').unwrap()].to_string()
            })
            .collect();
        assert_eq!(methods, ["system.hostname", "system.multicall"]);
        let calls: Vec<&str> = seen[1]
            .body
            .split("<name>methodName</name><value><string>")
            .skip(1)
            .map(|rest| &rest[..rest.find('<').unwrap()])
            .collect();
        assert_eq!(
            calls,
            [
                "d.multicall2",
                "throttle.global_down.rate",
                "throttle.global_up.rate"
            ]
        );
        assert!(
            seen[1].body.contains(
                "<name>params</name><value><array><data><value><string></string></value><value><string>main</string></value>"
            ),
            "{}",
            seen[1].body
        );
        assert!(seen[1].body.contains("<string>d.is_active=</string>"));
        assert_eq!(
            seen[1]
                .body
                .matches("<name>params</name><value><array><data></data></array></value>")
                .count(),
            2,
            "{}",
            seen[1].body
        );
        for call in &seen {
            assert_eq!(call.method, "POST");
            assert_eq!(call.target, "/RPC2");
            assert_eq!(call.header("content-type"), Some("text/xml"));
            assert_eq!(
                call.header("authorization"),
                Some(format!("Basic {CANARY_BASIC}").as_str())
            );
        }
    }

    #[test]
    fn the_summary_reads_as_sb_gos() {
        let plain = Paint::plain();
        assert_eq!(Counts::default().summary(&plain), "No torrents present");
        let counts = Counts {
            downloading: 1,
            seeding: 0,
            stopped: 0,
            errored: 0,
            down: 0,
            up: 1023,
        };
        assert_eq!(
            counts.summary(&plain),
            "↓0 B/s ↑1023 B/s | 1 Downloading | 0 Seeding"
        );
        let paint = painted();
        let all = Counts {
            stopped: 2,
            errored: 3,
            ..counts
        };
        assert_eq!(
            all.summary(&paint),
            format!(
                "↓{} ↑{} | {} Downloading | {} Seeding | {} | {}",
                paint.value("0 B/s"),
                paint.value("1023 B/s"),
                paint.value("1"),
                paint.value("0"),
                paint.error("2 Stopped"),
                paint.error("3 Error")
            )
        );
    }

    /// No login without both parts; an empty list; a fault; an answer of
    /// another type; a status error; a refused port; no URL.
    #[tokio::test]
    async fn rtorrent_errors_are_lines() {
        let fake = Fake::start().unwrap();
        fake.script("/a", answer("<string>h</string>"));
        fake.script(
            "/a",
            multi(&[
                &ok("<array><data></data></array>"),
                &ok("<i4>0</i4>"),
                &ok("<i4>0</i4>"),
            ]),
        );
        let shared = shared(&["rt-pass-canary"]);
        let ask = |path: &str| {
            let url = format!("{}{path}", fake.url());
            let shared = Arc::clone(&shared);
            async move {
                let one = section(vec![instance("rTorrent", &url)]);
                rtorrent(Some(&one), &shared, &Paint::plain()).await.text
            }
        };
        assert_eq!(ask("/a").await, "No torrents present");
        assert!(
            fake.seen()
                .iter()
                .all(|call| call.header("authorization").is_none())
        );
        fake.script(
            "/b",
            Reply::new(
                200,
                "<methodResponse><fault><value><struct><member><name>faultCode</name><value><i4>-501</i4></value></member><member><name>faultString</name><value><string>Unsupported target type found.</string></value></member></struct></value></fault></methodResponse>",
            ),
        );
        assert_eq!(
            ask("/b").await,
            "Error: could not connect to rtorrent: system.hostname XMLRPC call failed: fault -501: Unsupported target type found."
        );
        fake.script("/c", answer("<i4>1</i4>"));
        assert_eq!(
            ask("/c").await,
            "Error: could not connect to rtorrent: result isn't string"
        );
        fake.script("/f", Reply::new(401, "<html>no</html>"));
        assert_eq!(
            ask("/f").await,
            "Error: could not connect to rtorrent: system.hostname XMLRPC call failed: rTorrent returned status code 401"
        );
        fake.script("/g", Reply::new(200, "<html>not xml-rpc</html>"));
        assert!(ask("/g").await.starts_with("Error: could not connect to rtorrent: system.hostname XMLRPC call failed: unexpected answer"));
        // The data request's failure is the first call's, the rows'.
        fake.script("/h", answer("<string>h</string>"));
        fake.script("/h", Reply::new(500, "busy"));
        assert_eq!(
            ask("/h").await,
            "Error: could not get torrents: d.multicall2 XMLRPC call failed: rTorrent returned status code 500"
        );
        // An answer that carries no value at all.
        fake.script(
            "/j",
            Reply::new(200, "<methodResponse><params></params></methodResponse>"),
        );
        assert_eq!(
            ask("/j").await,
            "Error: could not connect to rtorrent: system.hostname XMLRPC call failed: an answer without a value"
        );
        let refused = refused();
        let gone = section(vec![instance("rTorrent", &refused)]);
        let text = rtorrent(Some(&gone), &shared, &Paint::plain()).await.text;
        assert!(text.contains("XMLRPC call failed: POST failed: "), "{text}");
        let none = section(vec![instance("rTorrent", "")]);
        assert_eq!(
            rtorrent(Some(&none), &shared, &Paint::plain()).await.text,
            ""
        );
    }

    /// Each data call's own failure inside the one request names that
    /// call, in sb-go's order: the rows, then each rate; an answer
    /// missing entries or of another shape too.
    #[tokio::test]
    async fn a_multicall_entrys_failure_names_its_call() {
        let fake = Fake::start().unwrap();
        let shared = shared(&[]);
        let none = ok("<array><data></data></array>");
        let zero = ok("<i4>0</i4>");
        let rows = "could not get torrents: d.multicall2 XMLRPC call failed";
        let down = "could not get download rate: throttle.global_down.rate XMLRPC call failed";
        let up = "could not get upload rate: throttle.global_up.rate XMLRPC call failed";
        let not_defined = fault(-506, "Method 'system.multicall' not defined");
        let cases: Vec<(Reply, String)> = vec![
            (
                multi(&[
                    &ok("<array><data><value><i8>1</i8></value></data></array>"),
                    &zero,
                ]),
                "could not get torrents: unexpected answer from rTorrent".into(),
            ),
            (
                multi(&[&none, &ok("<string>fast</string>"), &zero]),
                "could not get download rate: result isn't int".into(),
            ),
            (
                multi(&[&none, &zero, &ok("<string>x</string>")]),
                "could not get upload rate: result isn't int".into(),
            ),
            (
                multi(&[
                    &fault(-506, "Method 'd.multicall2' not defined"),
                    &zero,
                    &zero,
                ]),
                format!("{rows}: fault -506: Method 'd.multicall2' not defined"),
            ),
            (
                multi(&[&none, &zero, &fault(-501, "no")]),
                format!("{up}: fault -501: no"),
            ),
            (
                multi(&[&none]),
                format!("{down}: an answer without a value"),
            ),
            (
                multi(&["<array><data></data></array>", "<i4>0</i4>"]),
                format!("{rows}: an answer without a value"),
            ),
            (
                multi(&[&none, "<i4>0</i4>"]),
                format!(
                    "{down}: unexpected answer: a multicall entry that isn't an array or a fault"
                ),
            ),
            (
                answer("<i4>0</i4>"),
                format!("{rows}: unexpected answer: a multicall answer that isn't an array"),
            ),
            // The whole request's fault (an rTorrent without the method)
            // is the rows' call's.
            (
                Reply::new(
                    200,
                    format!(
                        "<methodResponse><fault><value>{not_defined}</value></fault></methodResponse>"
                    ),
                ),
                format!("{rows}: fault -506: Method 'system.multicall' not defined"),
            ),
        ];
        for (n, (reply, expected)) in cases.into_iter().enumerate() {
            let path = format!("/{n}");
            fake.script(&path, answer("<string>h</string>"));
            fake.script(&path, reply);
            let url = format!("{}{path}", fake.url());
            let one = section(vec![instance("rTorrent", &url)]);
            let text = rtorrent(Some(&one), &shared, &Paint::plain()).await.text;
            assert_eq!(text, format!("Error: {expected}"), "case {n}");
        }
    }

    /// A server echoing the request's `Authorization` header shows the
    /// login hidden: the Basic token is a credential's form too.
    #[tokio::test]
    async fn an_echoed_basic_login_is_hidden() {
        let fake = Fake::start().unwrap();
        fake.script(
            "/RPC2",
            Reply::new(
                200,
                format!("<methodResponse><fault><value><struct><member><name>faultCode</name><value><i4>-1</i4></value></member><member><name>faultString</name><value><string>Authorization: Basic {CANARY_BASIC}</string></value></member></struct></value></fault></methodResponse>"),
            ),
        );
        let one = section(vec![with_login(&format!("{}/RPC2", fake.url()))]);
        let text = rtorrent(Some(&one), &shared(&[]), &Paint::plain())
            .await
            .text;
        assert!(
            text.ends_with("fault -1: Authorization: Basic [REDACTED]"),
            "{text}"
        );
        let none = instance("rTorrent", "http://x");
        assert_eq!(shared(&[]).hide_login(&none, CANARY_BASIC), CANARY_BASIC);
    }

    /// A check that takes the connection and never answers ends at the
    /// fixed 1 s, whatever the instance's `timeout`.
    #[tokio::test]
    async fn a_hung_check_ends_at_one_second() {
        let fake = Fake::start().unwrap();
        fake.script("/RPC2", answer("<string>h</string>").held());
        let mut slow = instance("rTorrent", &format!("{}/RPC2", fake.url()));
        slow.timeout = Some(Duration::from_secs(5));
        let start = tokio::time::Instant::now();
        let text = rtorrent(Some(&section(vec![slow])), &shared(&[]), &Paint::plain())
            .await
            .text;
        let took = start.elapsed();
        assert!(
            text.starts_with("Error: could not connect to rtorrent: "),
            "{text}"
        );
        assert!(
            took >= Duration::from_millis(900) && took < Duration::from_secs(3),
            "{took:?}"
        );
    }

    #[test]
    fn rows_of_another_shape_are_unexpected() {
        for rows in [
            Value::Int(1),
            Value::Array(vec![Value::Int(1)]),
            Value::Array(vec![Value::Array(vec![Value::Int(1), Value::Int(1)])]),
            Value::Array(vec![Value::Array(vec![
                Value::Str("1".into()),
                Value::Int(1),
                Value::Str(String::new()),
            ])]),
        ] {
            assert!(count(&rows).is_err(), "{rows:?}");
        }
        assert_eq!(first(&Value::Array(Vec::new())), &Value::Array(Vec::new()));
        assert_eq!(first(&Value::Array(vec![Value::Int(4)])), &Value::Int(4));
    }
}
