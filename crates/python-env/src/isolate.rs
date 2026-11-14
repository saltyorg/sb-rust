//! The isolated child (python-env spec, "Child isolation"; sb-go's
//! `python/runtime.go`, bb69839; decision 11): every uv, Python and
//! entry-point child gets sb-go's environment rules, rebuilt on sb-exec's
//! `Cmd`. The rules are a pure function of the caller's environment
//! ([`changes`]); [`Env::isolated`] applies them to a `Cmd` with a fresh
//! scratch `TMPDIR`.
//!
//! The caller's environment is changed, not replaced: the variables the
//! rules drop are removed one by one, so the run log names only what
//! changed (exec's `Cmd::env_clear` would log every variable sb has).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

use sb_exec::Cmd;
use tempfile::TempDir;

use crate::Env;

/// sb-go's `standardSystemPath`.
const SYSTEM_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// The scratch directory's prefix (spec, difference 9).
const SCRATCH_PREFIX: &str = "sb-python-";

/// The empty Ansible config an `ansible` check gets, in its scratch
/// directory.
const ANSIBLE_CONFIG: &str = "ansible.cfg";

/// Which rules a child gets (sb-go's `managedCommandKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind<'a> {
    /// uv or uvx: `--no-config` first and `UV_CACHE_DIR`. `cache` creates
    /// the cache directory first (every uv call but a `--version` probe,
    /// as sb-go's `isVersionProbe`).
    Uv { cache: bool },
    /// A Python interpreter: `-I` first.
    Python,
    /// A venv's entry point (`ansible`, `certbot`, `apprise`): run in the
    /// venv, its `bin` first in `PATH`, user site and the script's
    /// directory off `sys.path`; `ansible` also with every `ANSIBLE_*`
    /// variable removed and an empty `ANSIBLE_CONFIG`.
    Entrypoint { venv: &'a Path, ansible: bool },
}

/// Why an isolated child couldn't be prepared, in sb-go's words.
#[derive(Debug, thiserror::Error)]
pub enum IsolateError {
    /// A relative certificate path needs the caller's working directory,
    /// which couldn't be read.
    #[error(
        "resolve {name} relative to current working directory: read current working directory for relative managed Python path: {cause}"
    )]
    CallerDir { name: String, cause: io::Error },
    /// The scratch `TMPDIR` couldn't be made.
    #[error("create managed Python scratch directory in {}: {cause}", dir.display())]
    Scratch { dir: PathBuf, cause: io::Error },
    /// uv's cache directory couldn't be made.
    #[error("create managed uv cache {}: {cause}", dir.display())]
    Cache { dir: PathBuf, cause: io::Error },
    /// The empty Ansible config of an `ansible` health check couldn't be
    /// written.
    #[error("create isolated Ansible config {}: {cause}", path.display())]
    AnsibleConfig { path: PathBuf, cause: io::Error },
}

/// A child with its rules applied, and the scratch directory it uses as
/// `TMPDIR`, removed when this is dropped (after the call).
#[derive(Debug)]
pub(crate) struct Isolated {
    pub(crate) cmd: Cmd,
    scratch: TempDir,
}

impl Isolated {
    /// The scratch directory (tests).
    #[cfg(test)]
    fn scratch_path(&self) -> &Path {
        self.scratch.path()
    }

    /// The command with `more` applied (its own arguments, a timeout).
    #[must_use]
    pub(crate) fn map(self, more: impl FnOnce(Cmd) -> Cmd) -> Self {
        Self {
            cmd: more(self.cmd),
            scratch: self.scratch,
        }
    }
}

/// The environment changes for one child: the caller's variables removed,
/// then the ones set (a set name is never also in `removed`).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Changes {
    pub(crate) removed: Vec<String>,
    pub(crate) set: BTreeMap<String, String>,
}

