//! bench's Speedtest section (ops spec, "`sb bench`", 3.; decision 13):
//! Ookla's CLI pinned per architecture, downloaded through release's
//! verified download, its one `speedtest` entry extracted, checked as an
//! ELF for this machine, then run against bench.sh's nodes
//! (`cmd/bench_script.sh`, `speed` and `speed_test`), each node's result
//! shown as the script shows it. docs/platform-notes/speedtest.md records
//! the CLI's JSON and `HOME` as one run showed them.

use std::fs::OpenOptions;
use std::io;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sb_cli_core::cancel::CancellationToken;
use sb_cli_core::theme::Role;
use sb_exec::{Cmd, Ending};
use sb_release::Arch;
use sb_term_ui::{Cell, Heading, Table};

use super::label;

/// A pinned archive: its URL, size and SHA-256 (hex).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Pin {
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

/// sb-go's pin (host/benchmark.go:20-22).
const X86_64_URL: &str =
    "https://install.speedtest.net/app/cli/ookla-speedtest-1.2.0-linux-x86_64.tgz";
const X86_64_SIZE: u64 = 1_106_829;
const X86_64_SHA256: &str = "5690596c54ff9bed63fa3732f818a05dbc2db19ad36ed68f21ca5f64d5cfeeb7";

/// Ookla's 1.2.0 `linux-aarch64` archive, its size and digest taken from
/// Ookla's download (docs/platform-notes/speedtest.md).
const AARCH64_URL: &str =
    "https://install.speedtest.net/app/cli/ookla-speedtest-1.2.0-linux-aarch64.tgz";
const AARCH64_SIZE: u64 = 1_082_287;
const AARCH64_SHA256: &str = "3953d231da3783e2bf8904b6dd72767c5c6e533e163d3742fd0437affa431bd3";

/// The largest archive and `speedtest` entry accepted (sb-go's 64 MiB).
pub const MAX_SIZE: u64 = 64 << 20;

/// Each node's limit (ops spec: 120 s).
pub const NODE_TIMEOUT: Duration = Duration::from_secs(120);

impl Pin {
    /// The pin for `arch`.
    #[must_use]
    pub fn for_arch(arch: Arch) -> Pin {
        let (url, size, sha256) = match arch {
            Arch::Amd64 => (X86_64_URL, X86_64_SIZE, X86_64_SHA256),
            Arch::Arm64 => (AARCH64_URL, AARCH64_SIZE, AARCH64_SHA256),
        };
        Pin {
            url: url.to_owned(),
            size,
            sha256: sha256.to_owned(),
        }
    }

    /// The pin as release's [`sb_release::Asset`].
    ///
    /// # Errors
    ///
    /// The URL or the digest isn't valid, or release refuses the asset.
    pub fn asset(&self) -> anyhow::Result<sb_release::Asset> {
        let url = reqwest::Url::parse(&self.url)?;
        let mut sha256 = [0u8; 32];
        hex::decode_to_slice(&self.sha256, &mut sha256)?;
        Ok(sb_release::Asset::pinned(url, self.size, sha256)?)
    }
}

/// A node bench.sh tests against: its name and Ookla server id (none for
/// Speedtest.net's choice).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node {
    pub name: &'static str,
    pub id: Option<&'static str>,
}

/// bench.sh's nodes, in its order (`speed`, bench_script.sh:115-128).
pub const NODES: [Node; 12] = [
    Node {
        name: "Speedtest.net",
        id: None,
    },
    Node {
        name: "Los Angeles, US",
        id: Some("7190"),
    },
    Node {
        name: "Dallas, US",
        id: Some("22288"),
    },
    Node {
        name: "Montreal, CA",
        id: Some("64420"),
    },
    Node {
        name: "Paris, FR",
        id: Some("61933"),
    },
    Node {
        name: "Amsterdam, NL",
        id: Some("41423"),
    },
    Node {
        name: "Suzhou, CN",
        id: Some("5396"),
    },
    Node {
        name: "Ningbo, CN",
        id: Some("59387"),
    },
    Node {
        name: "Hong Kong, CN",
        id: Some("32155"),
    },
    Node {
        name: "Singapore, SG",
        id: Some("13623"),
    },
    Node {
        name: "Taipei, CN",
        id: Some("65092"),
    },
    Node {
        name: "Tokyo, JP",
        id: Some("48463"),
    },
];

