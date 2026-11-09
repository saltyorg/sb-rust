//! sb's own read-only reader of systemd's journal files: pread only, pure
//! Rust, exact against each supported release's journalctl except the
//! differences the journal spec lists (docs/specs/journal.md). Why sb owns
//! the reader rather than using a crate or a `journalctl` child:
//! docs/decisions/ADR-013-own-journal-reader.md.

// So far: one file's reading (`file`, public under `testing` for the
// tools), the hashes, the output limit and the read errors. The walks,
// queries and the follow come in the plan's later tasks (tasks/plan.md).

use std::fmt;
use std::path::{Path, PathBuf};

#[cfg(any(test, feature = "testing"))]
pub mod file;
#[cfg(not(any(test, feature = "testing")))]
mod file;
mod hash;
#[cfg(feature = "testing")]
pub mod testing;

/// The output limit's default, our own bound on the payload bytes one
/// query reads and decodes, not a journald value: a corrupt file can give
/// each entry a decompression bomb, or an entry listing one object
/// millions of times, and the records are held for output. Failed reads
/// spend it too, so it bounds work as well as memory. 256 MiB is far above
/// an `sb logs` page (500 entries are typically well under a MiB). It is
/// below journald's `ENTRY_SIZE_MAX` (770 MiB), so it also bounds one entry:
/// only an entry carrying a coredump stored in the journal comes near it,
/// and it is left out with the limit reported, where journalctl would
/// print it (journal spec, Difference 5).
const OUTPUT_DEFAULT: usize = 256 << 20;

/// The bytes of payload one query (a page, or one file's share of a
/// follow's wake) may read and decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    output: usize,
}

impl Default for Limits {
    /// 256 MiB.
    fn default() -> Self {
        Self {
            output: OUTPUT_DEFAULT,
        }
    }
}

impl Limits {
    /// An output limit of `bytes`, capped at the journal format's own
    /// limit on one DATA object, systemd's `DATA_SIZE_MAX` (768 MiB): a
    /// larger payload is corruption or a bomb whatever the limit.
    #[must_use]
    pub fn with_output(bytes: usize) -> Self {
        Self {
            output: bytes.min(file::DATA_SIZE_MAX),
        }
    }

    /// The output limit in bytes.
    #[must_use]
    pub fn output(&self) -> usize {
        self.output
    }
}

/// A payload larger than the caller allowed. Not corruption by itself: a
/// lookup reads a candidate only as far as the value it looks for, and a
/// query stops once its output limit is spent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("payload over its limit of {limit} bytes")]
pub struct OverLimit {
    /// The bytes the read was allowed.
    pub limit: usize,
}

/// An entry lists a DATA object this query already failed to read: a
/// definitive error, not one to wait for, since the object had its read
/// (and, following, its wait) already.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("more entries list data {item:#x}, which can't be read; left out too")]
pub struct KnownBad {
    /// The DATA object's offset in its file.
    pub item: u64,
}

/// What went wrong reading a journal file. The file is the `ReadError`'s,
/// so no message here names it.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReadErrorKind {
    /// A payload over the read's limit (`OverLimit`), with where it was
    /// met (for example `data 0x1f8: zstd`).
    #[error("{}", prefixed(context, over))]
    OverLimit {
        /// Where the payload was met; empty when nothing more is known.
        context: String,
        /// The limit the payload went over.
        over: OverLimit,
    },
    /// A DATA object this query already failed to read (`KnownBad`).
    #[error(transparent)]
    KnownBad(#[from] KnownBad),
    /// The file's content breaks the format: a bad offset, size, link,
    /// header or compressed payload.
    #[error("{0}")]
    Corrupt(String),
    /// The system refused a read.
    #[error("{context}")]
    Io {
        /// What was being read.
        context: String,
        /// The system's error.
        #[source]
        source: std::io::Error,
    },
}

/// `context: what`, or `what` alone without a context.
fn prefixed(context: &str, what: &dyn fmt::Display) -> String {
    if context.is_empty() {
        what.to_string()
    } else {
        format!("{context}: {what}")
    }
}

