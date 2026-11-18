//! Verified downloads and activation on the fake TLS server (release
//! spec, "Download", "Activation", "Testing strategy"): what fails leaves
//! nothing staged and the target as it was. Nothing here reaches the
//! network.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use indicatif::InMemoryTerm;
use sb_cli_core::cancel::Shutdown;
use sb_release::testing::{Answer, Fake};
use sb_release::{Error, GithubRepo, Query, Releases, Stage, Staged, activate};
use sb_term_ui::{Labels, Options, Runner, Step, Target};
use sha2::{Digest, Sha256};

const SB: GithubRepo = GithubRepo {
    owner: "saltyorg",
    name: "sb-rust",
};
const ASSET: &str = "sb_linux_amd64";
const AGENT: &str = "sb/0.0.0-test";
const TAG: &str = "v1.2.3";
const PATH: &str = "/saltyorg/sb-rust/releases/download/v1.2.3/sb_linux_amd64";
const PREFIX: &str = ".sb-update-";

fn sha256_hex(body: &[u8]) -> String {
    hex::encode(Sha256::digest(body))
}

/// The fake's proxy describing `ASSET` with `size` and `digest`.
fn describe(fake: &Fake, size: usize, digest: &str) {
    fake.script(
        "/version",
        Answer::json(format!(
            r#"{{"tag_name":"{TAG}","assets":[{{"name":"{ASSET}","size":{size},"digest":"sha256:{digest}","browser_download_url":"{}{PATH}"}}]}}"#,
            fake.url()
        )),
    );
}

/// `body` run in one step on an in-memory screen.
async fn in_step<T>(body: impl AsyncFnOnce(&mut Step) -> T) -> T {
    let term = InMemoryTerm::new(40, 160);
    let options = Options {
        target: Target::InMemory {
            term: term.clone(),
            colour: false,
        },
    };
    let shutdown = Shutdown::new();
    let mut run = Runner::start("sb test", &shutdown, None, options);
    let mut result = None;
    let _ = run
        .step(Labels::new("Downloading", "downloaded"), async |step| {
            result = Some(body(step).await);
            Ok(())
        })
        .await;
    run.finish();
    result.expect("the step ran")
}

/// Look the asset up, then download it into `dir`.
async fn fetch(releases: &Releases, dir: &Path, max_size: u64) -> Result<Staged, Error> {
    in_step(async |step| {
        let release = releases.release(SB, Query::Latest, ASSET, step).await?;
        let stage = Stage {
            dir,
            prefix: PREFIX,
            max_size,
        };
        releases.download(&release.asset, stage, step).await
    })
    .await
}

/// The files in `dir`, by name.
fn files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("read dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn a_verified_download_is_staged_then_activated() {
    let fake = Fake::start().expect("fake");
    let body = b"#!/bin/sh\necho new\n".to_vec();
    describe(&fake, body.len(), &sha256_hex(&body));
    fake.script(PATH, Answer::new(200, body.clone()));
    let dir = tempfile::tempdir().expect("dir");
    let target = dir.path().join("sb");
    std::fs::write(&target, b"old").expect("old sb");
    let releases = fake.releases(AGENT).expect("client");
    let staged = fetch(&releases, dir.path(), 1 << 20).await.expect("staged");
    assert_eq!(staged.path().parent(), Some(dir.path()));
    let name = staged.path().file_name().expect("name").to_string_lossy();
    assert!(name.starts_with(PREFIX), "{name}");
    assert_eq!(std::fs::read(staged.path()).expect("staged"), body);
    activate(staged, &target, 0o755).expect("activated");
    assert_eq!(std::fs::read(&target).expect("target"), body);
    let mode = std::fs::metadata(&target)
        .expect("meta")
        .permissions()
        .mode();
    assert_eq!(mode & 0o7777, 0o755);
    assert_eq!(files(dir.path()), ["sb"]);
    // The download asked for the pinned URL, with the User-Agent.
    let download = fake
        .seen()
        .into_iter()
        .find(|s| s.target == PATH)
        .expect("asked");
    assert_eq!(download.header("user-agent"), Some(AGENT));
}

