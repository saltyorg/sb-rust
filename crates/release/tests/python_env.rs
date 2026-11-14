//! A stub caller of the interface python-env needs (release spec, "The
//! interface python-env needs", R1-R8; success criterion 8), on the fake
//! server: uv by tag from astral-sh's repository, the fact as the latest
//! release of saltyorg/ansible-facts, and bench's pinned Speedtest, each
//! the way its caller's spec says. Nothing here reaches the network.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use indicatif::InMemoryTerm;
use sb_cli_core::cancel::Shutdown;
use sb_release::testing::{Answer, Fake, stand_in};
use sb_release::{
    Arch, Asset, ElfError, GithubRepo, Query, Release, Releases, Stage, Staged, activate,
    check_elf, sha256_file,
};
use sb_term_ui::{Labels, Options, Runner, Step, Target};
use semver::Version;
use sha2::{Digest, Sha256};

/// R1's production client, as python-env builds it (not called: it would
/// reach GitHub); its signature is part of the interface.
const _PRODUCTION: fn(&str) -> Result<Releases, sb_release::Error> = Releases::production;

const UV: GithubRepo = GithubRepo {
    owner: "astral-sh",
    name: "uv",
};
const FACTS: GithubRepo = GithubRepo {
    owner: "saltyorg",
    name: "ansible-facts",
};

/// `body` run in one step on an in-memory screen; its result.
async fn in_step<T>(body: impl AsyncFnOnce(&mut Step) -> T) -> T {
    let options = Options {
        target: Target::InMemory {
            term: InMemoryTerm::new(40, 160),
            colour: false,
        },
    };
    let shutdown = Shutdown::new();
    let mut run = Runner::start("sb test", &shutdown, None, options);
    let mut result = None;
    let _ = run
        .step(Labels::new("Calling", "Called"), async |step| {
            result = Some(body(step).await);
            Ok(())
        })
        .await;
    run.finish();
    result.expect("the step ran")
}

/// `fake` serving `repo`'s release `tag` (at `api`, the API path the
/// query asks for) with one asset `name` holding `body`.
fn serve(fake: &Fake, repo: GithubRepo, api: &str, tag: &str, name: &str, body: &[u8]) {
    let path = format!(
        "/{}/{}/releases/download/{tag}/{name}",
        repo.owner, repo.name
    );
    let release = format!(
        r#"{{"tag_name":"{tag}","assets":[{{"name":"{name}","size":{},"digest":"sha256:{}","browser_download_url":"{}{path}"}}]}}"#,
        body.len(),
        hex::encode(Sha256::digest(body)),
        fake.url()
    );
    // The proxy is down, so GitHub's API answers (each path its own).
    fake.script("/version", Answer::new(500, "down"));
    fake.script(api, Answer::json(release));
    fake.script(&path, Answer::new(200, body.to_vec()));
}

/// uv (python-env, "uv install"): the release by tag from astral-sh, its
/// archive downloaded to a scratch directory (cap 128 MiB); python-env
/// extracts `uv` into its own staged file in the target directory and
/// activates it.
#[tokio::test]
async fn uv_by_tag_from_astral_sh() {
    let arch = Arch::current().expect("arch");
    let archive = format!("uv-{}-unknown-linux-gnu.tar.gz", arch.rust_name());
    let fake = Fake::start().expect("fake");
    let body = b"a uv archive".as_slice();
    serve(
        &fake,
        UV,
        "/api/repos/astral-sh/uv/releases/tags/0.9.5",
        "0.9.5",
        &archive,
        body,
    );
    let releases = Releases::testing(fake.bases(), "sb/test").expect("client");
    let scratch = tempfile::tempdir().expect("scratch");
    let staged: Staged = in_step(async |step| {
        let found: Release = releases
            .release(UV, Query::Tag("0.9.5".to_owned()), &archive, step)
            .await
            .expect("uv's release");
        assert_eq!(found.tag, "0.9.5");
        assert_eq!(found.asset.name(), archive);
        assert_eq!(found.asset.size(), body.len() as u64);
        assert_eq!(found.asset.sha256(), <[u8; 32]>::from(Sha256::digest(body)));
        let stage = Stage {
            dir: scratch.path(),
            prefix: ".uv-download-",
            max_size: 128 << 20,
        };
        releases
            .download(&found.asset, stage, step)
            .await
            .expect("downloaded")
    })
    .await;
    assert_eq!(std::fs::read(staged.path()).expect("staged"), body);
    // The extracted binary, staged by the caller itself in the target's
    // directory, then activated (R6 on a caller's own file).
    let bin = tempfile::tempdir().expect("bin");
    let target = bin.path().join("uv");
    let mut own = tempfile::Builder::new()
        .prefix(".uv-")
        .tempfile_in(bin.path())
        .expect("own staged file");
    std::io::Write::write_all(&mut own, b"the uv binary").expect("extract");
    activate(own.into_temp_path(), &target, 0o755).expect("activated");
    assert_eq!(std::fs::read(&target).expect("uv"), b"the uv binary");
    assert_eq!(only_file(bin.path()), "uv");
}

