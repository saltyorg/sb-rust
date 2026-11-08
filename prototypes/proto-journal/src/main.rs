//! proto-journal: can our own pure-Rust reader of systemd's journal files
//! serve `sb logs` exactly as `journalctl -u` does on every supported
//! Ubuntu release? (tasks/prototypes Task 11; host spec decision 4)

mod file;
mod hash;
mod query;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use query::{Group, Journal, Location, Release};
use std::io::{BufRead, BufWriter, Write};
use std::mem::MaybeUninit;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args)]
struct Source {
    /// Read these files, as `journalctl --file`.
    #[arg(long = "file")]
    files: Vec<PathBuf>,
    /// Read this directory, as `journalctl -D`.
    #[arg(long)]
    directory: Option<PathBuf>,
}

impl Source {
    fn external(&self) -> bool {
        !self.files.is_empty() || self.directory.is_some()
    }

    fn paths(&self) -> Result<Vec<PathBuf>> {
        if let Some(d) = &self.directory {
            query::directory_files(d)
        } else if !self.files.is_empty() {
            Ok(self.files.clone())
        } else {
            query::local_files()
        }
    }

    /// The directories to watch while following: those holding the files,
    /// and for the local journal both machine-id directories, or their
    /// parent when one doesn't exist yet (journald creates it on its first
    /// write or flush), so following never waits on no watch at all.
    fn watch_dirs(&self) -> Result<Vec<PathBuf>> {
        let mut dirs: Vec<PathBuf> = self
            .paths()?
            .iter()
            .filter_map(|p| p.parent().map(Path::to_path_buf))
            .collect();
        if let Some(d) = &self.directory {
            dirs.push(d.clone());
        }
        if !self.external() {
            let mid = std::fs::read_to_string("/etc/machine-id")?;
            for root in ["/run/log/journal", "/var/log/journal"] {
                let dir = Path::new(root).join(mid.trim());
                if dir.is_dir() {
                    dirs.push(dir);
                } else if Path::new(root).is_dir() {
                    dirs.push(PathBuf::from(root));
                }
            }
        }
        dirs.sort();
        dirs.dedup();
        Ok(dirs)
    }
}

#[derive(Subcommand)]
enum Command {
    /// The last entries (of a unit), optionally following.
    Tail {
        #[command(flatten)]
        source: Source,
        /// journalctl's `-u`.
        #[arg(long, short)]
        unit: Option<String>,
        /// A FIELD=value match; several are ANDed (the field may repeat to OR).
        #[arg(long = "match")]
        matches: Vec<String>,
        #[arg(short, default_value_t = 500)]
        n: usize,
        /// Only entries before this cursor (a page back).
        #[arg(long)]
        before: Option<String>,
        #[arg(long, short)]
        follow: bool,
        /// The release whose `-u` rule applies; default: this host's.
        #[arg(long)]
        release: Option<String>,
        /// Print timing to stderr.
        #[arg(long)]
        timing: bool,
    },
    /// Compare two `journalctl -o json` streams field by field, bytes
    /// exact; exits 1 on any difference.
    Compare { expected: PathBuf, actual: PathBuf },
    /// Recompute every DATA object's hash and compare with the stored one.
    HashCheck {
        #[command(flatten)]
        source: Source,
    },
    /// Decode every entry of every file, counting errors.
    DecodeAll {
        #[command(flatten)]
        source: Source,
    },
    /// The supported releases, one per line: VERSION_ID and the systemd
    /// major version (the matrix's default list).
    Releases,
    /// Write test entries in journal export format
    /// (https://systemd.io/JOURNAL_EXPORT_FORMATS/) for
    /// systemd-journal-remote: every `-u` branch for `demo.service` with
    /// decoys, ANSI and binary values, large (compressed) and repeated
    /// fields.
    GenExport {
        #[arg(long, default_value_t = 3000)]
        entries: u64,
        /// Another set of the same entries, later and on other boots: 0 is
        /// the matrix's fixtures; 1 and up are what a host writes after an
        /// upgrade, to merge with files from before it (compat.sh).
        #[arg(long, default_value_t = 0)]
        generation: u8,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Tail {
            source,
            unit,
            matches,
            n,
            before,
            follow,
            release,
            timing,
        } => {
            let release = match release {
                Some(r) => {
                    Release::parse(&r).with_context(|| format!("unsupported release {r}"))?
                }
                None => host_release()?,
            };
            let filter = filter(release, unit.as_deref(), &matches, source.external())?;
            tail(
                &source,
                &filter,
                n,
                before.as_deref(),
                follow,
                release,
                timing,
            )
        }
        Command::Compare { expected, actual } => compare(&expected, &actual),
        Command::HashCheck { source } => hash_check(&source),
        Command::DecodeAll { source } => decode_all(&source),
        Command::Releases => {
            for r in query::RELEASES {
                println!("{} {}", r.version_id, r.systemd);
            }
            Ok(())
        }
        Command::GenExport {
            entries,
            generation,
        } => gen_export(entries, generation),
    }
}

