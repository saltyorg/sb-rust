//! A generation's `manifest.json` (python-env spec, steps 4 and 11; sb-go's
//! `python/venv.go`, `Manifest`, `readManifest`, `writeManifest`): schema
//! 1, sb-go's fields in sb-go's order (69b968b's `requirements_unhashed`
//! last), so either binary reads the other's (decision 7).

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The schema sb-go and sb write.
pub const SCHEMA: u32 = 1;

/// The file's name in a generation directory.
pub const FILE: &str = "manifest.json";

/// A generation's record, field for field sb-go's (venv.go:42-56). Serde
/// writes the fields in declaration order (sb-go's), and
/// `python_install` is left out when empty and `requirements_unhashed`
/// when false (Go's `omitempty`; serde's `default` and
/// `skip_serializing_if`, <https://serde.rs/field-attrs.html>).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub generation_id: String,
    pub python_version: String,
    pub python_path: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub python_install: String,
    pub uv_version: String,
    pub lock_sha256: String,
    pub saltbox_commit: String,
    pub created_at: String,
    pub exported_commands: Vec<String>,
    /// An install from a checkout without a pinned Python, unhashed
    /// (ADR-031, D7). A manifest without it was hashed: every manifest
    /// written before the field.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub requirements_unhashed: bool,
}

/// Why a manifest couldn't be read, in sb-go's words.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("read generation manifest {}: {cause}", path.display())]
    Read { path: PathBuf, cause: io::Error },
    #[error("parse generation manifest {}: {cause}", path.display())]
    Parse {
        path: PathBuf,
        cause: serde_json::Error,
    },
}

/// What a manifest file holds, every field optional: a missing one makes
/// the generation unhealthy (spec, Testing Strategy), where Go's
/// `json.Unmarshal` would leave a zero value.
#[derive(Debug, Default, Deserialize)]
struct Stored {
    schema_version: Option<u32>,
    generation_id: Option<String>,
    python_version: Option<String>,
    python_path: Option<String>,
    python_install: Option<String>,
    uv_version: Option<String>,
    lock_sha256: Option<String>,
    saltbox_commit: Option<String>,
    created_at: Option<String>,
    exported_commands: Option<Vec<String>>,
    requirements_unhashed: Option<bool>,
}

impl Stored {
    /// The manifest, when every field but `python_install` and
    /// `requirements_unhashed` is there.
    fn complete(self) -> Option<Manifest> {
        Some(Manifest {
            schema_version: self.schema_version?,
            generation_id: self.generation_id?,
            python_version: self.python_version?,
            python_path: self.python_path?,
            python_install: self.python_install.unwrap_or_default(),
            uv_version: self.uv_version?,
            lock_sha256: self.lock_sha256?,
            saltbox_commit: self.saltbox_commit?,
            created_at: self.created_at?,
            exported_commands: self.exported_commands?,
            requirements_unhashed: self.requirements_unhashed.unwrap_or_default(),
        })
    }
}

/// The manifest in `generation` (sb-go's `readManifest`): `None` when a
/// field is missing.
///
/// # Errors
///
/// [`ManifestError`] when it can't be read or isn't JSON of the right
/// shape.
pub fn read(generation: &Path) -> Result<Option<Manifest>, ManifestError> {
    let path = generation.join(FILE);
    let text = std::fs::read(&path).map_err(|cause| ManifestError::Read {
        path: path.clone(),
        cause,
    })?;
    let stored: Stored =
        serde_json::from_slice(&text).map_err(|cause| ManifestError::Parse { path, cause })?;
    Ok(stored.complete())
}

/// Why a manifest couldn't be written, in sb-go's words.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("encode generation manifest: {0}")]
    Encode(serde_json::Error),
    #[error("create generation manifest: {0}")]
    Create(io::Error),
    #[error("write generation manifest: {0}")]
    Write(io::Error),
    #[error("chmod generation manifest: {0}")]
    Chmod(io::Error),
    #[error("activate generation manifest: {0}")]
    Activate(io::Error),
}