/// The fact (python-env, "The fact"): the latest release of
/// saltyorg/ansible-facts, downloaded into the fact's directory (prefix
/// `.saltbox.fact-`, cap 32 MiB), checked as this architecture's ELF,
/// activated 0755, and its installed digest compared with the asset's.
#[tokio::test]
async fn the_fact_as_the_latest_release() {
    let arch = Arch::current().expect("arch");
    let name = format!("saltbox-facts-{}", arch.go_name());
    let fake = Fake::start().expect("fake");
    let body = stand_in("1.4.0");
    serve(
        &fake,
        FACTS,
        "/api/repos/saltyorg/ansible-facts/releases/latest",
        "v1.4.0",
        &name,
        &body,
    );
    let releases = Releases::testing(fake.bases(), "sb/test").expect("client");
    let dir = tempfile::tempdir().expect("fact dir");
    let target = dir.path().join("saltbox.fact");
    let (asset, staged): (Asset, Staged) = in_step(async |step| {
        let found = releases
            .release(FACTS, Query::Latest, &name, step)
            .await
            .expect("the fact's release");
        assert_eq!(found.tag, "v1.4.0");
        // python-env's version decision reads the tag as a version.
        assert_eq!(found.version, Some(Version::new(1, 4, 0)));
        let stage = Stage {
            dir: dir.path(),
            prefix: ".saltbox.fact-",
            max_size: 32 << 20,
        };
        let staged = releases
            .download(&found.asset, stage, step)
            .await
            .expect("downloaded");
        (found.asset, staged)
    })
    .await;
    assert_eq!(check_elf(staged.path(), arch), Ok(()));
    activate(staged, &target, 0o755).expect("activated");
    let mode = std::fs::metadata(&target)
        .expect("fact")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755);
    assert_eq!(sha256_file(&target).expect("digest"), asset.sha256());
    // Another architecture's fact is refused with the variant python-env
    // words as `file is not an <arch> ELF binary`.
    let other = match arch {
        Arch::Amd64 => Arch::Arm64,
        Arch::Arm64 => Arch::Amd64,
    };
    assert_eq!(
        check_elf(&target, other),
        Err(ElfError::WrongMachine(other))
    );
}

/// A release without the asked-for asset: sb-go's text (R3).
#[tokio::test]
async fn a_release_without_the_asset_says_so() {
    let fake = Fake::start().expect("fake");
    serve(
        &fake,
        FACTS,
        "/api/repos/saltyorg/ansible-facts/releases/latest",
        "v1.4.0",
        "saltbox-facts-riscv64",
        b"x",
    );
    let releases = Releases::testing(fake.bases(), "sb/test").expect("client");
    let err = in_step(async |step| {
        releases
            .release(FACTS, Query::Latest, "saltbox-facts-amd64", step)
            .await
            .expect_err("missing")
    })
    .await;
    assert_eq!(
        err.to_string(),
        "release v1.4.0 is missing saltbox-facts-amd64"
    );
}

/// bench's Speedtest (ops decision 13): an asset pinned in source, its
/// URL, size and digest, downloaded and verified as any asset.
#[tokio::test]
async fn a_pinned_asset_downloads_verified() {
    let fake = Fake::start().expect("fake");
    let body = b"the speedtest archive".as_slice();
    let path = "/app/cli/ookla-speedtest-1.2.0-linux-x86_64.tgz";
    fake.script(path, Answer::new(200, body.to_vec()));
    let url = format!("{}{path}", fake.url()).parse().expect("url");
    let pinned =
        Asset::pinned(url, body.len() as u64, Sha256::digest(body).into()).expect("pinned");
    assert_eq!(pinned.name(), "ookla-speedtest-1.2.0-linux-x86_64.tgz");
    let releases = Releases::testing(fake.bases(), "sb/test").expect("client");
    let work = tempfile::tempdir().expect("work");
    let staged = in_step(async |step| {
        let stage = Stage {
            dir: work.path(),
            prefix: ".speedtest-",
            max_size: 16 << 20,
        };
        releases.download(&pinned, stage, step).await
    })
    .await
    .expect("downloaded");
    assert_eq!(sha256_file(staged.path()).expect("digest"), pinned.sha256());
    // A pin whose digest doesn't match the bytes is refused.
    let wrong = Asset::pinned(pinned.url().clone(), pinned.size(), [0; 32]).expect("pinned");
    let refused = in_step(async |step| {
        let stage = Stage {
            dir: work.path(),
            prefix: ".speedtest-",
            max_size: 16 << 20,
        };
        releases.download(&wrong, stage, step).await
    })
    .await;
    assert_eq!(
        refused.expect_err("refused").to_string(),
        r#"release asset "ookla-speedtest-1.2.0-linux-x86_64.tgz" digest mismatch"#
    );
    // Only the first download, still held, is staged: the refused one is
    // gone.
    assert_eq!(only_file(work.path()), file_name(staged.path()));
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .expect("name")
        .to_string_lossy()
        .into_owned()
}

/// The one file in `dir`.
fn only_file(dir: &Path) -> String {
    let names: Vec<String> = std::fs::read_dir(dir)
        .expect("dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
    names[0].clone()
}