/// A staged download dropped without activation is removed.
#[tokio::test]
async fn a_dropped_download_leaves_nothing() {
    let fake = Fake::start().expect("fake");
    let body = vec![7u8; 1000];
    describe(&fake, body.len(), &sha256_hex(&body));
    fake.script(PATH, Answer::new(200, body));
    let dir = tempfile::tempdir().expect("dir");
    let releases = fake.releases(AGENT).expect("client");
    let staged = fetch(&releases, dir.path(), 1 << 20).await.expect("staged");
    assert_eq!(files(dir.path()).len(), 1);
    drop(staged);
    assert_eq!(files(dir.path()), Vec::<String>::new());
}

/// A wrong digest, a longer body and a shorter one each fail, leaving
/// nothing staged.
#[tokio::test]
async fn an_unverified_body_leaves_nothing_staged() {
    let body = vec![1u8; 4096];
    let digest = sha256_hex(&body);
    for (served, expected) in [
        (
            vec![2u8; 4096],
            "release asset \"sb_linux_amd64\" digest mismatch".to_owned(),
        ),
        (
            vec![1u8; 4097],
            "release asset \"sb_linux_amd64\" is larger than expected: more than 4096 bytes"
                .to_owned(),
        ),
        (
            vec![1u8; 4000],
            "release asset \"sb_linux_amd64\" size mismatch: expected 4096 bytes, got 4000"
                .to_owned(),
        ),
    ] {
        let fake = Fake::start().expect("fake");
        describe(&fake, body.len(), &digest);
        fake.script(PATH, Answer::new(200, served));
        let dir = tempfile::tempdir().expect("dir");
        let releases = fake.releases(AGENT).expect("client");
        let err = fetch(&releases, dir.path(), 1 << 20)
            .await
            .expect_err("refused");
        assert_eq!(err.to_string(), expected);
        assert_eq!(files(dir.path()), Vec::<String>::new());
    }
}

/// A body larger than the asset says stops being read at the size: the
/// rest of a large body is never written.
#[tokio::test]
async fn an_oversize_body_stops_at_the_size() {
    let fake = Fake::start().expect("fake");
    let body = vec![1u8; 100];
    describe(&fake, body.len(), &sha256_hex(&body));
    fake.script(PATH, Answer::new(200, vec![1u8; 8 << 20]));
    let dir = tempfile::tempdir().expect("dir");
    let releases = fake.releases(AGENT).expect("client");
    let err = fetch(&releases, dir.path(), 1 << 30)
        .await
        .expect_err("refused");
    assert!(err.to_string().contains("is larger than expected"), "{err}");
    assert_eq!(files(dir.path()), Vec::<String>::new());
}

/// An asset larger than the caller's cap isn't asked for at all.
#[tokio::test]
async fn an_asset_over_the_cap_is_not_downloaded() {
    let fake = Fake::start().expect("fake");
    describe(&fake, 5000, &sha256_hex(b""));
    let dir = tempfile::tempdir().expect("dir");
    let releases = fake.releases(AGENT).expect("client");
    let err = fetch(&releases, dir.path(), 4096)
        .await
        .expect_err("refused");
    assert_eq!(
        err.to_string(),
        "release asset \"sb_linux_amd64\" exceeds the 4096 byte limit"
    );
    assert!(!fake.seen().iter().any(|s| s.target == PATH));
}

#[tokio::test]
async fn a_failed_request_is_named() {
    let fake = Fake::start().expect("fake");
    describe(&fake, 10, &sha256_hex(b"0123456789"));
    let dir = tempfile::tempdir().expect("dir");
    let releases = fake.releases(AGENT).expect("client");
    let err = fetch(&releases, dir.path(), 4096).await.expect_err("404");
    assert_eq!(
        err.to_string(),
        "download release asset \"sb_linux_amd64\": unexpected HTTP status 404 Not Found"
    );
    assert_eq!(files(dir.path()), Vec::<String>::new());
}

