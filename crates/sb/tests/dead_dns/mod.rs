//! sb with a host name lookup that hangs (config-L11, Task 60): the
//! lookup runs on a tokio blocking thread (hyper-util's `GaiResolver`),
//! which the runtime's drop would wait for. The host's network and
//! resolver settings are never touched: sb runs in private network and
//! mount namespaces whose only route leads into a dummy interface, which
//! drops every packet unanswered, and whose `/etc/resolv.conf` names a
//! nameserver behind it (TEST-NET-1, RFC 5737) with a 20 s timeout;
//! `/etc/nsswitch.conf` looks host names up in files and DNS only. Root
//! runs (`SB_TEST_ROOT=1`). The namespaces are made by `unshare`, `ip`
//! and `mount`, as `host_check.rs` does (a pure-Rust namespace needs
//! `pre_exec`, which is unsafe).

use std::process::Command;

use tempfile::TempDir;

/// The namespaces' `/etc` files, kept while sb runs.
pub struct DeadDns {
    etc: TempDir,
}

impl DeadDns {
    /// The files, after checking that namespaces can be made here.
    pub fn new() -> Self {
        let probe = Command::new("unshare")
            .args(["--mount", "--net", "true"])
            .output()
            .expect("run unshare");
        assert!(probe.status.success(), "no namespaces: {probe:?}");
        let etc = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("etc");
        std::fs::write(
            etc.path().join("resolv.conf"),
            "nameserver 192.0.2.1\noptions timeout:20 attempts:1\n",
        )
        .expect("write resolv.conf");
        std::fs::write(etc.path().join("nsswitch.conf"), "hosts: files dns\n")
            .expect("write nsswitch.conf");
        Self { etc }
    }

    /// sb with `args`, in the namespaces, with sb's test environment
    /// (`common::apply_sb_env`); the caller sets the rest. `unshare`
    /// without `--fork` execs the shell, which execs sb: the child's pid
    /// is sb's.
    pub fn sb(&self, args: &[&str]) -> Command {
        let script = r#"ip link add dead type dummy && ip addr add 192.0.2.2/24 dev dead && ip link set dead up && mount --bind "$1" /etc/resolv.conf && mount --bind "$2" /etc/nsswitch.conf && shift 2 && exec "$@""#;
        let mut cmd = Command::new("unshare");
        crate::common::apply_sb_env(&mut cmd);
        cmd.args([
            "--mount",
            "--propagation",
            "private",
            "--net",
            "sh",
            "-c",
            script,
            "sh",
        ])
        .arg(self.etc.path().join("resolv.conf"))
        .arg(self.etc.path().join("nsswitch.conf"))
        .arg(crate::common::sb().get_program())
        .args(args);
        cmd
    }
}

/// How many lookups sb (`pid`) has open: its namespace's UDP sockets sent
/// to the dead nameserver (192.0.2.1:53, as /proc/net/udp writes it; no
/// dependency in the tree reads that file, and a crate for one test line
/// isn't worth it).
pub fn lookups(pid: u32) -> usize {
    std::fs::read_to_string(format!("/proc/{pid}/net/udp"))
        .unwrap_or_default()
        .matches(" 010200C0:0035 ")
        .count()
}
