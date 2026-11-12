//! The run log on the host (spec: startup step 4; capability map,
//! decision 11).
//!
//! Every root run writes a record to `/var/log/sb`: a run span with an id,
//! the command's name, the command's own events, and how the run ended.
//! Argument values are never logged: they can carry secrets
//! (`sb install -e password=...`). tracing-subscriber's fmt layer writes
//! plain text to one file, `sb.log`, through a tracing-appender
//! `RollingFileAppender` that never rotates, built with `Builder` and
//! `Rotation::NEVER`
//! (<https://docs.rs/tracing-appender/0.2.5/tracing_appender/rolling/struct.Builder.html>).
//! Why not `rolling::never`
//! (<https://docs.rs/tracing-appender/0.2.5/tracing_appender/rolling/fn.never.html>):
//! it panics when the file can't be opened; `Builder::build` returns the
//! error, and an unopenable log is only a warning.
//! Saltbox's logrotate rotates it (maintainer, 2026-10-30): one path to
//! follow, compressed history, managed like Saltbox's other logs. Each run
//! opens the file afresh, so logrotate needs no signal: a run that
//! overlaps a rotation finishes in the renamed file, its record whole (the
//! rule uses `delaycompress`; docs/platform-notes/logrotate.md).

use std::error::Error;
use std::ffi::OsString;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use tracing::span::EnteredSpan;
use tracing_log::AsLog;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::{LevelFilter, Targets};

use crate::cancel::Shutdown;

/// The log file's name in the log directory.
const LOG_FILE: &str = "sb.log";

/// The level dependencies log at by default (term-ui decision 9), for the
/// tracing filter and the `log` bridge alike (converted with tracing-log's
/// `AsLog`): a `log` record the bridge drops never reaches the filter.
const DEPENDENCY_LEVEL: LevelFilter = LevelFilter::WARN;

/// Where runs log: `SB_LOG_DIR` when set (tests, debugging), otherwise
/// `/var/log/sb`.
pub fn log_dir(override_dir: Option<OsString>) -> PathBuf {
    override_dir.map_or_else(|| PathBuf::from("/var/log/sb"), PathBuf::from)
}

/// What a secret shows as, in place, on screen and in the log (term-ui
/// decision 8, amended: inline masking, in the form Rust's secrecy crate
/// uses, <https://docs.rs/secrecy/0.10.3/secrecy/>).
pub const WITHHELD: &str = "[REDACTED]";

/// A secret value inside a message: its Display and Debug are
/// [`WITHHELD`], so `format!("got {}", Secret::new(&token))` never holds
/// the token, only the placeholder in its place; the rest of the message
/// stays readable. The value is reachable only through
/// [`Secret::expose`]. Why not secrecy's `SecretBox`: it has no `Display`
/// on purpose, and inline masking needs one; it also zeroes the value on
/// drop, which this type doesn't (sb's secrets live in config files and
/// process memory anyway).
#[derive(Clone)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    /// Mark `value` as secret.
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// The value, for code that must use it (never to show or log).
    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T> std::fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(WITHHELD)
    }
}

impl<T> std::fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(WITHHELD)
    }
}

/// An error whose text must not be shown or logged at all (credentials,
/// tokens), for errors whose message sb doesn't write (a library's); when
/// sb writes the message, it marks just the value with [`Secret`].
/// It says nothing of what it holds: its Display and Debug are
/// [`WITHHELD`] and it has no cause, so wherever it sits in a chain, even
/// behind a transparent wrapper or in anyhow's `{:?}`, it and everything
/// below it show and log as the placeholder; what is above it shows as
/// usual. Why not `#[error(transparent)]`: it forwards Display and causes
/// to the secret, and a transparent wrapper around it then skips the
/// marker. The held error is reachable only through [`Sensitive::inner`].
/// It carries text, not control flow: an `Exit` inside it isn't a quiet
/// exit.
pub struct Sensitive(Box<dyn Error + Send + Sync>);

impl Sensitive {
    /// Mark `err` as sensitive.
    pub fn new(err: impl Into<Box<dyn Error + Send + Sync>>) -> Self {
        Self(err.into())
    }

