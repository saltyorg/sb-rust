//! Minimal facts model: one INI file per app (role), sections are copies
//! (instances). Deliberately simpler than sb-go's facts package (no locks, no
//! drift detection); the prototype is about the UI. Edits are line-based so
//! untouched lines keep their formatting.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub struct Fact {
    pub key: String,
    pub value: String,
}

pub struct Instance {
    pub name: String,
    pub facts: Vec<Fact>,
}

pub struct Role {
    pub name: String,
    pub path: PathBuf,
    pub instances: Vec<Instance>,
}

/// (role, instance, fact) indices into the loaded catalog.
pub type FactId = (usize, usize, usize);

/// What a pending change applies to: a whole app, one copy, or one setting.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Target {
    App(usize),
    Copy(usize, usize),
    Setting(FactId),
}

#[derive(Clone)]
pub enum Change {
    Set(String),
    Delete,
}

pub fn load(dir: &Path) -> Result<Vec<Role>> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("read {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "ini"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path)?;
            let name = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            Ok(Role {
                name,
                instances: parse(&text),
                path,
            })
        })
        .collect()
}

fn parse(text: &str) -> Vec<Instance> {
    let mut instances: Vec<Instance> = Vec::new();
    let mut last_indent: Option<usize> = None;
    for raw in text.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            last_indent = None;
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();
        if let (Some(prev), Some(inst)) = (last_indent, instances.last_mut())
            && indent > prev
            && let Some(fact) = inst.facts.last_mut()
        {
            fact.value.push('\n');
            fact.value.push_str(trimmed);
            continue;
        }
        if let Some(name) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            instances.push(Instance {
                name: name.to_string(),
                facts: Vec::new(),
            });
            last_indent = None;
        } else if let (Some((k, v)), Some(inst)) = (trimmed.split_once('='), instances.last_mut()) {
            inst.facts.push(Fact {
                key: k.trim().to_string(),
                value: v.trim().to_string(),
            });
            last_indent = Some(indent);
        }
    }
    instances
}

/// Applies pending changes. A deleted app removes its file; a deleted copy
/// removes its section; settings are rewritten line by line (temp file +
/// rename). Returns the number of files touched.
pub fn apply(roles: &[Role], pending: &BTreeMap<Target, Change>) -> Result<usize> {
    let mut touched = 0;
    for (ri, role) in roles.iter().enumerate() {
        if matches!(pending.get(&Target::App(ri)), Some(Change::Delete)) {
            std::fs::remove_file(&role.path)?;
            touched += 1;
            continue;
        }
        let relevant = pending.keys().any(|t| match t {
            Target::App(r) | Target::Copy(r, _) | Target::Setting((r, _, _)) => *r == ri,
        });
        if !relevant {
            continue;
        }
        let text = std::fs::read_to_string(&role.path)?;
        let mut out = String::with_capacity(text.len());
        let mut section = None::<usize>;
        let mut dropping_section = false;
        let mut skipping_continuation: Option<usize> = None;
        for raw in text.split_inclusive('\n') {
            let trimmed = raw.trim();
            let indent = raw.len() - raw.trim_start().len();
            if let Some(name) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                section = role.instances.iter().position(|i| i.name == name);
                dropping_section = section.is_some_and(|si| {
                    matches!(pending.get(&Target::Copy(ri, si)), Some(Change::Delete))
                });
                skipping_continuation = None;
            }
            if dropping_section {
                continue;
            }
            if let Some(base) = skipping_continuation {
                if !trimmed.is_empty() && indent > base {
                    continue;
                }
                skipping_continuation = None;
            }
            if let (Some(si), Some((k, _))) = (section, trimmed.split_once('='))
                && !trimmed.starts_with('[')
            {
                let key = k.trim();
                let fi = role.instances[si].facts.iter().position(|f| f.key == key);
                if let Some(change) = fi.and_then(|fi| pending.get(&Target::Setting((ri, si, fi))))
                {
                    skipping_continuation = Some(indent);
                    if let Change::Set(value) = change {
                        let newline = if raw.ends_with('\n') { "\n" } else { "" };
                        let lead = &raw[..indent];
                        let continuation = format!("\n{lead}    ");
                        let value = value.replace('\n', &continuation);
                        out.push_str(&format!("{lead}{key} = {value}{newline}"));
                    }
                    continue;
                }
            }
            out.push_str(raw);
        }
        replace_file(&role.path, out.as_bytes())?;
        touched += 1;
    }
    Ok(touched)
}

