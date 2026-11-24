//! `sb bench` end to end (ops spec, Testing Strategy, "bench"): the built
//! binary in a private mount namespace whose working directory is a small
//! tmpfs, so the disk test finds under 2 GiB free and writes nothing on
//! the host, against the network fakes (sb-ops' `NetworkFake` for
//! icanhazip and ipinfo, sb-release's TLS fake serving the Speedtest
//! archive) or none, so no run reaches a real service. Root runs
//! (`SB_TEST_ROOT=1`): the namespace and the command need root. The help
//! text needs neither.

mod common;
mod root_gate;

use std::ffi::OsString;
use std::path::Path;
use std::process::Output;

/// `sb bench` with a 16 MiB tmpfs as its working directory, in a mount
/// namespace of its own, with `envs` set.
fn bench_on_small_tmpfs(envs: &[(&str, OsString)]) -> Output {
    let dir = tempfile::tempdir().expect("dir");
    let script = r#"mount -t tmpfs -o size=16m sb-bench "$1" && cd "$1" && shift && exec "$@""#;
    let mut cmd = std::process::Command::new("unshare");
    common::apply_sb_env(&mut cmd);
    // The CA store as the test sets it, never the caller's directories
    // (rustls-native-certs loads those as well).
    cmd.env_remove("SSL_CERT_DIR");
    for (name, value) in envs {
        cmd.env(name, value);
    }
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
    let out = bench_on_small_tmpfs(&[]);
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
        // A test build without the fakes asks nothing.
        "✗ Offline / ✗ Offline",
        "No ISP detected",
        "Disk write",
        "Not enough space for the disk test",
        // Its whole reason wraps (piped_the_tables_wrap_at_80_columns).
        "Speedtest unavailable: a testing build needs",
        "\nFinished in          ",
        "\nTimestamp            ",
    ] {
        assert!(stdout.contains(row), "{row}\n{stdout}");
    }
    // The steps are transient: a run that succeeds leaves no progress.
    assert!(stderr.is_empty(), "{stderr}");
}

/// The Speedtest section in `stdout`: from its heading to the blank line
/// before the footer's rule and `Finished in`.
fn speedtest_table(stdout: &str) -> String {
    let lines: Vec<&str> = stdout.lines().collect();
    let end = lines
        .iter()
        .position(|line| line.starts_with("Finished in "))
        .expect("finished line");
    let heading = lines[..end]
        .iter()
        .rposition(|line| line.starts_with("Speedtest") && !line.starts_with("Speedtest."))
        .expect("speedtest heading");
    lines[heading..end - 2].join("\n")
}

