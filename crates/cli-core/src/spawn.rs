//! Work moved off the caller's thread, with the caller's tracing span
//! (cli-core spec, "Logging": every record carries the run id). sb's
//! only thread and blocking-pool spawns: clippy.toml disallows the bare
//! ones in library and binary code, and these three carry the span the
//! way tracing documents it, `Span::current()` before the spawn and
//! `in_scope` inside
//! (<https://docs.rs/tracing/0.1.44/tracing/struct.Span.html#method.in_scope>;
//! the thread example under `#method.or_current`). Why not tokio's or
//! std's spawn with a layer of tracing-subscriber: neither carries a span
//! into a new thread (tokio 1.54.1 gives its own task spans `parent:
//! None`), and `Instrument` covers futures only.
//!
//! Here, in cli-core, because cli-core's own threads (the signals, the
//! restore hooks, the deep-YAML stack) need it, and every crate that
//! starts threads depends on cli-core; sb-exec depends on cli-core, so it
//! couldn't hold them. (sb-journal depends on none of sb's crates and
//! starts no thread outside its tests.)

use std::thread::{Builder, JoinHandle, Scope, ScopedJoinHandle};

/// `f` on the thread `builder` describes (its name, its stack), in the
/// caller's span.
///
/// # Errors
///
/// std's: the thread couldn't be started (the system's thread or memory
/// limits).
#[expect(
    clippy::disallowed_methods,
    reason = "the one place sb starts a thread: it carries the caller's span (E6)"
)]
pub fn thread<F, T>(builder: Builder, f: F) -> std::io::Result<JoinHandle<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let span = tracing::Span::current();
    builder.spawn(move || span.in_scope(f))
}

/// [`thread`] for a thread in `scope`, which may borrow from the caller
/// (std's `Builder::spawn_scoped`).
///
/// # Errors
///
/// As [`thread`].
#[expect(
    clippy::disallowed_methods,
    reason = "the one place sb starts a scoped thread: it carries the caller's span (E6)"
)]
pub fn scoped_thread<'scope, 'env, F, T>(
    builder: Builder,
    scope: &'scope Scope<'scope, 'env>,
    f: F,
) -> std::io::Result<ScopedJoinHandle<'scope, T>>
where
    F: FnOnce() -> T + Send + 'scope,
    T: Send + 'scope,
{
    let span = tracing::Span::current();
    builder.spawn_scoped(scope, move || span.in_scope(f))
}

/// `f` on tokio's blocking pool, in the caller's span
/// (<https://docs.rs/tokio/1.54.1/tokio/task/fn.spawn_blocking.html>).
///
/// # Panics
///
/// Outside a tokio runtime, as tokio's `spawn_blocking` does; the panic
/// and tokio's spawn location name the caller (`#[track_caller]`, as
/// tokio's own).
#[track_caller]
#[expect(
    clippy::disallowed_methods,
    reason = "the one place sb uses the blocking pool: it carries the caller's span (E6)"
)]
pub fn blocking<F, T>(f: F) -> tokio::task::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let span = tracing::Span::current();
    tokio::task::spawn_blocking(move || span.in_scope(f))
}
