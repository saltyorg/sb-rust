//! Release lookup on the fake TLS server (release spec, "Release lookup",
//! "Testing strategy"): the proxy first, then GitHub once, with sb-go's
//! notices; the asset checks on every answer; the User-Agent on every
//! request. Nothing here reaches the network.

use indicatif::InMemoryTerm;
use sb_cli_core::cancel::Shutdown;
use sb_release::testing::{Answer, Fake};
use sb_release::{Error, GithubRepo, Query, Release, Releases};
use sb_term_ui::{Labels, Options, Runner, Step, Target};
use semver::Version;

const SB: GithubRepo = GithubRepo {
    owner: "saltyorg",
    name: "sb-go",
};
const ASSET: &str = "sb_linux_amd64";
const AGENT: &str = "sb/0.0.0-test";
const DIGEST: &str = "sha256:00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

/// An asset entry as GitHub's API lists it, served from `fake`.
fn asset(fake: &Fake, tag: &str, name: &str, digest: &str) -> String {
    format!(
        r#"{{"name":"{name}","size":12,"digest":"{digest}","browser_download_url":"{}/saltyorg/sb-go/releases/download/{tag}/{name}","id":1}}"#,
        fake.url()
    )
}

/// A release entry with the given assets.
fn release(tag: &str, draft: bool, prerelease: bool, assets: &[String]) -> String {
    format!(
        r#"{{"tag_name":"{tag}","draft":{draft},"prerelease":{prerelease},"assets":[{}],"name":"x"}}"#,
        assets.join(",")
    )
}

/// `body` run in one step on an in-memory screen; its result and the
/// screen.
async fn in_step<T>(body: impl AsyncFnOnce(&mut Step) -> T) -> (T, String) {
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
        .step(Labels::new("Checking", "checked"), async |step| {
            result = Some(body(step).await);
            Ok(())
        })
        .await;
    run.finish();
    (result.expect("the step ran"), term.contents())
}

async fn latest(releases: &Releases) -> (Result<Release, Error>, String) {
    in_step(async |step| releases.release(SB, Query::Latest, ASSET, step).await).await
}

fn notices(screen: &str) -> (usize, usize) {
    (
        screen.matches("trying GitHub directly").count(),
        screen.matches("GitHub fallback succeeded").count(),
    )
}

const LATEST: &str = "/api/repos/saltyorg/sb-go/releases/latest";

#[tokio::test]
async fn the_proxy_answers_first() {
    let fake = Fake::start().expect("fake");
    fake.script(
        "/version",
        Answer::json(release(
            "v1.2.3",
            false,
            false,
            &[asset(&fake, "v1.2.3", ASSET, DIGEST)],
        )),
    );
    let releases = fake.releases(AGENT).expect("client");
    let (found, screen) = latest(&releases).await;
    let found = found.expect("found");
    assert_eq!(found.tag, "v1.2.3");
    assert_eq!(found.version, Some(Version::new(1, 2, 3)));
    assert_eq!(found.asset.name(), ASSET);
    assert_eq!(found.asset.size(), 12);
    assert_eq!(notices(&screen), (0, 0), "{screen}");
    let seen = fake.seen();
    assert_eq!(seen.len(), 1);
    // The proxy is asked for GitHub's API URL, percent-encoded.
    let api = format!("{}{LATEST}", fake.url());
    let encoded: String = api
        .chars()
        .map(|c| match c {
            ':' => "%3A".to_owned(),
            '/' => "%2F".to_owned(),
            c => c.to_string(),
        })
        .collect();
    assert_eq!(seen[0].target, format!("/version?url={encoded}"));
    assert_eq!(seen[0].method, "GET");
}

/// A failing proxy: GitHub directly, with sb-go's warning and info, each
/// once per run however many lookups fail over.
#[tokio::test]
async fn a_failing_proxy_falls_back_to_github_once() {
    let fake = Fake::start().expect("fake");
    fake.script("/version", Answer::new(500, "proxy down"));
    fake.script(
        LATEST,
        Answer::json(release(
            "v1.2.3",
            false,
            false,
            &[asset(&fake, "v1.2.3", ASSET, DIGEST)],
        )),
    );
    let releases = fake.releases(AGENT).expect("client");
    let (found, screen) = in_step(async |step| {
        let first = releases.release(SB, Query::Latest, ASSET, step).await;
        let second = releases.release(SB, Query::Latest, ASSET, step).await;
        (first, second)
    })
    .await;
    assert_eq!(found.0.expect("first").tag, "v1.2.3");
    assert_eq!(found.1.expect("second").tag, "v1.2.3");
    assert!(
        screen.contains("SVM proxy returned HTTP 500; trying GitHub directly"),
        "{screen}"
    );
    assert_eq!(notices(&screen), (1, 1), "{screen}");
    let targets: Vec<String> = fake.seen().into_iter().map(|s| s.target).collect();
    assert_eq!(targets.len(), 4, "{targets:?}");
    assert!(targets[0].starts_with("/version?url="));
    assert_eq!(targets[1], LATEST);
}

