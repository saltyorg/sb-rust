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
//! (https://docs.rs/tracing-appender/0.2.5/tracing_appender/rolling/struct.Builder.html).
//! Why not `rolling::never`
//! (https://docs.rs/tracing-appender/0.2.5/tracing_appender/rolling/fn.never.html):
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

use crate::cancel::Shutdown;

/// The log file's name in the log directory.
const LOG_FILE: &str = "sb.log";

/// Where runs log: `SB_LOG_DIR` when set (tests, debugging), otherwise
/// `/var/log/sb`.
pub fn log_dir(override_dir: Option<OsString>) -> PathBuf {
    override_dir.map_or_else(|| PathBuf::from("/var/log/sb"), PathBuf::from)
}

/// What sensitive text shows as, on screen and in the log (term-ui
/// decision 8).
pub const WITHHELD: &str = "(sensitive, withheld)";

/// An error whose text must not be shown or logged (credentials, tokens).
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
pub struct RunLog {
    id: String,
    /// The file the run writes to.
    file: PathBuf,
}

impl RunLog {
    /// Open the log in `dir`, creating it root-only (0700), and the
    /// subscriber that writes to it; the caller installs the subscriber.
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
            .map_or(0, |d| d.as_nanos());
        // The low 64 bits move fastest; keeping only them is the point.
        let id = format!("{:016x}", (nanos as u64) ^ u64::from(std::process::id()));
        // Never rotating, the appender names the file by its prefix alone
        // (tracing-appender 0.2.5 rolling.rs `join_date`).
        let file = dir.join(LOG_FILE);
        Ok((Self { id, file }, subscriber))
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// The run's span, entered for the whole run: every event in it carries
    /// the run id.
    pub fn enter(&self) -> EnteredSpan {
        tracing::info_span!("run", run = %self.id).entered()
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
        match error {
            Some(error) => tracing::info!(outcome, error, "run finished"),
            None => tracing::info!(outcome, "run finished"),
        }
    }

    /// Where to look, for the error block's `details:` line.
    pub fn pointer(&self) -> String {
        format!("{} (run {})", self.file.display(), self.id)
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
        assert!(name.starts_with("sb.") && name.ends_with(".log"), "{name}");
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
            text.contains("error=check failed error.sources=[(sensitive, withheld)]"),
            "{text}"
        );
    }

    /// An error with one cause, for the test above.
    #[derive(Debug, thiserror::Error)]
    #[error("check failed")]
    struct Outer(#[source] Sensitive);
}