/// What a child inherits that would steer it away from sb's own setup
/// (sb-go's `filteredManagedEnvironmentName`): uv's, pip's, Python's,
/// virtualenv's and conda's variables, but uv's two TLS choices, which
/// the host's certificate setup may need.
fn dropped(name: &str) -> bool {
    if name == "UV_SYSTEM_CERTS" || name == "UV_NATIVE_TLS" {
        return false;
    }
    name == "UV"
        || name == "__PYVENV_LAUNCHER__"
        || ["UV_", "PIP_", "PYTHON", "VIRTUAL_ENV", "CONDA_", "_CONDA_"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

/// A variable holding one certificate file (sb-go's
/// `isCertificatePathEnvironment`).
fn certificate_file(name: &str) -> bool {
    matches!(
        name,
        "SSL_CERT_FILE" | "SSL_CLIENT_CERT" | "REQUESTS_CA_BUNDLE" | "CURL_CA_BUNDLE"
    )
}

/// The rules for `kind` over the caller's variables `parent` (sb-go's
/// `managedEnvironment` and `run`): dropped variables removed; a relative
/// certificate path (each entry of `SSL_CERT_DIR`'s list) made absolute
/// against `caller_dir`, read only when one needs it; then `PWD`, `PATH`
/// and the scratch `TMPDIR`, `TMP` and `TEMP`; uv's cache; an entry
/// point's `PATH`, `PYTHONNOUSERSITE` and `PYTHONSAFEPATH`; an `ansible`
/// check's empty config. A variable whose name or value isn't UTF-8 is
/// left as it is: exec's `Cmd` names variables with `String`s (none of the
/// dropped names can be one on a host sb runs on).
pub(crate) fn changes(
    kind: Kind<'_>,
    parent: impl IntoIterator<Item = (OsString, OsString)>,
    mut caller_dir: impl FnMut() -> io::Result<PathBuf>,
    scratch: &Path,
    cache: &Path,
) -> Result<Changes, IsolateError> {
    let ansible = matches!(kind, Kind::Entrypoint { ansible: true, .. });
    let mut changes = Changes::default();
    let mut absolute = |name: &str, path: &str| -> Result<String, IsolateError> {
        if path.is_empty() || Path::new(path).is_absolute() {
            return Ok(path.to_string());
        }
        let dir = caller_dir().map_err(|cause| IsolateError::CallerDir {
            name: name.to_string(),
            cause,
        })?;
        Ok(dir.join(path).to_string_lossy().into_owned())
    };
    for (name, value) in parent {
        let Some(name) = name.to_str() else { continue };
        if dropped(name) || (ansible && name.starts_with("ANSIBLE_")) {
            changes.removed.push(name.to_string());
            continue;
        }
        let Some(value) = value.to_str() else {
            continue;
        };
        let resolved = if certificate_file(name) {
            absolute(name, value)?
        } else if name == "SSL_CERT_DIR" {
            let mut list = Vec::new();
            for path in value.split(':') {
                list.push(absolute(name, path)?);
            }
            list.join(":")
        } else {
            continue;
        };
        if resolved != value {
            changes.set.insert(name.to_string(), resolved);
        }
    }
    let config = scratch.join(ANSIBLE_CONFIG).to_string_lossy().into_owned();
    let scratch = scratch.to_string_lossy().into_owned();
    let mut set = |name: &str, value: String| {
        changes.set.insert(name.to_string(), value);
    };
    let cwd = match kind {
        Kind::Entrypoint { venv, .. } => venv.to_string_lossy().into_owned(),
        Kind::Uv { .. } | Kind::Python => "/".to_string(),
    };
    set("PWD", cwd);
    set("PATH", SYSTEM_PATH.to_string());
    set("TMPDIR", scratch.clone());
    set("TMP", scratch.clone());
    set("TEMP", scratch.clone());
    match kind {
        Kind::Uv { .. } => set("UV_CACHE_DIR", cache.to_string_lossy().into_owned()),
        Kind::Python => {}
        Kind::Entrypoint { venv, .. } => {
            set("PYTHONNOUSERSITE", "1".to_string());
            set("PYTHONSAFEPATH", "1".to_string());
            let bin = venv.join("bin");
            set("PATH", format!("{}:{SYSTEM_PATH}", bin.to_string_lossy()));
        }
    }
    if ansible {
        set("ANSIBLE_CONFIG", config);
    }
    let set = &changes.set;
    changes.removed.retain(|name| !set.contains_key(name));
    Ok(changes)
}

impl Env<'_> {
    /// `program` as a `kind` child: the rules over sb's own environment
    /// ([`changes`]), its working directory (`/`, an entry point's venv),
    /// `--no-config` (uv) or `-I` (Python) first, and a fresh scratch
    /// directory under the layout's `scratch`, 0700 as sb-go's
    /// `os.MkdirTemp` makes it (tempfile's `Builder`,
    /// <https://docs.rs/tempfile/3.27.0/tempfile/struct.Builder.html#method.tempdir_in>
    /// and
    /// <https://docs.rs/tempfile/3.27.0/tempfile/struct.Builder.html#method.permissions>:
    /// a directory is 0777 less the umask otherwise), that lives as long
    /// as the returned [`Isolated`]. uv's cache is made 0755 whatever the
    /// umask, as sb-go's `os.MkdirAll(cacheDir, 0o755)` (std's
    /// `DirBuilderExt::mode`,
    /// <https://doc.rust-lang.org/std/os/unix/fs/trait.DirBuilderExt.html#tymethod.mode>).
    ///
    /// # Errors
    ///
    /// [`IsolateError`] when the scratch directory, uv's cache or an
    /// `ansible` check's config can't be made, or a relative certificate
    /// path can't be resolved.
    pub(crate) fn isolated(
        &self,
        kind: Kind<'_>,
        program: &Path,
    ) -> Result<Isolated, IsolateError> {
        self.isolated_from(kind, program, std::env::vars_os(), std::env::current_dir)
    }

    /// [`Env::isolated`] over `parent` and `caller_dir` in place of sb's
    /// own environment and working directory (the tests seed them).
    fn isolated_from(
        &self,
        kind: Kind<'_>,
        program: &Path,
        parent: impl IntoIterator<Item = (OsString, OsString)>,
        caller_dir: impl FnMut() -> io::Result<PathBuf>,
    ) -> Result<Isolated, IsolateError> {
        let layout = self.layout;
        if let Kind::Uv { cache: true } = kind {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o755)
                .create(&layout.uv_cache)
                .map_err(|cause| IsolateError::Cache {
                    dir: layout.uv_cache.clone(),
                    cause,
                })?;
        }
        let scratch = scratch(layout)?;
        let changes = changes(kind, parent, caller_dir, scratch.path(), &layout.uv_cache)?;
        let mut cmd = Cmd::new(program.as_os_str());
        cmd = match kind {
            Kind::Uv { .. } => cmd.cwd("/").arg("--no-config"),
            Kind::Python => cmd.cwd("/").arg("-I"),
            Kind::Entrypoint { venv, ansible } => {
                if ansible {
                    let path = scratch.path().join(ANSIBLE_CONFIG);
                    write_private(&path)
                        .map_err(|cause| IsolateError::AnsibleConfig { path, cause })?;
                }
                cmd.cwd(venv)
            }
        };
        for name in changes.removed {
            cmd = cmd.env_remove(name);
        }
        for (name, value) in changes.set {
            cmd = cmd.env(name, value);
        }
        Ok(Isolated { cmd, scratch })
    }
}