/// An answer the checks refuse is unusable: GitHub is asked.
#[tokio::test]
async fn an_unusable_proxy_answer_falls_back() {
    for proxied in [
        // The asset is missing.
        |fake: &Fake| {
            release(
                "v1.2.3",
                false,
                false,
                &[asset(fake, "v1.2.3", "other", DIGEST)],
            )
        },
        // Its digest is bad.
        |fake: &Fake| {
            release(
                "v1.2.3",
                false,
                false,
                &[asset(fake, "v1.2.3", ASSET, "sha256:00")],
            )
        },
        // Its URL is another repository's.
        |fake: &Fake| {
            release(
                "v1.2.3",
                false,
                false,
                &[asset(fake, "v1.2.3", ASSET, DIGEST)],
            )
            .replace("/saltyorg/sb-go/", "/someone/sb-go/")
        },
    ] {
        let fake = Fake::start().expect("fake");
        fake.script("/version", Answer::json(proxied(&fake)));
        fake.script(
            LATEST,
            Answer::json(release(
                "v1.2.3",
                false,
                false,
                &[asset(&fake, "v1.2.3", ASSET, DIGEST)],
            )),
        );
        let releases = fake.releases(AGENT).expect("client");
        let (found, screen) = latest(&releases).await;
        assert_eq!(found.expect("found").tag, "v1.2.3");
        assert!(
            screen.contains("SVM proxy returned unusable release metadata; trying GitHub directly"),
            "{screen}"
        );
        assert_eq!(notices(&screen), (1, 1), "{screen}");
    }
}

/// Invalid JSON, a wrong shape and a transport failure all fall back.
#[tokio::test]
async fn invalid_answers_fall_back() {
    let fake = Fake::start().expect("fake");
    fake.script("/version", Answer::json("{not json"));
    fake.script(LATEST, Answer::json("[]"));
    let releases = fake.releases(AGENT).expect("client");
    let (found, screen) = latest(&releases).await;
    let err = found.expect_err("both fail");
    assert!(
        screen.contains("SVM proxy returned invalid JSON; trying GitHub directly"),
        "{screen}"
    );
    assert!(
        err.to_string().starts_with(
            "returned invalid JSON: key must be a string at line 1 column 2; \
             fallback GitHub API request failed: returned invalid JSON: invalid length 0"
        ),
        "{err}"
    );
    assert_eq!(notices(&screen), (1, 0), "{screen}");
}

/// Both failing: sb-go's joined error; the error body is made safe.
#[tokio::test]
async fn both_failing_join_their_errors() {
    let fake = Fake::start().expect("fake");
    fake.script("/version", Answer::new(502, "bad \u{1b}[2Jgateway\n"));
    fake.script(LATEST, Answer::new(403, "API rate limit exceeded"));
    let releases = fake.releases(AGENT).expect("client");
    let (found, _) = latest(&releases).await;
    assert_eq!(
        found.expect_err("both fail").to_string(),
        "returned HTTP 502: bad gateway; fallback GitHub API request failed: \
         returned HTTP 403: API rate limit exceeded"
    );
}

/// A proxy that can't be reached: the transport's reason, then GitHub.
#[tokio::test]
async fn an_unreachable_proxy_falls_back() {
    let fake = Fake::start().expect("fake");
    fake.script(
        LATEST,
        Answer::json(release(
            "v1.2.3",
            false,
            false,
            &[asset(&fake, "v1.2.3", ASSET, DIGEST)],
        )),
    );
    let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("port");
    let port = closed.local_addr().expect("addr").port();
    drop(closed);
    let mut bases = fake.bases();
    bases.proxy = format!("https://127.0.0.1:{port}/version");
    let releases = Releases::testing(bases, AGENT).expect("client");
    let (found, screen) = latest(&releases).await;
    assert_eq!(found.expect("found").tag, "v1.2.3");
    assert!(
        screen.contains("SVM proxy request failed (error sending request"),
        "{screen}"
    );
}

