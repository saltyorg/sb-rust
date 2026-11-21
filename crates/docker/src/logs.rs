//! A container's log, read page by page for the log viewer (docker spec,
//! `logs.rs`; tui spec, "Sources"; docs/platform-notes/docker-logs.md).
//!
//! Positions are a nanosecond time plus an ordinal among the entries of
//! that time, counted in the order the daemon sends them (stable across
//! reads, the platform note). The daemon applies `tail` before `since`
//! and `until`, and both of those are one-sided (the note), so:
//! - [`tail`] is the only read with `tail`;
//! - [`older`] reads time windows without `tail`, widened from 60 s;
//! - [`newer`] and [`follow`] start at the newest position's second and
//!   drop what was already shown;
//!
//! and every read filters its window itself. Reads go through bollard's
//! `Docker::logs`
//! (<https://docs.rs/bollard/0.21.1/bollard/struct.Docker.html#method.logs>),
//! with `timestamps` on; the RFC 3339 prefix is parsed with jiff.

use std::collections::{HashMap, VecDeque};
use std::ops::ControlFlow;
use std::time::Duration;

use anyhow::Context as _;
use bollard::container::LogOutput;
use bollard::query_parameters::{LogsOptions, LogsOptionsBuilder};
use futures_util::stream::BoxStream;
use futures_util::{Stream, StreamExt as _};
use jiff::Timestamp;

use crate::connect::Daemon;

/// Where an entry is in a container's log: its time, then its place
/// among the entries of that time, in the daemon's order. The derived
/// order compares the fields as declared: keep `time_ns` first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Position {
    /// Nanoseconds since the Unix epoch.
    pub time_ns: i128,
    pub ordinal: u32,
}

/// Which stream an entry came from; a container with a TTY has one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogStream {
    Stdout,
    Stderr,
    Tty,
}

/// One log entry: its position, its stream and its bytes as the daemon
/// sent them after the timestamp (a trailing newline kept; a part of a
/// line over 16 KiB has none).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogLine {
    pub position: Position,
    pub stream: LogStream,
    pub bytes: Vec<u8>,
}

/// A page of [`older`] or [`newer`]: its lines in log order, and whether
/// more lie beyond them in that direction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    pub lines: Vec<LogLine>,
    pub more: bool,
}

/// The size of the parts the daemon stores a long line in (moby's log
/// copier buffer; the platform note).
const PART: usize = 16 * 1024;

/// The entries [`tail`] reads before the page, to tell whether its first
/// one starts a line no entry cut off shares a time with.
const TAIL_EXTRA: usize = 4;

/// The first window of [`older`].
const FIRST_WINDOW: Duration = Duration::from_mins(1);

const NANOS: i128 = 1_000_000_000;

/// How many times a follow counts ordinals for before it forgets the old
/// ones (a follow runs as long as the viewer, a line or more a second).
const COUNTED_TIMES: usize = 4096;

/// How far behind the newest time a follow keeps counting: the parts of a
/// long line, and entries stored a little out of order, come within it.
const COUNTED_SPAN: i128 = 10 * NANOS;

/// Past this, [`older`]'s window goes back to the start at once, rather
/// than doubling on (only when the creation time isn't known).
const LAST_WINDOW: Duration = Duration::from_hours(24 * 30);

/// Turns the daemon's output into lines with positions: the timestamp
/// parsed, the ordinals counted, a TTY line split into its entries.
#[derive(Debug, Default)]
struct Lines {
    /// Entries seen so far per time, for the next ordinal; at most
    /// [`COUNTED_TIMES`], then pruned to the last [`COUNTED_SPAN`] each
    /// time it reaches `prune_at`.
    counts: HashMap<i128, u32>,
    /// The map's size at which it is pruned next: twice its size after
    /// the last prune, [`COUNTED_TIMES`] at least. A log of more than
    /// [`COUNTED_TIMES`] entries in [`COUNTED_SPAN`] (410 lines a second)
    /// would otherwise walk the whole map at every entry, the prune
    /// removing nothing (perf round 5: 61% of a busy follow's
    /// instructions). Why not std's `BTreeMap::split_off`, which would
    /// prune to the span exactly: every entry looks its time up, and a
    /// tree's lookup costs more than a hash's on every line, where the
    /// prune comes once the map has doubled.
    prune_at: usize,
    /// The last parsed time, for an entry without one.
    previous: i128,
    /// The newest parsed time.
    newest: i128,
}

impl Lines {
    /// The lines in one piece of the daemon's output.
    fn push(&mut self, output: LogOutput) -> Vec<LogLine> {
        match output {
            LogOutput::StdOut { message } => vec![self.entry(LogStream::Stdout, &message)],
            LogOutput::StdErr { message } => vec![self.entry(LogStream::Stderr, &message)],
            // The daemon sends no stdin in a log.
            LogOutput::StdIn { .. } => Vec::new(),
            // One line a message: bollard's `Docker::logs` decodes with
            // `NewlineLogOutputDecoder::new(false)`, which cuts a TTY
            // stream at newlines and gives a last line without one at the
            // end (bollard 0.21.1 `src/docker.rs`
            // `decode_into_stream_string`, `src/read.rs`; read, not
            // documented, and pinned by the workspace's `=0.21.1`).
            LogOutput::Console { message } => tty_parts(&message)
                .into_iter()
                .map(|part| self.entry(LogStream::Tty, part))
                .collect(),
        }
    }

    /// One entry, `<RFC 3339 time> <bytes>`. Without a time that parses, it
    /// keeps the previous entry's time, with the next ordinal, and its
    /// bytes whole.
    fn entry(&mut self, stream: LogStream, payload: &[u8]) -> LogLine {
        let (time_ns, bytes) = match split_time(payload) {
            Some((time, bytes)) => (time.as_nanosecond(), bytes),
            None => (self.previous, payload),
        };
        self.previous = time_ns;
        self.newest = self.newest.max(time_ns);
        if self.counts.len() >= self.prune_at.max(COUNTED_TIMES) {
            let keep = self.newest - COUNTED_SPAN;
            self.counts.retain(|time, _| *time >= keep);
            self.prune_at = self.counts.len().saturating_mul(2);
        }
        let count = self.counts.entry(time_ns).or_default();
        let ordinal = *count;
        *count += 1;
        LogLine {
            position: Position { time_ns, ordinal },
            stream,
            bytes: bytes.to_vec(),
        }
    }
}

/// The time at the start of `payload` and the bytes after its space.
fn split_time(payload: &[u8]) -> Option<(Timestamp, &[u8])> {
    // An RFC 3339 time with nanoseconds is 30 bytes; a little room for an
    // offset instead of `Z`.
    let space = payload.iter().take(40).position(|b| *b == b' ')?;
    let time = std::str::from_utf8(&payload[..space]).ok()?.parse().ok()?;
    Some((time, &payload[space + 1..]))
}

/// A TTY line's entries: a part of a long line is its time, a space and
/// 16 KiB, followed at once by the next part's time (the platform note).
/// A split is made only where a time follows the 16 KiB.
fn tty_parts(line: &[u8]) -> Vec<&[u8]> {
    let mut parts = Vec::new();
    let mut rest = line;
    while let Some((_, bytes)) = split_time(rest) {
        let body = rest.len() - bytes.len();
        let end = body + PART;
        if rest.len() > end && split_time(&rest[end..]).is_some() {
            parts.push(&rest[..end]);
            rest = &rest[end..];
        } else {
            break;
        }
    }
    parts.push(rest);
    parts
}

/// The whole second at or before `time_ns`.
fn floor_secs(time_ns: i128) -> i128 {
    time_ns.div_euclid(NANOS)
}

