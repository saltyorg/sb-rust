//! The fake's `/events`, as dockerd serves it without `since`: one JSON
//! object a line, kept open, with the `type` filter applied
//! (docs/platform-notes/docker-logs.md, "Container events"). Unlike
//! dockerd, which sends only what happens after the request, each stream
//! starts with every event the fake has been given (with
//! [`FakeEngine::event`](super::FakeEngine::event)), so a test needn't
//! wait for the request before giving one. It ends once it has sent them
//! all after [`FakeEngine::end_events`](super::FakeEngine::end_events).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use super::{Fake, Shared, lock, record, refused};

/// A container event as dockerd writes it, for
/// [`FakeEngine::event`](super::FakeEngine::event).
#[must_use]
pub fn container_event(action: &str, id: &str, name: &str) -> Value {
    json!({
        "Type": "container",
        "Action": action,
        "Actor": { "ID": id, "Attributes": { "name": name } },
        "scope": "local",
        "time": 1_768_470_400,
        "timeNano": 1_768_470_400_000_000_000_i64,
    })
}

/// The `type` values of the request's `filters` (a JSON object of lists,
/// as bollard sends it); `None` without one.
fn types(uri: &Uri) -> Option<Vec<String>> {
    // Parsed as the controller fake does
    // (<https://docs.rs/form_urlencoded/1.2.2/form_urlencoded/fn.parse.html>);
    // why not axum's `Query`: the workspace builds axum without its `query`
    // feature.
    let query: HashMap<String, String> = uri
        .query()
        .map_or_default(|q| form_urlencoded::parse(q.as_bytes()).into_owned().collect());
    let filters: HashMap<String, Vec<String>> = serde_json::from_str(query.get("filters")?).ok()?;
    filters.get("type").cloned()
}

/// Each kept event, one a line.
fn encode(types: Option<&[String]>, events: &[Value]) -> Vec<u8> {
    let mut out = Vec::new();
    for event in events {
        let kind = event["Type"].as_str().unwrap_or_default();
        if types.is_none_or(|types| types.iter().any(|t| t == kind)) {
            out.extend_from_slice(event.to_string().as_bytes());
            out.push(b'\n');
        }
    }
    out
}

pub(super) async fn events(State(shared): Fake, uri: Uri) -> Response {
    events_answer(shared, &uri, None)
}

pub(super) async fn events_at(State(shared): Fake, uri: Uri, Path(v): Path<String>) -> Response {
    events_answer(shared, &uri, Some(&v))
}

fn events_answer(shared: Arc<Shared>, uri: &Uri, version: Option<&str>) -> Response {
    record(&shared, uri);
    if let Some(refusal) = refused(&shared, version) {
        return refusal;
    }
    let headers = [
        // What Docker 29 sends (the platform note).
        ("Content-Type", "application/jsonl".to_string()),
        ("Api-Version", shared.max_version()),
    ];
    let types = types(uri);
    let changes = shared.changes.subscribe();
    let state = (shared, types, 0_usize, changes);
    // As the log follow: futures-util's `unfold` into axum's
    // `Body::from_stream` (<https://docs.rs/axum/0.8.9/axum/body/struct.Body.html#method.from_stream>).
    let body = futures_util::stream::unfold(state, |mut state| async move {
        let (shared, types, sent, changes) = &mut state;
        loop {
            {
                let events = lock(&shared.events);
                if events.len() > *sent {
                    let bytes = encode(types.as_deref(), &events[*sent..]);
                    *sent = events.len();
                    drop(events);
                    if bytes.is_empty() {
                        continue;
                    }
                    return Some((Ok::<_, std::io::Error>(Bytes::from(bytes)), state));
                }
                if shared.events_ended.load(Ordering::SeqCst) {
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