/// GitHub's own answer is final: a release without the asset is sb-go's
/// error, not the joined one.
#[tokio::test]
async fn a_release_missing_its_asset_is_named() {
    let fake = Fake::start().expect("fake");
    fake.script("/version", Answer::new(500, ""));
    fake.script(
        LATEST,
        Answer::json(release(
            "v1.2.3",
            false,
            false,
            &[asset(&fake, "v1.2.3", "sb_linux_arm64", DIGEST)],
        )),
    );
    let releases = fake.releases(AGENT).expect("client");
    let (found, screen) = latest(&releases).await;
    assert_eq!(
        found.expect_err("missing").to_string(),
        "release v1.2.3 is missing sb_linux_amd64"
    );
    assert_eq!(notices(&screen), (1, 0), "{screen}");
    // A bad asset on GitHub: its check's text, wrapped as sb-go's.
    let fake = Fake::start().expect("fake");
    fake.script("/version", Answer::new(500, ""));
    fake.script(
        LATEST,
        Answer::json(release(
            "v1.2.3",
            false,
            false,
            &[asset(&fake, "v1.2.3", ASSET, "")],
        )),
    );
    let releases = fake.releases(AGENT).expect("client");
    assert_eq!(
        latest(&releases).await.0.expect_err("bad").to_string(),
        "release v1.2.3 has unusable sb_linux_amd64 metadata: \
         release asset \"sb_linux_amd64\" is missing a SHA-256 digest"
    );
}

/// A tag query asks for that tag, the tag one path segment.
#[tokio::test]
async fn a_tag_is_asked_for_by_name() {
    let fake = Fake::start().expect("fake");
    fake.script(
        "/version",
        Answer::json(release(
            "0.9.1",
            false,
            false,
            &[asset(&fake, "0.9.1", ASSET, DIGEST)],
        )),
    );
    let releases = fake.releases(AGENT).expect("client");
    let (found, _) = in_step(async |step| {
        releases
            .release(SB, Query::Tag("0.9.1".into()), ASSET, step)
            .await
    })
    .await;
    let found = found.expect("found");
    assert_eq!(found.tag, "0.9.1");
    let target = &fake.seen()[0].target;
    assert!(
        target.ends_with("%2Fapi%2Frepos%2Fsaltyorg%2Fsb-go%2Freleases%2Ftags%2F0.9.1"),
        "{target}"
    );
}

/// The newest stable release: drafts, prereleases and tags that aren't
/// versions skipped; `None` when it isn't newer than the running one.
#[tokio::test]
async fn the_newest_stable_release_is_chosen() {
    let fake = Fake::start().expect("fake");
    let list = format!(
        "[{}]",
        [
            release("v2.0.0", true, false, &[]),
            release("v1.9.0", false, true, &[]),
            release("nightly", false, false, &[]),
            release(
                "1.4.0",
                false,
                false,
                &[asset(&fake, "1.4.0", ASSET, DIGEST)]
            ),
            release(
                "v1.10.0",
                false,
                false,
                &[asset(&fake, "v1.10.0", ASSET, DIGEST)]
            ),
            release("v1.2.0", false, false, &[]),
        ]
        .join(",")
    );
    fake.script("/version", Answer::json(list));
    let releases = fake.releases(AGENT).expect("client");
    let newest = |current: Version| {
        let releases = &releases;
        async move {
            in_step(async |step| releases.newest(SB, &current, ASSET, step).await)
                .await
                .0
        }
    };
    let found = newest(Version::new(1, 4, 0))
        .await
        .expect("ok")
        .expect("newer");
    assert_eq!(found.tag, "v1.10.0");
    assert_eq!(found.version, Some(Version::new(1, 10, 0)));
    assert_eq!(newest(Version::new(1, 10, 0)).await.expect("ok"), None);
    assert_eq!(newest(Version::new(1, 11, 0)).await.expect("ok"), None);
    assert!(fake.seen()[0].target.ends_with("%2Freleases"));
}

