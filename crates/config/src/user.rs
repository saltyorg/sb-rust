//! The Saltbox user (config spec, Scope; sb-go's `GetSaltboxUser` in
//! host/system.go): `user.name` from accounts.yml, read without the
//! schema, so an unrelated issue elsewhere in the file doesn't stop
//! `update`, `branch` or python-env's commands.

use std::path::Path;

use sb_cli_core::config::{read_small, yaml};

use crate::parse;
use crate::scalar::Value;
use crate::tree::{self, Data, Tree};

/// accounts.yml, relative to the Saltbox root.
const ACCOUNTS: &str = "accounts.yml";

/// Why the Saltbox user couldn't be read, in sb-go's words. Callers add
/// their own wrapping (`error getting saltbox user: ...` in python-env's
/// commands, `get Saltbox user: ...` in setup).
#[derive(Debug, thiserror::Error)]
pub enum UserError {
    /// The file couldn't be read (missing included).
    #[error("failed to read accounts.yml: {0}")]
    Read(String),
    /// It isn't YAML Ansible would load, or its root isn't a mapping.
    #[error("failed to unmarshal accounts.yml: {0}")]
    Unmarshal(String),
    /// No `user` mapping.
    #[error("user section not found in accounts.yml")]
    NoSection,
    /// No `user.name` string.
    #[error("user.name not found in accounts.yml")]
    NoName,
}

/// `user.name` from `<root>/accounts.yml`, `root` being the Saltbox root
/// (`SB_SALTBOX_DIR` or `/srv/git/saltbox`).
///
/// The file is read as validate-config reads it: cli-core's `read_small`
/// (a regular UTF-8 file of at most 1 MiB, links followed as sb-go's) and sb-config's
/// YAML entry (Ansible's loader's refusals, yaml.v3's scalars;
/// ADR-022), but no schema rule. As sb-go's type assertions: `user` must
/// be a mapping and `name` a string (`name: 123` is no name). The name
/// isn't checked further: an empty one is returned as sb-go returns it,
/// and a caller hands it on as an argument (chown's owner), never through
/// a shell.
///
/// # Errors
///
/// [`UserError`], each with sb-go's text.
pub fn saltbox_user(root: &Path) -> Result<String, UserError> {
    let path = root.join(ACCOUNTS);
    let text = read_small(&path)
        .map_err(|err| UserError::Read(format!("{}: {err}", path.display())))?
        .ok_or_else(|| UserError::Read(format!("{}: no such file or directory", path.display())))?;
    user_in(&text)
}

/// `user.name` in accounts.yml's `text`.
fn user_in(text: &str) -> Result<String, UserError> {
    let unmarshal = |issues: Vec<crate::issue::Issue>| {
        UserError::Unmarshal(
            issues
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        )
    };
    let prepared = parse::prepare(ACCOUNTS, text).map_err(unmarshal)?;
    // A deep file is loaded, walked and dropped on yaml's deep stack, as
    // validate-config's are.
    yaml::with_stack(prepared.depth(), || {
        let root = parse::load(ACCOUNTS, &prepared).map_err(unmarshal)?;
        // No issue shows a value: accounts.yml holds secrets.
        let tree = Tree::new(tree::build(ACCOUNTS, &root, &|_| true).map_err(unmarshal)?);
        let user = tree.get("user").ok_or(UserError::NoSection)?;
        if !matches!(user.data, Data::Mapping(_)) {
            return Err(UserError::NoSection);
        }
        match user.get("name").map(|name| &name.data) {
            Some(Data::Scalar(Value::Str(name))) => Ok(name.clone()),
            _ => Err(UserError::NoName),
        }
    })
    .map_err(|err| UserError::Read(format!("can't start the YAML reader's thread: {err}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> Result<String, UserError> {
        user_in(text)
    }

    #[test]
    fn the_name_is_read() {
        assert_eq!(user("user:\n  name: seed\n").expect("user"), "seed");
        assert_eq!(user("user:\n  name: '123'\n").expect("user"), "123");
        assert_eq!(user("user:\n  name: ''\n").expect("user"), "");
    }

    #[test]
    fn an_unrelated_schema_issue_doesnt_stop_it() {
        let text = "user:\n  name: seed\n  email: not-an-email\n  domain: ''\n\
                    cloudflare:\n  api: [1, 2]\nunknown_key: true\n";
        assert_eq!(user(text).expect("user"), "seed");
    }

    #[test]
    fn no_user_section_is_sb_gos_text() {
        for text in [
            "",
            "other: 1\n",
            "user: seed\n",
            "user:\n  - name: seed\n",
            "user:\n",
        ] {
            let error = user(text).expect_err(text);
            assert_eq!(
                error.to_string(),
                "user section not found in accounts.yml",
                "{text:?}"
            );
        }
    }

    #[test]
    fn no_name_string_is_sb_gos_text() {
        for text in [
            "user:\n  email: a@b.c\n",
            "user:\n  name: 123\n",
            "user:\n  name: true\n",
            "user:\n  name:\n",
            "user:\n  name: [a]\n",
        ] {
            let error = user(text).expect_err(text);
            assert_eq!(
                error.to_string(),
                "user.name not found in accounts.yml",
                "{text:?}"
            );
        }
    }

    #[test]
    fn yaml_ansible_refuses_is_the_unmarshal_error() {
        for text in [
            "user: [\n",
            "- a\n- b\n",
            "user:\n  name: a\nuser:\n  name: b\n",
        ] {
            let error = user(text).expect_err(text);
            assert!(
                error
                    .to_string()
                    .starts_with("failed to unmarshal accounts.yml: accounts.yml:"),
                "{error}"
            );
        }
    }

    #[test]
    fn a_tag_error_never_shows_the_value() {
        let error =
            user("user:\n  name: seed\n  pass: !!int not-a-real-secret-7\n").expect_err("tag");
        let text = error.to_string();
        assert!(
            text.starts_with("failed to unmarshal accounts.yml: "),
            "{text}"
        );
        assert!(!text.contains("not-a-real-secret-7"), "{text}");
    }

    #[test]
    fn a_deep_file_is_read_on_the_deep_stack() {
        let text = format!(
            "user:\n  name: seed\ndeep:\n  {}v\n",
            "- ".repeat(yaml::MAX_DEPTH - 1)
        );
        assert_eq!(user(&text).expect("user"), "seed");
    }

    #[test]
    fn the_file_is_read_under_the_root() {
        let root = tempfile::tempdir().expect("root");
        std::fs::write(root.path().join(ACCOUNTS), "user:\n  name: seed\n").expect("write");
        assert_eq!(saltbox_user(root.path()).expect("user"), "seed");
    }

    #[test]
    fn a_missing_file_is_the_read_error() {
        let root = tempfile::tempdir().expect("root");
        let error = saltbox_user(root.path()).expect_err("missing");
        assert_eq!(
            error.to_string(),
            format!(
                "failed to read accounts.yml: {}/accounts.yml: no such file or directory",
                root.path().display()
            )
        );
    }

    #[test]
    fn an_unreadable_file_is_the_read_error() {
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir(root.path().join(ACCOUNTS)).expect("dir");
        let error = saltbox_user(root.path()).expect_err("a directory");
        assert!(
            error.to_string().starts_with(&format!(
                "failed to read accounts.yml: {}/accounts.yml: ",
                root.path().display()
            )),
            "{error}"
        );
    }
}
