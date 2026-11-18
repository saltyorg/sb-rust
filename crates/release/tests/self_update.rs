//! sb-release's self-update flow on the fake GitHub, with the build and
//! the target as parameters (release spec, "`sb self-update`"): a released
//! build updates, a disabled one only says so, a forced one goes on; the
//! answer given as `--yes` would give it. No root and no sb binary: the
//! target is a file in a temporary directory.

use std::path::Path;

use indicatif::InMemoryTerm;
use sb_cli_core::build_info::{BuildInfo, Channel};
use sb_cli_core::cancel::Shutdown;
use sb_release::testing::{Answer, Fake, stand_in, stand_in_on};
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
    name: "sb-rust",
};
/// A fake offering `tag` with the stand-in sb of that version as this
/// architecture's asset.
fn fake(tag: &str) -> Fake {
    fake_serving(tag, &stand_in(tag.trim_start_matches('v')))
}

/// A fake offering `tag` with `body` as this architecture's asset.
fn fake_serving(tag: &str, body: &[u8]) -> Fake {
    let fake = Fake::start().expect("fake");
    let asset = sb_release::asset_name(Channel::Release).expect("asset");
    let path = format!("/saltyorg/sb-rust/releases/download/{tag}/{asset}");
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
    try_run_on(fake, build, target, force, None, message).await
}

/// [`try_run`], `--channel` as `switch_to`.
async fn try_run_on(
    fake: &Fake,
    build: &BuildInfo,
    target: &Path,
    force: bool,
    switch_to: Option<Channel>,
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
        switch_to,
        message,
        shutdown: &shutdown,
    };
    let mut runner = Runner::start(
        "sb self-update",
        &shutdown,
        None,
        Options {
            target: Target::InMemory {
                term: term.clone(),
                colour: false,
            },
        },
    );
    let outcome = self_update(&releases, update, &mut runner).await;
    runner.finish();
    (outcome, term.contents())
}

fn target() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("dir");
    let target = dir.path().join("sb");
    std::fs::write(&target, b"old").expect("old sb");
    (dir, target)
}

fn released(version: &str) -> BuildInfo {
    on(Channel::Release, version)
}