    /// The held error, for code that must act on it (never to show or log).
    #[must_use]
    pub fn inner(&self) -> &(dyn Error + Send + Sync + 'static) {
        self.0.as_ref()
    }
}

impl std::fmt::Display for Sensitive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(WITHHELD)
    }
}

impl std::fmt::Debug for Sensitive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sensitive({WITHHELD})")
    }
}

impl Error for Sensitive {}

/// One run's log: its id and where it is written.
#[derive(Debug)]
pub struct RunLog {
    id: String,
    /// The file the run writes to.
    file: PathBuf,
    /// The run's span ([`RunLog::enter`], the panic hook).
    span: std::sync::OnceLock<tracing::Span>,
}

impl RunLog {
    /// Open the log in `dir`, creating it root-only (0700), and the
    /// subscriber that writes to it; the caller installs the subscriber.
    /// It also installs the process's `log` logger, tracing-log's bridge,
    /// so dependencies logging through `log` reach that subscriber.
    ///
    /// # Errors
    ///
    /// The I/O error when the directory can't be created or read, belongs
    /// to another user, holds files that aren't sb's logs while its
    /// permissions are looser than root-only, can't be made root-only, or
    /// the log file can't be opened.
    pub fn open(
        dir: &Path,
    ) -> std::io::Result<(Self, impl tracing::Subscriber + Send + Sync + use<>)> {
        // Root-only, also when the directory already existed with looser
        // permissions (the files inside follow the umask). An existing
        // directory is only locked down when it holds nothing but sb's logs:
        // a mistaken `SB_LOG_DIR=/tmp` as root must not make /tmp 0700.
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        let meta = std::fs::metadata(dir)?;
        // Only a directory of the user running sb: another user's could hold
        // `sb.log` as a link to a root file, which the appender would follow
        // and append to.
        if meta.uid() != crate::root::effective_uid() {
            return Err(std::io::Error::other(format!(
                "{} belongs to another user; not logging there",
                dir.display()
            )));
        }
        if meta.permissions().mode() & 0o777 != 0o700 {
            if !holds_only_sb_logs(dir)? {
                return Err(std::io::Error::other(format!(
                    "{} holds files that aren't sb's logs; not making it root-only",
                    dir.display()
                )));
            }
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let appender = tracing_appender::rolling::Builder::new()
            .rotation(tracing_appender::rolling::Rotation::NEVER)
            .filename_prefix(LOG_FILE)
            .build(dir)
            .map_err(std::io::Error::other)?;
        // No colours of fmt's own. Every record goes through `OneLine`, so
        // no value can break it over lines. Child output is sanitized
        // before it is logged (term-ui's runner, `DisplayText` with colour
        // off). A failed write (full disk) isn't reported on stderr, where
        // it would land among the command's own output.
        let subscriber = tracing_subscriber::fmt()
            .with_writer(appender)
            .with_ansi(false)
            // Every line names its source, so a dependency's warning says
            // which crate wrote it (maintainer, docker Checkpoint A).
            .with_target(true)
            .log_internal_errors(false)
            .map_event_format(OneLine)
            // fmt's own default is INFO for everything.
            .with_max_level(LevelFilter::TRACE)
            .finish();
        // Verbose by default, no flag (term-ui decision 9): sb's own crates
        // (every target starting with `sb`: sb, sb_cli_core, sb_term_ui,
        // sb_host, sb_docker; a prefix, so a dependency named `sb…` would
        // log at DEBUG too, and none is in the tree) at DEBUG, dependencies
        // at WARN, so the log carries what a debug flag would have without
        // their noise.
        // https://docs.rs/tracing-subscriber/0.3.23/tracing_subscriber/filter/targets/struct.Targets.html
        let subscriber = Targets::new()
            .with_default(DEPENDENCY_LEVEL)
            .with_target("sb", LevelFilter::DEBUG)
            .with_subscriber(subscriber);
        // Dependencies that log through the `log` crate (bollard) reach the
        // run log through tracing-log's bridge, installed here as the
        // process's `log` logger; only what the filter keeps for
        // dependencies is turned into events (and tracing-subscriber's
        // `tracing-log` feature has fmt write them under their own target,
        // without tracing-log's `log.*` fields). A second install (a second
        // RunLog in one process, as in tests) fails and changes nothing:
        // the bridge forwards to whichever subscriber is current, so the
        // first one serves every RunLog. sb sets no other `log` logger.
        // https://docs.rs/tracing-log/0.2.0/tracing_log/struct.LogTracer.html
        let _ = tracing_log::LogTracer::builder()
            .with_max_level(DEPENDENCY_LEVEL.as_log())
            .init();
        // Only needs to tell runs apart within the log: start time and pid.
        // Why not uuid or fastrand: neither (nor getrandom) is in sb's
        // dependency tree, and the id only has to differ between runs on
        // one host.
        // The nanoseconds' low 64 bits, which move fastest: the wrapping
        // arithmetic keeps only them.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| {
                d.as_secs()
                    .wrapping_mul(1_000_000_000)
                    .wrapping_add(u64::from(d.subsec_nanos()))
            });
        let id = format!("{:016x}", nanos ^ u64::from(std::process::id()));
        // Never rotating, the appender names the file by its prefix alone
        // (tracing-appender 0.2.5 rolling.rs `join_date`).
        let file = dir.join(LOG_FILE);
        Ok((
            Self {
                id,
                file,
                span: std::sync::OnceLock::new(),
            },
            subscriber,
        ))
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The run's span, entered for the whole run: every event in it carries
    /// the run id.
    pub fn enter(&self) -> EnteredSpan {
        self.span().entered()
    }