/// This host's release: `VERSION_ID` through sysinfo, as sb-host's
/// supported-release check reads it
/// (https://docs.rs/sysinfo/0.40.0/sysinfo/struct.System.html#method.os_version).
/// In a crate this calls sb-host instead.
fn host_release() -> Result<Release> {
    let version = sysinfo::System::os_version().context("no VERSION_ID in /etc/os-release")?;
    Release::parse(&version).with_context(|| format!("unsupported Ubuntu release {version}"))
}

fn filter(
    release: Release,
    unit: Option<&str>,
    matches: &[String],
    external: bool,
) -> Result<Vec<Group>> {
    if let Some(u) = unit {
        let u = if u.contains('.') {
            u.to_string()
        } else {
            format!("{u}.service")
        };
        return Ok(query::unit_filter(release, &u, external));
    }
    if matches.is_empty() {
        return Ok(Vec::new());
    }
    // One group; matches on the same field OR together, as sd-journal does.
    let mut by_field: Vec<(String, Vec<Vec<u8>>)> = Vec::new();
    for m in matches {
        // journalctl refuses a match without `=`; it would match nothing.
        let Some((field, _)) = m.split_once('=') else {
            bail!("invalid match {m:?}: expected FIELD=value");
        };
        match by_field.iter_mut().find(|(f, _)| f == field) {
            Some((_, v)) => v.push(m.clone().into_bytes()),
            None => by_field.push((field.to_string(), vec![m.clone().into_bytes()])),
        }
    }
    Ok(vec![by_field.into_iter().map(|(_, v)| v).collect()])
}

fn tail(
    source: &Source,
    filter: &[Group],
    n: usize,
    before: Option<&str>,
    follow: bool,
    release: Release,
    timing: bool,
) -> Result<()> {
    let started = Instant::now();
    let before = before
        .map(|c| Location::parse_cursor(c).context("bad cursor"))
        .transpose()?;
    // Follow: one inotify fd on the journal directories; journald truncates
    // the file to its own size after writing, which raises IN_MODIFY
    // (JOURNAL_FILE_FORMAT, "Concurrency"). Rotation renames the file and
    // creates a new one: the file set is re-read on every wake. The
    // watches go on before the first read, so nothing written in between
    // waits for a later event.
    // https://docs.rs/rustix/1.1.6/rustix/fs/inotify/index.html
    use rustix::fs::inotify;
    let fd = if follow {
        let fd = inotify::init(inotify::CreateFlags::CLOEXEC)?;
        watch(&fd, source)?;
        Some(fd)
    } else {
        None
    };

    let mut journal = Journal::open(&source.paths()?);
    let records = journal.tail(filter, n, before.as_ref());
    report(&mut journal);
    let mut out = BufWriter::new(std::io::stdout().lock());
    for r in &records {
        writeln!(out, "{}", query::to_json(r, release))?;
    }
    out.flush()?;
    if timing {
        eprintln!(
            "proto-journal: {} entries in {:?}",
            records.len(),
            started.elapsed()
        );
    }
    let Some(fd) = fd else { return Ok(()) };
    // Follow from the newest match even when none was printed (`-n 0`).
    let mut last = match records.last() {
        Some(r) => Some(r.location.clone()),
        None => journal.tail(filter, 1, None).pop().map(|r| r.location),
    };

    let mut buf = [const { MaybeUninit::<u8>::uninit() }; 8192];
    let mut reader = inotify::Reader::new(&fd, &mut buf);
    loop {
        // Block for one event, then drain what is already buffered, so a
        // burst of writes costs one reread, not one per event
        // (Reader::is_buffer_empty, "useful to avoid further blocking reads").
        reader.next()?;
        while !reader.is_buffer_empty() {
            reader.next()?;
        }
        // A directory may have appeared (journald creates the machine-id
        // directory on its first write or flush): watch it from now on.
        // Adding a watch twice returns the same one.
        watch(&fd, source)?;
        journal = Journal::open(&source.paths()?);
        let fresh = match &last {
            Some(l) => journal.newer(filter, l),
            None => journal.tail(filter, usize::MAX, None),
        };
        report(&mut journal);
        let mut out = std::io::stdout().lock();
        for r in &fresh {
            writeln!(out, "{}", query::to_json(r, release))?;
        }
        out.flush()?;
        if let Some(r) = fresh.last() {
            last = Some(r.location.clone());
        }
    }
}

