//! The run log on the host (spec: startup step 4; capability map,
//! decision 11).
//!
//! Every root run writes a record to `/var/log/sb`: a run span with an id,
//! the command's name, the command's own events, and how the run ended.
//! Argument values are never logged: they can carry secrets
//! (`sb install -e password=...`). tracing-subscriber's fmt layer writes
//! plain text to a tracing-appender `RollingFileAppender` that starts a new
//! file each UTC day and keeps the last 13 to 14 (it prunes to one below
//! `max_log_files` at every start), as proved in proto-runner
//! (https://docs.rs/tracing-appender/0.2.5/tracing_appender/rolling/struct.Builder.html).

use std::error::Error;
use std::ffi::OsString;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use tracing::span::EnteredSpan;

use crate::cancel::Shutdown;

/// Daily files tracing-appender keeps (13 to 14 days, see above).
const LOG_FILES_KEPT: usize = 14;

/// Where runs log: `SB_LOG_DIR` when set (tests, debugging), otherwise
/// `/var/log/sb`.
pub fn log_dir(override_dir: Option<OsString>) -> PathBuf {
    override_dir.map_or_else(|| PathBuf::from("/var/log/sb"), PathBuf::from)
}

/// One run's log: its id and where it is written.
pub struct RunLog {
    id: String,
    dir: PathBuf,
}

impl RunLog {
    /// Open the log in `dir`, creating it root-only (0700), and the
    /// subscriber that writes to it; the caller installs the subscriber.
    pub fn open(
        dir: &Path,
    ) -> std::io::Result<(Self, impl tracing::Subscriber + Send + Sync + use<>)> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        let appender = tracing_appender::rolling::Builder::new()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix("sb")
            .filename_suffix("log")
            .max_log_files(LOG_FILES_KEPT)
            .build(dir)
            .map_err(std::io::Error::other)?;
        // No colours of fmt's own. Escape codes inside logged values are
        // escaped (written as `\x1b`), not removed: child output is stripped
        // where it is captured (the exec module, `console::strip_ansi_codes`).
        // A failed write (full disk) isn't reported on stderr, where it
        // would land among the command's own output.
        let subscriber = tracing_subscriber::fmt()
            .with_writer(appender)
            .with_ansi(false)
            .with_target(false)
            .log_internal_errors(false)
            .finish();
        // Only needs to tell runs apart within the log: start time and pid.
        // Why not uuid or fastrand: neither (nor getrandom) is in sb's
        // dependency tree, and the id only has to differ between runs on
        // one host.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let id = format!("{:08x}", nanos ^ std::process::id());
        Ok((
            Self {
                id,
                dir: dir.to_path_buf(),
            },
            subscriber,
        ))
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// The run's span, entered for the whole run: every event in it carries
    /// the run id.
    pub fn enter(&self) -> EnteredSpan {
        tracing::info_span!("run", run = %self.id).entered()
    }

    /// The run record: sb's version and the command's name, never its
    /// arguments.
    pub fn started(&self, command: &str) {
        tracing::info!(version = env!("CARGO_PKG_VERSION"), command, "run started");
    }

    /// How the run ended (see [`outcome`]). A failure's error goes in as
    /// tracing's error field, which prints its causes too
    /// (`error=... error.sources=[...]`).
    pub fn finished(&self, outcome: &str, error: Option<&(dyn Error + 'static)>) {
        match error {
            Some(error) => tracing::info!(outcome, error, "run finished"),
            None => tracing::info!(outcome, "run finished"),
        }
    }

    /// Where to look, for the error block's `details:` line.
    pub fn pointer(&self) -> String {
        format!("{} (run {})", self.dir.display(), self.id)
    }
}

/// How a run ended, for its last log line: a signal, a requested or quiet
/// exit code (classified as [`crate::exit::report`] does), a failure, or
/// done.
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
    use std::os::unix::fs::PermissionsExt;

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
        assert!(name.starts_with("sb.") && name.ends_with(".log"), "{name}");
        std::fs::read_to_string(&files[0]).expect("read log")
    }

    #[test]
    fn a_run_records_its_command_events_and_outcome() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (log, subscriber) = RunLog::open(dir.path()).expect("open");
        tracing::subscriber::with_default(subscriber, || {
            let _run = log.enter();
            log.started("version");
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
            text.contains("run finished") && text.contains("outcome=\"done\""),
            "{text}"
        );
        assert!(text.contains("Get: jq"), "{text}");
        assert!(!text.contains('\x1b'), "escape codes in the log: {text:?}");
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

    /// A directory that can't be made fails `open`; the caller warns and
    /// runs on without a log.
    #[test]
    fn an_unusable_directory_fails_to_open() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        assert!(RunLog::open(&file.path().join("sb")).is_err());
    }

    #[test]
    fn the_pointer_names_the_directory_and_run() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (log, _) = RunLog::open(dir.path()).expect("open");
        assert_eq!(
            log.pointer(),
            format!("{} (run {})", dir.path().display(), log.id())
        );
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
        let pipe = std::io::Error::from(std::io::ErrorKind::BrokenPipe);
        assert_eq!(outcome(&shutdown, Some(&pipe)), "exit 141");
        assert_eq!(outcome(&shutdown, Some(&crate::exit::Exit(5))), "exit 5");
        let interrupted = Shutdown::new();
        interrupted.signalled(signal_hook::consts::SIGTERM);
        assert_eq!(outcome(&interrupted, Some(&err)), "interrupted (exit 143)");
        let requested = Shutdown::new();
        requested.request(4);
        assert_eq!(outcome(&requested, None), "exit 4");
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
}