    /// The run's one span (made on first use, with the run id), so the
    /// panic hook's record and the run's events share it.
    fn span(&self) -> tracing::Span {
        self.span
            .get_or_init(|| tracing::info_span!("run", run = %self.id))
            .clone()
    }

    /// Record panics in the run log (sb-M2): from now on a panic, on any
    /// thread, leaves a record with its message and location in the run's
    /// span (so with the run id, whether or not that thread is in it),
    /// then the hook that was in place before runs (std's: the message on
    /// stderr). Call it once, with the log's subscriber in place; the hook
    /// holds the run's span for the life of the process. The record goes
    /// through the log's formatter, so a message with a newline stays one
    /// record. std's documented way: `take_hook` the previous hook,
    /// `set_hook` one that calls it
    /// (<https://doc.rust-lang.org/std/panic/fn.take_hook.html>,
    /// <https://doc.rust-lang.org/std/panic/struct.PanicHookInfo.html>).
    /// Known limit: a panic inside the log's own writing (the formatter
    /// holding the file) can't be recorded there.
    pub fn record_panics(&self) {
        let run = self.span();
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // A payload that isn't text reads as std's own hook writes it.
            let message = info.payload_as_str().unwrap_or("Box<dyn Any>");
            let location = info
                .location()
                .map_or_else(|| "unknown".to_string(), ToString::to_string);
            run.in_scope(|| tracing::error!(location, "panicked: {message}"));
            previous(info);
        }));
    }

    /// The run record: sb's version (the binary's build info, not this
    /// library's package version) and the command's name, never its
    /// arguments.
    pub fn started(&self, command: &str, version: &str) {
        tracing::info!(version, command, "run started");
    }

    /// How the run ended (see [`outcome`]). A failure's error goes in as
    /// tracing's error field, which prints its causes too
    /// (`error=... error.sources=[...]`); a [`Sensitive`] error in the chain
    /// writes only the placeholder, as its Display is that and it has no
    /// cause.
    pub fn finished(&self, outcome: &str, error: Option<&(dyn Error + 'static)>) {
        // A quiet exit is the outcome (`exit 7`, `interrupted (exit 130)`),
        // not an error: logging it beside would read `error=exit code 1`.
        // Accepted (Task 8a review): a context wrapped around the exit, or
        // "broken pipe" for a quiet 141, isn't recorded either; what went
        // wrong before a quiet exit is logged where it happened (a step).
        let error = error.filter(|err| crate::exit::quiet_exit(*err).is_none());
        if let Some(error) = error {
            tracing::info!(outcome, error, "run finished");
        } else {
            tracing::info!(outcome, "run finished");
        }
    }

    /// Where to look, for the error block's `details:` line.
    #[must_use]
    pub fn pointer(&self) -> String {
        format!("{} (run {})", self.file.display(), self.id)
    }
}