/// The whole second at or after `time_ns`. Why not `div_ceil`: signed
/// `div_ceil` is unstable (`int_roundings`) on the pinned toolchain.
fn ceil_secs(time_ns: i128) -> i128 {
    -(-time_ns).div_euclid(NANOS)
}

/// A second as the Engine API's `since`/`until`, at least 1 (0 would mean
/// no bound) and at most `i32::MAX`: bollard-stubs' `LogsOptions` has
/// `i32` seconds, so windows past 2038 need a bollard that widens them.
fn api_secs(secs: i128) -> i32 {
    i32::try_from(secs.max(1)).unwrap_or(i32::MAX)
}

fn options() -> LogsOptionsBuilder {
    LogsOptionsBuilder::new()
        .stdout(true)
        .stderr(true)
        .timestamps(true)
}

/// `id`'s log as bollard reads it with `options`, boxed
/// (`StreamExt::boxed`,
/// <https://docs.rs/futures-util/0.3.34/futures_util/stream/trait.StreamExt.html#method.boxed>),
/// so sb-docker alone compiles bollard's and hyper's client under it:
/// sb-tui polls every read, and polling bollard's own stream compiled a
/// copy of that client there (ADR-026's amendment). Not inlined, so
/// no caller's crate gets the wrapper, and with it the stream's code,
/// back.
#[inline(never)]
fn outputs(
    daemon: &Daemon,
    id: &str,
    options: LogsOptions,
) -> BoxStream<'static, Result<LogOutput, bollard::errors::Error>> {
    daemon.docker.logs(id, Some(options)).boxed()
}

