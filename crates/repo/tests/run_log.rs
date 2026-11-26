//! What `fetch_and_reset` leaves in sb's run log, through cli-core's own
//! `RunLog`, as `sb` opens it: the process's global subscriber, the run's
//! span entered on the main thread, a current-thread runtime. A test
//! binary of its own: the global subscriber is set once per process, and
//! tracing caches each callsite's interest process-wide
//! (<https://docs.rs/tracing-core/0.1.36/tracing_core/callsite/index.html>).
//! Real git on temp dirs (`file://` only); nothing touches a Saltbox
//! checkout or the network.

use std::path::Path;
use std::process::Command;

use sb_cli_core::cancel::Shutdown;
use sb_cli_core::log::RunLog;
use sb_repo::Repo;
use sb_term_ui::{Labels, Options, Runner, Target};

/// `git <args>` in `dir`, which must succeed.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        // None of the host's git settings, as tests/update.rs's helper:
        // its global and system config files (commit signing, rewrites)
        // and its per-user ignore and attributes files stay out.
        // GIT_CONFIG_GLOBAL and GIT_CONFIG_SYSTEM set to /dev/null skip
        // the config files (git 2.32+):
        // https://git-scm.com/docs/git#Documentation/git.txt-GITCONFIGGLOBAL
        // The ignore and attributes files are read without any config,
        // from $XDG_CONFIG_HOME/git, unless these name others:
        // https://git-scm.com/docs/git-config#Documentation/git-config.txt-coreexcludesFile
        .args(["-c", "core.excludesFile=/dev/null"])
        .args(["-c", "core.attributesFile=/dev/null"])
        .args(["-c", "user.name=sb", "-c", "user.email=sb"])
        .args(["-c", "init.defaultBranch=master"])
        .args(args)
        .current_dir(dir)
        .env("LC_ALL", "C")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Every line of an update, which ends by giving the checkout to its
/// owner on the blocking pool, carries the run id (the maintainer,
/// 2026-11-15: grepping a run id missed the ownership records).
#[test]
fn every_line_of_an_update_carries_the_run_id() {
    // Ownership is given only as root (check.sh runs the tests so); root
    // owns the fresh files already, so nothing changes, and the record
    // is still written.
    if !sb_cli_core::testing::root_tests() {
        return;
    }
    let logs = tempfile::tempdir().expect("log dir");
    let (log, subscriber) = RunLog::open(logs.path()).expect("open");
    tracing::subscriber::set_global_default(subscriber).expect("global subscriber");
    let dir = tempfile::tempdir().expect("tempdir");
    let (work, checkout) = (dir.path().join("work"), dir.path().join("checkout"));
    let remote = format!("file://{}", dir.path().join("remote.git").display());
    git(dir.path(), &["init", "--quiet", "--bare", "remote.git"]);
    git(dir.path(), &["init", "--quiet", "work"]);
    std::fs::write(work.join("tracked.txt"), "one\n").expect("write");
    git(&work, &["add", "."]);
    git(&work, &["commit", "--quiet", "-m", "first"]);
    git(&work, &["push", "--quiet", &remote, "master"]);
    git(dir.path(), &["clone", "--quiet", &remote, "checkout"]);
    let repo = Repo::open("Sandbox", &checkout).expect("open");

    let _run = log.enter();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let (error, screen) = runtime.block_on(async {
        let term = indicatif::InMemoryTerm::new(80, 400);
        let options = Options {
            target: Target::InMemory {
                term: term.clone(),
                colour: false,
            },
        };
        let shutdown = Shutdown::new();
        let mut runner = Runner::start("sb test", &shutdown, None, options);
        let mut error = None;
        let labels = Labels::new("Updating Sandbox repository", "Sandbox repository updated");
        let _ = runner
            .step(labels, async |step| {
                let result = repo.fetch_and_reset("master", "root", step).await;
                error = result.as_ref().err().map(ToString::to_string);
                result
            })
            .await;
        runner.finish();
        (error, term.contents())
    });
    assert_eq!(error, None, "{screen}");

    let text = std::fs::read_to_string(logs.path().join("sb.log")).expect("log");
    let owned = format!("recursive ownership root={}", checkout.display());
    assert!(text.contains(&owned), "no {owned:?} line:\n{text}");
    let run = format!("run{{run={}}}", log.id());
    let without: Vec<&str> = text.lines().filter(|line| !line.contains(&run)).collect();
    assert!(
        without.is_empty(),
        "lines without {run}:\n{}",
        without.join("\n")
    );
}