/// The run log's event formatter: fmt's own [`Format`] (the builder's,
/// configured above: its layout, `key=value` fields, `error.sources`, the
/// bare message, tracing-log's `log.*` fields left out) writes the record
/// into a buffer, and the record goes to the file through [`Escaping`],
/// its final newline kept. So every record (its message, an error and its
/// causes, Debug and Display fields, span fields, those recorded later,
/// and records bridged from `log`, their target included, which fmt
/// writes outside the field formatter) is always one line (san-M2). The
/// one choke point, tracing-subscriber's documented hook for how a whole
/// event is written
/// (<https://docs.rs/tracing-subscriber/0.3.23/tracing_subscriber/fmt/trait.FormatEvent.html>,
/// set with
/// <https://docs.rs/tracing-subscriber/0.3.23/tracing_subscriber/fmt/struct.SubscriberBuilder.html#method.map_event_format>).
/// The guarantee is one line per record, not per field: a value can
/// append text that looks like a field inside its own record (the first
/// occurrence of a key is the real one).
///
/// Why not fmt's own escaping: since 0.3.20 it escapes ESC, BEL, BS, FF,
/// DEL and C1 in messages and errors only (as `\x1b`, kept as it is),
/// never `\n` or `\r`, and nothing in a `%` field. Why not `fmt().json()`:
/// it escapes everything but changes the log's format (cli-core spec,
/// step 4) and adds the `json` feature. Why not `debug_fn` as the field
/// formatter: its visitor gets an error as its Display only, losing
/// `error.sources`, and a target isn't a field.
///
/// [`Format`]: tracing_subscriber::fmt::format::Format
struct OneLine<E>(E);

impl<S, N, E> tracing_subscriber::fmt::FormatEvent<S, N> for OneLine<E>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    N: for<'a> tracing_subscriber::fmt::FormatFields<'a> + 'static,
    E: tracing_subscriber::fmt::FormatEvent<S, N>,
{
    fn format_event(
        &self,
        ctx: &tracing_subscriber::fmt::FmtContext<'_, S, N>,
        mut writer: tracing_subscriber::fmt::format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        let mut record = String::new();
        self.0.format_event(
            ctx,
            tracing_subscriber::fmt::format::Writer::new(&mut record),
            event,
        )?;
        let line = record.strip_suffix('\n').unwrap_or(&record);
        std::fmt::Write::write_str(&mut Escaping(&mut writer), line)?;
        std::fmt::Write::write_char(&mut writer, '\n')
    }
}

/// A writer that escapes control characters (`\n`, `\r`, `\t`, `\u{1b}`)
/// and the line and paragraph separators U+2028 and U+2029, which split
/// lines in some tools, in std's `char::escape_debug` forms
/// (<https://doc.rust-lang.org/std/primitive.char.html#method.escape_debug>),
/// and passes everything else through, fmt's escapes (`\x1b`) included.
/// Bidi and zero-width marks pass as they are, as in table cells (the
/// plan's decision 19): they can't end a record.
struct Escaping<W>(W);

impl<W: std::fmt::Write> std::fmt::Write for Escaping<W> {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        let mut rest = 0;
        let ends_a_line = |c: char| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}');
        for (at, c) in text.char_indices().filter(|&(_, c)| ends_a_line(c)) {
            self.0.write_str(&text[rest..at])?;
            write!(self.0, "{}", c.escape_debug())?;
            rest = at + c.len_utf8();
        }
        self.0.write_str(&text[rest..])
    }
}

/// Whether every entry in `dir` is one of sb's log files: a regular file
/// (`file_type` doesn't follow symlinks) named `sb.log` or one of
/// logrotate's names for its rotated copies (`sb.log.1`, `sb.log.2.gz`,
/// `sb.log-20261030` with `dateext`; not `extension`, other `dateformat`s
/// or `olddir`: docs/platform-notes/logrotate.md). An empty directory
/// counts as sb's.
fn holds_only_sb_logs(dir: &Path) -> std::io::Result<bool> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let is_sb_log = name
            .strip_prefix(LOG_FILE)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(['.', '-']));
        if !(entry.file_type()?.is_file() && is_sb_log) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// How a run ended, for its last log line: a signal, a requested or quiet
