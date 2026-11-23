//! A program bench extracts runs while other threads start children
//! (rust-lang/rust#114554): a child forked while the extraction's write
//! handle is open inherits it until its own exec, and running the
//! program meanwhile fails with ETXTBSY. Alone in its test binary, so
//! every child here starts through sb-exec, as every child of sb does.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sb_cli_core::cancel::CancellationToken;
use sb_exec::{Cmd, SpawnCause, SpawnError};

/// Write-and-run rounds. Unguarded, 31% of runs failed here (625 of
/// 2,000 on four cores); at even the 4% per run measured
/// for its own script, all 500 passing by chance is under 1 in 10^8.
const ROUNDS: usize = 500;

/// Threads starting children all the while, as sb's blocking pool, its
/// probes and its concurrent git phases do.
const SPAWNERS: usize = 3;

/// A gzipped tar holding a `speedtest` that exits 0.
fn archive(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("archive.tgz");
    let file = std::fs::File::create(&path).expect("create");
    let gz = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
    let mut builder = tar::Builder::new(gz);
    let body = b"#!/bin/sh\nexit 0\n";
    let mut header = tar::Header::new_gnu();
    header.set_size(u64::try_from(body.len()).expect("size"));
    header.set_mode(0o755);
    header.set_cksum();
    builder
        .append_data(&mut header, "speedtest", &body[..])
        .expect("append");
    builder.into_inner().expect("tar").finish().expect("gz");
    path
}

/// Whether `err` is the exec's ETXTBSY (std's `ExecutableFileBusy`,
/// <https://doc.rust-lang.org/std/io/enum.ErrorKind.html#variant.ExecutableFileBusy>).
fn text_busy(err: &SpawnError) -> bool {
    matches!(err.cause(), SpawnCause::Io(cause) if cause.kind() == io::ErrorKind::ExecutableFileBusy)
}

/// A runtime of its own for each spawning thread.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

#[test]
fn an_extracted_program_runs_while_other_threads_start_children() {
    let dir = tempfile::tempdir().expect("dir");
    let archive = archive(dir.path());
    let stop = Arc::new(AtomicBool::new(false));
    let spawners: Vec<_> = (0..SPAWNERS)
        .map(|_| {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let token = CancellationToken::new();
                runtime().block_on(async {
                    while !stop.load(Ordering::Relaxed) {
                        let _ = Cmd::new("true").capture(&token).await;
                    }
                });
            })
        })
        .collect();
    let token = CancellationToken::new();
    let busy = runtime().block_on(async {
        let mut busy = 0;
        for round in 0..ROUNDS {
            let program = dir.path().join(format!("speedtest-{round}"));
            sb_ops::bench::speedtest::extract(&archive, &program).expect("extract");
            match Cmd::new(program.as_os_str()).capture(&token).await {
                Ok(ran) => assert!(ran.check().is_ok(), "round {round}"),
                Err(err) if text_busy(&err) => busy += 1,
                Err(err) => panic!("round {round}: {err}: {}", err.cause()),
            }
        }
        busy
    });
    stop.store(true, Ordering::Relaxed);
    for spawner in spawners {
        spawner.join().expect("spawner");
    }
    eprintln!("{busy} of {ROUNDS} runs failed with ETXTBSY");
    assert_eq!(busy, 0, "{busy} of {ROUNDS} runs failed with ETXTBSY");
}
