//! cli-core's spawn helpers carry the caller's span onto the new thread
//! (cli-core spec, "Logging": every record carries the run id), through
//! the run log as `sb` opens it. A test binary of its own: the global
//! subscriber is set once per process, and a thread sees only the
//! global one (a `with_default` subscriber is the setting thread's). The
//! events use the target `sb`, which the run log records (it keeps `sb*` at DEBUG).

use sb_cli_core::log::RunLog;
use sb_cli_core::spawn;

#[test]
fn each_helper_runs_its_work_in_the_callers_span() {
    let dir = tempfile::tempdir().expect("log dir");
    let (log, subscriber) = RunLog::open(dir.path()).expect("open");
    tracing::subscriber::set_global_default(subscriber).expect("global subscriber");
    let _run = log.enter();

    spawn::thread(std::thread::Builder::new(), || {
        tracing::info!(target: "sb", "from a thread");
    })
    .expect("thread")
    .join()
    .expect("joined");
    let borrowed = String::from("from a scoped thread");
    std::thread::scope(|scope| {
        spawn::scoped_thread(std::thread::Builder::new(), scope, || {
            tracing::info!(target: "sb", "{borrowed}");
        })
        .expect("scoped thread")
        .join()
        .expect("joined");
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    runtime
        .block_on(async {
            spawn::blocking(|| tracing::info!(target: "sb", "from the blocking pool")).await
        })
        .expect("blocking");

    let text = std::fs::read_to_string(dir.path().join("sb.log")).expect("log");
    let run = format!("run{{run={}}}", log.id());
    for message in [
        "from a thread",
        "from a scoped thread",
        "from the blocking pool",
    ] {
        let line = text
            .lines()
            .find(|line| line.contains(message))
            .unwrap_or_else(|| panic!("no {message:?} line:\n{text}"));
        assert!(line.contains(&run), "{message:?} without {run}: {line}");
    }
}
