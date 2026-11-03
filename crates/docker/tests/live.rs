//! The local Docker daemon, read-only (docker spec, Testing strategy):
//! opt-in with `SB_LIVE_DOCKER=1`, capped with systemd-run, one host at a
//! time. It connects as `sb docker ps` does (DOCKER_HOST or the default
//! socket), asks the version, lists and inspects every container, and on
//! success prints only counts: nothing from a real host is committed.

use sb_docker::connect;

#[tokio::test]
async fn the_local_daemon_lists_and_inspects_every_container() {
    // Any value but `1` (`true`, `yes`) looks like asking: refused rather
    // than quietly skipped, as sb's root gate does.
    let Some(value) = std::env::var_os("SB_LIVE_DOCKER") else {
        eprintln!("not run: set SB_LIVE_DOCKER=1 to read the local Docker daemon");
        return;
    };
    assert_eq!(
        value, "1",
        "SB_LIVE_DOCKER must be 1 to read the local daemon"
    );
    let env = |name| std::env::var(name).ok();
    let address = connect::address(
        env("DOCKER_HOST").as_deref(),
        env("DOCKER_TLS_VERIFY").as_deref(),
        env("DOCKER_CERT_PATH").as_deref(),
    )
    .expect("a supported DOCKER_HOST");
    let daemon = connect::connect(&address)
        .await
        .expect("the daemon answers");
    assert!(daemon.api_version.is_some(), "no API version");
    let listing = connect::list(&daemon).await.expect("the list");
    assert!(
        listing.failures.is_empty(),
        "{} inspects failed",
        listing.failures.len()
    );
    eprintln!(
        "live: API {}, {} containers listed and inspected",
        daemon.api_version.as_deref().unwrap_or("?"),
        listing.rows.len()
    );
}