/// Atomically replaces `path` with `contents`, keeping its owner and mode,
/// as sb-go does (facts/apply.go, facts/session.go): fact files can hold
/// secrets, so a save must never widen who can read them.
///
/// - A fact file that is a symlink is refused, like sb-go, rather than
///   replacing the link with a regular file while its target stays as it was.
/// - The temp file comes from tempfile: unpredictable name, created
///   exclusively with mode 0600 in the same directory, named like sb-go's
///   (`.<name>-XXXXXX.tmp`), and removed again if anything fails.
///   https://docs.rs/tempfile/3.27.0/tempfile/struct.Builder.html#method.tempfile_in
///   https://docs.rs/tempfile/3.27.0/tempfile/struct.NamedTempFile.html#method.persist
/// - Owner, then mode, are copied with std (`fchown`, then
///   `File::set_permissions`, an `fchmod`), owner first because changing it
///   can clear special mode bits. Why not tempfile's `Builder::permissions`:
///   it applies the mode at creation, masked by the umask, so it can't
///   reproduce the original exactly. Why not the atomic-write-file crate: its
///   default owner preservation silently ignores EPERM, where sb-go (and this)
///   fails the save.
/// - Data is synced before the rename and the directory after it; tempfile's
///   `persist` syncs neither. If only the directory sync fails, the file has
///   already been replaced, and the error says so.
fn replace_file(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::MetadataExt as _;
    let original = std::fs::symlink_metadata(path)?;
    if !original.file_type().is_file() {
        bail!(
            "{} must be a regular file, not a symlink or other special file",
            path.display()
        );
    }
    let dir = path.parent().context("fact file has no directory")?;
    let name = path.file_stem().unwrap_or_default().to_string_lossy();
    let mut temp = tempfile::Builder::new()
        .prefix(&format!(".{name}-"))
        .suffix(".tmp")
        .tempfile_in(dir)?;
    std::os::unix::fs::fchown(temp.as_file(), Some(original.uid()), Some(original.gid()))
        .context("keep the fact file's owner")?;
    temp.as_file().set_permissions(original.permissions())?;
    temp.write_all(contents)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    std::fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .with_context(|| {
            format!(
                "{} was saved, but syncing its directory failed",
                path.display()
            )
        })?;
    Ok(())
}

pub fn looks_secret(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    ["pass", "secret", "token", "key", "api"]
        .iter()
        .any(|w| k.contains(w))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Abuse case: saving must not widen the permissions of a file that
    /// holds secrets, or leave copies of it behind.
    #[test]
    fn saving_keeps_the_file_mode_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("demo.ini");
        std::fs::write(&path, "[demo]\napi_key = old\nother = x\n").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        let roles = load(dir.path()).expect("load");
        let mut pending = BTreeMap::new();
        pending.insert(Target::Setting((0, 0, 0)), Change::Set("new".into()));

        assert_eq!(apply(&roles, &pending).expect("apply"), 1);

        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "mode widened to {mode:o}");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "[demo]\napi_key = new\nother = x\n"
        );
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .expect("list")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("demo.ini")]);
    }

    /// Abuse case: a fact file that is a symlink is refused, as sb-go does
    /// (facts/session.go), instead of being replaced by a regular file while
    /// the link's target stays unchanged.
    #[test]
    fn saving_refuses_a_symlinked_fact_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("elsewhere.conf");
        std::fs::write(&target, "[demo]\nkey = old\n").expect("write");
        let link = dir.path().join("demo.ini");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let roles = load(dir.path()).expect("load");
        let mut pending = BTreeMap::new();
        pending.insert(Target::Setting((0, 0, 0)), Change::Set("new".into()));

        let err = apply(&roles, &pending).expect_err("a symlink must be refused");
        assert!(format!("{err:#}").contains("symlink"), "{err:#}");
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("stat")
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(&target).expect("read"),
            "[demo]\nkey = old\n"
        );
    }

    /// Saving keeps the file's owner. Handing a file to another user needs
    /// root; without root the file keeps the current user, which is still
    /// checked.
    #[test]
    fn saving_keeps_the_file_owner() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("demo.ini");
        std::fs::write(&path, "[demo]\nkey = old\n").expect("write");
        const NOBODY: u32 = 65534;
        let _ = std::os::unix::fs::chown(&path, Some(NOBODY), Some(NOBODY));
        let before = std::fs::metadata(&path).expect("stat");
        let roles = load(dir.path()).expect("load");
        let mut pending = BTreeMap::new();
        pending.insert(Target::Setting((0, 0, 0)), Change::Set("new".into()));
        apply(&roles, &pending).expect("apply");
        let after = std::fs::metadata(&path).expect("stat");
        assert_eq!((after.uid(), after.gid()), (before.uid(), before.gid()));
    }
}
