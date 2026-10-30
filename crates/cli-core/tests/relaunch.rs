//! The root relaunch execs `sudo` (spec: startup step 3; success
//! criterion 3).
//!
//! The test re-runs this binary as the child with a fake `sudo` first on
//! `PATH`. The fake records its arguments and exits 7, so the parent sees
//! what sudo was given and that its exit code became the child's own (exec
//! replaces the process; nothing of the child runs afterwards).

use std::path::Path;
use std::process::Command;

use sb_cli_core::root::relaunch_as_root;

const CHILD: &str = "SB_RELAUNCH_CHILD";

#[test]
fn relaunch_execs_sudo_with_the_same_arguments() {
    if let Some(record) = std::env::var_os(CHILD) {
        // In the child: relaunch with arguments that must reach sudo as is.
        let err = relaunch_as_root(
            Path::new("/usr/local/bin/sb"),
            &["update".into(), "--yes".into()],
        );
        // Only reached if exec failed.
        std::fs::write(record, format!("exec failed: {err}")).expect("write");
        std::process::exit(99);
    }
    let dir = tempfile::tempdir().expect("temp dir");
    let record = dir.path().join("args");
    let sudo = dir.path().join("sudo");
    std::fs::write(
        &sudo,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" \"SB_RELAUNCHED=$SB_RELAUNCHED\" > \"$SB_RELAUNCH_CHILD\"\nexit 7\n",
    )
    .expect("fake sudo");
    std::fs::set_permissions(&sudo, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("chmod");
    let path = format!(
        "{}:{}",
        dir.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let status = Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "relaunch_execs_sudo_with_the_same_arguments",
            "--exact",
            "--quiet",
        ])
        .env(CHILD, &record)
        .env("PATH", path)
        .status()
        .expect("run child");
    assert_eq!(status.code(), Some(7), "exit code is sudo's own");
    assert_eq!(
        std::fs::read_to_string(&record).expect("args recorded"),
        "--preserve-env=VISUAL,EDITOR,SB_RELAUNCHED\n/usr/local/bin/sb\nupdate\n--yes\nSB_RELAUNCHED=1\n"
    );
}