/// An error reading one journal file: the file and what went wrong.
///
/// `Display` writes the path escaped (`str::escape_debug`), so control
/// bytes in a file name can't reach a terminal (security audit L5).
///
/// `Display` gives the file and the context only. For `ReadErrorKind::Io`
/// the system's error is not in it but is the `source()`, so callers
/// render the whole chain (anyhow's `{:#}`, or walking `source()`) to show
/// it.
#[derive(Debug)]
pub struct ReadError {
    path: PathBuf,
    kind: ReadErrorKind,
}

impl ReadError {
    /// The file the error is about.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What went wrong, for matching (`ReadErrorKind::OverLimit`,
    /// `ReadErrorKind::KnownBad`).
    #[must_use]
    pub fn kind(&self) -> &ReadErrorKind {
        &self.kind
    }
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: {}",
            self.path.to_string_lossy().escape_debug(),
            self.kind
        )
    }
}

impl std::error::Error for ReadError {
    /// The kind's own source (the system's error for `Io`): the kind's
    /// message is already part of this error's.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        std::error::Error::source(&self.kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_output_limit_is_256_mib() {
        assert_eq!(Limits::default().output(), 256 * 1024 * 1024);
    }

    #[test]
    fn the_output_limit_is_capped_at_data_size_max() {
        assert_eq!(Limits::with_output(4096).output(), 4096);
        assert_eq!(
            Limits::with_output(file::DATA_SIZE_MAX).output(),
            768 * 1024 * 1024
        );
        assert_eq!(Limits::with_output(usize::MAX).output(), 768 * 1024 * 1024);
    }

    fn error(path: &str, kind: ReadErrorKind) -> ReadError {
        ReadError {
            path: PathBuf::from(path),
            kind,
        }
    }

    /// A file name carrying escape sequences and other controls is
    /// written escaped: no control byte of it reaches a terminal (L5).
    #[test]
    fn display_escapes_control_bytes_in_the_path() {
        let e = error(
            "/run/log/journal/x\u{1b}]0;title\u{7}\u{9b}31m\n.journal",
            ReadErrorKind::Corrupt("bad header size".into()),
        );
        let shown = e.to_string();
        assert_eq!(
            shown,
            "/run/log/journal/x\\u{1b}]0;title\\u{7}\\u{9b}31m\\n.journal: bad header size"
        );
        assert!(!shown.chars().any(char::is_control), "{shown:?}");
        assert_eq!(
            e.path(),
            Path::new("/run/log/journal/x\u{1b}]0;title\u{7}\u{9b}31m\n.journal")
        );
    }

    #[test]
    fn display_names_the_file_then_the_kind() {
        let over = error(
            "/var/log/journal/m/system.journal",
            ReadErrorKind::OverLimit {
                context: "data 0x1f8: zstd".into(),
                over: OverLimit { limit: 5 },
            },
        );
        assert_eq!(
            over.to_string(),
            "/var/log/journal/m/system.journal: data 0x1f8: zstd: payload over its limit of 5 bytes"
        );
        let bare = ReadErrorKind::OverLimit {
            context: String::new(),
            over: OverLimit { limit: 5 },
        };
        assert_eq!(bare.to_string(), "payload over its limit of 5 bytes");
        let known = error("/f", KnownBad { item: 0x2a0 }.into());
        assert_eq!(
            known.to_string(),
            "/f: more entries list data 0x2a0, which can't be read; left out too"
        );
        assert!(matches!(
            known.kind(),
            ReadErrorKind::KnownBad(KnownBad { item: 0x2a0 })
        ));
    }

    /// The system's error is the source, not repeated in the message.
    #[test]
    fn an_io_error_is_the_source() {
        let e = error(
            "/f",
            ReadErrorKind::Io {
                context: "read at 0x110".into(),
                source: std::io::Error::other("gone"),
            },
        );
        assert_eq!(e.to_string(), "/f: read at 0x110");
        let source = std::error::Error::source(&e).expect("a source");
        assert_eq!(source.to_string(), "gone");
        let corrupt = error("/f", ReadErrorKind::Corrupt("x".into()));
        assert!(std::error::Error::source(&corrupt).is_none());
    }
}
