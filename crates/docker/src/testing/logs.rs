//! The fake's `/containers/{id}/logs`, as dockerd serves it
//! (docs/platform-notes/docker-logs.md): `tail` applied before `since`
//! and `until`, both one-sided (skip to the first entry at or after
//! `since`, stop at the first after `until`), multiplexed frames without a
//! TTY and the raw stream with one, and `follow` sending entries appended
//! with [`FakeEngine::append`](super::FakeEngine::append) until
//! [`FakeEngine::end_follows`](super::FakeEngine::end_follows).

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use jiff::Timestamp;
use serde_json::json;

use super::{Fake, Shared, answer, lock, record, refused};
use crate::logs::LogStream;

/// One entry of a fake container's log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FakeEntry {
    pub time: Timestamp,
    pub stream: LogStream,
    /// What the container wrote, its newline included (none for a part of
    /// a long line); a TTY container's lines end in `\r\n`.
    pub bytes: Vec<u8>,
}

impl FakeEntry {
    /// An entry on stdout (or the TTY, for a TTY container's log).
    #[must_use]
    pub fn new(time: Timestamp, bytes: impl Into<Vec<u8>>) -> Self {
        FakeEntry {
            time,
            stream: LogStream::Stdout,
            bytes: bytes.into(),
        }
    }

    /// The same entry on `stream`.
    #[must_use]
    pub fn on(mut self, stream: LogStream) -> Self {
        self.stream = stream;
        self
    }
}

/// A fake container's log.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FakeLog {
    /// The container has a TTY: the raw stream, no frames.
    pub tty: bool,
    pub entries: Vec<FakeEntry>,
}

/// The query, as bollard sends it (always both streams: the fake serves
/// both).
#[derive(Debug, Default)]
struct Asked {
    follow: bool,
    timestamps: bool,
    /// `None` for `all`.
    tail: Option<usize>,
    /// Whole seconds; 0 is no bound.
    since: i128,
    until: i128,
}

impl Asked {
    fn from(uri: &Uri) -> Self {
        // Parsed as the controller fake does
        // (<https://docs.rs/form_urlencoded/1.2.2/form_urlencoded/fn.parse.html>).
        let query: HashMap<String, String> = uri
            .query()
            .map_or_default(|q| form_urlencoded::parse(q.as_bytes()).into_owned().collect());
        let flag = |name: &str| matches!(query.get(name).map(String::as_str), Some("1" | "true"));
        let secs = |name: &str| query.get(name).and_then(|v| v.parse().ok()).unwrap_or(0);
        Asked {
            follow: flag("follow"),
            timestamps: flag("timestamps"),
            tail: query.get("tail").and_then(|t| t.parse().ok()),
            since: secs("since"),
            until: secs("until"),
        }
    }

    /// dockerd's window: the tail first, then `since`, then `until`.
    fn window<'a>(&self, entries: &'a [FakeEntry]) -> Vec<&'a FakeEntry> {
        let start = self.tail.map_or(0, |n| entries.len().saturating_sub(n));
        let since = self.since * 1_000_000_000;
        let until = self.until * 1_000_000_000;
        entries[start..]
            .iter()
            .skip_while(|e| e.time.as_nanosecond() < since)
            .take_while(|e| self.until == 0 || e.time.as_nanosecond() <= until)
            .collect()
    }

    /// The entries as dockerd writes them: each with its time (fixed nine
    /// decimals, as Docker's) when asked, in a frame unless `tty`.
    fn encode<'a>(&self, tty: bool, entries: impl IntoIterator<Item = &'a FakeEntry>) -> Vec<u8> {
        let mut out = Vec::new();
        for entry in entries {
            let mut payload = Vec::new();
            if self.timestamps {
                // jiff's `Display` with nine decimals, as Docker writes them
                // (<https://docs.rs/jiff/0.2.38/jiff/struct.Timestamp.html#impl-Display-for-Timestamp>).
                payload.extend_from_slice(format!("{:.9} ", entry.time).as_bytes());
            }
            payload.extend_from_slice(&entry.bytes);
            if !tty {
                let kind = if entry.stream == LogStream::Stderr {
                    2
                } else {
                    1
                };
                let len = u32::try_from(payload.len()).unwrap_or(u32::MAX);
                out.extend_from_slice(&[kind, 0, 0, 0]);
                out.extend_from_slice(&len.to_be_bytes());
            }
            out.extend_from_slice(&payload);
        }
        out
    }
}

pub(super) async fn logs(State(shared): Fake, uri: Uri, Path(id): Path<String>) -> Response {
    logs_answer(shared, &uri, None, id)
}

pub(super) async fn logs_at(
    State(shared): Fake,
    uri: Uri,
    Path((v, id)): Path<(String, String)>,
) -> Response {
    logs_answer(shared, &uri, Some(&v), id)
}

fn logs_answer(shared: Arc<Shared>, uri: &Uri, version: Option<&str>, id: String) -> Response {
    record(&shared, uri);
    if let Some(refusal) = refused(&shared, version) {
        return refusal;
    }
    let asked = Asked::from(uri);
    let (first, tty, sent) = {
        let logs = lock(&shared.logs);
        let Some(log) = logs.get(&id) else {
            return answer(
                &shared,
                StatusCode::NOT_FOUND,
                json!({ "message": format!("No such container: {id}") }),
            );
        };
        (
            asked.encode(log.tty, asked.window(&log.entries)),
            log.tty,
            log.entries.len(),
        )
    };
    let kind = if tty {
        "application/vnd.docker.raw-stream"
    } else {
        "application/vnd.docker.multiplexed-stream"
    };
    let headers = [
        ("Content-Type", kind.to_string()),
        ("Api-Version", shared.max_version()),
    ];
    if !asked.follow {
        return (StatusCode::OK, headers, first).into_response();
    }
    let changes = shared.changes.subscribe();
    let state = (shared, id, asked, sent, changes, Some(first));
    // The rest as it is appended, until the fake ends it (futures-util's
    // `unfold`, <https://docs.rs/futures-util/0.3.34/futures_util/stream/fn.unfold.html>;
    // axum's `Body::from_stream`, <https://docs.rs/axum/0.8.9/axum/body/struct.Body.html#method.from_stream>).
    let body = futures_util::stream::unfold(state, |mut state| async move {
        let (shared, id, asked, sent, changes, first) = &mut state;
        if let Some(first) = first.take() {
            return Some((Ok::<_, std::io::Error>(Bytes::from(first)), state));
        }
        loop {
            {
                let logs = lock(&shared.logs);
                let log = logs.get(id.as_str())?;
                if log.entries.len() > *sent {
                    let new = &log.entries[*sent..];
                    let bytes = asked.encode(log.tty, new);
                    *sent = log.entries.len();
                    drop(logs);
                    return Some((Ok(Bytes::from(bytes)), state));
                }
                if lock(&shared.ended).contains(id.as_str()) {
                    return None;
                }
            }
            if changes.changed().await.is_err() {
                return None;
            }
        }
    });
    (StatusCode::OK, headers, Body::from_stream(body)).into_response()
}