/// Why the archive's `speedtest` couldn't be taken.
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("read archive: {0}")]
    Read(io::Error),
    #[error("archive contains duplicate speedtest entries")]
    Duplicate,
    #[error("speedtest archive entry is not a regular file")]
    NotRegular,
    #[error("speedtest archive entry has invalid size {0}")]
    Size(u64),
    #[error("write Speedtest executable: {0}")]
    Write(io::Error),
    #[error("archive does not contain speedtest")]
    Missing,
}

/// The one regular entry named `speedtest` in the gzipped tar `archive`,
/// written to `destination` (created exclusively, 0700, synced), as
/// sb-go's `extractSpeedtest`: a second such entry, one that isn't a
/// regular file, or a size of 0 or over 64 MiB fails, and a file it
/// created is removed then. tar's reader over flate2's `MultiGzDecoder`,
/// as Go's multistream `gzip.Reader`
/// (<https://docs.rs/tar/0.4.46/tar/struct.Archive.html#method.entries>,
/// <https://docs.rs/flate2/1.1.10/flate2/read/struct.MultiGzDecoder.html>).
/// Why not tar's `Entry::unpack`: it takes the archive's mode and
/// times, overwrites a file already there, and has no size cap or sb-go's
/// texts (as python-env's uv extraction notes). Blocking: run it on the
/// blocking pool.
///
/// # Errors
///
/// [`ExtractError`] saying which.
pub fn extract(archive: &Path, destination: &Path) -> Result<(), ExtractError> {
    let file = std::fs::File::open(archive).map_err(ExtractError::Read)?;
    let mut tar = tar::Archive::new(flate2::read::MultiGzDecoder::new(file));
    let mut found = false;
    for entry in tar.entries().map_err(ExtractError::Read)? {
        let mut entry = entry.map_err(ExtractError::Read)?;
        if entry.path().map_err(ExtractError::Read)?.as_os_str() != "speedtest" {
            continue;
        }
        if found {
            let _ = std::fs::remove_file(destination);
            return Err(ExtractError::Duplicate);
        }
        if !entry.header().entry_type().is_file() {
            return Err(ExtractError::NotRegular);
        }
        let size = entry.size();
        if size == 0 || size > MAX_SIZE {
            return Err(ExtractError::Size(size));
        }
        write_entry(&mut entry, size, destination).map_err(ExtractError::Write)?;
        found = true;
    }
    if found {
        Ok(())
    } else {
        Err(ExtractError::Missing)
    }
}

/// `entry` (tar already limits it to its `size`) into a new 0700 file at
/// `destination`, synced; the file is removed again when anything after
/// its creation fails, and one already there is left alone. Opened,
/// written and closed under cli-core's fork lock, so no child sb starts
/// meanwhile holds it open when the nodes run it (ETXTBSY, `fork_lock`);
/// synced after through a handle of its own, so the sync holds no child
/// back.
fn write_entry(entry: &mut impl io::Read, size: u64, destination: &Path) -> io::Result<()> {
    // The outer error, a failed create, leaves a file already there
    // alone; the inner one, a failed copy, removes the file made here.
    let copied = sb_cli_core::fork_lock::writing(|| {
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(destination)?;
        Ok::<_, io::Error>(io::copy(entry, &mut output))
    })?;
    let written = copied.and_then(|written| {
        if written == size {
            // A read-only handle can be synced on Linux (fsync(2),
            // HISTORY: a writable one is System V Release 4's rule,
            // "forbidden by POSIX",
            // <https://man7.org/linux/man-pages/man2/fsync.2.html>).
            std::fs::File::open(destination).and_then(|output| output.sync_all())
        } else {
            Err(io::Error::other(format!(
                "entry size mismatch: expected {size} bytes, got {written}"
            )))
        }
    });
    if written.is_err() {
        let _ = std::fs::remove_file(destination);
    }
    written
}

/// One node's row, as bench.sh shows it.
#[derive(Debug, Clone, PartialEq)]
pub enum Measured {
    /// Upload and download in Mbps and the latency in ms, each with two
    /// decimals and its unit, as the CLI's text prints them.
    Speeds {
        upload: String,
        download: String,
        latency: String,
    },
    /// The CLI failed (bench.sh's `Test failed`).
    Failed,
    /// It answered without the three values (`Failed to parse results`).
    Unparsed,
}

/// The three values a node's JSON gives; serde skips every other field
/// (the interface, server and result ones are never read).
#[derive(serde::Deserialize)]
struct NodeJson {
    ping: Ping,
    download: Bandwidth,
    upload: Bandwidth,
}

