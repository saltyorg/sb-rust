//! sb-release's self-update flow on the fake GitHub, with the build and
//! the target as parameters (release spec, "`sb self-update`"): a released
//! build updates, a disabled one only says so, a forced one goes on; the
//! answer given as `--yes` would give it. No root and no sb binary: the
//! target is a file in a temporary directory.

use std::path::Path;

use indicatif::InMemoryTerm;
use sb_cli_core::build_info::BuildInfo;
use sb_cli_core::cancel::Shutdown;
use sb_release::testing::{Answer, Fake, stand_in};
use sb_release::{GithubRepo, Outcome, SelfUpdate, self_update};
use sb_term_ui::{Options, Runner, Target};
use semver::Version;
use sha2::{Digest, Sha256};

/// Taken by [`try_run`] for the whole flow, which writes the download and
/// then runs it for its version.
///
/// No other test's fork in this process may hold the staged file's write
/// descriptor when it runs, or exec fails with ETXTBSY (ADR-021, "Process
/// state"; sb-host's and sb-docker's `SPAWNS`). Every flow in this binary
/// runs through `try_run`; anything else here that starts a process must
/// take the lock too. In real use sb starts no other child while it stages
/// the new binary.
static SPAWNS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const SB: GithubRepo = GithubRepo {
    owner: "saltyorg",
    name: "sb-go",
};
/// A fake offering `tag` with the stand-in sb of that version as this
/// architecture's asset.
fn fake(tag: &str) -> Fake {
    fake_serving(tag, &stand_in(tag.trim_start_matches('v')))
}

/// A fake offering `tag` with `body` as this architecture's asset.
fn fake_serving(tag: &str, body: &[u8]) -> Fake {
    let fake = Fake::start().expect("fake");
    let asset = sb_release::asset_name().expect("asset");
    let path = format!("/saltyorg/sb-go/releases/download/{tag}/{asset}");
    fake.script(
        "/version",
        Answer::json(format!(
            r#"[{{"tag_name":"{tag}","assets":[{{"name":"{asset}","size":{},"digest":"sha256:{}","browser_download_url":"{}{path}"}}]}}]"#,
            body.len(),
            hex::encode(Sha256::digest(body)),
            fake.url()
        )),
    );
    fake.script(&path, Answer::new(200, body.to_vec()));
    fake
}

/// The flow for `build` on `target`, `--yes` given, `force` and `message`
/// as asked; its outcome and what the screen showed.
async fn run(
    fake: &Fake,
    build: &BuildInfo,
    target: &Path,
    force: bool,
    message: Option<&str>,
) -> (Outcome, String) {
    let (outcome, screen) = try_run(fake, build, target, force, message).await;
    (outcome.expect("ran"), screen)
}

/// [`run`], its error kept.
async fn try_run(
    fake: &Fake,
    build: &BuildInfo,
    target: &Path,
    force: bool,
    message: Option<&str>,
) -> (anyhow::Result<Outcome>, String) {
    let _spawns = SPAWNS.lock().await;
    let releases = fake.releases("sb/test").expect("client");
    let term = InMemoryTerm::new(40, 160);
    let shutdown = Shutdown::new();
    let update = SelfUpdate {
        repo: SB,
        build,
        target,
        yes: true,
        force,
        message,
    };
    let outcome = self_update(&releases, update, || {
        Runner::start(
            "sb self-update",
            &shutdown,
            None,
            Options {
                target: Target::InMemory {
                    term: term.clone(),
                    colour: false,
                },
            },
        )
    })
    .await;
    (outcome, term.contents())
}

fn target() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("dir");
    let target = dir.path().join("sb");
    std::fs::write(&target, b"old").expect("old sb");
    (dir, target)
}

fn released(version: &str) -> BuildInfo {
    BuildInfo::from_env("0.1.0", Some(version), Some("abc1234"), None)
}

/// The path a user takes: a released build, a newer release, `--yes`.
#[tokio::test]
async fn a_released_build_updates() {
    let fake = fake("v2.0.0");
    let (_dir, target) = target();
    let (outcome, screen) = run(
        &fake,
        &released("1.0.0"),
        &target,
        false,
        Some("Read the news"),
    )
    .await;
    assert_eq!(outcome, Outcome::Updated(Version::new(2, 0, 0)));
    assert_eq!(std::fs::read(&target).expect("target"), stand_in("2.0.0"));
    for line in [
        "New sb CLI version available: 2.0.0 (current: 1.0.0)",
        "Checked the new binary",
        "Successfully updated sb CLI to version: 2.0.0",
        "Read the news",
    ] {
        assert!(screen.contains(line), "{line}: {screen}");
    }
    assert!(!screen.contains("Forcing"), "{screen}");
}

