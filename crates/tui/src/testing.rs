//! Test support (`testing` feature; tui spec, Project Structure): a fake
//! source the test drives by hand, for the viewer's tests against the
//! source protocol.

use std::str::FromStr as _;

use sb_host::journal::Cursor;
use tokio::sync::mpsc;

use crate::source::{Event, Inner, Line, Outbox, Position, Request, SourceHandle};

/// A fake source: the test reads the viewer's requests and sends the
/// events it chooses.
#[derive(Debug)]
pub struct FakeSource {
    requests: mpsc::UnboundedReceiver<Request>,
    outbox: Outbox,
}

/// A source handle whose requests reach the returned [`FakeSource`],
/// which sends through `outbox`.
#[must_use]
pub fn fake_source(outbox: Outbox) -> (SourceHandle, FakeSource) {
    let (handle, requests) = SourceHandle::detached(outbox.generation());
    (handle, FakeSource { requests, outbox })
}

impl FakeSource {
    /// The next request the viewer sent, if one is waiting.
    pub fn request(&mut self) -> Option<Request> {
        self.requests.try_recv().ok()
    }

    /// Resolves once the viewer is gone (its channel's receiver
    /// dropped), so a test can keep the source open until then.
    pub async fn viewer_gone(&self) {
        self.outbox.closed().await;
    }

    /// Send `event` as the source would. `false` once the viewer is gone.
    #[must_use]
    pub fn send(&self, event: Event) -> bool {
        self.outbox.send(event)
    }

    /// Send `event` tagged with another generation, as a source the
    /// viewer left would have.
    #[must_use]
    pub fn send_stale(&self, event: Event) -> bool {
        let stale = self.outbox.generation().next();
        self.outbox.retagged(stale).send(event)
    }
}

/// A position that orders as `n` does: a journal cursor of one boot,
/// sequence number `n`.
///
/// # Panics
///
/// Never: the cursor text is well formed.
#[must_use]
pub fn position(n: u64) -> Position {
    let id = "1".repeat(32);
    let text = format!("s={id};i={n:x};b={id};m={n:x};t={n:x};x=0");
    let cursor = Cursor::from_str(&text).expect("a cursor's text");
    Position(Inner::Journal(cursor))
}

/// A line at `position(n)` with `message`, no time and no origin.
#[must_use]
pub fn line(n: u64, message: &str) -> Line {
    Line {
        time: None,
        origin: Vec::new(),
        message: message.as_bytes().to_vec(),
        cut: 0,
        position: position(n),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::Notify;

    use super::*;
    use crate::source::Generation;

    #[test]
    fn a_fake_source_takes_requests_and_sends_events() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let generation = Generation::default().next();
        let outbox = Outbox::new(tx, Arc::new(Notify::new()), generation);
        let (handle, mut fake) = fake_source(outbox);
        assert_eq!(handle.generation(), generation);
        assert_eq!(fake.request(), None);
        handle.send(Request::Tail);
        handle.send(Request::Older(position(3)));
        assert_eq!(fake.request(), Some(Request::Tail));
        assert_eq!(fake.request(), Some(Request::Older(position(3))));
        assert!(fake.send(Event::Followed(vec![line(4, "m4")])));
        let got = rx.try_recv().expect("an event");
        assert_eq!(got.generation, generation);
        assert_eq!(got.event, Event::Followed(vec![line(4, "m4")]));
    }

    #[test]
    fn positions_differ_by_their_number() {
        assert_eq!(position(1), position(1));
        assert_ne!(position(1), position(2));
        assert_eq!(line(2, "x").message, b"x");
    }
}