#[derive(serde::Deserialize)]
struct Ping {
    latency: f64,
}

#[derive(serde::Deserialize)]
struct Bandwidth {
    bandwidth: f64,
}

/// A node's row from the CLI's JSON (`--format=json`): `upload.bandwidth`
/// and `download.bandwidth` in bytes per second, shown in Mbps (10^6
/// bits) as the CLI's text does, and `ping.latency` in ms, its text's
/// `Idle Latency` (docs/platform-notes/speedtest.md).
#[must_use]
pub fn parse(stdout: &str) -> Measured {
    match serde_json::from_str::<NodeJson>(stdout) {
        Ok(result) => Measured::Speeds {
            upload: format!("{:.2} Mbps", result.upload.bandwidth * 8.0 / 1e6),
            download: format!("{:.2} Mbps", result.download.bandwidth * 8.0 / 1e6),
            latency: format!("{:.2} ms", result.ping.latency),
        },
        Err(_) => Measured::Unparsed,
    }
}

/// `speedtest` (`program`) against `node`, as bench.sh runs it, with
/// `--format=json` (the CLI's own machine-readable output), `HOME` set to
/// `home` (and `XDG_CONFIG_HOME` removed) so its licence file stays there, within `timeout`, stopped by
/// `token`. Only an exit of 0 with the three values measures. Its stdout
/// and stderr reach neither the screen nor the log.
///
/// # Errors
///
/// `interrupted` when `token` was cancelled while it ran.
pub async fn run_node(
    program: &Path,
    home: &Path,
    node: Node,
    timeout: Duration,
    token: &CancellationToken,
) -> anyhow::Result<Measured> {
    let mut cmd = Cmd::new(program.as_os_str())
        .arg("--format=json")
        .arg("--progress=no")
        .arg("--accept-license")
        .arg("--accept-gdpr");
    if let Some(id) = node.id {
        cmd = cmd.shown_value(format!("--server-id={id}"));
    }
    // The licence file goes to `$XDG_CONFIG_HOME` before `$HOME`; neither
    // may be root's.
    let ran = cmd
        .env("HOME", home.to_string_lossy())
        .env_remove("XDG_CONFIG_HOME")
        .timeout(timeout)
        .capture(token)
        .await;
    let ran = match ran {
        Ok(ran) => ran,
        Err(err) => {
            tracing::warn!(node = node.name, error = %err, cause = %err.cause(), "speedtest didn't start");
            return Ok(Measured::Failed);
        }
    };
    match ran.outcome.ending {
        Ending::Interrupted => anyhow::bail!("interrupted"),
        Ending::Exited(0) => {
            let measured = parse(&ran.stdout);
            if measured == Measured::Unparsed {
                tracing::warn!(node = node.name, "speedtest's answer had no results");
            }
            Ok(measured)
        }
        ending => {
            tracing::warn!(node = node.name, %ending, "speedtest failed");
            Ok(Measured::Failed)
        }
    }
}

/// The section's outcome.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// It couldn't be prepared: why.
    Unavailable(String),
    /// Each node run, in order.
    Nodes(Vec<(Node, Measured)>),
}

/// The Speedtest section's heading: its name, and the node count, muted,
/// when the nodes ran (`sb list`'s `  31 tags`).
#[must_use]
pub fn heading(outcome: &Outcome) -> Heading {
    let heading = super::heading("Speedtest");
    match outcome {
        Outcome::Unavailable(_) => heading,
        Outcome::Nodes(nodes) => heading.hint(format!("  {} nodes", nodes.len()), Role::Muted),
    }
}

/// The Speedtest section's rows (bench's ruled layout): `Node`, `Upload`,
/// `Download` and `Latency` under muted headings, the figures
/// right-aligned in columns 14, 14 and 11 wide, a failed node's text
/// right-aligned in the Upload column in Problem; or the one `Skipped`
/// row saying why it didn't run, in Warn.
#[must_use]
pub fn table(outcome: &Outcome) -> Table {
    match outcome {
        Outcome::Unavailable(cause) => {
            let mut table = super::rows();
            table.row([
                label("Skipped"),
                Cell::new(format!("Speedtest unavailable: {cause}")).role(Role::Warn),
            ]);
            table
        }
        Outcome::Nodes(nodes) => {
            let mut table = nodes_table();
            for (node, measured) in nodes {
                table.row(node_row(node, measured));
            }
            table
        }
    }
}

