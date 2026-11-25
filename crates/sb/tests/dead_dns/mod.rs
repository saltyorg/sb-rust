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
//! `pre_exec`, which is unsafe). Their stdout and stderr, and sb's, go to
//! files beside the `/etc` files, so a run that ends early names its
//! cause ([`DeadDns::output`]).
//!
//! The two files reach the namespace's `/etc` through an overlay on it,
//! whose upper layer, a tmpfs of the namespace's own, holds them: a name
//! in the upper layer hides the lower layer's, whatever that is
//! (<https://docs.kernel.org/filesystems/overlayfs.html#directories>),
//! and the lower layer, the host's `/etc`, need not be writable
//! (<https://docs.kernel.org/filesystems/overlayfs.html#upper-and-lower>).
//! Why not `mount --bind` onto each file: the kernel follows a symlink at
//! a mount point, and a runner's `/etc/resolv.conf` is systemd-resolved's
//! link into `/run`, which check.sh's sandbox covers with an empty
//! read-only tmpfs; the link leads nowhere there, and mount(8) refuses it
//! ("mount point is a symbolic link to nowhere", exit 32), nor can its
//! target be made under that `/run`
//! (docs/platform-notes/mount-symlink-targets.md). The lower layer is
//! `/etc`'s file system without the mounts on it: in a container, its
//! image's `/etc/hosts` and `/etc/hostname`, not Docker's.

use std::fs::File;
use std::process::Command;

use tempfile::TempDir;

/// The namespaces' `/etc` files, kept while sb runs.
pub struct DeadDns {
    etc: TempDir,
}

impl DeadDns {
    /// The files, after checking that namespaces can be made here.
    ///
    /// # Panics
    ///
    /// When namespaces can't be made, the files can't be written, or the
    /// target directory's path can't be named in an overlay's options.
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
        // The overlay's layers, on a tmpfs mounted here in the namespace.
        std::fs::create_dir(etc.path().join("layer")).expect("layer");
        // An overlay's mount options are split at commas, with a
        // backslash as their escape; this path goes in as it is.
        let path = etc.path().to_str().expect("a UTF-8 temporary directory");
        assert!(
            !path.contains([',', '\\']),
            "an overlay can't name {path}: move the target directory"
        );
        Self { etc }
    }

    /// sb with `args`, in the namespaces, with sb's test environment
    /// (`common::apply_sb_env`), its stdout and stderr in files read by
    /// [`Self::output`]; the caller sets the rest. `unshare` without
    /// `--fork` execs the shell, which execs sb: the child's pid is sb's.
    /// `-n`: no `/run/mount/utab` entry, as check.sh's sandbox mounts.
    ///
    /// # Panics
    ///
    /// When the output files can't be made.
    pub fn sb(&self, args: &[&str]) -> Command {
        let script = r#"set -e
ip link add dead type dummy
ip addr add 192.0.2.2/24 dev dead
ip link set dead up
mount -n -t tmpfs -o mode=0755,size=1m sb-dead-dns "$1/layer"
mkdir "$1/layer/upper" "$1/layer/work"
cp "$1/resolv.conf" "$1/nsswitch.conf" "$1/layer/upper/"
mount -n -t overlay -o "lowerdir=/etc,upperdir=$1/layer/upper,workdir=$1/layer/work" sb-dead-dns /etc
shift
exec "$@""#;
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
        .arg(self.etc.path())
        .arg(crate::common::sb().get_program())
        .args(args)
        .stdout(File::create(self.etc.path().join("stdout")).expect("stdout file"))
        .stderr(File::create(self.etc.path().join("stderr")).expect("stderr file"));
        cmd
    }

    /// What the run wrote to stdout and stderr (unshare's, `ip`'s and
    /// `mount`'s refusals, then sb's), for a failure message.
    pub fn output(&self) -> String {
        let read = |name| {
            String::from_utf8_lossy(&std::fs::read(self.etc.path().join(name)).unwrap_or_default())
                .into_owned()
        };
        format!("stdout:\n{}\nstderr:\n{}", read("stdout"), read("stderr"))
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