/// A released build of `version` on `channel`.
fn on(channel: Channel, version: &str) -> BuildInfo {
    BuildInfo::from_env("0.1.0", Some(version), Some("abc1234"), channel)
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
    let disabled = BuildInfo::from_env("0.1.0", Some("1.0.0"), None, Channel::None);
    let (outcome, screen) = run(&fake, &disabled, &target, false, None).await;
    assert_eq!(outcome, Outcome::Disabled);
    // Both `›` notices under the check's step (term-ui decision 14).
    assert!(
        screen.contains("› Self-update is disabled in this build"),
        "{screen}"
    );
    assert!(
        screen.contains("› Use --force-update to override this restriction"),
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

/// A fake offering each release of `releases` (its tag, whether GitHub
/// flags it a prerelease, and its channels), each channel's asset a
/// stand-in sb of that version reporting that channel; newest first, as
/// GitHub lists them.
fn fake_list(releases: &[(&str, bool, &[Channel])]) -> Fake {
    let fake = Fake::start().expect("fake");
    let mut entries = Vec::new();
    for (tag, prerelease, channels) in releases {
        let mut assets = Vec::new();
        for channel in *channels {
            let asset = sb_release::asset_name(*channel).expect("asset");
            let body = stand_in_on(tag, channel.name());
            let path = format!("/saltyorg/sb-rust/releases/download/{tag}/{asset}");
            assets.push(format!(
                r#"{{"name":"{asset}","size":{},"digest":"sha256:{}","browser_download_url":"{}{path}"}}"#,
                body.len(),
                hex::encode(Sha256::digest(&body)),
                fake.url()
            ));
            fake.script(&path, Answer::new(200, body));
        }
        entries.push(format!(
            r#"{{"tag_name":"{tag}","prerelease":{prerelease},"assets":[{}]}}"#,
            assets.join(",")
        ));
    }
    fake.script("/version", Answer::json(format!("[{}]", entries.join(","))));
    fake
}

const BOTH: &[Channel] = &[Channel::Release, Channel::Beta];
const BETA: &[Channel] = &[Channel::Beta];

/// A beta host takes the newest release, rc included, and its `_beta`
/// asset; a release host never sees the rc.
#[tokio::test]
async fn each_channel_follows_its_releases() {
    let fake = fake_list(&[("1.1.0-rc.1", true, BETA), ("1.0.0", false, BOTH)]);
    let (_dir, target) = target();
    let (outcome, screen) = run(&fake, &on(Channel::Release, "1.0.0"), &target, false, None).await;
    assert_eq!(outcome, Outcome::Latest, "{screen}");
    let (outcome, screen) = run(&fake, &on(Channel::Beta, "1.0.0"), &target, false, None).await;
    assert_eq!(
        outcome,
        Outcome::Updated(Version::parse("1.1.0-rc.1").expect("v")),
        "{screen}"
    );
    assert_eq!(
        std::fs::read(&target).expect("target"),
        stand_in_on("1.1.0-rc.1", "beta")
    );
}

/// A stable release published after an rc: the beta host at the rc gets
/// the stable release's `_beta` asset, and stays beta.
#[tokio::test]
async fn a_stable_release_after_an_rc_keeps_a_beta_host_beta() {
    let fake = fake_list(&[("1.0.0", false, BOTH), ("1.0.0-rc.2", true, BETA)]);
    let (_dir, target) = target();
    let (outcome, screen) = run(
        &fake,
        &on(Channel::Beta, "1.0.0-rc.2"),
        &target,
        false,
        None,
    )
    .await;
    assert_eq!(outcome, Outcome::Updated(Version::new(1, 0, 0)), "{screen}");
    assert_eq!(
        std::fs::read(&target).expect("target"),
        stand_in_on("1.0.0", "beta")
    );
}

/// `--channel beta` on a release host installs the newest beta once; back
/// to `release` from a newer rc installs the lower stable release, said
/// before the question.
#[tokio::test]
async fn channel_switches_both_ways() {
    let fake = fake_list(&[("1.1.0-rc.1", true, BETA), ("1.0.0", false, BOTH)]);
    let (_dir, target) = target();
    let (outcome, screen) = try_run_on(
        &fake,
        &on(Channel::Release, "1.0.0"),
        &target,
        false,
        Some(Channel::Beta),
        None,
    )
    .await;
    assert_eq!(
        outcome.expect("ran"),
        Outcome::Updated(Version::parse("1.1.0-rc.1").expect("v"))
    );
    assert!(
        screen
            .contains("Switching sb CLI to the beta channel: 1.1.0-rc.1 (current: 1.0.0, release)"),
        "{screen}"
    );
    assert_eq!(
        std::fs::read(&target).expect("target"),
        stand_in_on("1.1.0-rc.1", "beta")
    );
    let (outcome, screen) = try_run_on(
        &fake,
        &on(Channel::Beta, "1.1.0-rc.1"),
        &target,
        false,
        Some(Channel::Release),
        None,
    )
    .await;
    assert_eq!(
        outcome.expect("ran"),
        Outcome::Updated(Version::new(1, 0, 0))
    );
    for line in [
        "Switching sb CLI to the release channel: 1.0.0 (current: 1.1.0-rc.1, beta)",
        "1.0.0 is older than the running 1.1.0-rc.1",
    ] {
        assert!(screen.contains(line), "{line}: {screen}");
    }
    assert_eq!(
        std::fs::read(&target).expect("target"),
        stand_in_on("1.0.0", "release")
    );
    // The build's own channel is a normal update: nothing newer, nothing done.
    let (outcome, _) = try_run_on(
        &fake,
        &on(Channel::Release, "1.0.0"),
        &target,
        false,
        Some(Channel::Release),
        None,
    )
    .await;
    assert_eq!(outcome.expect("ran"), Outcome::Latest);
}

/// An asset reporting another channel than the one asked for is refused,
/// sb left as it was: a mislabelled asset can't switch a host.
#[tokio::test]
async fn an_asset_of_another_channel_is_refused() {
    let fake = Fake::start().expect("fake");
    let asset = sb_release::asset_name(Channel::Beta).expect("asset");
    let body = stand_in_on("1.1.0-rc.1", "release");
    let path = format!("/saltyorg/sb-rust/releases/download/1.1.0-rc.1/{asset}");
    fake.script(
        "/version",
        Answer::json(format!(
            r#"[{{"tag_name":"1.1.0-rc.1","prerelease":true,"assets":[{{"name":"{asset}","size":{},"digest":"sha256:{}","browser_download_url":"{}{path}"}}]}}]"#,
            body.len(),
            hex::encode(Sha256::digest(&body)),
            fake.url()
        )),
    );
    fake.script(&path, Answer::new(200, body));
    let (_dir, target) = target();
    let (outcome, screen) = try_run(&fake, &on(Channel::Beta, "1.0.0"), &target, false, None).await;
    assert!(outcome.is_err(), "{screen}");
    assert!(
        screen.contains("the new binary reports channel release, not beta"),
        "{screen}"
    );
    assert_eq!(std::fs::read(&target).expect("target"), b"old");
    assert_eq!(staged(&target), Vec::<String>::new());
}

/// `none` is off, `--channel` or not; only `--force-update` goes on.
#[tokio::test]
async fn the_none_channel_is_off() {
    let fake = fake_list(&[("1.1.0-rc.1", true, BETA), ("1.0.0", false, BOTH)]);
    let (_dir, target) = target();
    let none = on(Channel::None, "0.9.0");
    let (outcome, screen) =
        try_run_on(&fake, &none, &target, false, Some(Channel::Beta), None).await;
    assert_eq!(outcome.expect("ran"), Outcome::Disabled, "{screen}");
    assert!(
        screen.contains("Self-update is disabled in this build"),
        "{screen}"
    );
    assert_eq!(fake.seen(), Vec::new());
    let (outcome, _) = try_run_on(&fake, &none, &target, true, Some(Channel::Beta), None).await;
    assert_eq!(
        outcome.expect("ran"),
        Outcome::Updated(Version::parse("1.1.0-rc.1").expect("v"))
    );
    assert_eq!(
        std::fs::read(&target).expect("target"),
        stand_in_on("1.1.0-rc.1", "beta")
    );
}