/// Watch every directory the source reads from now (see
/// `Source::watch_dirs`).
fn watch(fd: &impl std::os::fd::AsFd, source: &Source) -> Result<()> {
    use rustix::fs::inotify::{WatchFlags, add_watch};
    for d in source.watch_dirs()? {
        let watched = add_watch(
            fd,
            &d,
            WatchFlags::MODIFY
                | WatchFlags::CREATE
                | WatchFlags::MOVED_TO
                | WatchFlags::MOVED_FROM
                | WatchFlags::DELETE
                | WatchFlags::CLOSE_WRITE,
        );
        match watched {
            // Gone between listing and watching (the runtime journal after
            // a flush): its parent's watch stays, so following goes on.
            Err(rustix::io::Errno::NOENT) => {}
            other => {
                other.with_context(|| format!("watch {}", d.display()))?;
            }
        }
    }
    Ok(())
}

/// Print and clear the errors gathered from files and entries that
/// couldn't be read; the rest of the output stands.
fn report(journal: &mut Journal) {
    for e in journal.errors.drain(..) {
        eprintln!("proto-journal: {e}");
    }
}

/// Each JSON line as a field map. serde_json's map is ordered by key, so
/// field order doesn't count, while a value's shape (string or byte
/// array) and the order of a repeated field's values do.
fn read_jsonl(path: &Path) -> Result<Vec<serde_json::Map<String, serde_json::Value>>> {
    let f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut out = Vec::new();
    for line in std::io::BufReader::new(f).lines() {
        let line = line?;
        if !line.starts_with('{') {
            continue;
        }
        let serde_json::Value::Object(m) = serde_json::from_str(&line)? else {
            bail!("not an object")
        };
        out.push(m);
    }
    Ok(out)
}