/// `manifest` into `generation` as sb-go's `writeManifest` writes it:
/// `json.MarshalIndent`'s two-space form and a newline (`serde_json`'s
/// pretty printer gives the same bytes, see the tests), through a
/// temporary `.manifest-*.json` beside it, mode 0644 set on the open file
/// (why not tempfile's `Builder::permissions`: the umask applies to it,
/// not to sb-go's chmod), then renamed over `manifest.json` (tempfile's `NamedTempFile::persist`,
/// <https://docs.rs/tempfile/3.27.0/tempfile/struct.NamedTempFile.html#method.persist>).
/// Go's encoder also escapes `<`, `>`, `&`, U+2028 and U+2029 in
/// strings; no field sb writes holds one (paths under `/srv`, versions, hex and command
/// names).
///
/// # Errors
///
/// [`WriteError`]: the first part that fails; the temporary file goes.
pub fn write(generation: &Path, manifest: &Manifest) -> Result<(), WriteError> {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;
    let mut text = serde_json::to_string_pretty(manifest).map_err(WriteError::Encode)?;
    text.push('\n');
    let mut file = tempfile::Builder::new()
        .prefix(".manifest-")
        .suffix(".json")
        .tempfile_in(generation)
        .map_err(WriteError::Create)?;
    file.write_all(text.as_bytes()).map_err(WriteError::Write)?;
    file.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o644))
        .map_err(WriteError::Chmod)?;
    file.persist(generation.join(FILE))
        .map_err(|err| WriteError::Activate(err.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// sb-go's `TestManifestRoundTrip` values, as its `json.MarshalIndent`
    /// writes them (two-space indent, a final newline).
    const SB_GO: &str = r#"{
  "schema_version": 1,
  "generation_id": "20260807T120000.000000000-0123456789ab",
  "python_version": "3.12.13",
  "python_path": "/srv/python/releases/python-3-12-13/bin/python3.12",
  "python_install": "/srv/python/releases/python-3-12-13",
  "uv_version": "0.12.3",
  "lock_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "saltbox_commit": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
  "created_at": "2026-08-07T12:00:00Z",
  "exported_commands": [
    "ansible",
    "ansible-playbook",
    "apprise",
    "certbot"
  ]
}
"#;

    fn sb_go() -> Manifest {
        Manifest {
            schema_version: 1,
            generation_id: "20260807T120000.000000000-0123456789ab".into(),
            python_version: "3.12.13".into(),
            python_path: "/srv/python/releases/python-3-12-13/bin/python3.12".into(),
            python_install: "/srv/python/releases/python-3-12-13".into(),
            uv_version: "0.12.3".into(),
            lock_sha256: "a".repeat(64),
            saltbox_commit: "b".repeat(40),
            created_at: "2026-08-07T12:00:00Z".into(),
            exported_commands: ["ansible", "ansible-playbook", "apprise", "certbot"]
                .map(String::from)
                .to_vec(),
            requirements_unhashed: false,
        }
    }

    fn written(text: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(FILE), text).expect("write");
        dir
    }

    #[test]
    fn sb_gos_manifest_is_read() {
        let dir = written(SB_GO);
        assert_eq!(read(dir.path()).expect("read"), Some(sb_go()));
    }

    /// `serde_json`'s pretty printer and Go's `MarshalIndent` agree on
    /// these values: the same fields, order and indent. (Go also escapes
    /// `<`, `>`, `&`, U+2028 and U+2029, which no value here holds.)
    #[test]
    fn the_fields_serialize_in_sb_gos_order() {
        let mut text = serde_json::to_string_pretty(&sb_go()).expect("json");
        text.push('\n');
        assert_eq!(text, SB_GO);
    }

    /// Go's `omitempty`: an empty Python install is left out, and a
    /// manifest without it is read as empty.
    #[test]
    fn an_empty_python_install_is_left_out() {
        let manifest = Manifest {
            python_install: String::new(),
            ..sb_go()
        };
        let text = serde_json::to_string_pretty(&manifest).expect("json");
        assert!(!text.contains("python_install"), "{text}");
        let dir = written(&text);
        assert_eq!(read(dir.path()).expect("read"), Some(manifest));
    }

    /// sb-go 69b968b's `requirements_unhashed,omitempty`: an unhashed
    /// install is recorded after the other fields, and a manifest without
    /// the field (every one written before it, sb-go's and sb's) was a
    /// hashed install.
    #[test]
    fn an_unhashed_install_is_recorded_last_and_its_absence_is_hashed() {
        let unhashed = Manifest {
            requirements_unhashed: true,
            ..sb_go()
        };
        let mut text = serde_json::to_string_pretty(&unhashed).expect("json");
        text.push('\n');
        let want = SB_GO.replace("  ]\n}\n", "  ],\n  \"requirements_unhashed\": true\n}\n");
        assert_eq!(text, want);
        let dir = written(&text);
        assert_eq!(read(dir.path()).expect("read"), Some(unhashed));
        let dir = written(SB_GO);
        let read = read(dir.path()).expect("read").expect("complete");
        assert!(!read.requirements_unhashed);
    }

    /// A missing field makes the generation unhealthy, not an error.
    #[test]
    fn a_missing_field_reads_as_incomplete() {
        for field in [
            "schema_version",
            "python_version",
            "lock_sha256",
            "exported_commands",
        ] {
            let mut value: serde_json::Value = serde_json::from_str(SB_GO).expect("json");
            value.as_object_mut().expect("object").remove(field);
            let dir = written(&value.to_string());
            assert_eq!(read(dir.path()).expect(field), None, "{field}");
        }
    }

    /// `write` gives sb-go's bytes, 0644, and leaves no temporary file.
    #[test]
    fn the_written_manifest_is_sb_gos_bytes() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        write(dir.path(), &sb_go()).expect("written");
        let path = dir.path().join(FILE);
        assert_eq!(std::fs::read_to_string(&path).expect("read"), SB_GO);
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o7777, 0o644);
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .expect("list")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(names, [FILE]);
    }

    /// A manifest that can't take its place (a directory there) fails
    /// its activation, and the temporary file goes.
    #[test]
    fn a_manifest_that_cant_replace_its_place_leaves_no_temporary() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(FILE).join("taken")).expect("in the way");
        let err = write(dir.path(), &sb_go()).expect_err("not activated");
        assert!(matches!(err, WriteError::Activate(_)), "{err:?}");
        assert!(
            err.to_string()
                .starts_with("activate generation manifest: "),
            "{err}"
        );
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .expect("list")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(names, [FILE]);
    }

    #[test]
    fn a_manifest_in_a_missing_generation_is_named() {
        let err = write(Path::new("/nonexistent/generation"), &sb_go()).expect_err("missing");
        assert!(
            err.to_string().starts_with("create generation manifest: "),
            "{err}"
        );
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let text = SB_GO.replacen('{', "{\n  \"later\": true,", 1);
        let dir = written(&text);
        assert_eq!(read(dir.path()).expect("read"), Some(sb_go()));
    }

    #[test]
    fn a_bad_manifest_is_named() {
        let dir = written("{ not json");
        let err = read(dir.path()).expect_err("bad json");
        let path = dir.path().join(FILE);
        assert!(
            err.to_string()
                .starts_with(&format!("parse generation manifest {}: ", path.display())),
            "{err}"
        );
        std::fs::remove_file(&path).expect("rm");
        let err = read(dir.path()).expect_err("missing");
        assert!(
            err.to_string()
                .starts_with(&format!("read generation manifest {}: ", path.display())),
            "{err}"
        );
    }
}