/// A fresh scratch directory under `layout.scratch`, 0700 (sb-go's
/// `newScratch`): each child's `TMPDIR`, and the uv archive's download
/// (spec, step 2). Removed when dropped.
///
/// # Errors
///
/// [`IsolateError::Scratch`] when it can't be made.
pub(crate) fn scratch(layout: &crate::Layout) -> Result<TempDir, IsolateError> {
    tempfile::Builder::new()
        .prefix(SCRATCH_PREFIX)
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(&layout.scratch)
        .map_err(|cause| IsolateError::Scratch {
            dir: layout.scratch.clone(),
            cause,
        })
}

/// An empty file only its owner reads (sb-go writes it 0600), made new
/// (std's `OpenOptionsExt::mode`,
/// <https://doc.rust-lang.org/std/os/unix/fs/trait.OpenOptionsExt.html#tymethod.mode>).
fn write_private(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value)))
            .collect()
    }

    fn no_dir() -> io::Result<PathBuf> {
        Err(io::Error::other("no caller dir in this test"))
    }

    const SCRATCH: &str = "/tmp/sb-python-x";
    const CACHE: &str = "/var/cache/sb-go/uv";

    fn uv(parent: &[(&str, &str)]) -> Changes {
        changes(
            Kind::Uv { cache: true },
            vars(parent),
            || Ok(PathBuf::from("/work/caller")),
            Path::new(SCRATCH),
            Path::new(CACHE),
        )
        .expect("changes")
    }

    /// Every prefix sb-go drops, and the two uv TLS choices it keeps.
    #[test]
    fn steering_variables_are_removed_but_uvs_tls_choices() {
        let parent = [
            ("UV", "1"),
            ("UV_INDEX_URL", "x"),
            ("UV_PYTHON", "x"),
            ("PIP_INDEX_URL", "x"),
            ("PYTHONPATH", "x"),
            ("PYTHONHOME", "x"),
            ("VIRTUAL_ENV", "x"),
            ("VIRTUAL_ENV_PROMPT", "x"),
            ("CONDA_PREFIX", "x"),
            ("_CONDA_ROOT", "x"),
            ("__PYVENV_LAUNCHER__", "x"),
            ("UV_SYSTEM_CERTS", "1"),
            ("UV_NATIVE_TLS", "1"),
            ("UVICORN_HOST", "x"),
            ("HOME", "/root"),
            ("ANSIBLE_CONFIG", "x"),
        ];
        let got = uv(&parent);
        assert_eq!(
            got.removed,
            [
                "UV",
                "UV_INDEX_URL",
                "UV_PYTHON",
                "PIP_INDEX_URL",
                "PYTHONPATH",
                "PYTHONHOME",
                "VIRTUAL_ENV",
                "VIRTUAL_ENV_PROMPT",
                "CONDA_PREFIX",
                "_CONDA_ROOT",
                "__PYVENV_LAUNCHER__",
            ]
        );
        for kept in ["UV_SYSTEM_CERTS", "UV_NATIVE_TLS", "HOME", "UVICORN_HOST"] {
            assert!(!got.set.contains_key(kept), "{kept}");
        }
    }

    #[test]
    fn a_uv_child_gets_the_system_path_scratch_and_cache() {
        let got = uv(&[("PATH", "/work/caller/bin:/usr/bin")]);
        let want: BTreeMap<String, String> = [
            (
                "PATH",
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            ),
            ("PWD", "/"),
            ("TEMP", SCRATCH),
            ("TMP", SCRATCH),
            ("TMPDIR", SCRATCH),
            ("UV_CACHE_DIR", CACHE),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
        assert_eq!(got.set, want);
        assert_eq!(got.removed, Vec::<String>::new());
    }

    #[test]
    fn a_python_child_gets_no_uv_cache() {
        let got = changes(
            Kind::Python,
            vars(&[("UV_CACHE_DIR", "/elsewhere")]),
            no_dir,
            Path::new(SCRATCH),
            Path::new(CACHE),
        )
        .expect("changes");
        assert_eq!(got.removed, ["UV_CACHE_DIR"]);
        assert!(!got.set.contains_key("UV_CACHE_DIR"));
        assert_eq!(got.set["PWD"], "/");
    }

    /// Relative CA paths are made absolute against the caller's directory;
    /// absolute and empty ones are left alone; `SSL_CERT_DIR` entry by
    /// entry.
    #[test]
    fn relative_certificate_paths_are_made_absolute() {
        let got = uv(&[
            ("SSL_CERT_FILE", "certs/ca.pem"),
            ("SSL_CLIENT_CERT", "client.pem"),
            ("REQUESTS_CA_BUNDLE", "/etc/ssl/ca.pem"),
            ("CURL_CA_BUNDLE", ""),
            ("SSL_CERT_DIR", "/etc/ssl/certs:local::more"),
        ]);
        assert_eq!(got.set["SSL_CERT_FILE"], "/work/caller/certs/ca.pem");
        assert_eq!(got.set["SSL_CLIENT_CERT"], "/work/caller/client.pem");
        assert!(!got.set.contains_key("REQUESTS_CA_BUNDLE"));
        assert!(!got.set.contains_key("CURL_CA_BUNDLE"));
        assert_eq!(
            got.set["SSL_CERT_DIR"],
            "/etc/ssl/certs:/work/caller/local::/work/caller/more"
        );
    }

    /// The caller's directory is read only when a relative path needs it,
    /// and its failure is sb-go's text.
    #[test]
    fn the_caller_dir_is_read_only_when_needed() {
        let absolute = changes(
            Kind::Python,
            vars(&[("SSL_CERT_FILE", "/etc/ca.pem")]),
            no_dir,
            Path::new(SCRATCH),
            Path::new(CACHE),
        );
        assert!(absolute.is_ok());
        let err = changes(
            Kind::Python,
            vars(&[("SSL_CERT_DIR", "relative")]),
            no_dir,
            Path::new(SCRATCH),
            Path::new(CACHE),
        )
        .expect_err("no caller dir");
        assert_eq!(
            err.to_string(),
            "resolve SSL_CERT_DIR relative to current working directory: read current working directory for relative managed Python path: no caller dir in this test"
        );
    }

    #[test]
    fn an_entry_point_runs_in_its_venv_with_its_bin_first() {
        let venv = Path::new("/srv/ansible/releases/g/venv");
        let got = changes(
            Kind::Entrypoint {
                venv,
                ansible: false,
            },
            vars(&[("PYTHONNOUSERSITE", "0"), ("ANSIBLE_CONFIG", "/etc/x.cfg")]),
            no_dir,
            Path::new(SCRATCH),
            Path::new(CACHE),
        )
        .expect("changes");
        assert_eq!(got.set["PWD"], "/srv/ansible/releases/g/venv");
        assert_eq!(
            got.set["PATH"],
            "/srv/ansible/releases/g/venv/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
        );
        assert_eq!(got.set["PYTHONNOUSERSITE"], "1");
        assert_eq!(got.set["PYTHONSAFEPATH"], "1");
        // Set, so not also removed; ANSIBLE_* stays for the others.
        assert_eq!(got.removed, Vec::<String>::new());
        assert!(!got.set.contains_key("ANSIBLE_CONFIG"));
        assert!(!got.set.contains_key("UV_CACHE_DIR"));
    }

    /// The `ansible` check: every `ANSIBLE_*` gone, the config an empty
    /// file in the scratch directory.
    #[test]
    fn the_ansible_check_drops_ansible_variables() {
        let got = changes(
            Kind::Entrypoint {
                venv: Path::new("/v"),
                ansible: true,
            },
            vars(&[
                ("ANSIBLE_CONFIG", "/etc/x.cfg"),
                ("ANSIBLE_STDOUT_CALLBACK", "json"),
            ]),
            no_dir,
            Path::new(SCRATCH),
            Path::new(CACHE),
        )
        .expect("changes");
        assert_eq!(got.removed, ["ANSIBLE_STDOUT_CALLBACK"]);
        assert_eq!(got.set["ANSIBLE_CONFIG"], "/tmp/sb-python-x/ansible.cfg");
    }

    #[test]
    fn errors_name_what_failed() {
        let cause = || io::Error::other("denied");
        let texts = [
            (
                IsolateError::Scratch {
                    dir: "/tmp".into(),
                    cause: cause(),
                },
                "create managed Python scratch directory in /tmp: denied",
            ),
            (
                IsolateError::Cache {
                    dir: "/c".into(),
                    cause: cause(),
                },
                "create managed uv cache /c: denied",
            ),
            (
                IsolateError::AnsibleConfig {
                    path: "/a.cfg".into(),
                    cause: cause(),
                },
                "create isolated Ansible config /a.cfg: denied",
            ),
        ];
        for (err, want) in texts {
            assert_eq!(err.to_string(), want);
        }
    }
}