/// Field by field, values exact as JSON. A different entry count fails
/// at once: zipping two streams of different lengths would misalign them.
fn compare(expected: &Path, actual: &Path) -> Result<()> {
    let want = read_jsonl(expected)?;
    let got = read_jsonl(actual)?;
    if want.len() != got.len() {
        println!(
            "count differs: expected {} entries, got {}",
            want.len(),
            got.len()
        );
        std::process::exit(1);
    }
    let mut mismatches = 0usize;
    for (i, (w, g)) in want.iter().zip(&got).enumerate() {
        let mut keys: Vec<&String> = w.keys().chain(g.keys()).collect();
        keys.sort();
        keys.dedup();
        for k in keys {
            if w.get(k) != g.get(k) {
                mismatches += 1;
                if mismatches <= 20 {
                    println!("entry {i}: field {k} differs");
                }
            }
        }
    }
    println!("compared {} entries: {mismatches} mismatches", want.len());
    if mismatches > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn hash_check(source: &Source) -> Result<()> {
    let journal = Journal::open(&source.paths()?);
    for e in &journal.errors {
        println!("error: {e}");
    }
    let (mut ok, mut bad) = (0u64, 0u64);
    for f in &journal.files {
        let mut file_bad = 0u64;
        for (off, stored) in f.all_data()? {
            if f.hash(&f.data_payload(off)?) == stored {
                ok += 1;
            } else {
                file_bad += 1;
            }
        }
        bad += file_bad;
        println!(
            "{}: flags {:#x}, header {} bytes, keyed {}, {} bad hashes",
            f.path.file_name().unwrap_or_default().to_string_lossy(),
            f.header.incompatible,
            f.header.header_size,
            f.header.keyed(),
            file_bad
        );
    }
    println!("hash-check: {ok} match, {bad} differ");
    if bad > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn decode_all(source: &Source) -> Result<()> {
    let started = Instant::now();
    let journal = Journal::open(&source.paths()?);
    let mut errors = journal.errors.len();
    for e in &journal.errors {
        println!("open error: {e}");
    }
    let mut entries = 0u64;
    for f in &journal.files {
        for off in f.all_entries()? {
            match f.entry(off) {
                Ok(e) => {
                    entries += 1;
                    for item in e.items {
                        if let Err(err) = f.data_payload(item) {
                            errors += 1;
                            if errors <= 10 {
                                println!("data error: {err:#}");
                            }
                        }
                    }
                }
                Err(err) => {
                    errors += 1;
                    if errors <= 10 {
                        println!("entry error: {err:#}");
                    }
                }
            }
        }
    }
    println!(
        "decode-all: {} files, {entries} entries, {errors} errors, {:?}",
        journal.files.len(),
        started.elapsed()
    );
    Ok(())
}

/// One export-format field: text as `NAME=value`, anything with a newline
/// or a byte that isn't printable as `NAME`, newline, little-endian u64
/// length, the bytes, newline.
fn export_field(out: &mut impl Write, name: &str, value: &[u8]) -> std::io::Result<()> {
    if value.contains(&b'\n') || !query::printable(value) {
        out.write_all(name.as_bytes())?;
        out.write_all(b"\n")?;
        out.write_all(&(value.len() as u64).to_le_bytes())?;
        out.write_all(value)?;
        out.write_all(b"\n")
    } else {
        out.write_all(name.as_bytes())?;
        out.write_all(b"=")?;
        out.write_all(value)?;
        out.write_all(b"\n")
    }
}

fn gen_export(entries: u64, generation: u8) -> Result<()> {
    let mut out = BufWriter::new(std::io::stdout().lock());
    // Synthetic boot ids, their first byte XORed with the generation (so
    // generation 0 keeps the matrix's ids); the coredump id is systemd's
    // SD_MESSAGE_COREDUMP.
    let boots = [
        format!("{:02x}112233445566778899aabbccddeeff", generation),
        format!("{:02x}eeddccbbaa99887766554433221100", 0xff ^ generation),
    ];
    // Each generation starts 10^6 s (about 11.6 days) after the last.
    let base = 1_700_000_000_000_000 + u64::from(generation) * 1_000_000_000_000;
    let coredump = "fc2e22bc6ee647b6b90729ab34a250b1";
    // A fixed-seed xorshift: the same fixtures on every run and release.
    let mut rng: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    for i in 0..entries {
        let boot = &boots[usize::from(i >= entries / 2)];
        let mono = 1_000_000 + i * 1000;
        let f = &mut out;
        export_field(
            f,
            "__REALTIME_TIMESTAMP",
            (base + i * 1_000_000).to_string().as_bytes(),
        )?;
        export_field(f, "__MONOTONIC_TIMESTAMP", mono.to_string().as_bytes())?;
        export_field(f, "_BOOT_ID", boot.as_bytes())?;
        export_field(f, "_TRANSPORT", b"journal")?;
        let message = format!("entry {i}");
        // (unit fields, pid, uid, identifier) per branch; decoys don't match
        // `-u demo.service` on any release, or only on some (noted).
        let (fields, pid, uid): (Vec<(&str, String)>, &str, &str) = match i % 12 {
            0..=2 => (
                vec![("_SYSTEMD_UNIT", "demo.service".into())],
                "4242",
                "1000",
            ),
            3 => (vec![("UNIT", "demo.service".into())], "1", "0"),
            4 => (vec![("UNIT", "demo.service".into())], "77", "0"), // decoy: not PID 1
            5 => (
                vec![("OBJECT_SYSTEMD_UNIT", "demo.service".into())],
                "99",
                "0",
            ),
            6 => (
                vec![("OBJECT_SYSTEMD_UNIT", "demo.service".into())],
                "99",
                "1000",
            ), // decoy
            7 => (
                vec![
                    ("MESSAGE_ID", coredump.into()),
                    ("COREDUMP_UNIT", "demo.service".into()),
                ],
                "500",
                "0",
            ),
            8 => (
                // 249/255: decoy (uid 1000); 259 with --file: a match
                vec![
                    ("MESSAGE_ID", coredump.into()),
                    ("COREDUMP_UNIT", "demo.service".into()),
                ],
                "501",
                "1000",
            ),
            9 if i % 24 == 9 => (
                // 259.1+: a match (init.scope helper); 249/255: a decoy (not PID 1)
                vec![
                    ("UNIT", "demo.service".into()),
                    ("_SYSTEMD_CGROUP", "/init.scope".into()),
                ],
                "88",
                "0",
            ),
            9 => (vec![("COREDUMP_UNIT", "demo.service".into())], "502", "0"), // decoy: no MESSAGE_ID
            10 => (
                vec![("_SYSTEMD_UNIT", "other.service".into())],
                "4343",
                "1000",
            ),
            _ => (
                vec![("_SYSTEMD_UNIT", "demodemo.service".into())],
                "4444",
                "1000",
            ), // decoy: prefix
        };
        for (k, v) in &fields {
            export_field(f, k, v.as_bytes())?;
        }
        export_field(f, "_PID", pid.as_bytes())?;
        export_field(f, "_UID", uid.as_bytes())?;
        export_field(f, "SYSLOG_IDENTIFIER", b"sb-eval")?;
        export_field(f, "PRIORITY", (i % 8).to_string().as_bytes())?;
        match i % 7 {
            0 => export_field(
                f,
                "MESSAGE",
                format!("\x1b[31m{message} red\x1b[0m").as_bytes(),
            )?,
            1 => export_field(f, "MESSAGE", &[0xff, 0xfe, b'b', b'i', b'n', 0x00, 0x01])?,
            2 => {
                // Large and repetitive: compressed by journald (> 512 bytes).
                export_field(
                    f,
                    "MESSAGE",
                    format!("{message} {}", "compress me ".repeat(900)).as_bytes(),
                )?;
            }
            3 => {
                // Large and random: stored as written or compressed, per writer.
                let noise: String = (0..6000)
                    .map(|_| char::from(b'a' + (next() % 26) as u8))
                    .collect();
                export_field(f, "MESSAGE", format!("{message} {noise}").as_bytes())?;
            }
            4 => {
                export_field(
                    f,
                    "MESSAGE",
                    format!("{message}\nsecond line\tTab").as_bytes(),
                )?;
                export_field(f, "EXTRA", b"one")?;
                export_field(f, "EXTRA", b"two")?;
                // The same FIELD=value twice: systemd 249 links the entry
                // twice in the value's entry array (file.rs `chain`).
                export_field(f, "SAME", b"again")?;
                export_field(f, "SAME", b"again")?;
            }
            5 => export_field(
                f,
                "MESSAGE",
                "unicode \u{00e6}\u{00f8}\u{00e5} \u{1f980} \u{0085}c1".as_bytes(),
            )?,
            _ => export_field(f, "MESSAGE", message.as_bytes())?,
        }
        out.write_all(b"\n")?;
    }
    out.flush()?;
    Ok(())
}