#[tokio::test]
async fn a_released_build_at_the_newest_is_left() {
    let fake = fake("v1.0.0");
    let (_dir, target) = target();
    let (outcome, screen) = run(&fake, &released("1.0.0"), &target, false, None).await;
    assert_eq!(outcome, Outcome::Latest);
    assert!(
        screen.contains("Current binary is the latest version: 1.0.0"),
        "{screen}"
    );
    assert_eq!(std::fs::read(&target).expect("target"), b"old");
}

/// A build with self-update disabled says so (sb-go's two lines) and asks
/// nothing of GitHub; forced, it updates.
#[tokio::test]
async fn a_disabled_build_only_updates_when_forced() {
    let fake = fake("v2.0.0");
    let (_dir, target) = target();
    let disabled = BuildInfo::from_env("0.1.0", Some("1.0.0"), None, Some("true"));
    let (outcome, screen) = run(&fake, &disabled, &target, false, None).await;
    assert_eq!(outcome, Outcome::Disabled);
    assert!(
        screen.contains("Self-update is disabled in this build"),
        "{screen}"
    );
    assert!(
        screen.contains("Use --force-update to override this restriction"),
        "{screen}"
    );
    assert_eq!(fake.seen(), Vec::new());
    assert_eq!(std::fs::read(&target).expect("target"), b"old");
    let (outcome, screen) = run(&fake, &disabled, &target, true, None).await;
    assert_eq!(outcome, Outcome::Updated(Version::new(2, 0, 0)));
    assert!(
        screen.contains("Forcing self-update despite build configuration"),
        "{screen}"
    );
    assert_eq!(std::fs::read(&target).expect("target"), stand_in("2.0.0"));
}

/// A version that isn't semantic can't be compared: sb-go's error.
#[tokio::test]
async fn an_unreadable_version_fails_the_check() {
    let fake = fake("v2.0.0");
    let (_dir, target) = target();
    let (outcome, screen) = try_run(&fake, &released("nightly"), &target, false, None).await;
    let err = outcome.expect_err("fails");
    // The step failed, showing why; the run ends with its exit code.
    assert_eq!(
        err.downcast_ref::<sb_cli_core::exit::Exit>()
            .map(|exit| exit.0),
        Some(1),
        "{err}"
    );
    assert!(
        screen.contains("check for updates: invalid current version \"nightly\""),
        "{screen}"
    );
}

/// The files in `target`'s directory but `target`: none is left staged.
fn staged(target: &Path) -> Vec<String> {
    std::fs::read_dir(target.parent().expect("dir"))
        .expect("dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name != "sb")
        .collect()
}

/// The new binary must report the release's version (decided.md D-Q5):
/// one reporting another is refused, sb left as it was, nothing staged.
#[tokio::test]
async fn a_binary_reporting_another_version_is_refused() {
    let fake = fake_serving("v2.0.0", &stand_in("1.9.0"));
    let (_dir, target) = target();
    let (outcome, screen) = try_run(&fake, &released("1.0.0"), &target, false, None).await;
    assert!(outcome.is_err(), "{screen}");
    for line in [
        "binary update failed validation",
        "the new binary reports version 1.9.0, not 2.0.0",
    ] {
        assert!(screen.contains(line), "{line}: {screen}");
    }
    assert_eq!(std::fs::read(&target).expect("target"), b"old");
    assert_eq!(staged(&target), Vec::<String>::new());
}

/// An asset that isn't an ELF binary is refused before it runs (sb-go's
/// text), sb left as it was, nothing staged.
#[tokio::test]
async fn an_asset_that_is_not_an_elf_is_refused() {
    let fake = fake_serving("v2.0.0", b"#!/bin/sh\necho '{\"version\":\"2.0.0\"}'\n");
    let (_dir, target) = target();
    let (outcome, screen) = try_run(&fake, &released("1.0.0"), &target, false, None).await;
    assert!(outcome.is_err(), "{screen}");
    for line in [
        "binary update failed validation",
        "open ELF executable: Unknown file magic",
    ] {
        assert!(screen.contains(line), "{line}: {screen}");
    }
    assert_eq!(std::fs::read(&target).expect("target"), b"old");
    assert_eq!(staged(&target), Vec::<String>::new());
}