/// Read `id`'s log with `options`, giving each line to `keep` until it
/// breaks or the log ends.
async fn read(
    daemon: &Daemon,
    id: &str,
    options: LogsOptions,
    mut keep: impl FnMut(LogLine) -> ControlFlow<()>,
) -> anyhow::Result<()> {
    let mut stream = outputs(daemon, id, options);
    let mut lines = Lines::default();
    while let Some(output) = stream.next().await {
        let output = output.with_context(|| format!("reading the log of {id}"))?;
        for line in lines.push(output) {
            if keep(line).is_break() {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// The last `n` entries of `id`'s log.
///
/// A tail can begin in the middle of entries sharing one time, and would
/// count their ordinals from the wrong one; every other read counts them
/// from the first. So [`TAIL_EXTRA`] entries more are read: when fewer
/// come back, the read is the whole log, counted from its start. Else,
/// when the last of them on the first's stream ends a line and none shares
/// the first's time, nothing cut off shares it either (the daemon sends a
/// log in time order, but for an entry or two stored out of it, the
/// platform note). Otherwise that time's second is read again and the
/// tail's first entries are numbered after the ones it cut off (the
/// daemon's order is stable across reads, the platform note). The second
/// read is a `since` window, which the daemon answers by passing over the
/// whole log: 0.56 s of its CPU on a log at Saltbox's cap, before the
/// first page shows (perf round 5, item 4).
///
/// # Errors
///
/// The daemon's error (its message kept), or a broken stream.
pub async fn tail(daemon: &Daemon, id: &str, n: usize) -> anyhow::Result<Vec<LogLine>> {
    // What the read gave, then only the entries before the page.
    let mut extra = Vec::new();
    let asked = n.saturating_add(TAIL_EXTRA);
    let last = options().tail(&asked.to_string()).build();
    read(daemon, id, last, |line| {
        extra.push(line);
        ControlFlow::Continue(())
    })
    .await?;
    let whole = extra.len() < asked;
    let mut found = extra.split_off(extra.len().saturating_sub(n));
    if whole {
        // The whole log: its ordinals count from its first entry.
        return Ok(found);
    }
    // Numbered as a read from the page's first entry numbers them.
    for line in &mut found {
        let earlier = extra
            .iter()
            .filter(|b| b.position.time_ns == line.position.time_ns)
            .count();
        let earlier = u32::try_from(earlier).unwrap_or(u32::MAX);
        line.position.ordinal = line.position.ordinal.saturating_sub(earlier);
    }
    let Some(first) = found.first() else {
        return Ok(found);
    };
    if starts_clean(&extra, first) {
        return Ok(found);
    }
    let first = first.position.time_ns;
    let mut total: u32 = 0;
    let window = options()
        .since(api_secs(floor_secs(first)))
        .until(api_secs(ceil_secs(first)))
        .build();
    read(daemon, id, window, |line| {
        if line.position.time_ns == first {
            total += 1;
        }
        ControlFlow::Continue(())
    })
    .await?;
    let cut = found
        .iter()
        .take_while(|line| line.position.time_ns == first)
        .count();
    // Saturating: an entry of a later second stored before the rest of this
    // time stops the window's read early (`until` is one-sided), and the
    // count comes short; the tail then keeps its own numbers.
    let skipped = total.saturating_sub(u32::try_from(cut).unwrap_or(u32::MAX));
    for line in &mut found[..cut] {
        line.position.ordinal += skipped;
    }
    Ok(found)
}

/// Whether nothing cut off before `first`, the first of a tail, can share
/// its time, judged from the entries read before it: the last of them on
/// its stream ends a line (so `first` starts one, not a later part of a
/// long line), and none has its time.
fn starts_clean(before: &[LogLine], first: &LogLine) -> bool {
    let line_start = before
        .iter()
        .rev()
        .find(|line| line.stream == first.stream)
        .is_some_and(|line| line.bytes.last() == Some(&b'\n'));
    line_start
        && !before
            .iter()
            .any(|line| line.position.time_ns == first.position.time_ns)
}

/// The first window for [`older`] just before a page of `lines` (in log
/// order), to find `n` entries: the time the lines span, scaled to `n`
/// of them and by half again, in whole seconds, one at least; `None` for
/// fewer than two lines. Every window costs the daemon a pass over the
/// whole log (`since` reads it from its start, the platform note), so a
/// window that holds the page at once saves the passes a fixed first
/// window doubles through on a slow log, and the lines it reads on a
/// busy one (perf round 5, item 3).
#[must_use]
pub fn window_before(lines: &[LogLine], n: usize) -> Option<Duration> {
    let [first, .., last] = lines else {
        return None;
    };
    let span = (last.position.time_ns - first.position.time_ns).unsigned_abs();
    let count = u128::try_from(lines.len()).unwrap_or(u128::MAX);
    let wanted = u128::try_from(n).unwrap_or(u128::MAX);
    let nanos = span.saturating_mul(wanted).saturating_mul(3) / count.saturating_mul(2);
    let secs = nanos.div_ceil(NANOS.unsigned_abs()).max(1);
    Some(Duration::from_secs(u64::try_from(secs).unwrap_or(u64::MAX)))
}

/// The next window's width after one of `width` seconds found `found` of
/// the `n` entries wanted: twice as wide, or as wide as the entries found
/// say holds `n` and half again, whichever is wider, and never under the
/// usual first window. A first window sized from the page before that
/// falls short (the log was slower before it) so reaches what holds the
/// page in a read or two, not by doubling from a second (perf round 5,
/// item 3: a log slow, then busy, read one window more at the change
/// widening to `usual` and doubling).
fn wider(width: i128, found: usize, n: usize, usual: i128) -> i128 {
    let doubled = width.saturating_mul(2);
    let paced = match (i128::try_from(found), i128::try_from(n)) {
        (Ok(found), Ok(n)) if found > 0 => {
            width.saturating_mul(n.saturating_mul(3)) / found.saturating_mul(2)
        }
        _ => 0,
    };
    doubled.max(paced).max(usual)
}

/// Up to `n` entries before `before`, the last ones before it. Windows of
/// whole seconds end at the second after `before`; the first starts
/// `first` before it ([`window_before`] the page before; 60 s without
/// one), and each next one twice as far (with `first`, as far as the
/// entries found say holds `n` when that is farther, 60 s at least:
/// [`wider`]), until `n` entries are found
/// or the window starts at `created` (the container's creation, when
/// known). Only then is `more` false, unless more than `n` were
/// found.
///
/// # Errors
///
/// The daemon's error (its message kept), or a broken stream.
pub async fn older(
    daemon: &Daemon,
    id: &str,
    before: Position,
    n: usize,
    created: Option<Timestamp>,
    first: Option<Duration>,
) -> anyhow::Result<Page> {
    let until = ceil_secs(before.time_ns);
    let start = created.map_or(1, |created| floor_secs(created.as_nanosecond()).max(1));
    let usual = i128::from(FIRST_WINDOW.as_secs());
    let mut width = first.map_or(usual, |first| i128::from(first.as_secs()).max(1));
    loop {
        let mut since =
            floor_secs(before.time_ns.saturating_sub(width.saturating_mul(NANOS))).max(start);
        if width > i128::from(LAST_WINDOW.as_secs()) {
            since = start;
        }
        // A creation after `before` (a clock moved back): one window.
        let since = since.min(until);
        let mut found = Vec::new();
        let options = options()
            .since(api_secs(since))
            .until(api_secs(until))
            .build();
        read(daemon, id, options, |line| {
            if line.position < before {
                found.push(line);
            }
            ControlFlow::Continue(())
        })
        .await?;
        let whole = since <= start;
        if found.len() >= n || whole {
            // In position order, so the page is cut where the filter cut:
            // an entry stored out of time order (the platform note) lands
            // on its side of `before`.
            found.sort_by_key(|line| line.position);
            let more = !whole || found.len() > n;
            let lines = found.split_off(found.len().saturating_sub(n));
            return Ok(Page { lines, more });
        }
        width = if first.is_some() {
            wider(width, found.len(), n, usual)
        } else {
            width.saturating_mul(2)
        };
    }
}

/// Up to `n` entries after `after`: read from its second, the entries at
/// or before it dropped, stopped at one more than `n` (which sets `more`).
///
/// # Errors
///
/// The daemon's error (its message kept), or a broken stream.
pub async fn newer(daemon: &Daemon, id: &str, after: Position, n: usize) -> anyhow::Result<Page> {
    let mut lines = Vec::new();
    let options = options().since(api_secs(floor_secs(after.time_ns))).build();
    read(daemon, id, options, |line| {
        if line.position > after {
            lines.push(line);
        }
        if lines.len() > n {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .await?;
    let more = lines.len() > n;
    lines.truncate(n);
    Ok(Page { lines, more })
}

/// Which entries [`scan`] reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Range {
    /// The whole log, from the container's creation.
    All,
    /// The entries before this position.
    Before(Position),
    /// The entries after this position.
    After(Position),
}

/// The log text [`scan`] hands on between yields to the runtime: the log
/// viewer's runtime has one thread, which the screen shares (tui spec,
/// "Whole-log search", Docker). Why not tokio's
/// `task::coop::consume_budget`
/// (<https://docs.rs/tokio/1.54.1/tokio/task/coop/fn.consume_budget.html>):
/// its budget counts operations, and 128 entries of up to 64 KiB each are
/// about 8 MiB between yields; the spec bounds a step by its text.
const YIELD_EVERY: usize = 256 * 1024;

/// Every entry of `range`, oldest first, each to `each` until it breaks:
/// one read of the log, for the log viewer's whole-log search. The
/// Engine API has no search, and its `since` reads from the
/// log's start anyway (the platform note), so the read is one window:
/// from `created` (when known, else the log's start) to the second after
/// `Before`'s position, or from `After`'s second on, filtered here as
/// every read is. Ordinals count from each second's first entry, as the
/// pages' windows do, so a position found here is where a page shows it.
/// The task yields after every 256 KiB of log text, so the screen draws
/// between.
///
/// # Errors
///
/// The daemon's error (its message kept), or a broken stream.
pub async fn scan(
    daemon: &Daemon,
    id: &str,
    range: Range,
    created: Option<Timestamp>,
    mut each: impl FnMut(LogLine) -> ControlFlow<()>,
) -> anyhow::Result<ControlFlow<()>> {
    let start = created.map_or(1, |created| floor_secs(created.as_nanosecond()).max(1));
    let options = match range {
        Range::All => options().since(api_secs(start)).build(),
        Range::Before(before) => options()
            .since(api_secs(start.min(ceil_secs(before.time_ns))))
            .until(api_secs(ceil_secs(before.time_ns)))
            .build(),
        Range::After(after) => options().since(api_secs(floor_secs(after.time_ns))).build(),
    };
    let keep = |position: Position| match range {
        Range::All => true,
        Range::Before(before) => position < before,
        Range::After(after) => position > after,
    };
    let mut stream = outputs(daemon, id, options);
    let mut lines = Lines::default();
    let mut unyielded = 0;
    while let Some(output) = stream.next().await {
        let output = output.with_context(|| format!("reading the log of {id}"))?;
        for line in lines.push(output) {
            unyielded += line.bytes.len();
            if keep(line.position) && each(line).is_break() {
                return Ok(ControlFlow::Break(()));
            }
        }
        if unyielded >= YIELD_EVERY {
            unyielded = 0;
            tokio::task::yield_now().await;
        }
    }
    Ok(ControlFlow::Continue(()))
}

/// The first `n` entries of the log (the log viewer's Home, tui spec,
/// "Home and End"): one read from the creation (when known, else the
/// log's start), stopped at one more than `n` (which sets `more`), the
/// daemon's stream dropped there, so only the log's start is read. Why
/// not `tail`: it counts from the newest end, and the Engine API has no
/// head option (`LogsOptions` is follow, stdout, stderr, since, until,
/// timestamps and tail).
///
/// # Errors
///
/// The daemon's error (its message kept), or a broken stream.
pub async fn head(
    daemon: &Daemon,
    id: &str,
    n: usize,
    created: Option<Timestamp>,
) -> anyhow::Result<Page> {
    let start = created.map_or(1, |created| floor_secs(created.as_nanosecond()).max(1));
    let mut lines = Vec::new();
    let options = options().since(api_secs(start)).build();
    read(daemon, id, options, |line| {
        lines.push(line);
        if lines.len() > n {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .await?;
    let more = lines.len() > n;
    lines.truncate(n);
    Ok(Page { lines, more })
}

/// The entries after `after` as they come: [`newer`]'s read with
/// `follow`, until the daemon ends the stream (the container stopped) or
/// fails, which is the stream's last item.
pub fn follow(
    daemon: &Daemon,
    id: &str,
    after: Position,
) -> impl Stream<Item = anyhow::Result<LogLine>> + Send + 'static {
    let options = options()
        .since(api_secs(floor_secs(after.time_ns)))
        .follow(true)
        .build();
    let state = Follow {
        outputs: outputs(daemon, id, options),
        lines: Lines::default(),
        queue: VecDeque::new(),
        done: false,
        id: id.to_string(),
        after,
    };
    // futures-util's documented way to turn a state machine into a
    // stream (<https://docs.rs/futures-util/0.3.34/futures_util/stream/fn.unfold.html>).
    futures_util::stream::unfold(state, |mut state| async move {
        loop {
            if let Some(line) = state.queue.pop_front() {
                return Some((Ok(line), state));
            }
            if state.done {
                return None;
            }
            match state.outputs.next().await {
                Some(Ok(output)) => {
                    let lines = state.lines.push(output);
                    state.keep(lines);
                }
                Some(Err(err)) => {
                    state.done = true;
                    let err = anyhow::Error::new(err)
                        .context(format!("following the log of {}", state.id));
                    return Some((Err(err), state));
                }
                None => state.done = true,
            }
        }
    })
}

/// [`follow`]'s state between items.
struct Follow {
    outputs: BoxStream<'static, Result<LogOutput, bollard::errors::Error>>,
    lines: Lines,
    queue: VecDeque<LogLine>,
    done: bool,
    id: String,
    after: Position,
}

impl Follow {
    fn keep(&mut self, lines: Vec<LogLine>) {
        let after = self.after;
        self.queue
            .extend(lines.into_iter().filter(|line| line.position > after));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdout(text: &str) -> LogOutput {
        LogOutput::StdOut {
            message: text.as_bytes().to_vec().into(),
        }
    }

    fn stderr(text: &str) -> LogOutput {
        LogOutput::StdErr {
            message: text.as_bytes().to_vec().into(),
        }
    }

    fn console(bytes: &[u8]) -> LogOutput {
        LogOutput::Console {
            message: bytes.to_vec().into(),
        }
    }

    const T1: &str = "2026-01-15T10:00:00.123456789Z";
    const T2: &str = "2026-01-15T10:00:01.234567891Z";

    fn ns(time: &str) -> i128 {
        time.parse::<Timestamp>().expect("time").as_nanosecond()
    }

    fn at(time: &str, ordinal: u32) -> Position {
        Position {
            time_ns: ns(time),
            ordinal,
        }
    }

    fn summary(lines: &[LogLine]) -> Vec<(Position, LogStream, String)> {
        lines
            .iter()
            .map(|l| {
                (
                    l.position,
                    l.stream,
                    String::from_utf8_lossy(&l.bytes).into_owned(),
                )
            })
            .collect()
    }

    #[test]
    fn an_entry_is_its_time_and_its_bytes() {
        let mut lines = Lines::default();
        let found = lines.push(stdout(&format!("{T1} hello\n")));
        assert_eq!(
            summary(&found),
            [(at(T1, 0), LogStream::Stdout, "hello\n".into())]
        );
        let found = lines.push(stderr(&format!("{T2} oops\n")));
        assert_eq!(
            summary(&found),
            [(at(T2, 0), LogStream::Stderr, "oops\n".into())]
        );
    }

    /// The parts of a long line share a time, with another stream's entry
    /// between them (the platform note): ordinals count per time.
    #[test]
    fn equal_times_count_ordinals_in_the_daemons_order() {
        let mut lines = Lines::default();
        let found: Vec<_> = [
            stdout(&format!("{T1} part one")),
            stderr(&format!("{T2} between\n")),
            stdout(&format!("{T1} part two")),
            stdout(&format!("{T1} part three\n")),
        ]
        .into_iter()
        .flat_map(|output| lines.push(output))
        .collect();
        let positions: Vec<_> = found.iter().map(|l| l.position).collect();
        assert_eq!(positions, [at(T1, 0), at(T2, 0), at(T1, 1), at(T1, 2)]);
    }

    #[test]
    fn an_entry_without_a_time_keeps_the_previous_one_with_the_next_ordinal() {
        let mut lines = Lines::default();
        let first = lines.push(stdout("no time here\n"));
        assert_eq!(
            summary(&first),
            [(
                Position {
                    time_ns: 0,
                    ordinal: 0
                },
                LogStream::Stdout,
                "no time here\n".into()
            )]
        );
        lines.push(stdout(&format!("{T1} timed\n")));
        let found = lines.push(stdout("yesterday at noon\n"));
        assert_eq!(
            summary(&found),
            [(at(T1, 1), LogStream::Stdout, "yesterday at noon\n".into())]
        );
        let bad = lines.push(stdout("2026-13-45T99:00:00Z nonsense\n"));
        assert_eq!(bad[0].position, at(T1, 2));
        assert_eq!(bad[0].bytes, b"2026-13-45T99:00:00Z nonsense\n");
    }

    #[test]
    fn stdin_is_no_entry() {
        let mut lines = Lines::default();
        let stdin = LogOutput::StdIn {
            message: format!("{T1} typed\n").into_bytes().into(),
        };
        assert_eq!(lines.push(stdin), Vec::<LogLine>::new());
    }

    /// One message, one line (bollard cuts a TTY stream at newlines).
    #[test]
    fn a_tty_message_is_one_line() {
        let mut lines = Lines::default();
        let found = lines.push(console(format!("{T1} one\r\n").as_bytes()));
        assert_eq!(
            summary(&found),
            [(at(T1, 0), LogStream::Tty, "one\r\n".into())]
        );
        let found = lines.push(console(format!("{T1} prompt> ").as_bytes()));
        assert_eq!(
            summary(&found),
            [(at(T1, 1), LogStream::Tty, "prompt> ".into())]
        );
    }

    /// A follow's ordinal counts stay bounded: old times are forgotten,
    /// recent equal times still count.
    #[test]
    fn a_long_follow_forgets_old_times() {
        let mut lines = Lines::default();
        let base = ns(T1);
        for i in 0..10_000_i128 {
            let time = Timestamp::from_nanosecond(base + i * NANOS / 100).expect("time");
            lines.push(stdout(&format!("{time} line\n")));
            assert!(
                lines.counts.len() <= COUNTED_TIMES,
                "{}",
                lines.counts.len()
            );
        }
        let newest = Timestamp::from_nanosecond(base + 9_999 * NANOS / 100).expect("time");
        let again = lines.push(stdout(&format!("{newest} again\n")));
        assert_eq!(again[0].position.ordinal, 1);
        let oldest = Timestamp::from_nanosecond(base).expect("time");
        let forgotten = lines.push(stdout(&format!("{oldest} late\n")));
        assert_eq!(forgotten[0].position.ordinal, 0);
    }

    /// A follow busier than `COUNTED_TIMES` in `COUNTED_SPAN` (1,000 lines a
    /// second): the map is pruned again only once it has doubled since
    /// the last prune, not at every line, so it holds up to twice the
    /// last ten seconds' times; recent equal times still count, older
    /// ones are forgotten.
    #[test]
    fn a_busy_follow_prunes_once_the_map_has_doubled() {
        let mut lines = Lines::default();
        let base = ns(T1);
        let at = |i: i128| Timestamp::from_nanosecond(base + i * NANOS / 1_000).expect("time");
        let mut sizes = Vec::new();
        for i in 0..30_000_i128 {
            lines.push(stdout(&format!("{} line\n", at(i))));
            sizes.push(lines.counts.len());
        }
        // Ten seconds hold 10,001 times: today's prune would keep the map
        // at that size from the first prune on.
        let largest = sizes.iter().copied().max().unwrap_or_default();
        assert!(largest <= 2 * 10_001, "{largest}");
        assert!(lines.counts.len() > 12_000, "{}", lines.counts.len());
        // The map shrank twice: at 16,384 times down to the last ten
        // seconds (at 4,096 and 8,192 nothing was older), and at twice
        // that.
        let prunes = sizes.windows(2).filter(|w| w[1] < w[0]).count();
        assert_eq!(prunes, 2, "{prunes}");
        let again = lines.push(stdout(&format!("{} again\n", at(29_999))));
        assert_eq!(again[0].position.ordinal, 1);
        let late = lines.push(stdout(&format!("{} late\n", at(0))));
        assert_eq!(late[0].position.ordinal, 0);
    }

    /// `<time> <16 KiB><time> <rest>\r\n` (the platform note): two parts.
    #[test]
    fn a_long_tty_line_is_split_into_its_parts() {
        let first = "y".repeat(PART);
        let line = format!("{T2} {first}{T2} rest\r\n");
        let mut lines = Lines::default();
        let found = lines.push(console(line.as_bytes()));
        assert_eq!(
            summary(&found),
            [
                (at(T2, 0), LogStream::Tty, first.clone()),
                (at(T2, 1), LogStream::Tty, "rest\r\n".into()),
            ]
        );
        // Three parts, the last one short.
        let line = format!("{T2} {first}{T2} {first}{T2} end\r\n");
        let found = Lines::default().push(console(line.as_bytes()));
        assert_eq!(found.len(), 3);
        assert_eq!(found[2].bytes, b"end\r\n");
    }

    /// Text that only looks like a part boundary stays whole: a time must
    /// follow exactly 16 KiB.
    #[test]
    fn a_long_tty_line_without_a_time_at_16_kib_stays_whole() {
        let long = "z".repeat(PART + 10);
        let line = format!("{T1} {long}\r\n");
        let found = Lines::default().push(console(line.as_bytes()));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].bytes.len(), PART + 12);
        // A time in the text, but not at 16 KiB.
        let line = format!("{T1} {} {T2} later\r\n", "z".repeat(PART - 100));
        assert_eq!(Lines::default().push(console(line.as_bytes())).len(), 1);
        // Exactly 16 KiB and the newline: one entry.
        let line = format!("{T1} {}\n", "z".repeat(PART));
        assert_eq!(Lines::default().push(console(line.as_bytes())).len(), 1);
    }

    #[test]
    fn seconds_round_down_and_up() {
        assert_eq!(floor_secs(1_500_000_000), 1);
        assert_eq!(ceil_secs(1_500_000_000), 2);
        assert_eq!(floor_secs(2_000_000_000), 2);
        assert_eq!(ceil_secs(2_000_000_000), 2);
        assert_eq!(floor_secs(-1), -1);
        assert_eq!(ceil_secs(-1), 0);
        assert_eq!(api_secs(0), 1);
        assert_eq!(api_secs(-5), 1);
        assert_eq!(api_secs(1_768_471_201), 1_768_471_201);
        assert_eq!(api_secs(i128::from(i32::MAX) + 1), i32::MAX);
    }

    // Against the fake Engine API, which reads its log as dockerd does.

    use crate::connect::connect;
    use crate::testing::{FakeEngine, FakeEntry, FakeLog, Options};

    const ID: &str = "c0ffee";

    /// The `i`th second after a fixed start.
    fn second(i: i64) -> Timestamp {
        Timestamp::from_second(1_768_470_400 + i).expect("time")
    }

    /// A fake serving `log` as `ID`'s.
    fn engine(log: FakeLog) -> FakeEngine {
        FakeEngine::start(Options {
            logs: [(ID.to_string(), log)].into(),
            ..Options::default()
        })
        .expect("fake Engine API")
    }

    /// `count` lines a second apart, `line N` each, with three lines
    /// sharing one time every 7 seconds (`N.a`, `N.b`, `N.c`).
    fn long_log(count: i64) -> FakeLog {
        let mut entries = Vec::new();
        for i in 0..count {
            if i % 7 == 3 {
                for part in ["a", "b", "c"] {
                    entries.push(FakeEntry::new(second(i), format!("line {i}.{part}\n")));
                }
            } else {
                entries.push(FakeEntry::new(second(i), format!("line {i}\n")));
            }
        }
        FakeLog {
            tty: false,
            entries,
        }
    }

    fn texts(lines: &[LogLine]) -> Vec<String> {
        lines
            .iter()
            .map(|l| String::from_utf8_lossy(&l.bytes).trim_end().to_string())
            .collect()
    }

    fn all_texts(log: &FakeLog) -> Vec<String> {
        log.entries
            .iter()
            .map(|e| String::from_utf8_lossy(&e.bytes).trim_end().to_string())
            .collect()
    }

    /// A tail that begins inside entries sharing a time leaves them to
    /// `older`, which counts their ordinals from the first.
    #[tokio::test]
    async fn the_tail_leaves_a_cut_time_to_older() {
        let log = long_log(5);
        let engine = engine(log.clone());
        let daemon = connect(&engine.host()).await.expect("connect");
        // The last three are 3.b, 3.c and 4, numbered after 3.a.
        let lines = tail(&daemon, ID, 3).await.expect("tail");
        assert_eq!(texts(&lines), ["line 3.b", "line 3.c", "line 4"]);
        let ordinals: Vec<u32> = lines.iter().map(|l| l.position.ordinal).collect();
        assert_eq!(ordinals, [1, 2, 0]);
        let page = older(&daemon, ID, lines[0].position, 2, Some(second(0)), None)
            .await
            .expect("older");
        assert_eq!(texts(&page.lines), ["line 2", "line 3.a"]);
        assert_eq!(page.lines[1].position.ordinal, 0);
        // A tail shorter than asked is the whole log: nothing is cut, and
        // nothing is read again.
        let before = engine.paths().len();
        let lines = tail(&daemon, ID, 50).await.expect("tail");
        assert_eq!(texts(&lines), all_texts(&log));
        assert_eq!(engine.paths().len(), before + 1);
    }

    /// A tail all of one time, cut: numbered after the entries before.
    #[tokio::test]
    async fn a_tail_of_one_time_is_numbered_after_the_cut() {
        let engine = engine(long_log(4));
        let daemon = connect(&engine.host()).await.expect("connect");
        let lines = tail(&daemon, ID, 2).await.expect("tail");
        let positions: Vec<u32> = lines.iter().map(|l| l.position.ordinal).collect();
        assert_eq!(texts(&lines), ["line 3.b", "line 3.c"]);
        assert_eq!(positions, [1, 2]);
    }

    #[tokio::test]
    async fn the_tail_is_the_last_entries() {
        let log = long_log(10);
        let engine = engine(log.clone());
        let daemon = connect(&engine.host()).await.expect("connect");
        let lines = tail(&daemon, ID, 3).await.expect("tail");
        assert_eq!(texts(&lines), ["line 7", "line 8", "line 9"]);
        assert_eq!(
            lines[0].position,
            Position {
                time_ns: second(7).as_nanosecond(),
                ordinal: 0
            }
        );
        assert_eq!(lines[0].stream, LogStream::Stdout);
        // Four entries more than the page: the one before the page's
        // first ends a line, and none of them shares its time, so nothing
        // is read again.
        let paths = engine.paths();
        assert_eq!(paths.len(), 2, "{paths:?}");
        let path = paths[1].clone();
        assert!(
            path.contains("tail=7") && path.contains("timestamps=true"),
            "{path}"
        );
        assert!(!path.contains("follow=true"), "{path}");
    }

    /// A tail cut inside a long line (the entry before the page's first,
    /// on its stream, ends no line), or with no entry of the first's
    /// stream before it in the four read more, reads the first's second
    /// again; the page and its ordinals are those of a plain tail.
    #[tokio::test]
    async fn a_tail_that_may_start_inside_a_line_reads_its_time_again() {
        let mut entries: Vec<_> = (0..5)
            .map(|i| FakeEntry::new(second(i), format!("line {i}\n")))
            .collect();
        entries.extend([
            FakeEntry::new(second(5), "a long line's first part"),
            FakeEntry::new(second(5), "on stderr\n").on(LogStream::Stderr),
            FakeEntry::new(second(6), "its last part\n"),
        ]);
        entries.extend((7..10).map(|i| FakeEntry::new(second(i), format!("line {i}\n"))));
        let engine = engine(FakeLog {
            tty: false,
            entries,
        });
        let daemon = connect(&engine.host()).await.expect("connect");
        let lines = tail(&daemon, ID, 3).await.expect("tail");
        assert_eq!(texts(&lines), ["line 7", "line 8", "line 9"]);
        let lines = tail(&daemon, ID, 4).await.expect("tail");
        assert_eq!(
            texts(&lines),
            ["its last part", "line 7", "line 8", "line 9"]
        );
        let ordinals: Vec<u32> = lines.iter().map(|l| l.position.ordinal).collect();
        assert_eq!(ordinals, [0, 0, 0, 0]);
        let paths = engine.paths();
        // The first tail once; the second with its window.
        assert_eq!(paths.len(), 4, "{paths:?}");
        assert!(
            paths[3].contains("since=") && paths[3].contains("tail=all"),
            "{paths:?}"
        );
        // Only stderr before the page's first stdout entry.
        let mut entries = vec![FakeEntry::new(second(0), "line 0\n")];
        entries.extend(
            (1..5).map(|i| FakeEntry::new(second(i), format!("e{i}\n")).on(LogStream::Stderr)),
        );
        entries.push(FakeEntry::new(second(5), "line 5\n"));
        let engine = self::engine(FakeLog {
            tty: false,
            entries,
        });
        let daemon = connect(&engine.host()).await.expect("connect");
        let lines = tail(&daemon, ID, 1).await.expect("tail");
        assert_eq!(texts(&lines), ["line 5"]);
        assert_eq!(engine.paths().len(), 3, "{:?}", engine.paths());
    }

    /// Fewer entries than the page and the four more: the whole log, read
    /// once, its ordinals counted from its first entry, a time cut at the
    /// page's start included.
    #[tokio::test]
    async fn a_tail_of_the_whole_log_counts_from_its_start() {
        let entries = ["0.a", "0.b", "0.c"]
            .iter()
            .map(|t| FakeEntry::new(second(0), format!("line {t}\n")))
            .chain((1..3).map(|i| FakeEntry::new(second(i), format!("line {i}\n"))))
            .collect();
        let engine = engine(FakeLog {
            tty: false,
            entries,
        });
        let daemon = connect(&engine.host()).await.expect("connect");
        let lines = tail(&daemon, ID, 4).await.expect("tail");
        assert_eq!(texts(&lines), ["line 0.b", "line 0.c", "line 1", "line 2"]);
        let ordinals: Vec<u32> = lines.iter().map(|l| l.position.ordinal).collect();
        assert_eq!(ordinals, [1, 2, 0, 0]);
        assert_eq!(engine.paths().len(), 2, "{:?}", engine.paths());
    }

    /// Paging back from the tail to the start gives every entry once, in
    /// order, those sharing a time included (tui success criterion 5).
    #[tokio::test]
    async fn older_pages_reach_the_start_with_every_entry_once() {
        let log = long_log(400);
        let engine = engine(log.clone());
        let daemon = connect(&engine.host()).await.expect("connect");
        let mut seen = tail(&daemon, ID, 50).await.expect("tail");
        let created = Some(second(0));
        loop {
            let page = older(&daemon, ID, seen[0].position, 50, created, None)
                .await
                .expect("older");
            assert!(page.lines.len() <= 50);
            let mut lines = page.lines;
            lines.append(&mut seen);
            seen = lines;
            if !page.more {
                break;
            }
        }
        assert_eq!(texts(&seen), all_texts(&log));
        let positions: Vec<_> = seen.iter().map(|l| l.position).collect();
        assert!(
            positions.windows(2).all(|w| w[0] < w[1]),
            "in order, no duplicate"
        );
        // Windows without `tail`, of whole seconds.
        let paths = engine.paths();
        assert!(
            paths[2..].iter().all(|p| !p.contains("since=0&")
                && !p.contains("until=0&")
                && p.contains("tail=all")),
            "{paths:?}"
        );
    }

    /// An hour's gap: the window doubles until it reaches the entries.
    #[tokio::test]
    async fn older_widens_its_window_across_a_gap() {
        let mut log = long_log(5);
        log.entries
            .push(FakeEntry::new(second(3_600), "after the gap\n"));
        let engine = engine(log);
        let daemon = connect(&engine.host()).await.expect("connect");
        let last = tail(&daemon, ID, 1).await.expect("tail");
        let page = older(&daemon, ID, last[0].position, 3, Some(second(0)), None)
            .await
            .expect("older");
        assert_eq!(texts(&page.lines), ["line 3.b", "line 3.c", "line 4"]);
        assert!(page.more);
        // 60 s, 120 s, ... : each window starts twice as far back (after
        // the version and the tail, read once: its first entry starts a
        // line).
        let sinces: Vec<i64> = engine
            .paths()
            .iter()
            .skip(2)
            .filter_map(|p| p.split("since=").nth(1)?.split('&').next()?.parse().ok())
            .collect();
        let start = second(3_600).as_second();
        assert_eq!(sinces[0], start - 60);
        assert_eq!(sinces[1], start - 120);
        assert!(sinces.windows(2).all(|w| w[0] > w[1]), "{sinces:?}");
    }

    /// Lines `secs` seconds from the log's start.
    fn at_seconds(secs: &[i64]) -> Vec<LogLine> {
        secs.iter()
            .map(|&i| LogLine {
                position: Position {
                    time_ns: second(i).as_nanosecond(),
                    ordinal: 0,
                },
                stream: LogStream::Stdout,
                bytes: b"x\n".to_vec(),
            })
            .collect()
    }

    /// The window before a page: its lines' span scaled to `n` lines and
    /// by half again, whole seconds, one at least; none without a span.
    #[test]
    fn the_window_before_a_page_follows_its_pace() {
        let ten: Vec<i64> = (0..10).collect();
        // 9 s for 10 lines, 20 asked: 27 s.
        assert_eq!(
            window_before(&at_seconds(&ten), 20),
            Some(Duration::from_secs(27))
        );
        assert_eq!(
            window_before(&at_seconds(&ten), 10),
            Some(Duration::from_secs(14))
        );
        // All in one second: one second.
        assert_eq!(
            window_before(&at_seconds(&[5, 5, 5]), 500),
            Some(Duration::from_secs(1))
        );
        assert_eq!(window_before(&at_seconds(&[5]), 500), None);
        assert_eq!(window_before(&[], 500), None);
    }

    /// Given a first window, `older` starts with it: one read where it
    /// holds the page.
    #[tokio::test]
    async fn older_starts_with_the_window_given() {
        let engine = engine(long_log(400));
        let daemon = connect(&engine.host()).await.expect("connect");
        let last = tail(&daemon, ID, 1).await.expect("tail");
        let page = older(
            &daemon,
            ID,
            last[0].position,
            3,
            Some(second(0)),
            Some(Duration::from_secs(5)),
        )
        .await
        .expect("older");
        assert_eq!(texts(&page.lines), ["line 396", "line 397", "line 398"]);
        assert!(page.more);
        let paths = engine.paths();
        assert_eq!(paths.len(), 3, "{paths:?}");
        let since = second(399).as_second() - 5;
        assert!(paths[2].contains(&format!("since={since}&")), "{paths:?}");
    }

    /// A first window shorter than [`FIRST_WINDOW`] that falls short
    /// widens to it at once, then doubles: never more reads than without
    /// it, but the first.
    #[tokio::test]
    async fn a_short_first_window_widens_to_the_usual_one() {
        let mut log = long_log(5);
        log.entries
            .push(FakeEntry::new(second(3_600), "after the gap\n"));
        let engine = engine(log);
        let daemon = connect(&engine.host()).await.expect("connect");
        let last = tail(&daemon, ID, 1).await.expect("tail");
        let page = older(
            &daemon,
            ID,
            last[0].position,
            3,
            Some(second(0)),
            Some(Duration::from_secs(2)),
        )
        .await
        .expect("older");
        assert_eq!(texts(&page.lines), ["line 3.b", "line 3.c", "line 4"]);
        let sinces: Vec<i64> = engine
            .paths()
            .iter()
            .skip(2)
            .filter_map(|p| p.split("since=").nth(1)?.split('&').next()?.parse().ok())
            .collect();
        let start = second(3_600).as_second();
        assert_eq!(sinces[..3], [start - 2, start - 60, start - 120]);
    }

    /// The next window: twice as wide, or the found entries' pace to `n`
    /// and half again, the usual first window at least.
    #[test]
    fn a_window_that_fell_short_widens_by_what_it_found() {
        assert_eq!(wider(60, 0, 500, 60), 120);
        assert_eq!(wider(1, 0, 500, 60), 60);
        assert_eq!(wider(60, 30, 500, 60), 1_500);
        assert_eq!(wider(600, 400, 500, 60), 1_200);
        assert_eq!(wider(2, 2, 50, 60), 75);
        assert_eq!(wider(i128::MAX, 1, 500, 60), i128::MAX);
    }

    /// A short first window with entries in it: the next one from their
    /// pace, one read more, not a doubling from it.
    #[tokio::test]
    async fn a_short_first_window_widens_by_its_entries_pace() {
        let engine = engine(long_log(400));
        let daemon = connect(&engine.host()).await.expect("connect");
        let last = tail(&daemon, ID, 1).await.expect("tail");
        let page = older(
            &daemon,
            ID,
            last[0].position,
            50,
            Some(second(0)),
            Some(Duration::from_secs(2)),
        )
        .await
        .expect("older");
        assert_eq!(page.lines.len(), 50);
        let sinces: Vec<i64> = engine
            .paths()
            .iter()
            .skip(2)
            .filter_map(|p| p.split("since=").nth(1)?.split('&').next()?.parse().ok())
            .collect();
        let start = second(399).as_second();
        assert_eq!(sinces, [start - 2, start - 75]);
    }

    /// A window reaching the creation is the last: `more` is false, also
    /// with fewer entries than asked.
    #[tokio::test]
    async fn older_stops_at_the_creation() {
        let log = long_log(5);
        let engine = engine(log);
        let daemon = connect(&engine.host()).await.expect("connect");
        let last = tail(&daemon, ID, 1).await.expect("tail");
        let page = older(&daemon, ID, last[0].position, 100, Some(second(0)), None)
            .await
            .expect("older");
        assert_eq!(
            texts(&page.lines),
            [
                "line 0", "line 1", "line 2", "line 3.a", "line 3.b", "line 3.c"
            ]
        );
        assert!(!page.more);
        // Exactly what was asked, from the whole log: nothing more.
        let page = older(&daemon, ID, last[0].position, 6, Some(second(0)), None)
            .await
            .expect("older");
        assert_eq!(page.lines.len(), 6);
        assert!(!page.more);
        let page = older(&daemon, ID, last[0].position, 5, Some(second(0)), None)
            .await
            .expect("older");
        assert_eq!(texts(&page.lines)[0], "line 1");
        assert!(page.more);
    }

    /// Paging forward from the first entry gives every entry once, the
    /// ones sharing the first's time included.
    #[tokio::test]
    async fn newer_pages_reach_the_end_with_every_entry_once() {
        let log = long_log(100);
        let engine = engine(log.clone());
        let daemon = connect(&engine.host()).await.expect("connect");
        let first = older(
            &daemon,
            ID,
            Position {
                time_ns: second(1).as_nanosecond(),
                ordinal: 0,
            },
            1,
            Some(second(0)),
            None,
        )
        .await
        .expect("older");
        let mut seen = first.lines;
        loop {
            let after = seen.last().expect("a line").position;
            let page = newer(&daemon, ID, after, 9).await.expect("newer");
            assert!(page.lines.len() <= 9);
            seen.extend(page.lines);
            if !page.more {
                break;
            }
        }
        assert_eq!(texts(&seen), all_texts(&log));
        let path = engine.paths().pop().expect("a request");
        assert!(
            path.contains("since=") && !path.contains("follow=true"),
            "{path}"
        );
    }

    /// After the first of three entries sharing a time, the other two.
    #[tokio::test]
    async fn newer_keeps_the_rest_of_a_shared_time() {
        let engine = engine(long_log(5));
        let daemon = connect(&engine.host()).await.expect("connect");
        let after = Position {
            time_ns: second(3).as_nanosecond(),
            ordinal: 0,
        };
        let page = newer(&daemon, ID, after, 2).await.expect("newer");
        assert_eq!(texts(&page.lines), ["line 3.b", "line 3.c"]);
        assert!(page.more);
        let page = newer(&daemon, ID, after, 3).await.expect("newer");
        assert_eq!(texts(&page.lines), ["line 3.b", "line 3.c", "line 4"]);
        assert!(!page.more);
    }

    #[tokio::test]
    async fn follow_sends_new_entries_until_the_daemon_ends_it() {
        let engine = engine(long_log(3));
        let daemon = connect(&engine.host()).await.expect("connect");
        let last = tail(&daemon, ID, 1).await.expect("tail")[0].position;
        let mut followed = std::pin::pin!(follow(&daemon, ID, last));
        // Before the follow's request: in its first answer.
        engine.append(ID, FakeEntry::new(second(2), "same second, later\n"));
        let first = followed.next().await.expect("an entry").expect("read");
        assert_eq!(texts(std::slice::from_ref(&first)), ["same second, later"]);
        assert_eq!(
            first.position,
            Position {
                time_ns: second(2).as_nanosecond(),
                ordinal: 1
            }
        );
        // After it: sent as it comes.
        engine.append(ID, FakeEntry::new(second(9), "new\n").on(LogStream::Stderr));
        let second_line = followed.next().await.expect("an entry").expect("read");
        assert_eq!(
            (
                texts(std::slice::from_ref(&second_line)),
                second_line.stream
            ),
            (vec!["new".to_string()], LogStream::Stderr)
        );
        engine.end_follows(ID);
        assert!(followed.next().await.is_none());
        let path = engine.paths().pop().expect("a request");
        assert!(path.contains("follow=true"), "{path}");
    }

    /// A TTY container's log, a long line in its two parts.
    #[tokio::test]
    async fn a_tty_log_comes_in_lines_and_parts() {
        // 16 KiB, written out: the size the daemon cuts at.
        let part = "y".repeat(16_384);
        let log = FakeLog {
            tty: true,
            entries: vec![
                FakeEntry::new(second(1), "one\r\n").on(LogStream::Tty),
                FakeEntry::new(second(2), part.clone()).on(LogStream::Tty),
                FakeEntry::new(second(2), "rest\r\n").on(LogStream::Tty),
                FakeEntry::new(second(3), "no newline").on(LogStream::Tty),
            ],
        };
        let engine = engine(log);
        let daemon = connect(&engine.host()).await.expect("connect");
        let lines = tail(&daemon, ID, 10).await.expect("tail");
        assert_eq!(
            texts(&lines),
            ["one".to_string(), part, "rest".into(), "no newline".into()]
        );
        assert!(lines.iter().all(|l| l.stream == LogStream::Tty));
        assert_eq!(lines[2].position.ordinal, 1);
    }

    #[tokio::test]
    async fn a_daemon_error_keeps_its_message() {
        let engine = engine(long_log(1));
        let daemon = connect(&engine.host()).await.expect("connect");
        let err = format!(
            "{:#}",
            tail(&daemon, "gone", 5).await.expect_err("an error")
        );
        assert!(
            err.contains("reading the log of gone") && err.contains("No such container: gone"),
            "{err}"
        );
        let err = older(
            &daemon,
            "gone",
            Position {
                time_ns: second(5).as_nanosecond(),
                ordinal: 0,
            },
            5,
            None,
            None,
        )
        .await
        .expect_err("an error");
        assert!(format!("{err:#}").contains("No such container"));
        let err = newer(
            &daemon,
            "gone",
            Position {
                time_ns: 0,
                ordinal: 0,
            },
            5,
        )
        .await
        .expect_err("an error");
        assert!(format!("{err:#}").contains("No such container"));
        let mut followed = std::pin::pin!(follow(
            &daemon,
            "gone",
            Position {
                time_ns: 0,
                ordinal: 0
            }
        ));
        let err = followed
            .next()
            .await
            .expect("an item")
            .expect_err("an error");
        assert!(
            format!("{err:#}").contains("following the log of gone"),
            "{err:#}"
        );
        assert!(followed.next().await.is_none());
    }

    /// The platform note's layout: a long line's parts at one time, with a
    /// later stderr entry stored between them.
    fn out_of_order() -> FakeLog {
        let t1 =
            Timestamp::from_nanosecond(second(100).as_nanosecond() + 500_000_000).expect("time");
        let t2 =
            Timestamp::from_nanosecond(second(101).as_nanosecond() + 200_000_000).expect("time");
        FakeLog {
            tty: false,
            entries: vec![
                FakeEntry::new(t1, "part a"),
                FakeEntry::new(t2, "between\n").on(LogStream::Stderr),
                FakeEntry::new(t1, "part b"),
                FakeEntry::new(t1, "part c\n"),
            ],
        }
    }

    async fn read_texts(daemon: &Daemon, options: LogsOptions) -> Vec<String> {
        let mut found = Vec::new();
        read(daemon, ID, options, |line| {
            found.push(line);
            ControlFlow::Continue(())
        })
        .await
        .expect("read");
        texts(&found)
    }

    /// The fake's `since` and `until` are one-sided, as dockerd's: `until`
    /// stops at the first later entry, `since` keeps everything after the
    /// first match.
    #[tokio::test]
    async fn the_fakes_since_and_until_are_one_sided() {
        let engine = engine(out_of_order());
        let daemon = connect(&engine.host()).await.expect("connect");
        let second_101 = api_secs(i128::from(second(101).as_second()));
        let until = options().until(second_101).build();
        assert_eq!(read_texts(&daemon, until).await, ["part a"]);
        let since = options().since(second_101).build();
        assert_eq!(
            read_texts(&daemon, since).await,
            ["between", "part b", "part c"]
        );
    }

    /// An older page is in position order, the parts of one time together.
    #[tokio::test]
    async fn an_older_page_is_in_position_order() {
        let engine = engine(out_of_order());
        let daemon = connect(&engine.host()).await.expect("connect");
        let before = Position {
            time_ns: second(200).as_nanosecond(),
            ordinal: 0,
        };
        let page = older(&daemon, ID, before, 10, Some(second(90)), None)
            .await
            .expect("older");
        assert_eq!(
            texts(&page.lines),
            ["part a", "part b", "part c", "between"]
        );
        assert!(!page.more);
    }

    /// Without a creation time, the window doubles to a month, then goes
    /// back to the start at once.
    #[tokio::test]
    async fn older_without_a_creation_goes_back_to_the_start() {
        let engine = engine(long_log(3));
        let daemon = connect(&engine.host()).await.expect("connect");
        let before = Position {
            time_ns: second(2).as_nanosecond(),
            ordinal: 0,
        };
        let page = older(&daemon, ID, before, 10, None, None)
            .await
            .expect("older");
        assert_eq!(texts(&page.lines), ["line 0", "line 1"]);
        assert!(!page.more);
        let sinces: Vec<i64> = engine
            .paths()
            .iter()
            .filter_map(|p| p.split("since=").nth(1)?.split('&').next()?.parse().ok())
            .collect();
        // 60 s doubled past 30 days is 16 windows, then the start.
        assert_eq!(sinces.len(), 17, "{sinces:?}");
        assert_eq!(sinces.last(), Some(&1));
    }

    /// A creation after `before` (a clock moved back) is one window, its
    /// `since` no later than its `until`.
    #[tokio::test]
    async fn a_creation_after_before_is_one_window() {
        let engine = engine(long_log(3));
        let daemon = connect(&engine.host()).await.expect("connect");
        let before = Position {
            time_ns: second(2).as_nanosecond(),
            ordinal: 0,
        };
        let page = older(&daemon, ID, before, 10, Some(second(500)), None)
            .await
            .expect("older");
        assert!(!page.more);
        let path = engine.paths().pop().expect("a request");
        let until = second(2).as_second();
        assert!(
            path.contains(&format!("since={until}&until={until}")),
            "{path}"
        );
    }

    /// The fake reads as dockerd does (the platform note): `tail` before
    /// `until`, so sb-go's way of paging back finds nothing.
    #[tokio::test]
    async fn the_fake_applies_tail_before_until() {
        let engine = engine(long_log(10));
        let daemon = connect(&engine.host()).await.expect("connect");
        let options = options()
            .tail("3")
            .until(api_secs(i128::from(second(5).as_second())))
            .build();
        let mut found = 0;
        read(&daemon, ID, options, |_| {
            found += 1;
            ControlFlow::Continue(())
        })
        .await
        .expect("read");
        assert_eq!(found, 0);
    }
    /// The whole-log search's read (tui spec, "Whole-log search, save and
    /// print"): every entry of the range once, oldest first, those sharing
    /// a time included, with the positions the pages give; `All` from the
    /// creation.
    #[tokio::test]
    async fn a_scan_reads_each_range_once_oldest_first() {
        let log = long_log(400);
        let engine = engine(log.clone());
        let daemon = connect(&engine.host()).await.expect("connect");
        let created = Some(second(0));
        let collect = |range| {
            let daemon = &daemon;
            async move {
                let mut seen = Vec::new();
                let ended = scan(daemon, ID, range, created, |line| {
                    seen.push(line);
                    ControlFlow::Continue(())
                })
                .await
                .expect("scan");
                assert_eq!(ended, ControlFlow::Continue(()));
                seen
            }
        };
        let all = collect(Range::All).await;
        assert_eq!(texts(&all), all_texts(&log));
        // The same positions a tail gives, so a match found here is where
        // a page shows it.
        let last = tail(&daemon, ID, 20).await.expect("tail");
        assert_eq!(all[all.len() - 20..], last[..]);
        let middle = all[200].position;
        assert_eq!(collect(Range::Before(middle)).await, all[..200]);
        assert_eq!(collect(Range::After(middle)).await, all[201..]);
    }

    /// A scan stops when its caller breaks.
    #[tokio::test]
    async fn a_scan_stops_when_asked() {
        let engine = engine(long_log(100));
        let daemon = connect(&engine.host()).await.expect("connect");
        let mut n = 0;
        let ended = scan(&daemon, ID, Range::All, Some(second(0)), |_| {
            n += 1;
            if n == 5 {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        })
        .await
        .expect("scan");
        assert_eq!(ended, ControlFlow::Break(()));
        assert_eq!(n, 5);
    }

    /// The log viewer's Home (tui spec, "Home and End"): the log's first
    /// `n` entries from the creation, with the positions every read
    /// gives; `more` when the log goes on; a log shorter than a page whole.
    #[tokio::test]
    async fn a_head_is_the_logs_first_entries() {
        let log = long_log(400);
        let engine = engine(log.clone());
        let daemon = connect(&engine.host()).await.expect("connect");
        let created = Some(second(0));
        let mut all = Vec::new();
        let _ = scan(&daemon, ID, Range::All, created, |line| {
            all.push(line);
            ControlFlow::Continue(())
        })
        .await
        .expect("scan");
        let first = head(&daemon, ID, 50, created).await.expect("head");
        assert_eq!(first.lines, all[..50]);
        assert!(first.more);
        let whole = head(&daemon, ID, 1_000, created).await.expect("head");
        assert_eq!(whole.lines, all);
        assert!(!whole.more);
        let exactly = head(&daemon, ID, all.len(), created).await.expect("head");
        assert_eq!(exactly.lines.len(), all.len());
        assert!(!exactly.more, "a page of exactly the log has nothing more");
        // A page after the head's last entry goes on from there.
        let on = newer(&daemon, ID, first.lines[49].position, 5)
            .await
            .expect("newer");
        assert_eq!(on.lines, all[50..55]);
    }
}