/// The rules as the child gets them: a recording stand-in run through the
/// built `Cmd`, from a seeded environment.
#[cfg(test)]
mod applied {
    use super::*;
    use crate::testing::{Tree, record_line, stand_in};

    /// The child's record: its argv line, its working directory and its
    /// environment, name to value; and the mode and size of the `ansible`
    /// check's config while the child ran, if there was one.
    struct Record {
        argv: String,
        cwd: String,
        env: BTreeMap<String, String>,
        config: Option<(u32, u64)>,
    }

    /// Run under the caller's hold of `SPAWNS`, from the seeded `parent`
    /// environment, or sb's own (the test process's) with `None`.
    async fn run(tree: &Tree, kind: Kind<'_>, parent: Option<&[(&str, &str)]>) -> Record {
        let dir = tree.root.path().join("record");
        let log = dir.join("log");
        let program = stand_in(&dir, "child", &record_line(&log));
        let env = tree.env();
        let isolated = match parent {
            Some(parent) => {
                let parent: Vec<(OsString, OsString)> = parent
                    .iter()
                    .map(|(name, value)| ((*name).into(), (*value).into()))
                    .collect();
                env.isolated_from(kind, &program, parent, || Ok(PathBuf::from("/caller")))
            }
            None => env.isolated(kind, &program),
        };
        let isolated = isolated
            .expect("isolated")
            .map(|cmd| cmd.arg("--version").value("a value"));
        let token = tokio_util::sync::CancellationToken::new();
        let ran = isolated.cmd.capture(&token).await.expect("spawn");
        let _ = ran.check().expect("ran");
        let scratch = isolated.scratch_path().to_path_buf();
        let mode = std::fs::metadata(&scratch)
            .expect("scratch")
            .permissions()
            .mode();
        assert_eq!(mode & 0o7777, 0o700, "the scratch is root's alone");
        let config = std::fs::metadata(scratch.join(ANSIBLE_CONFIG))
            .ok()
            .map(|meta| (meta.permissions().mode() & 0o7777, meta.len()));
        drop(isolated);
        assert!(!scratch.exists(), "the scratch goes after the call");
        let text = std::fs::read_to_string(&log).expect("record");
        let mut lines = text.lines();
        let argv = lines.next().expect("argv").to_string();
        let cwd = lines
            .next()
            .expect("cwd")
            .trim_start_matches("cwd: ")
            .to_string();
        let env = lines
            .take_while(|line| *line != "--")
            .filter_map(|line| line.split_once('='))
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        Record {
            argv,
            cwd,
            env,
            config,
        }
    }

