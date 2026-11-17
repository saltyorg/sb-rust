//! `sb bench` end to end (ops spec, Testing Strategy, "bench"): the built
//! binary in a private mount namespace whose working directory is a small
//! tmpfs, so the disk test finds under 2 GiB free and writes nothing on
//! the host. Root runs (`SB_TEST_ROOT=1`): the namespace and the command
//! need root. The help text needs neither.

mod common;
mod root_gate;

use std::process::Output;

/// `sb bench` with a 16 MiB tmpfs as its working directory, in a mount
/// namespace of its own.
fn bench_on_small_tmpfs() -> Output {
    let dir = tempfile::tempdir().expect("dir");
    let script = r#"mount -t tmpfs -o size=16m sb-bench "$1" && cd "$1" && shift && exec "$@""#;
    let mut cmd = std::process::Command::new("unshare");
    common::apply_sb_env(&mut cmd);
    cmd.args([
        "--mount",
        "--propagation",
        "private",
        "sh",
        "-c",
        script,
        "sh",
    ])
    .arg(dir.path())
    .arg(common::sb().get_program())
    .arg("bench");
    cmd.output().expect("run unshare")
}

#[test]
fn bench_prints_the_system_and_skips_the_disk_test_without_space() {
    if !root_gate::requested() {
        return;
    }
    let out = bench_on_small_tmpfs();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stdout}\n{stderr}");
    for row in [
        "System",
        "CPU Model",
        "CPU Cores",
        "AES-NI",
        "Total Disk",
        "Total RAM",
        "System Uptime",
        "Load Average",
        "OS",
        "Arch",
        "Kernel",
        "TCP Congestion Ctrl",
        "Virtualization",
        "Disk write",
        "Not enough space for the disk test",
        "Finished in: ",
        "Timestamp:   ",
    ] {
        assert!(stdout.contains(row), "{row}\n{stdout}");
    }
    // The steps are transient: a run that succeeds leaves no progress.
    assert!(stderr.is_empty(), "{stderr}");
}

#[test]
fn bench_has_its_own_help() {
    let out = common::sb()
        .args(["bench", "--help"])
        .output()
        .expect("run sb");
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Benchmarks this server's system, disk and network"),
        "{stdout}"
    );
}