/// exit code (classified as [`crate::exit::report`] does), a failure, or
/// done.
#[must_use]
pub fn outcome(shutdown: &Shutdown, error: Option<&(dyn Error + 'static)>) -> String {
    match (shutdown.requested(), error) {
        (Some(code), _) if shutdown.interrupted() => format!("interrupted (exit {code})"),
        (Some(code), _) => format!("exit {code}"),
        (None, Some(err)) => match crate::exit::quiet_exit(err) {
            Some(code) => format!("exit {code}"),
            None => "failed".to_string(),
        },
        (None, None) => "done".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one log file a test run wrote.
    fn log_text(dir: &Path) -> String {
        let files: Vec<_> = std::fs::read_dir(dir)
            .expect("log dir")
            .map(|e| e.expect("entry").path())
            .collect();
        assert_eq!(files.len(), 1, "{files:?}");
        let name = files[0]
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        assert!(
            name.starts_with("sb.") && Path::new(&name).extension().is_some_and(|e| e == "log"),
            "{name}"
        );
        std::fs::read_to_string(&files[0]).expect("read log")
    }

    #[test]
    fn a_run_records_its_command_events_and_outcome() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (log, subscriber) = RunLog::open(dir.path()).expect("open");
        tracing::subscriber::with_default(subscriber, || {
            let _run = log.enter();
            log.started("version", "3.1.4");
            tracing::info!("\x1b[32mGet: jq\x1b[0m");
            log.finished("done", None);
        });
        let text = log_text(dir.path());
        assert!(text.contains(&format!("run{{run={}}}", log.id())), "{text}");
        assert!(
            text.contains("run started") && text.contains("command=\"version\""),
            "{text}"
        );
        assert!(
            text.contains("version=\"3.1.4\""),
            "sb's version, not the library's: {text}"
        );
        assert!(
            text.contains("run finished") && text.contains("outcome=\"done\""),
            "{text}"
        );
        assert!(text.contains("Get: jq"), "{text}");
        assert!(!text.contains('\x1b'), "escape codes in the log: {text:?}");
    }

    /// A forged record: a newline, then what a "run finished" line looks
    /// like, and a carriage return.
    const FORGED: &str =
        "boom\n2026-11-12T00:00:00.000000Z  INFO run{run=0}: sb: run finished outcome=\"done\"\r";

    /// The forged text through a Debug that writes it raw (std's Debug for
    /// `str` would escape it itself).
    struct RawDebug;

    impl std::fmt::Debug for RawDebug {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(FORGED)
        }
    }

    /// An error whose message and cause hold the forged text.
    #[derive(Debug)]
    struct Forged(Option<Box<Forged>>);

    impl std::fmt::Display for Forged {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(FORGED)
        }
    }

    impl Error for Forged {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            self.0.as_deref().map(|e| e as &(dyn Error + 'static))
        }
    }

    /// No logged value can forge a record (san-M2, cli-M3, docker-L1): a
    /// newline and a "run finished" line through the message, an error
    /// and its cause, a Debug field, a Display field (`%`, as docker's
    /// `error = %cause`), a span field, one recorded later, a record
    /// bridged from `log` and one whose target is built from data each
    /// stay in one record, escaped.
    #[test]
    fn no_logged_value_can_forge_a_record() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (log, subscriber) = RunLog::open(dir.path()).expect("open");
        let error = Forged(Some(Box::new(Forged(None))));
        let target = format!("dependency {FORGED}");
        tracing::subscriber::with_default(subscriber, || {
            let _run = log.enter();
            tracing::info!("message {FORGED}");
            tracing::info!(error = &error as &(dyn Error + 'static), "error");
            tracing::info!(value = ?RawDebug, "debug");
            tracing::info!(value = %FORGED, "display");
            tracing::info_span!("step", label = %FORGED).in_scope(|| tracing::info!("span"));
            let span = tracing::info_span!("later", label = tracing::field::Empty);
            span.record("label", tracing::field::display(FORGED));
            span.in_scope(|| tracing::info!("recorded"));
            tracing_log::log::warn!(target: "dependency", "bridged {FORGED}");
            tracing_log::log::warn!(target: &target, "dynamic");
        });
        let text = log_text(dir.path());
        let records: Vec<_> = text.lines().collect();
        assert_eq!(records.len(), 8, "{text}");
        assert!(!text.contains('\r'), "{text:?}");
        let escaped = r#"boom\n2026-11-12T00:00:00.000000Z  INFO run{run=0}: sb: run finished outcome="done"\r"#;
        for (record, kind) in records.iter().zip([
            "message", "error", "debug", "display", "span", "recorded", "bridged", "dynamic",
        ]) {
            assert!(record.contains(kind), "{kind}: {record}");
            assert!(record.contains(escaped), "{kind}: {record}");
        }
        // The error's cause too.
        assert!(records[1].contains("error.sources=["), "{}", records[1]);
        assert_eq!(
            records[1].matches("run finished").count(),
            2,
            "{}",
            records[1]
        );
    }

    /// The line and paragraph separators, which split lines in some tools,
    /// are escaped too; an ESC is written once in fmt's own `\x1b` form in
    /// a message, and escaped in a `%` field, where fmt leaves it.
    #[test]
    fn separators_and_escapes_stay_in_the_record() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (log, subscriber) = RunLog::open(dir.path()).expect("open");
        tracing::subscriber::with_default(subscriber, || {
            let _run = log.enter();
            tracing::info!("separators a\u{2028}b\u{2029}c");
            tracing::info!("esc \x1b[31mred");
            tracing::info!(value = %"x\x1b[2Jy", "esc display");
        });
        let text = log_text(dir.path());
        assert_eq!(text.lines().count(), 3, "{text}");
        assert!(!text.contains(['\u{2028}', '\u{2029}', '\x1b']), "{text:?}");
        assert!(text.contains(r"separators a\u{2028}b\u{2029}c"), "{text}");
        assert!(text.contains(r"esc \x1b[31mred"), "{text}");
        assert!(text.contains(r"value=x\u{1b}[2Jy"), "{text}");
    }

    /// The directory is root-only (decision 11): 0700.
    #[test]
    fn the_log_directory_is_private() {
        let parent = tempfile::tempdir().expect("temp dir");
        let dir = parent.path().join("sb");
        let _ = RunLog::open(&dir).expect("open");
        let mode = std::fs::metadata(&dir).expect("dir").permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    /// An existing directory made with looser permissions becomes 0700 too:
    /// the files inside follow the umask (typically 0644).
    #[test]
    fn an_existing_open_directory_is_made_private() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
        let _ = RunLog::open(dir.path()).expect("open");
        let mode = std::fs::metadata(dir.path())
            .expect("dir")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }

    /// The case the check exists for: an existing, too-open `/var/log/sb`
    /// that already holds sb's logs is made private and used.
    #[test]
    fn an_open_directory_of_sb_logs_is_made_private() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("sb.log.1"), "old run\n").expect("write");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
        assert!(RunLog::open(dir.path()).is_ok());
        let mode = std::fs::metadata(dir.path())
            .expect("dir")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }

    /// An existing directory holding anything but sb's logs isn't sb's to
    /// lock down (a mistaken `SB_LOG_DIR=/tmp` as root): `open` refuses it
    /// and leaves its permissions alone.
    #[test]
    fn a_directory_with_other_files_is_refused_and_left_alone() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
        std::fs::write(dir.path().join("someone-elses.txt"), "x").expect("write");
        assert!(RunLog::open(dir.path()).is_err());
        let mode = std::fs::metadata(dir.path())
            .expect("dir")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755);
    }

    /// A directory that can't be made fails `open`; the caller warns and
    /// runs on without a log.
    #[test]
    fn an_unusable_directory_fails_to_open() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        assert!(RunLog::open(&file.path().join("sb")).is_err());
    }

    /// The pointer names the file the appender writes, and the run.
    #[test]
    fn the_pointer_names_the_file_and_run() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (log, subscriber) = RunLog::open(dir.path()).expect("open");
        tracing::subscriber::with_default(subscriber, || {
            let _run = log.enter();
            log.started("version", "0.1.0");
        });
        // The one file in the directory is the one the appender wrote.
        let written: Vec<_> = std::fs::read_dir(dir.path())
            .expect("dir")
            .map(|e| e.expect("entry").path())
            .collect();
        assert_eq!(written.len(), 1, "{written:?}");
        assert_eq!(
            log.pointer(),
            format!("{} (run {})", written[0].display(), log.id())
        );
    }

    /// An already-private directory is used as it is, other files and all
    /// (`SB_LOG_DIR` pointing somewhere root-only); nothing is changed.
    #[test]
    fn an_already_private_directory_is_used_as_is() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        std::fs::write(dir.path().join("someone-elses.txt"), "x").expect("write");
        assert!(RunLog::open(dir.path()).is_ok());
        let mode = std::fs::metadata(dir.path())
            .expect("dir")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("someone-elses.txt")).expect("still there"),
            "x"
        );
    }

    /// A symlink (or directory) named like a log isn't one of sb's logs:
    /// only regular files with sb's log names count.
    #[test]
    fn a_link_named_like_a_log_is_not_sbs() {
        let dir = tempfile::tempdir().expect("temp dir");
        let elsewhere = tempfile::NamedTempFile::new().expect("temp file");
        std::os::unix::fs::symlink(elsewhere.path(), dir.path().join("sb.log")).expect("symlink");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
        assert!(RunLog::open(dir.path()).is_err());
        let mode = std::fs::metadata(dir.path())
            .expect("dir")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755);
    }

    /// A directory owned by another user isn't trusted, private or not: its
    /// owner could plant `sb.log` as a link to a root file for a
    /// root run to append to. Making one needs root (`SB_TEST_ROOT=1`).
    #[test]
    fn another_users_directory_is_refused() {
        if std::env::var_os("SB_TEST_ROOT").is_none_or(|v| v != "1") {
            return;
        }
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        std::os::unix::fs::chown(dir.path(), Some(65534), Some(65534)).expect("chown");
        assert!(RunLog::open(dir.path()).is_err());
    }

    /// Only sb's log names count: another program's `.log` makes
    /// the directory someone else's.
    #[test]
    fn another_programs_log_is_not_sbs() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
        std::fs::write(dir.path().join("notes.log"), "x").expect("write");
        assert!(RunLog::open(dir.path()).is_err());
    }

    /// One file, `sb.log`, whatever the date: Saltbox's logrotate rotates
    /// it.
    #[test]
    fn runs_write_to_sb_log() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (log, subscriber) = RunLog::open(dir.path()).expect("open");
        tracing::subscriber::with_default(subscriber, || {
            let _run = log.enter();
            log.started("version", "0.1.0");
        });
        assert!(dir.path().join("sb.log").is_file());
        assert!(
            log.pointer()
                .starts_with(&format!("{} ", dir.path().join("sb.log").display()))
        );
    }

    /// logrotate's names for rotated copies (numbered, dated with
    /// `dateext`, compressed) are sb's logs too, so a too-open directory
    /// holding them is still made private.
    #[test]
    fn rotated_logs_are_sbs() {
        let dir = tempfile::tempdir().expect("temp dir");
        for name in [
            "sb.log",
            "sb.log.1",
            "sb.log.2.gz",
            "sb.log-20261030",
            "sb.log-20261029.gz",
        ] {
            std::fs::write(dir.path().join(name), "old run\n").expect("write");
        }
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
        assert!(RunLog::open(dir.path()).is_ok());
    }

    /// A name that only starts like sb's log isn't one.
    #[test]
    fn a_name_like_sb_log_is_not_sbs() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("sb.logger"), "x").expect("write");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
        assert!(RunLog::open(dir.path()).is_err());
    }

    #[test]
    fn the_directory_defaults_to_var_log_sb() {
        assert_eq!(log_dir(None), PathBuf::from("/var/log/sb"));
        assert_eq!(log_dir(Some("/tmp/x".into())), PathBuf::from("/tmp/x"));
    }

    /// The outcome word says how the run ended; a failure's error and its
    /// causes go in as a field (see `finished`).
    #[test]
    fn outcomes_name_how_the_run_ended() {
        let shutdown = Shutdown::new();
        assert_eq!(outcome(&shutdown, None), "done");
        let err = std::io::Error::other("disk full");
        assert_eq!(outcome(&shutdown, Some(&err)), "failed");
        let pipe = crate::exit::output_error(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
        assert_eq!(outcome(&shutdown, Some(&pipe)), "exit 141");
        assert_eq!(outcome(&shutdown, Some(&crate::exit::Exit(5))), "exit 5");
        let interrupted = Shutdown::new();
        interrupted.signalled(signal_hook::consts::SIGTERM);
        assert_eq!(outcome(&interrupted, Some(&err)), "interrupted (exit 143)");
        let requested = Shutdown::new();
        let _ = requested.request(4);
        assert_eq!(outcome(&requested, None), "exit 4");
    }

    /// A quiet exit (a requested code, the runner's `Exit(1)` after an
    /// interruption, a broken pipe) is the outcome, not an error: the run
    /// record doesn't repeat it as `error=exit code 1` (config Task 12's
    /// review, carried to exec Task 8a).
    #[test]
    fn a_quiet_exit_is_not_logged_as_an_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (log, subscriber) = RunLog::open(dir.path()).expect("open");
        let quiet = crate::exit::Exit(1);
        tracing::subscriber::with_default(subscriber, || {
            let _run = log.enter();
            log.finished("interrupted (exit 130)", Some(&quiet));
        });
        let text = log_text(dir.path());
        let record = text
            .lines()
            .find(|l| l.contains("run finished"))
            .expect("record");
        assert!(
            record.contains(r#"outcome="interrupted (exit 130)""#),
            "{record}"
        );
        assert!(!record.contains("error"), "{record}");
    }

    /// A failure's error and causes reach the log through tracing's error
    /// field (`error=... error.sources=[...]`).
    #[test]
    fn a_failure_logs_its_error_and_causes() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (log, subscriber) = RunLog::open(dir.path()).expect("open");
        let err = crate::root::RelaunchError::Exec(std::io::Error::other("no sudo"));
        tracing::subscriber::with_default(subscriber, || {
            let _run = log.enter();
            log.finished("failed", Some(&err));
        });
        let text = log_text(dir.path());
        assert!(text.contains("outcome=\"failed\""), "{text}");
        assert!(
            text.contains("error=could not relaunch as root through sudo"),
            "{text}"
        );
        assert!(text.contains("error.sources=[no sudo]"), "{text}");
    }

    /// An error marked sensitive never reaches the log: the record says it
    /// was withheld (term-ui decision 8; CONSTRAINTS.md, logging).
    #[test]
    fn a_sensitive_error_is_withheld_from_the_log() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (log, subscriber) = RunLog::open(dir.path()).expect("open");
        let secret = std::io::Error::other("secret hunter2-not-real");
        let err = Sensitive::new(secret);
        tracing::subscriber::with_default(subscriber, || {
            let _run = log.enter();
            log.finished("failed", Some(&err));
        });
        let text = log_text(dir.path());
        assert!(!text.contains("hunter2"), "{text}");
        assert!(text.contains(WITHHELD), "{text}");
        assert!(text.contains("outcome=\"failed\""), "{text}");
    }

    /// The held error stays reachable for code that acts on it, while the
    /// marker itself prints only the placeholder.
    #[test]
    fn a_sensitive_error_keeps_its_inner_error() {
        let err = Sensitive::new(std::io::Error::other("secret hunter2-not-real"));
        assert_eq!(err.inner().to_string(), "secret hunter2-not-real");
        assert_eq!(err.to_string(), WITHHELD);
        assert_eq!(format!("{err:?}"), format!("Sensitive({WITHHELD})"));
    }

    /// An outer error with a sensitive cause logs the outer part and the
    /// placeholder.
    #[test]
    fn a_sensitive_cause_is_withheld_from_the_log() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (log, subscriber) = RunLog::open(dir.path()).expect("open");
        let secret = Sensitive::new(std::io::Error::other("secret hunter2-not-real"));
        let err = Outer(secret);
        tracing::subscriber::with_default(subscriber, || {
            let _run = log.enter();
            log.finished("failed", Some(&err));
        });
        let text = log_text(dir.path());
        assert!(!text.contains("hunter2"), "{text}");
        assert!(
            text.contains("error=check failed error.sources=[[REDACTED]]"),
            "{text}"
        );
    }

    /// An error with one cause, for the test above.
    #[derive(Debug, thiserror::Error)]
    #[error("check failed")]
    struct Outer(#[source] Sensitive);
}