/// The nodes' table: `Node`, `Upload`, `Download` and `Latency` under
/// muted headings, the figures right-aligned in columns 14, 14 and 11
/// wide.
fn nodes_table() -> Table {
    Table::ruled(["Node", "Upload", "Download", "Latency"])
        .min_width(0, super::LABEL)
        .right_align(1)
        .min_width(1, super::RATE)
        .right_align(2)
        .min_width(2, super::RATE)
        .right_align(3)
        .min_width(3, LATENCY)
}

/// A node's row: its name and figures, or its failure in Problem in the
/// Upload column.
fn node_row(node: &Node, measured: &Measured) -> Vec<Cell> {
    let failed = |text: &str| vec![Cell::new(node.name), Cell::new(text).role(Role::Problem)];
    match measured {
        Measured::Speeds {
            upload,
            download,
            latency,
        } => [node.name, upload, download, latency]
            .map(Cell::new)
            .to_vec(),
        Measured::Failed => failed("Test failed"),
        Measured::Unparsed => failed("Failed to parse results"),
    }
}

/// The section's heading while its nodes run: the node count they will
/// have when they all ran.
#[must_use]
pub fn live_heading() -> Heading {
    super::heading("Speedtest").hint(format!("  {} nodes", NODES.len()), Role::Muted)
}

/// The section while the Speedtest is downloaded (ops spec, "`sb
/// bench`", Progress): one `Preparing` row, `preparing` its value.
#[must_use]
pub fn preparing_table(preparing: Cell) -> Table {
    let mut table = super::rows();
    table.row([label("Preparing"), preparing]);
    table
}

/// The section while its nodes run: the nodes measured so far (`done`,
/// in [`NODES`]' order) as [`table`] shows them, `testing` in the next
/// node's Upload cell, the nodes after it listed by name, muted.
#[must_use]
pub fn live_table(done: &[Measured], testing: Cell) -> Table {
    let mut table = nodes_table();
    let mut testing = Some(testing);
    for (n, node) in NODES.iter().enumerate() {
        match done.get(n) {
            Some(measured) => table.row(node_row(node, measured)),
            None => match testing.take() {
                Some(cell) => table.row([Cell::new(node.name), cell]),
                None => table.row([Cell::new(node.name).role(Role::Muted)]),
            },
        }
    }
    table
}

/// The Latency column's width, its gap included: `270.00 ms` and two
/// spaces.
const LATENCY: usize = 11;

/// A prepared Speedtest: the private directory holding the program (and
/// the CLI's `HOME`), removed when dropped.
#[derive(Debug)]
pub struct Prepared {
    dir: tempfile::TempDir,
    program: PathBuf,
}

impl Prepared {
    /// A prepared Speedtest of `program` in `dir` (tests: a stand-in that
    /// isn't an ELF).
    #[cfg(test)]
    pub(crate) fn of(dir: tempfile::TempDir, program: PathBuf) -> Self {
        Self { dir, program }
    }

    /// The program's path.
    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// The directory the CLI's `HOME` points at.
    #[must_use]
    pub fn home(&self) -> &Path {
        self.dir.path()
    }
}