/// No stable versioned release, or the newest one without the asset:
/// unusable from the proxy, sb-go's errors from GitHub.
#[tokio::test]
async fn a_list_without_a_usable_newest_release_fails() {
    for (list, expected) in [
        (
            vec![
                release("nightly", false, false, &[]),
                release("v3.0.0", false, true, &[]),
            ],
            "no stable releases with valid semantic versions".to_owned(),
        ),
        (
            vec![],
            "no stable releases with valid semantic versions".to_owned(),
        ),
        (
            vec![release("v1.0.0", false, false, &[])],
            "release v1.0.0 is missing sb_linux_amd64".to_owned(),
        ),
    ] {
        let fake = Fake::start().expect("fake");
        let list = format!("[{}]", list.join(","));
        fake.script("/version", Answer::json(list.clone()));
        fake.script("/api/repos/saltyorg/sb-go/releases", Answer::json(list));
        let releases = fake.releases(AGENT).expect("client");
        let (found, screen) = in_step(async |step| {
            releases
                .newest(SB, &Version::new(0, 1, 0), ASSET, step)
                .await
        })
        .await;
        assert_eq!(found.expect_err("unusable").to_string(), expected);
        assert_eq!(notices(&screen), (1, 0), "{screen}");
    }
}

/// Every request carries the User-Agent (GitHub refuses requests without
/// one).
#[tokio::test]
async fn every_request_has_the_user_agent() {
    let fake = Fake::start().expect("fake");
    fake.script("/version", Answer::new(500, ""));
    fake.script(LATEST, Answer::new(404, ""));
    let releases = fake.releases(AGENT).expect("client");
    let _ = latest(&releases).await;
    let seen = fake.seen();
    assert_eq!(seen.len(), 2);
    for request in seen {
        assert_eq!(request.header("user-agent"), Some(AGENT), "{request:?}");
    }
}

/// A body past 4 MiB is invalid JSON, not read whole.
#[tokio::test]
async fn an_oversize_body_is_refused() {
    let fake = Fake::start().expect("fake");
    let mut big = release(
        "v1.2.3",
        false,
        false,
        &[asset(&fake, "v1.2.3", ASSET, DIGEST)],
    );
    big.push_str(&" ".repeat(4 << 20));
    fake.script("/version", Answer::json(big.clone()));
    fake.script(LATEST, Answer::json(big));
    let releases = fake.releases(AGENT).expect("client");
    let (found, _) = latest(&releases).await;
    assert_eq!(
        found.expect_err("too big").to_string(),
        "returned invalid JSON: larger than 4 MiB; fallback GitHub API request failed: \
         returned invalid JSON: larger than 4 MiB"
    );
}

/// The step's token stops a lookup.
#[tokio::test]
async fn a_cancelled_step_stops_the_lookup() {
    let fake = Fake::start().expect("fake");
    fake.script("/version", Answer::new(500, ""));
    let releases = fake.releases(AGENT).expect("client");
    let (found, _) = in_step(async |step| {
        step.token().cancel();
        releases.release(SB, Query::Latest, ASSET, step).await
    })
    .await;
    assert_eq!(found.expect_err("stopped").to_string(), "interrupted");
}

/// Production's client builds (no request is sent).
#[test]
fn the_production_client_builds() {
    assert!(Releases::production(AGENT).is_ok());
}

async fn tag(releases: &Releases) -> (Result<Release, Error>, String) {
    in_step(async |step| {
        releases
            .release(SB, Query::Tag("0.9.1".into()), ASSET, step)
            .await
    })
    .await
}

/// A tag query takes only the tag asked for: another release from the
/// proxy is unusable (GitHub is asked), from GitHub an error.
#[tokio::test]
async fn a_tag_query_takes_only_that_tag() {
    let fake = Fake::start().expect("fake");
    fake.script(
        "/version",
        Answer::json(release(
            "0.1.0",
            false,
            false,
            &[asset(&fake, "0.1.0", ASSET, DIGEST)],
        )),
    );
    let tagged = "/api/repos/saltyorg/sb-go/releases/tags/0.9.1";
    fake.script(
        tagged,
        Answer::json(release(
            "0.9.1",
            false,
            false,
            &[asset(&fake, "0.9.1", ASSET, DIGEST)],
        )),
    );
    let releases = fake.releases(AGENT).expect("client");
    let (found, screen) = tag(&releases).await;
    assert_eq!(found.expect("found").tag, "0.9.1");
    assert_eq!(notices(&screen), (1, 1), "{screen}");
    let fake = Fake::start().expect("fake");
    let other = release(
        "\\u001b[2J0.1.0",
        false,
        false,
        &[asset(&fake, "0.1.0", ASSET, DIGEST)],
    );
    fake.script("/version", Answer::json(other.clone()));
    fake.script(tagged, Answer::json(other));
    let releases = fake.releases(AGENT).expect("client");
    assert_eq!(
        tag(&releases).await.0.expect_err("wrong tag").to_string(),
        "release 0.9.1 answered as 0.1.0"
    );
}