    #[tokio::test]
    async fn uv_gets_no_config_first_its_cache_and_a_scratch_tmpdir() {
        let _spawns = crate::tests::SPAWNS.lock().await;
        let tree = Tree::new();
        let got = run(
            &tree,
            Kind::Uv { cache: true },
            Some(&[
                ("UV_INDEX_URL", "https://example.invalid/simple"),
                ("PIP_REQUIRE_VIRTUALENV", "1"),
            ]),
        )
        .await;
        assert!(
            got.argv.ends_with("/child --no-config --version a value"),
            "{}",
            got.argv
        );
        assert_eq!(got.cwd, "/");
        let cache = tree.layout.uv_cache.display().to_string();
        assert_eq!(got.env["UV_CACHE_DIR"], cache);
        let mode = std::fs::metadata(&tree.layout.uv_cache)
            .expect("the cache is made")
            .permissions()
            .mode();
        assert_eq!(mode & 0o7777, 0o755);
        assert_eq!(got.config, None);
        assert_eq!(got.env["PATH"], SYSTEM_PATH);
        let tmp = &got.env["TMPDIR"];
        let scratch = tree.layout.scratch.display().to_string();
        assert!(tmp.starts_with(&format!("{scratch}/sb-python-")), "{tmp}");
        assert_eq!(&got.env["TMP"], tmp);
        assert_eq!(&got.env["TEMP"], tmp);
        // Seeded but not sb's own: neither reaches the child.
        assert!(!got.env.contains_key("UV_INDEX_URL"));
        assert!(!got.env.contains_key("PIP_REQUIRE_VIRTUALENV"));
    }