/// Download `pin` through `releases` into a new private directory in
/// `parent` (0700, tempfile's `TempDir` with `Builder::permissions`:
/// tempfile's own default is the umask's; `parent` is `/tmp`, sticky,
/// not `$TMPDIR`), take its `speedtest` out and check it is an ELF for
/// `arch`; the archive is removed once read
/// (<https://docs.rs/tempfile/3.27.0/tempfile/struct.Builder.html#method.permissions>).
///
/// # Errors
///
/// The cause, in sb-go's words where it has them (`prepare Speedtest:
/// ...` is the caller's): the pin, the download or its digest, the
/// archive, the ELF check, or `interrupted`.
pub async fn prepare(
    releases: &sb_release::Releases,
    pin: &Pin,
    arch: Arch,
    parent: &Path,
    step: &mut sb_term_ui::Step,
) -> anyhow::Result<Prepared> {
    use std::os::unix::fs::PermissionsExt as _;
    let asset = pin.asset()?;
    let dir = tempfile::Builder::new()
        .prefix("sb-bench-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(parent)?;
    let staged = releases
        .download(
            &asset,
            sb_release::Stage {
                dir: dir.path(),
                prefix: "speedtest-",
                max_size: MAX_SIZE,
            },
            step,
        )
        .await?;
    let program = dir.path().join("speedtest");
    let (archive, target) = (staged.path().to_path_buf(), program.clone());
    sb_cli_core::spawn::blocking(move || {
        extract(&archive, &target)?;
        sb_release::check_elf(&target, arch)?;
        anyhow::Ok(())
    })
    .await??;
    drop(staged);
    Ok(Prepared { dir, program })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use sb_cli_core::theme::Theme;

    use super::*;

    #[test]
    fn the_pins_are_ookla_s_1_2_0_archives() {
        let amd64 = Pin::for_arch(Arch::Amd64);
        // The URL as a literal, so a version bumped alone (Renovate's,
        // ADR-026) fails here until the size and digest follow.
        assert_eq!(
            amd64.url,
            "https://install.speedtest.net/app/cli/ookla-speedtest-1.2.0-linux-x86_64.tgz"
        );
        assert_eq!(amd64.size, 1_106_829);
        assert_eq!(
            amd64.sha256,
            "5690596c54ff9bed63fa3732f818a05dbc2db19ad36ed68f21ca5f64d5cfeeb7"
        );
        let arm64 = Pin::for_arch(Arch::Arm64);
        assert!(
            arm64
                .url
                .ends_with("ookla-speedtest-1.2.0-linux-aarch64.tgz")
        );
        assert_eq!(arm64.size, 1_082_287);
        for pin in [amd64, arm64] {
            let asset = pin.asset().expect("asset");
            assert_eq!(asset.size(), pin.size);
            assert_eq!(hex::encode(asset.sha256()), pin.sha256);
        }
    }

    #[test]
    fn a_bad_pin_is_refused() {
        let bad = |url: &str, sha256: &str| Pin {
            url: url.to_owned(),
            size: 1,
            sha256: sha256.to_owned(),
        };
        let sha = "00".repeat(32);
        assert!(bad("not a url", &sha).asset().is_err());
        assert!(bad("http://example.invalid/x.tgz", &sha).asset().is_err());
        assert!(bad("https://example.invalid/x.tgz", "zz").asset().is_err());
    }

    #[test]
    fn the_nodes_are_bench_sh_s() {
        assert_eq!(
            NODES[0],
            Node {
                name: "Speedtest.net",
                id: None
            }
        );
        let ids: Vec<_> = NODES.iter().filter_map(|node| node.id).collect();
        assert_eq!(
            ids,
            [
                "7190", "22288", "64420", "61933", "41423", "5396", "59387", "32155", "13623",
                "65092", "48463"
            ]
        );
        assert_eq!(NODES[11].name, "Tokyo, JP");
    }

    /// An archive entry: its name, bytes and kind.
    type Entry<'a> = (&'a str, &'a [u8], tar::EntryType);

    /// A gzipped tar of `entries`.
    fn archive(dir: &Path, entries: &[Entry<'_>]) -> PathBuf {
        let path = dir.join("archive.tgz");
        let file = std::fs::File::create(&path).expect("create");
        let gz = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(gz);
        for (name, bytes, kind) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(*kind);
            header.set_size(u64::try_from(bytes.len()).expect("size"));
            header.set_mode(0o755);
            if *kind == tar::EntryType::Symlink {
                header.set_link_name("/etc/passwd").expect("link");
            }
            header.set_cksum();
            builder
                .append_data(&mut header, name, *bytes)
                .expect("append");
        }
        builder.into_inner().expect("tar").finish().expect("gz");
        path
    }

    #[test]
    fn extract_takes_the_one_speedtest_entry() {
        let dir = tempfile::tempdir().expect("dir");
        let path = archive(
            dir.path(),
            &[
                ("speedtest.md", b"docs", tar::EntryType::Regular),
                ("speedtest", b"\x7fELF program", tar::EntryType::Regular),
                ("speedtest.5", b"man", tar::EntryType::Regular),
            ],
        );
        let out = dir.path().join("speedtest");
        extract(&path, &out).expect("extract");
        assert_eq!(std::fs::read(&out).expect("read"), b"\x7fELF program");
        let mode = std::fs::metadata(&out).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        // Created exclusively: a file already there fails, and stays.
        let err = extract(&path, &out).expect_err("exists");
        assert!(matches!(err, ExtractError::Write(_)), "{err}");
        assert_eq!(std::fs::read(&out).expect("kept"), b"\x7fELF program");
    }

    /// The program is written under cli-core's fork lock: never while a
    /// child starts, which would inherit its write handle (ETXTBSY when a
    /// node runs it).
    #[test]
    fn the_program_is_not_written_while_a_child_starts() {
        let dir = tempfile::tempdir().expect("dir");
        let path = archive(
            dir.path(),
            &[("speedtest", b"\x7fELF program", tar::EntryType::Regular)],
        );
        let out = dir.path().join("speedtest");
        let into = out.clone();
        sb_cli_core::testing::waits_for_a_fork(move || extract(&path, &into)).expect("extract");
        assert_eq!(std::fs::read(&out).expect("read"), b"\x7fELF program");
    }

    #[test]
    fn extract_refuses_what_sb_go_refuses() {
        let dir = tempfile::tempdir().expect("dir");
        let cases: [(&[Entry<'_>], &str); 4] = [
            (
                &[("speedtest", b"", tar::EntryType::Symlink)],
                "speedtest archive entry is not a regular file",
            ),
            (
                &[("speedtest", b"", tar::EntryType::Regular)],
                "speedtest archive entry has invalid size 0",
            ),
            (
                &[
                    ("speedtest", b"one", tar::EntryType::Regular),
                    ("speedtest", b"two", tar::EntryType::Regular),
                ],
                "archive contains duplicate speedtest entries",
            ),
            (
                &[("bin/speedtest", b"x", tar::EntryType::Regular)],
                "archive does not contain speedtest",
            ),
        ];
        for (entries, expected) in cases {
            let path = archive(dir.path(), entries);
            let out = dir.path().join("speedtest");
            let err = extract(&path, &out).expect_err(expected);
            assert_eq!(err.to_string(), expected);
            assert!(!out.exists(), "{expected}");
        }
        let err = extract(&dir.path().join("none.tgz"), &dir.path().join("x")).expect_err("none");
        assert!(err.to_string().starts_with("read archive: "), "{err}");
        std::fs::write(dir.path().join("plain"), "not gzip").expect("write");
        let err = extract(&dir.path().join("plain"), &dir.path().join("x")).expect_err("plain");
        assert!(err.to_string().starts_with("read archive: "), "{err}");
    }

    /// A gzipped tar whose `speedtest` header claims `size` with no data.
    fn claiming(dir: &Path, size: u64) -> PathBuf {
        let path = dir.join("claiming.tgz");
        let file = std::fs::File::create(&path).expect("create");
        let mut gz = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut header = tar::Header::new_gnu();
        header.set_path("speedtest").expect("path");
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(size);
        header.set_cksum();
        io::Write::write_all(&mut gz, header.as_bytes()).expect("header");
        gz.finish().expect("gz");
        path
    }

    #[test]
    fn an_entry_of_exactly_64_mib_is_taken_and_a_short_one_removed() {
        let dir = tempfile::tempdir().expect("dir");
        let out = dir.path().join("speedtest");
        // Not refused on its size: read, found short, and removed.
        let err = extract(&claiming(dir.path(), MAX_SIZE), &out).expect_err("short");
        assert!(matches!(err, ExtractError::Write(_)), "{err}");
        assert!(!out.exists());
    }

    #[test]
    fn an_entry_over_64_mib_is_refused_before_it_is_read() {
        let dir = tempfile::tempdir().expect("dir");
        // A header claiming 64 MiB + 1 over no data: refused on its size.
        let path = claiming(dir.path(), MAX_SIZE + 1);
        let err = extract(&path, &dir.path().join("speedtest")).expect_err("big");
        assert_eq!(
            err.to_string(),
            format!("speedtest archive entry has invalid size {}", MAX_SIZE + 1)
        );
    }

    /// JSON as the CLI 1.2.0 writes it (docs/platform-notes/speedtest.md),
    /// synthetic values, the address fields included to show they're
    /// never read.
    const RESULT: &str = r#"{"type":"result","ping":{"jitter":0.1,"latency":12.345,"low":12.0,"high":13.0},
        "download":{"bandwidth":112500000,"bytes":1,"elapsed":1},
        "upload":{"bandwidth":100000000,"bytes":1,"elapsed":1},
        "interface":{"internalIp":"INTERNAL","externalIp":"EXTERNAL","macAddr":"MAC"},
        "server":{"id":7190,"host":"HOST","ip":"SERVERIP"}}"#;

    #[test]
    fn parse_shows_mbps_and_ms_with_two_decimals() {
        assert_eq!(
            parse(RESULT),
            Measured::Speeds {
                upload: "800.00 Mbps".to_owned(),
                download: "900.00 Mbps".to_owned(),
                latency: "12.35 ms".to_owned(),
            }
        );
    }

    #[test]
    fn an_answer_without_the_values_is_unparsed() {
        assert_eq!(parse(""), Measured::Unparsed);
        assert_eq!(parse("not json"), Measured::Unparsed);
        assert_eq!(
            parse(r#"{"ping":{"latency":1},"download":{"bandwidth":1}}"#),
            Measured::Unparsed
        );
        assert_eq!(
            parse(
                r#"{"ping":{"latency":"x"},"download":{"bandwidth":1},"upload":{"bandwidth":1}}"#
            ),
            Measured::Unparsed
        );
    }

    /// A stand-in `speedtest` script: prints `stdout`, its arguments and
    /// `HOME` to files in `dir`, exits `code`.
    fn stand_in(dir: &Path, stdout: &str, code: i32, sleep: u32) -> PathBuf {
        let path = dir.join("speedtest");
        let script = format!(
            "#!/bin/sh\necho \"$@\" > '{args}'\necho \"$HOME\" > '{home}'\nsleep {sleep}\n\
             cat <<'EOF'\n{stdout}\nEOF\necho EXTERNAL >&2\nexit {code}\n",
            args = dir.join("args").display(),
            home = dir.join("home").display(),
        );
        sb_cli_core::testing::write_program(&path, script);
        path
    }

    #[tokio::test]
    async fn a_node_runs_with_bench_sh_s_flags_json_and_home() {
        let _spawns = crate::bench::SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let program = stand_in(dir.path(), RESULT, 0, 0);
        let home = dir.path().join("h");
        let measured = run_node(
            &program,
            &home,
            NODES[1],
            NODE_TIMEOUT,
            &CancellationToken::new(),
        )
        .await
        .expect("ran");
        assert!(matches!(measured, Measured::Speeds { .. }), "{measured:?}");
        let args = std::fs::read_to_string(dir.path().join("args")).expect("args");
        assert_eq!(
            args.trim(),
            "--format=json --progress=no --accept-license --accept-gdpr --server-id=7190"
        );
        let seen_home = std::fs::read_to_string(dir.path().join("home")).expect("home");
        assert_eq!(seen_home.trim(), home.display().to_string());
        // Speedtest.net's own choice: no server id.
        run_node(
            &program,
            &home,
            NODES[0],
            NODE_TIMEOUT,
            &CancellationToken::new(),
        )
        .await
        .expect("ran");
        let args = std::fs::read_to_string(dir.path().join("args")).expect("args");
        assert!(!args.contains("--server-id"), "{args}");
    }

    #[tokio::test]
    async fn a_failing_or_silent_node_is_failed_or_unparsed() {
        let _spawns = crate::bench::SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let home = dir.path().to_path_buf();
        let token = CancellationToken::new();
        let failed = stand_in(dir.path(), "", 2, 0);
        assert_eq!(
            run_node(&failed, &home, NODES[1], NODE_TIMEOUT, &token)
                .await
                .expect("ran"),
            Measured::Failed
        );
        let silent = stand_in(dir.path(), "{}", 0, 0);
        assert_eq!(
            run_node(&silent, &home, NODES[1], NODE_TIMEOUT, &token)
                .await
                .expect("ran"),
            Measured::Unparsed
        );
        let slow = stand_in(dir.path(), RESULT, 0, 5);
        assert_eq!(
            run_node(&slow, &home, NODES[1], Duration::from_millis(200), &token)
                .await
                .expect("ran"),
            Measured::Failed
        );
        assert_eq!(
            run_node(
                &dir.path().join("none"),
                &home,
                NODES[1],
                NODE_TIMEOUT,
                &token
            )
            .await
            .expect("ran"),
            Measured::Failed
        );
    }

    /// Speedtest's own error output (docs/platform-notes/speedtest.md,
    /// "Failure": exit 2, nothing on stdout, a JSON log line on stderr),
    /// and the same log object on stdout with an exit of 0: the first is
    /// the row's `Test failed`, the second `Failed to parse results`.
    #[tokio::test]
    async fn speedtest_s_error_object_is_test_failed_or_unparsed() {
        let _spawns = crate::bench::SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let token = CancellationToken::new();
        let error = r#"{"type":"log","timestamp":"2026-11-18T00:00:00Z","message":"Configuration - NO-SUCH-HOST","level":"error"}"#;
        let program = |name: &str, stdout: &str, stderr: &str, code: i32| {
            let path = dir.path().join(name);
            let script = format!(
                "#!/bin/sh\ncat <<'EOF'\n{stdout}\nEOF\ncat >&2 <<'EOF'\n{stderr}\nEOF\nexit {code}\n"
            );
            sb_cli_core::testing::write_program(&path, script);
            path
        };
        let failed = program("failed", "", error, 2);
        let failed = run_node(&failed, dir.path(), NODES[1], NODE_TIMEOUT, &token)
            .await
            .expect("ran");
        assert_eq!(failed, Measured::Failed);
        let logged = program("logged", error, "", 0);
        let logged = run_node(&logged, dir.path(), NODES[2], NODE_TIMEOUT, &token)
            .await
            .expect("ran");
        assert_eq!(logged, Measured::Unparsed);
        let text = rendered(&Outcome::Nodes(vec![
            (NODES[1], failed),
            (NODES[2], logged),
        ]));
        // Each node's row: its name, then the text in the Upload column.
        let row = |name: &str| -> Vec<String> {
            text.lines()
                .find(|line| line.starts_with(name))
                .map_or_default(|line| {
                    line[name.len()..]
                        .split("  ")
                        .map(str::trim)
                        .filter(|cell| !cell.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
        };
        assert_eq!(row(NODES[1].name), ["Test failed"], "{text}");
        assert_eq!(row(NODES[2].name), ["Failed to parse results"], "{text}");
    }

    #[tokio::test]
    async fn an_interrupted_node_is_an_error() {
        let _spawns = crate::bench::SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let slow = stand_in(dir.path(), RESULT, 0, 5);
        let token = CancellationToken::new();
        token.cancel();
        let err = run_node(&slow, dir.path(), NODES[1], NODE_TIMEOUT, &token)
            .await
            .expect_err("interrupted");
        assert_eq!(err.to_string(), "interrupted");
    }

    fn rendered(outcome: &Outcome) -> String {
        table(outcome).render(&Theme::plain(), false, None)
    }

    #[test]
    fn the_table_has_the_ruled_columns_and_bench_sh_s_texts() {
        let text = rendered(&Outcome::Nodes(vec![
            (NODES[0], parse(RESULT)),
            (NODES[1], Measured::Failed),
        ]));
        // Muted headings over right-aligned figures, a failed node's text
        // right-aligned in the Upload column (ops spec, "Layout").
        assert_eq!(
            text,
            "\
Node                         Upload      Download    Latency
Speedtest.net           800.00 Mbps   900.00 Mbps   12.35 ms
Los Angeles, US         Test failed"
        );
        let text = rendered(&Outcome::Nodes(vec![
            (NODES[0], parse(RESULT)),
            (NODES[2], Measured::Unparsed),
        ]));
        // A longer text widens the column, every figure still aligned.
        assert!(
            text.ends_with("\nDallas, US             Failed to parse results"),
            "{text}"
        );
        for never in ["EXTERNAL", "INTERNAL", "MAC", "HOST", "SERVERIP"] {
            assert!(!text.contains(never), "{never}\n{text}");
        }
        let theme = sb_cli_core::theme::Theme::resolve(
            &sb_cli_core::theme::Palette::default(),
            sb_cli_core::theme::Profile::TrueColor,
        );
        let coloured =
            table(&Outcome::Nodes(vec![(NODES[0], parse(RESULT))])).render(&theme, true, None);
        for heading in ["Node", "Upload", "Download", "Latency"] {
            assert!(
                coloured.contains(&sb_term_ui::style::paint(
                    &theme,
                    Role::Muted,
                    true,
                    heading
                )),
                "{heading}: {coloured:?}"
            );
        }
        // The node count in the heading, muted.
        let nodes = Outcome::Nodes(vec![
            (NODES[0], parse(RESULT)),
            (NODES[1], Measured::Failed),
        ]);
        assert_eq!(
            heading(&nodes).render(&Theme::plain(), false, 4),
            "Speedtest  2 nodes\n----\n"
        );
        let text = rendered(&Outcome::Unavailable("digest mismatch".to_owned()));
        assert_eq!(
            text,
            "Skipped              Speedtest unavailable: digest mismatch"
        );
    }
}
