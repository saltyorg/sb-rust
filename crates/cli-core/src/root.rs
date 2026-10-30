//! Root relaunch for commands that need it (spec: startup step 3).
//!
//! `--help`, `version` and completion run as the calling user. Every other
//! command, when not root, replaces sb with
//! `sudo --preserve-env=VISUAL,EDITOR,SB_RELAUNCHED <exe> <args>`. sb-go
//! spawns sudo as a child and forwards its exit code (host/system.go
//! `RelaunchAsRoot`); sb-rust execs instead, so signals and the exit code
//! belong to the real run and nothing of the first process is left. If
//! sudo asks for a password, the user answers its own prompt.
//!
//! `SB_RELAUNCHED=1` marks the relaunched run. If sudo runs sb as someone
//! other than root (a sudoers rule with another run-as user), the marked
//! run stops with an error instead of relaunching again: each relaunch is
//! a new sudo process, so the loop piles them up. sb-go has no such guard.
//! Why not the `sudo` crate: it hardcodes /usr/bin/sudo, spawns rather than
//! execs and can't pass `--preserve-env`.

use std::ffi::{OsStr, OsString};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

/// Whether to relaunch through sudo: the command needs root and the
/// effective user isn't root.
pub fn needs_relaunch(needs_root: bool, euid: u32) -> bool {
    needs_root && euid != 0
}

/// This process's effective user ID (rustix, no unsafe libc call).
pub fn effective_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// The environment variable marking a relaunched run.
pub const RELAUNCHED: &str = "SB_RELAUNCHED";

/// sudo's arguments: keep the user's editor choice and the relaunch marker
/// (sudo filters the environment), then sb and its arguments unchanged.
pub fn sudo_args(exe: &Path, args: &[OsString]) -> Vec<OsString> {
    let mut all = vec![
        OsString::from(format!("--preserve-env=VISUAL,EDITOR,{RELAUNCHED}")),
        exe.into(),
    ];
    all.extend(args.iter().cloned());
    all
}

/// The relaunch couldn't make this run root.
#[derive(Debug, thiserror::Error)]
pub enum RelaunchError {
    #[error("could not relaunch as root through sudo")]
    Exec(#[source] std::io::Error),
    #[error("relaunched through sudo but still not root (euid {0})")]
    StillNotRoot(u32),
}

/// Startup step 3: when the command needs root and this isn't root, hand
/// this executable and its arguments to `exec` ([`relaunch_as_root`] in
/// sb), which only returns on failure. `relaunched` says this run already
/// came through sudo (`SB_RELAUNCHED` is set). `Ok` means run the command
/// here.
pub fn ensure_root(
    needs_root: bool,
    euid: u32,
    relaunched: bool,
    exec: impl FnOnce(&Path, &[OsString]) -> std::io::Error,
) -> Result<(), RelaunchError> {
    if !needs_relaunch(needs_root, euid) {
        return Ok(());
    }
    if relaunched {
        return Err(RelaunchError::StillNotRoot(euid));
    }
    let exe = std::env::current_exe().map_err(RelaunchError::Exec)?;
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    Err(RelaunchError::Exec(exec(&exe, &args)))
}

/// Replace this process with sudo running sb again (found on `PATH`, as
/// sb-go does). Returns only if the exec failed
/// (https://doc.rust-lang.org/std/os/unix/process/trait.CommandExt.html#tymethod.exec).
pub fn relaunch_as_root(exe: &Path, args: &[OsString]) -> std::io::Error {
    Command::new(OsStr::new("sudo"))
        .args(sudo_args(exe, args))
        .env(RELAUNCHED, "1")
        .exec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::path::Path;

    /// The kernel makes `/proc/self` belong to the process's effective
    /// user, an independent view of the same ID.
    #[test]
    fn effective_uid_is_this_process_owner() {
        use std::os::unix::fs::MetadataExt;
        let owner = std::fs::metadata("/proc/self").expect("/proc/self").uid();
        assert_eq!(effective_uid(), owner);
    }

    #[test]
    fn a_root_command_relaunches_when_not_root() {
        assert!(needs_relaunch(true, 1000));
    }

    #[test]
    fn root_itself_never_relaunches() {
        assert!(!needs_relaunch(true, 0));
    }

    #[test]
    fn an_unprivileged_command_never_relaunches() {
        assert!(!needs_relaunch(false, 1000));
    }

    /// Not root and the command needs it: exec is called with sb and its
    /// arguments; if exec fails, the error says so (sb-go: "Error
    /// relaunching as root").
    #[test]
    fn ensure_root_execs_and_reports_a_failed_exec() {
        let mut seen = None;
        let err = ensure_root(true, 1000, false, |exe, args| {
            seen = Some((exe.to_path_buf(), args.to_vec()));
            std::io::Error::from(std::io::ErrorKind::NotFound)
        })
        .expect_err("exec failed");
        let (exe, args) = seen.expect("exec called");
        assert_eq!(exe, std::env::current_exe().expect("exe"));
        assert_eq!(args, std::env::args_os().skip(1).collect::<Vec<_>>());
        assert_eq!(err.to_string(), "could not relaunch as root through sudo");
        assert_eq!(
            std::error::Error::source(&err).map(ToString::to_string),
            Some(std::io::Error::from(std::io::ErrorKind::NotFound).to_string())
        );
    }

    /// Root already, or a command that doesn't need it: run here, no exec.
    #[test]
    fn ensure_root_runs_here_when_no_relaunch_is_needed() {
        for (needs_root, euid) in [(true, 0), (false, 1000)] {
            let result = ensure_root(needs_root, euid, false, |_, _| {
                panic!("exec must not be called")
            });
            assert!(result.is_ok(), "{needs_root} {euid}");
        }
    }

    /// Already relaunched and still not root (sudo ran sb as someone else):
    /// stop with an error instead of relaunching forever. Each relaunch is
    /// a new sudo process, so a loop piles them up.
    #[test]
    fn a_relaunched_run_that_still_is_not_root_stops() {
        let err = ensure_root(true, 1000, true, |_, _| panic!("must not relaunch again"))
            .expect_err("refused");
        assert_eq!(
            err.to_string(),
            "relaunched through sudo but still not root (euid 1000)"
        );
    }

    /// sb-go: `sudo --preserve-env=VISUAL,EDITOR <exe> <args...>`, the
    /// arguments passed through unchanged (including ones that aren't UTF-8).
    #[test]
    fn sudo_gets_sb_go_arguments() {
        let odd = OsString::from_vec(b"x\xffy".to_vec());
        let args = sudo_args(
            Path::new("/usr/local/bin/sb"),
            &[OsString::from("install"), OsString::from("-e"), odd.clone()],
        );
        assert_eq!(
            args,
            [
                OsString::from("--preserve-env=VISUAL,EDITOR,SB_RELAUNCHED"),
                OsString::from("/usr/local/bin/sb"),
                OsString::from("install"),
                OsString::from("-e"),
                odd,
            ]
        );
    }
}