    /// A probe leaves the cache alone (sb-go's `isVersionProbe`).
    #[tokio::test]
    async fn a_probe_makes_no_cache() {
        let _spawns = crate::tests::SPAWNS.lock().await;
        let tree = Tree::new();
        let got = run(&tree, Kind::Uv { cache: false }, Some(&[])).await;
        assert!(!tree.layout.uv_cache.exists());
        assert!(got.env.contains_key("UV_CACHE_DIR"));
    }

    #[tokio::test]
    async fn python_gets_isolated_mode_first() {
        let _spawns = crate::tests::SPAWNS.lock().await;
        let tree = Tree::new();
        let got = run(&tree, Kind::Python, Some(&[("SSL_CERT_FILE", "ca.pem")])).await;
        assert!(
            got.argv.ends_with("/child -I --version a value"),
            "{}",
            got.argv
        );
        assert_eq!(got.cwd, "/");
        assert_eq!(got.env["SSL_CERT_FILE"], "/caller/ca.pem");
        assert!(!got.env.contains_key("UV_CACHE_DIR"));
    }

    #[tokio::test]
    async fn ansible_runs_in_its_venv_with_an_empty_config() {
        let _spawns = crate::tests::SPAWNS.lock().await;
        let tree = Tree::new();
        let venv = tree.root.path().join("venv");
        std::fs::create_dir_all(venv.join("bin")).expect("venv");
        let got = run(
            &tree,
            Kind::Entrypoint {
                venv: &venv,
                ansible: true,
            },
            Some(&[("ANSIBLE_STDOUT_CALLBACK", "json")]),
        )
        .await;
        assert!(
            got.argv.ends_with("/child --version a value"),
            "{}",
            got.argv
        );
        assert_eq!(got.cwd, venv.display().to_string());
        assert_eq!(got.env["PYTHONNOUSERSITE"], "1");
        assert_eq!(got.env["PYTHONSAFEPATH"], "1");
        assert!(got.env["PATH"].starts_with(&format!("{}/bin:", venv.display())));
        let config = &got.env["ANSIBLE_CONFIG"];
        assert_eq!(
            Path::new(config),
            Path::new(&got.env["TMPDIR"]).join("ansible.cfg")
        );
        // There while the child ran, empty and root's alone: a missing
        // file would send Ansible to its other config locations.
        assert_eq!(got.config, Some((0o600, 0)));
    }