/// A redirect to plain http is refused (`https_only`); the proxy's
/// refusal falls back.
#[tokio::test]
async fn a_redirect_to_http_is_refused() {
    let fake = Fake::start().expect("fake");
    let plain = fake.url().replace("https://", "http://");
    fake.script(
        "/version",
        Answer::new(302, "").header("location", &format!("{plain}/elsewhere")),
    );
    fake.script(
        LATEST,
        Answer::new(302, "").header("location", &format!("{plain}/elsewhere")),
    );
    let releases = fake.releases(AGENT).expect("client");
    let (found, screen) = latest(&releases).await;
    let err = found.expect_err("refused").to_string();
    assert!(err.starts_with("fetch releases: "), "{err}");
    assert!(
        err.contains("; fallback GitHub API request failed: fetch releases: "),
        "{err}"
    );
    assert!(screen.contains("SVM proxy request failed ("), "{screen}");
    assert!(!fake.seen().iter().any(|s| s.target == "/elsewhere"));
    // The warning gives the cause, not the proxied URL (the refused
    // target is part of the cause).
    assert!(!screen.contains("/version?url"), "{screen}");
}

/// Build metadata doesn't make a release newer (sb-go's comparison).
#[tokio::test]
async fn build_metadata_is_not_newer() {
    let fake = Fake::start().expect("fake");
    fake.script(
        "/version",
        Answer::json(format!(
            "[{}]",
            release(
                "v1.2.3+build.5",
                false,
                false,
                &[asset(&fake, "v1.2.3+build.5", ASSET, DIGEST)]
            )
        )),
    );
    let releases = fake.releases(AGENT).expect("client");
    let (found, _) = in_step(async |step| {
        releases
            .newest(SB, &Version::new(1, 2, 3), ASSET, step)
            .await
    })
    .await;
    assert_eq!(found.expect("ok"), None);
}

/// A remote tag in an error is made safe.
#[tokio::test]
async fn a_remote_tag_in_an_error_is_made_safe() {
    let fake = Fake::start().expect("fake");
    fake.script("/version", Answer::new(500, ""));
    fake.script(
        LATEST,
        Answer::json(release("\\u001b]0;x\\u0007v1", false, false, &[])),
    );
    let releases = fake.releases(AGENT).expect("client");
    assert_eq!(
        latest(&releases).await.0.expect_err("missing").to_string(),
        "release v1 is missing sb_linux_amd64"
    );
}

/// A testing build reaches only the fake its bases file names, and
/// refuses to run without one: it never falls back to GitHub.
#[tokio::test]
async fn a_testing_build_uses_the_bases_file_only() {
    let fake = Fake::start().expect("fake");
    fake.script(
        "/version",
        Answer::json(release(
            "v1.2.3",
            false,
            false,
            &[asset(&fake, "v1.2.3", ASSET, DIGEST)],
        )),
    );
    let dir = tempfile::tempdir().expect("dir");
    let file = dir.path().join("bases.json");
    fake.write_bases(&file).expect("written");
    let child = std::env::var_os("SB_RELEASE_BASES_CHILD");
    if child.is_none() {
        // The variable is process-wide: the checks run in re-runs of this
        // test, with and without it.
        for (bases, expected) in [
            (Some(&file), "found v1.2.3"),
            (
                None,
                "refused: build the HTTP client: a testing build needs SB_RELEASE_TEST_BASES",
            ),
        ] {
            let mut run = std::process::Command::new(std::env::current_exe().expect("binary"));
            run.args([
                "a_testing_build_uses_the_bases_file_only",
                "--exact",
                "--nocapture",
            ])
            .env("SB_RELEASE_BASES_CHILD", "1")
            .env_remove(sb_release::testing::BASES_VARIABLE);
            if let Some(bases) = bases {
                run.env(sb_release::testing::BASES_VARIABLE, bases);
            }
            let out = run.output().expect("child");
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(out.status.success(), "{out:?}");
            assert!(stdout.contains(expected), "{stdout}");
        }
        return;
    }
    match Releases::for_sb(AGENT) {
        Ok(releases) => {
            let (found, _) = latest(&releases).await;
            println!("found {}", found.expect("found").tag);
        }
        Err(err) => println!("refused: {err}"),
    }
}
