//! A generation's `manifest.json` (python-env spec, steps 4 and 11; sb-go's
//! `python/venv.go`, `Manifest`, `readManifest`, `writeManifest`): schema
//! 1, sb-go's fields in sb-go's order, so either binary reads the other's
//! (decision 7).

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The schema sb-go and sb write.
pub const SCHEMA: u32 = 1;

/// The file's name in a generation directory.
pub const FILE: &str = "manifest.json";

/// A generation's record, field for field sb-go's (venv.go:42-53). Serde
/// writes the fields in declaration order (sb-go's), and
/// `python_install` is left out when empty (Go's `omitempty`; serde's
/// `default` and `skip_serializing_if`,
/// <https://serde.rs/field-attrs.html>).
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
}

impl Stored {
    /// The manifest, when every field but `python_install` is there.
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