    /// Another entry point gets no config of its own.
    #[tokio::test]
    async fn certbot_gets_no_ansible_config() {
        let _spawns = crate::tests::SPAWNS.lock().await;
        let tree = Tree::new();
        let venv = tree.root.path().join("venv");
        std::fs::create_dir_all(venv.join("bin")).expect("venv");
        let kind = Kind::Entrypoint {
            venv: &venv,
            ansible: false,
        };
        let got = run(&tree, kind, Some(&[])).await;
        assert!(!got.env.contains_key("ANSIBLE_CONFIG"));
        assert_eq!(got.config, None);
    }

    /// Variables really inherited, not seeded: this test runs its own
    /// binary again (filtered to [`inherited_inner`]) with uv's, pip's,
    /// Python's and virtualenv's variables in its environment; the child
    /// of `Env::isolated` there must get none of them. The test process
    /// can't set them on itself: `std::env::set_var` is `unsafe` in Rust
    /// 2024 (<https://doc.rust-lang.org/std/env/fn.set_var.html>), and
    /// `unsafe` is forbidden here.
    #[test]
    fn inherited_steering_variables_never_reach_the_child() {
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "isolate::applied::inherited_inner",
                "--nocapture",
            ])
            .env(INNER, "1")
            .env("UV_INDEX_URL", "https://example.invalid/simple")
            .env("PIP_INDEX_URL", "https://example.invalid/simple")
            .env("PYTHONPATH", "/somewhere")
            .env("VIRTUAL_ENV", "/somewhere/venv")
            .env("UV_NATIVE_TLS", "1")
            .output()
            .expect("run the inner test");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The variable telling [`inherited_inner`] it runs for
    /// [`inherited_steering_variables_never_reach_the_child`].
    const INNER: &str = "SB_PYTHON_ENV_INHERITED_TEST";

    /// The inner half of
    /// [`inherited_steering_variables_never_reach_the_child`]; passes at
    /// once in any other run.
    #[tokio::test]
    async fn inherited_inner() {
        if std::env::var_os(INNER).is_none() {
            return;
        }
        let _spawns = crate::tests::SPAWNS.lock().await;
        let tree = Tree::new();
        let got = run(&tree, Kind::Uv { cache: false }, None).await;
        for name in ["UV_INDEX_URL", "PIP_INDEX_URL", "PYTHONPATH", "VIRTUAL_ENV"] {
            assert!(!got.env.contains_key(name), "{name} reached the child");
        }
        assert_eq!(got.env["UV_NATIVE_TLS"], "1");
        assert_eq!(got.env[INNER], "1");
    }

    #[tokio::test]
    async fn a_missing_scratch_parent_is_named() {
        let _spawns = crate::tests::SPAWNS.lock().await;
        let tree = Tree::new();
        std::fs::remove_dir(&tree.layout.scratch).expect("rm scratch");
        let err = tree
            .env()
            .isolated(Kind::Python, Path::new("/bin/true"))
            .expect_err("no scratch");
        assert!(
            err.to_string().starts_with(&format!(
                "create managed Python scratch directory in {}: ",
                tree.layout.scratch.display()
            )),
            "{err}"
        );
    }
}
