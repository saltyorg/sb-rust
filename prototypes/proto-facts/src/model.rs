//! Minimal facts model: one INI file per role, sections are instances.
//! Deliberately simpler than sb-go's facts package (no locks, no drift
//! detection); the prototype is about the UI. Edits are line-based so
//! untouched lines keep their formatting.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

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

#[derive(Clone)]
pub enum Pending {
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

/// Applies pending changes by rewriting only the affected lines of each role
/// file (temp file + rename). Returns the paths written.
pub fn apply(roles: &[Role], pending: &BTreeMap<FactId, Pending>) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    for (ri, role) in roles.iter().enumerate() {
        let edits: Vec<_> = pending.iter().filter(|((r, _, _), _)| *r == ri).collect();
        if edits.is_empty() {
            continue;
        }
        let text = std::fs::read_to_string(&role.path)?;
        let mut out = String::with_capacity(text.len());
        let mut section = None::<usize>;
        let mut skipping_continuation: Option<usize> = None;
        for raw in text.split_inclusive('\n') {
            let trimmed = raw.trim();
            let indent = raw.len() - raw.trim_start().len();
            if let Some(base) = skipping_continuation {
                if !trimmed.is_empty() && indent > base {
                    continue;
                }
                skipping_continuation = None;
            }
            if let Some(name) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                section = role.instances.iter().position(|i| i.name == name);
            } else if let (Some(si), Some((k, _))) = (section, trimmed.split_once('=')) {
                let key = k.trim();
                let hit = edits.iter().find(|((_, i, f), _)| {
                    *i == si
                        && role.instances[si]
                            .facts
                            .get(*f)
                            .is_some_and(|fact| fact.key == key)
                });
                if let Some((_, change)) = hit {
                    skipping_continuation = Some(indent);
                    if let Pending::Set(value) = change {
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
        let tmp = role.path.with_extension("ini.tmp");
        std::fs::write(&tmp, out)?;
        std::fs::rename(&tmp, &role.path)?;
        written.push(role.path.clone());
    }
    Ok(written)
}

pub fn looks_secret(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    ["pass", "secret", "token", "key", "api"]
        .iter()
        .any(|w| k.contains(w))
}