/// github.com redirects downloads to its asset host: an https redirect is
/// followed, one to plain http refused.
#[tokio::test]
async fn redirects_are_followed_over_https_only() {
    let fake = Fake::start().expect("fake");
    let body = b"payload".to_vec();
    describe(&fake, body.len(), &sha256_hex(&body));
    fake.script(
        PATH,
        Answer::new(302, "").header("location", &format!("{}/objects/1", fake.url())),
    );
    fake.script("/objects/1", Answer::new(200, body.clone()));
    let dir = tempfile::tempdir().expect("dir");
    let releases = fake.releases(AGENT).expect("client");
    let staged = fetch(&releases, dir.path(), 4096).await.expect("followed");
    assert_eq!(std::fs::read(staged.path()).expect("staged"), body);

    let fake = Fake::start().expect("fake");
    describe(&fake, body.len(), &sha256_hex(&body));
    let plain = fake.url().replace("https://", "http://");
    fake.script(
        PATH,
        Answer::new(302, "").header("location", &format!("{plain}/objects/1")),
    );
    let releases = fake.releases(AGENT).expect("client");
    let dir = tempfile::tempdir().expect("dir");
    let err = fetch(&releases, dir.path(), 4096)
        .await
        .expect_err("refused");
    let text = err.to_string();
    assert!(
        text.starts_with("download release asset \"sb_linux_amd64\": "),
        "{text}"
    );
    assert!(text.contains("URL scheme is not allowed"), "{text}");
    assert_eq!(files(dir.path()), Vec::<String>::new());
}

/// No total timeout: a body that keeps coming finishes.
#[tokio::test]
async fn a_slow_body_finishes() {
    let fake = Fake::start().expect("fake");
    let body = vec![3u8; 64 * 1024];
    describe(&fake, body.len(), &sha256_hex(&body));
    fake.script(
        PATH,
        Answer::new(200, body.clone()).trickled(8 * 1024, Duration::from_millis(50)),
    );
    let dir = tempfile::tempdir().expect("dir");
    // The lookup on the usual client; the download on one whose metadata
    // total is shorter than the whole body takes: it applies to metadata
    // only.
    let looking = fake.releases(AGENT).expect("client");
    let mut bases = fake.bases();
    bases.metadata_timeout = Duration::from_millis(200);
    let downloading = Releases::testing(bases, AGENT).expect("client");
    let staged = in_step(async |step| {
        let release = looking.release(SB, Query::Latest, ASSET, step).await?;
        let stage = Stage {
            dir: dir.path(),
            prefix: PREFIX,
            max_size: 1 << 20,
        };
        downloading.download(&release.asset, stage, step).await
    })
    .await
    .expect("finished");
    assert_eq!(std::fs::read(staged.path()).expect("staged"), body);
}

/// Set in a re-run of this binary that runs one test's child side.
const CHILD: &str = "SB_RELEASE_CHILD";