/// Piped, every section lays out as at 80 columns, its cells wrapped as
/// on a terminal (ops spec, "`sb bench`", Results; the maintainer,
/// 2026-11-18): the Speedtest row's long reason wraps in its column
/// instead of drawing the line about 110 columns wide, and the rules are
/// 80 wide.
#[test]
fn piped_the_tables_wrap_at_80_columns() {
    if !root_gate::requested() {
        return;
    }
    let out = bench_on_small_tmpfs(&[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    for line in stdout.lines() {
        assert!(console::measure_text_width(line) <= 80, "{line}\n{stdout}");
    }
    insta::assert_snapshot!("bench_piped_speedtest", speedtest_table(&stdout));
}

/// A gzipped tar holding `program` as `speedtest`, served by `fake`, and
/// its pin.
fn serve_speedtest(
    fake: &sb_release::testing::Fake,
    dir: &Path,
    program: &[u8],
) -> sb_ops::bench::speedtest::Pin {
    let path = dir.join("speedtest.tgz");
    let file = std::fs::File::create(&path).expect("create");
    let gz = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
    let mut builder = tar::Builder::new(gz);
    let mut header = tar::Header::new_gnu();
    header.set_size(u64::try_from(program.len()).expect("size"));
    header.set_mode(0o755);
    header.set_cksum();
    builder
        .append_data(&mut header, "speedtest", program)
        .expect("append");
    builder.into_inner().expect("tar").finish().expect("gz");
    let bytes = std::fs::read(&path).expect("read");
    fake.script(
        "/app/speedtest.tgz",
        sb_release::testing::Answer::new(200, bytes.clone()),
    );
    sb_ops::bench::speedtest::Pin {
        url: format!("{}/app/speedtest.tgz", fake.url()),
        size: u64::try_from(bytes.len()).expect("size"),
        sha256: hex::encode(sb_release::sha256_file(&path).expect("digest")),
    }
}

#[test]
fn bench_asks_the_fakes_and_keeps_the_addresses_out() {
    if !root_gate::requested() {
        return;
    }
    let dir = tempfile::tempdir().expect("dir");
    let network = sb_ops::testing::NetworkFake::start(
        "ADDRESS-ONE\n",
        r#"{"ip":"ADDRESS-TWO","hostname":"HOSTNAME","org":"AS1 Example","city":"Falkenstein","country":"DE","region":"Saxony","loc":"LOCATION"}"#,
    )
    .expect("network fake");
    let release = sb_release::testing::Fake::start().expect("release fake");
    // The stand-in sb: an ELF for this machine, answering no JSON.
    let pin = serve_speedtest(
        &release,
        dir.path(),
        &sb_release::testing::stand_in("1.0.0"),
    );
    let network_file = dir.path().join("network.json");
    network
        .write_network(pin, &network_file)
        .expect("network file");
    let bases = dir.path().join("bases.json");
    release.write_bases(&bases).expect("bases");
    let logs = dir.path().join("logs");
    let out = bench_on_small_tmpfs(&[
        ("SB_BENCH_TEST_NETWORK", network_file.into_os_string()),
        ("SB_RELEASE_TEST_BASES", bases.into_os_string()),
        ("SB_LOG_DIR", logs.clone().into_os_string()),
    ]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stdout}\n{stderr}");
    for row in [
        "✓ Online / ✓ Online",
        "\nOrganization         AS1 Example\n",
        "\nLocation             Falkenstein / DE\n",
        "\nRegion               Saxony\n",
        // The headings, the Upload column widened by the failure text.
        "\nNode  ",
        "Upload      Download    Latency\n",
        "Speedtest.net",
        "Tokyo, JP",
    ] {
        assert!(stdout.contains(row), "{row}\n{stdout}");
    }
    // Every node failed: bench's eleven (ops decisions 48 and 50).
    assert_eq!(
        stdout.matches("Failed to parse results").count(),
        11,
        "{stdout}"
    );
    assert!(!stdout.contains("No ISP detected"), "{stdout}");
    // The addresses, host name and location reach neither the output nor
    // the run log.
    let mut logged = String::new();
    for entry in walk(&logs) {
        logged.push_str(&String::from_utf8_lossy(
            &std::fs::read(entry).expect("log"),
        ));
    }
    assert!(!logged.is_empty(), "no run log");
    for never in ["ADDRESS-ONE", "ADDRESS-TWO", "HOSTNAME", "LOCATION"] {
        assert!(!stdout.contains(never), "{never}\n{stdout}");
        assert!(!stderr.contains(never), "{never}\n{stderr}");
        assert!(!logged.contains(never), "{never} logged");
    }
    // One download of the archive; each family asked icanhazip once.
    assert_eq!(release.seen().len(), 1);
    let seen = network.seen();
    assert_eq!(
        seen.iter().filter(|seen| seen.path == "/").count(),
        2,
        "{seen:?}"
    );
    assert!(seen.iter().all(|seen| {
        seen.user_agent
            .as_deref()
            .is_some_and(|ua| ua.starts_with("sb/"))
    }));
}

/// Without the system's CA certificates (`SSL_CERT_FILE` an empty file,
/// which rustls-native-certs reads instead of the system's store) the
/// network rows are one Problem row with the fix, not `✗ Offline` and
/// `No ISP detected`, and no request is made (ops spec, decision 44).
#[test]
fn without_ca_certificates_bench_says_so() {
    if !root_gate::requested() {
        return;
    }
    let dir = tempfile::tempdir().expect("dir");
    let network = sb_ops::testing::NetworkFake::start("ADDRESS\n", r#"{"org":"AS1 Example"}"#)
        .expect("network fake");
    let release = sb_release::testing::Fake::start().expect("release fake");
    let pin = serve_speedtest(
        &release,
        dir.path(),
        &sb_release::testing::stand_in("1.0.0"),
    );
    let network_file = dir.path().join("network.json");
    network
        .write_network(pin, &network_file)
        .expect("network file");
    let empty = dir.path().join("empty.pem");
    std::fs::write(&empty, "").expect("empty bundle");
    let out = bench_on_small_tmpfs(&[
        ("SB_BENCH_TEST_NETWORK", network_file.into_os_string()),
        ("SSL_CERT_FILE", empty.into_os_string()),
    ]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    // The row wraps in its column, piped at 80: its words, in order.
    let words = stdout.split_whitespace().collect::<Vec<_>>().join(" ");
    // The last System row, then Disk write's heading.
    let row = format!(
        "IPv4/IPv6 Not checked: {} Disk write",
        sb_cli_core::tls::MISSING_CA
    );
    assert!(words.contains(&row), "{stdout}");
    for never in ["Offline", "No ISP detected", "Organization"] {
        assert!(!stdout.contains(never), "{never}\n{stdout}");
    }
    assert!(network.seen().is_empty(), "{:?}", network.seen());
}

/// Every file under `dir`.
fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).expect("read dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
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