/// A write that fails on the last chunk (a full disk; here a file-size
/// limit, its signal ignored, as `ulimit -f` sets it) fails the download:
/// a file short on disk is never returned as verified. The child side
/// runs in a re-run of this binary under the limit; the parent checks it
/// ran.
#[tokio::test]
async fn a_failed_write_fails_the_download() {
    if std::env::var_os(CHILD).is_some() {
        let fake = Fake::start().expect("fake");
        // One byte past bash's `ulimit -f 60` (60 KiB; dash counts 512-byte
        // blocks): only the byte in the last chunk can't be written, so the
        // last write is the one that fails.
        let body = vec![5u8; 60 * 1024 + 1];
        describe(&fake, body.len(), &sha256_hex(&body));
        fake.script(PATH, Answer::new(200, body));
        let dir = tempfile::tempdir().expect("dir");
        let releases = fake.releases(AGENT).expect("client");
        let err = fetch(&releases, dir.path(), 1 << 20)
            .await
            .expect_err("short on disk");
        assert!(
            err.to_string().starts_with(
                "copy release asset \"sb_linux_amd64\" into staging: write staged file: "
            ),
            "{err}"
        );
        assert_eq!(files(dir.path()), Vec::<String>::new());
        // A write failing before the last chunk fails at the next one.
        let fake = Fake::start().expect("fake");
        let body = vec![6u8; 512 * 1024];
        describe(&fake, body.len(), &sha256_hex(&body));
        fake.script(PATH, Answer::new(200, body));
        let dir = tempfile::tempdir().expect("dir");
        let releases = fake.releases(AGENT).expect("client");
        let err = fetch(&releases, dir.path(), 1 << 20)
            .await
            .expect_err("short on disk");
        assert!(err.to_string().contains("write staged file: "), "{err}");
        assert_eq!(files(dir.path()), Vec::<String>::new());
        println!("child checked");
        return;
    }
    let me = std::env::current_exe().expect("test binary");
    let out = std::process::Command::new("bash")
        .args(["-c", "trap '' XFSZ; ulimit -f 60; exec \"$0\" \"$@\""])
        .arg(me)
        .args([
            "a_failed_write_fails_the_download",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .output()
        .expect("run child");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{out:?}");
    assert!(stdout.contains("child checked"), "{stdout}");
}

/// A body that breaks off fails the download, naming the read.
#[tokio::test]
async fn a_broken_body_leaves_nothing() {
    let fake = Fake::start().expect("fake");
    let body = vec![4u8; 4096];
    describe(&fake, body.len(), &sha256_hex(&body));
    fake.script(PATH, Answer::new(200, vec![4u8; 1000]).cut_off());
    let dir = tempfile::tempdir().expect("dir");
    let releases = fake.releases(AGENT).expect("client");
    let err = fetch(&releases, dir.path(), 1 << 20)
        .await
        .expect_err("broken");
    assert!(
        err.to_string().starts_with(
            "copy release asset \"sb_linux_amd64\" into staging: read response body: "
        ),
        "{err}"
    );
    assert_eq!(files(dir.path()), Vec::<String>::new());
}

/// A staging directory that isn't there fails before anything is written.
#[tokio::test]
async fn a_missing_staging_directory_is_named() {
    let fake = Fake::start().expect("fake");
    let body = b"payload".to_vec();
    describe(&fake, body.len(), &sha256_hex(&body));
    fake.script(PATH, Answer::new(200, body));
    let dir = tempfile::tempdir().expect("dir");
    let releases = fake.releases(AGENT).expect("client");
    let err = fetch(&releases, &dir.path().join("gone"), 1 << 20)
        .await
        .expect_err("no directory");
    assert!(
        err.to_string().starts_with("create staged release asset: "),
        "{err}"
    );
}

/// A staged file gone before activation fails at its mode, the target
/// as it was.
#[test]
fn a_vanished_staged_file_changes_nothing() {
    let dir = tempfile::tempdir().expect("dir");
    let target = dir.path().join("sb");
    std::fs::write(&target, b"old").expect("old sb");
    let staged = tempfile::NamedTempFile::new_in(dir.path()).expect("staged");
    let path = staged.into_temp_path();
    std::fs::remove_file(&path).expect("removed");
    let err = activate(path, &target, 0o755).expect_err("gone");
    assert!(
        err.to_string()
            .starts_with("set staged release asset permissions: "),
        "{err}"
    );
    assert_eq!(std::fs::read(&target).expect("target"), b"old");
}

/// An asset exactly at the caller's cap is downloaded.
#[tokio::test]
async fn an_asset_at_the_cap_is_downloaded() {
    let fake = Fake::start().expect("fake");
    let body = vec![9u8; 4096];
    describe(&fake, body.len(), &sha256_hex(&body));
    fake.script(PATH, Answer::new(200, body.clone()));
    let dir = tempfile::tempdir().expect("dir");
    let releases = fake.releases(AGENT).expect("client");
    let staged = fetch(&releases, dir.path(), 4096)
        .await
        .expect("at the cap");
    assert_eq!(std::fs::read(staged.path()).expect("staged"), body);
}

/// The step's token stops a download; nothing is left staged.
#[tokio::test]
async fn a_cancelled_download_leaves_nothing() {
    let fake = Fake::start().expect("fake");
    let body = vec![3u8; 64 * 1024];
    describe(&fake, body.len(), &sha256_hex(&body));
    fake.script(
        PATH,
        Answer::new(200, body).trickled(1024, Duration::from_millis(100)),
    );
    let dir = tempfile::tempdir().expect("dir");
    let releases = fake.releases(AGENT).expect("client");
    let err = in_step(async |step| {
        let release = releases.release(SB, Query::Latest, ASSET, step).await?;
        let stage = Stage {
            dir: dir.path(),
            prefix: PREFIX,
            max_size: 1 << 20,
        };
        let token = step.token();
        let (done, ()) = tokio::join!(releases.download(&release.asset, stage, step), async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            token.cancel();
        });
        done
    })
    .await
    .expect_err("stopped");
    assert_eq!(err.to_string(), "interrupted");
    assert_eq!(files(dir.path()), Vec::<String>::new());
}

/// A metadata request that trickles past its total times out: the
/// proxy's warning says so and GitHub is asked.
#[tokio::test]
async fn a_trickling_proxy_times_out() {
    let fake = Fake::start().expect("fake");
    let body = b"payload".to_vec();
    let listing = format!(
        r#"{{"tag_name":"{TAG}","assets":[{{"name":"{ASSET}","size":{},"digest":"sha256:{}","browser_download_url":"{}{PATH}"}}]}}"#,
        body.len(),
        sha256_hex(&body),
        fake.url()
    );
    fake.script(
        "/version",
        // Some 30 parts a tenth of a second apart: about 3 s, three times
        // the timeout, which leaves GitHub's answer 1 s on a loaded host.
        Answer::json(listing.clone()).trickled(listing.len() / 30 + 1, Duration::from_millis(100)),
    );
    fake.script(
        "/api/repos/saltyorg/sb-rust/releases/latest",
        Answer::json(listing),
    );
    let mut bases = fake.bases();
    bases.metadata_timeout = Duration::from_secs(1);
    let releases = Releases::testing(bases, AGENT).expect("client");
    let term = InMemoryTerm::new(40, 160);
    let options = Options {
        target: Target::InMemory {
            term: term.clone(),
            colour: false,
        },
    };
    let shutdown = Shutdown::new();
    let mut run = Runner::start("sb test", &shutdown, None, options);
    let mut found = None;
    let _ = run
        .step(Labels::new("Checking", "checked"), async |step| {
            found = Some(releases.release(SB, Query::Latest, ASSET, step).await);
            Ok(())
        })
        .await;
    run.finish();
    assert_eq!(found.expect("ran").expect("from GitHub").tag, TAG);
    let screen = term.contents();
    assert!(
        screen.contains("SVM proxy timed out; trying GitHub directly"),
        "{screen}"
    );
    // A timeout is a network error: the proxy is asked four times first.
    let proxied = fake
        .seen()
        .iter()
        .filter(|seen| seen.target.starts_with("/version"))
        .count();
    assert_eq!(proxied, 4);
    assert_eq!(releases.waits(), [SECOND, 2 * SECOND, 4 * SECOND]);
}

/// A staged file outside the target's directory is refused before
/// anything changes, and removed.
#[test]
fn activation_needs_the_targets_directory() {
    let dir = tempfile::tempdir().expect("dir");
    let other = tempfile::tempdir().expect("other");
    let target = dir.path().join("sb");
    std::fs::write(&target, b"old").expect("old sb");
    let staged = tempfile::NamedTempFile::new_in(other.path()).expect("staged");
    let path = staged.path().to_owned();
    let err = activate(staged.into_temp_path(), &target, 0o755).expect_err("refused");
    assert_eq!(
        err.to_string(),
        "staged release asset must be in the target directory"
    );
    assert_eq!(std::fs::read(&target).expect("target"), b"old");
    assert!(!path.exists());
}

/// A file the caller staged itself (uv's extracted binaries) activates
/// as a download does, a new target included.
#[test]
fn a_callers_own_staged_file_activates() {
    use std::io::Write;
    let dir = tempfile::tempdir().expect("dir");
    let target = dir.path().join("uv");
    let mut staged = tempfile::NamedTempFile::new_in(dir.path()).expect("staged");
    staged.write_all(b"uv binary").expect("write");
    activate(staged.into_temp_path(), &target, 0o750).expect("activated");
    assert_eq!(std::fs::read(&target).expect("target"), b"uv binary");
    let mode = std::fs::metadata(&target)
        .expect("meta")
        .permissions()
        .mode();
    assert_eq!(mode & 0o7777, 0o750);
    assert_eq!(files(dir.path()), ["uv"]);
}

/// A failure at the rename (the target is a directory) leaves the
/// target and removes the staged file.
#[test]
fn a_failed_rename_changes_nothing() {
    let dir = tempfile::tempdir().expect("dir");
    let target = dir.path().join("sb");
    std::fs::create_dir(&target).expect("a directory in the way");
    std::fs::write(target.join("keep"), b"x").expect("keep");
    let staged = tempfile::NamedTempFile::new_in(dir.path()).expect("staged");
    let err = activate(staged.into_temp_path(), &target, 0o755).expect_err("refused");
    assert!(
        err.to_string().starts_with("activate release asset: "),
        "{err}"
    );
    assert!(target.join("keep").exists());
    assert_eq!(files(dir.path()), ["sb"]);
}

const SECOND: Duration = Duration::from_secs(1);

/// How many times `fake` was sent the download.
fn downloads(fake: &Fake) -> usize {
    fake.seen()
        .iter()
        .filter(|seen| seen.target == PATH)
        .count()
}

/// A 5xx, a 429 with `Retry-After`, a short body and a broken one are each
/// tried again, into a fresh staged file: the next whole body is used.
#[tokio::test]
async fn a_transient_failure_is_retried() {
    let body = vec![6u8; 4096];
    for (first, wait) in [
        (Answer::new(503, "busy"), SECOND),
        (Answer::new(429, "").header("retry-after", "2"), 2 * SECOND),
        (Answer::new(200, vec![6u8; 1000]), SECOND),
        (Answer::new(200, vec![6u8; 1000]).cut_off(), SECOND),
    ] {
        let fake = Fake::start().expect("fake");
        describe(&fake, body.len(), &sha256_hex(&body));
        fake.script(PATH, first.clone());
        fake.script(PATH, Answer::new(200, body.clone()));
        let dir = tempfile::tempdir().expect("dir");
        let releases = fake.releases(AGENT).expect("client");
        let staged = fetch(&releases, dir.path(), 1 << 20)
            .await
            .unwrap_or_else(|err| panic!("{first:?}: {err}"));
        assert_eq!(std::fs::read(staged.path()).expect("staged"), body);
        assert_eq!(downloads(&fake), 2, "{first:?}");
        assert_eq!(releases.waits(), [wait], "{first:?}");
        assert_eq!(files(dir.path()).len(), 1);
    }
}

/// Four attempts in all, waiting 1, 2 and 4 s; the last failure is the
/// error, and nothing is left staged.
#[tokio::test]
async fn four_attempts_then_the_failure() {
    let fake = Fake::start().expect("fake");
    let body = vec![6u8; 4096];
    describe(&fake, body.len(), &sha256_hex(&body));
    fake.script(PATH, Answer::new(200, vec![6u8; 1000]));
    let dir = tempfile::tempdir().expect("dir");
    let releases = fake.releases(AGENT).expect("client");
    let err = fetch(&releases, dir.path(), 1 << 20)
        .await
        .expect_err("short");
    assert_eq!(
        err.to_string(),
        "release asset \"sb_linux_amd64\" size mismatch: expected 4096 bytes, got 1000"
    );
    assert_eq!(downloads(&fake), 4);
    assert_eq!(releases.waits(), [SECOND, 2 * SECOND, 4 * SECOND]);
    assert_eq!(files(dir.path()), Vec::<String>::new());
}

/// A status that won't change, a digest mismatch and a longer body fail at
/// once.
#[tokio::test]
async fn a_lasting_failure_is_not_retried() {
    let body = vec![6u8; 4096];
    for answer in [
        Answer::new(404, "Not Found"),
        Answer::new(200, vec![7u8; 4096]),
        Answer::new(200, vec![6u8; 5000]),
    ] {
        let fake = Fake::start().expect("fake");
        describe(&fake, body.len(), &sha256_hex(&body));
        fake.script(PATH, answer.clone());
        let dir = tempfile::tempdir().expect("dir");
        let releases = fake.releases(AGENT).expect("client");
        let _ = fetch(&releases, dir.path(), 1 << 20)
            .await
            .expect_err("refused");
        assert_eq!(downloads(&fake), 1, "{answer:?}");
        assert_eq!(releases.waits(), Vec::<Duration>::new());
        assert_eq!(files(dir.path()), Vec::<String>::new());
    }
}

/// A rate limit resetting in two hours gives up at once, with the reason.
#[tokio::test]
async fn a_wait_over_a_minute_gives_up_at_once() {
    let fake = Fake::start().expect("fake");
    let body = vec![6u8; 4096];
    describe(&fake, body.len(), &sha256_hex(&body));
    fake.script(PATH, Answer::new(429, "").header("retry-after", "7200"));
    let dir = tempfile::tempdir().expect("dir");
    let releases = fake.releases(AGENT).expect("client");
    let err = fetch(&releases, dir.path(), 1 << 20)
        .await
        .expect_err("gave up");
    assert_eq!(
        err.to_string(),
        "download release asset \"sb_linux_amd64\": unexpected HTTP status 429 Too Many \
         Requests; retry wait 7200s exceeds the 60s limit"
    );
    assert_eq!(downloads(&fake), 1);
}

/// A body that stalls for longer than the read timeout fails each
/// attempt (the timeout set short through the test constructor).
#[tokio::test]
async fn a_stalled_body_times_out() {
    let fake = Fake::start().expect("fake");
    let body = vec![6u8; 4096];
    describe(&fake, body.len(), &sha256_hex(&body));
    fake.script(
        PATH,
        Answer::new(200, body).trickled(1024, Duration::from_secs(2)),
    );
    let mut bases = fake.bases();
    bases.read_timeout = Duration::from_millis(300);
    let releases = Releases::testing(bases, AGENT).expect("client");
    let dir = tempfile::tempdir().expect("dir");
    let err = fetch(&releases, dir.path(), 1 << 20)
        .await
        .expect_err("stalled");
    let text = err.to_string();
    assert!(
        text.starts_with(
            "copy release asset \"sb_linux_amd64\" into staging: read response body: "
        ),
        "{text}"
    );
    assert!(text.contains("timed out"), "{text}");
    assert_eq!(downloads(&fake), 4);
    assert_eq!(releases.waits(), [SECOND, 2 * SECOND, 4 * SECOND]);
    assert_eq!(files(dir.path()), Vec::<String>::new());
}

/// Ctrl-C during a retry wait ends it at once: interrupted, one request,
/// nothing staged. The wait is taken in full here (no divisor).
#[tokio::test]
async fn a_cancelled_wait_ends_at_once() {
    let fake = Fake::start().expect("fake");
    let body = vec![6u8; 4096];
    describe(&fake, body.len(), &sha256_hex(&body));
    fake.script(PATH, Answer::new(429, "").header("retry-after", "30"));
    let mut bases = fake.bases();
    bases.wait_divisor = 1;
    let releases = Releases::testing(bases, AGENT).expect("client");
    let dir = tempfile::tempdir().expect("dir");
    let start = std::time::Instant::now();
    let err = in_step(async |step| {
        let release = releases.release(SB, Query::Latest, ASSET, step).await?;
        let token = step.token();
        let stage = Stage {
            dir: dir.path(),
            prefix: PREFIX,
            max_size: 1 << 20,
        };
        let cancel = async {
            while downloads(&fake) == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            token.cancel();
        };
        let (staged, ()) = tokio::join!(releases.download(&release.asset, stage, step), cancel);
        staged
    })
    .await
    .expect_err("interrupted");
    assert_eq!(err.to_string(), "interrupted");
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(downloads(&fake), 1);
    assert_eq!(files(dir.path()), Vec::<String>::new());
}
