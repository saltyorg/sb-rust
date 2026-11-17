//! The two checkouts ops' commands work on (ops spec, assumption 7):
//! Saltbox's, at the Saltbox root, and Sandbox's, under the inventory's
//! appdata path. One type for the branch commands, `update` and the
//! announcements (module build M85: `branch::Checkout`, M79, and
//! `announcements::Repo`, M74, merged).

use std::path::{Path, PathBuf};

use sb_cli_core::config;

/// Which checkout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checkout {
    /// Saltbox's own checkout, the Saltbox directory itself.
    Saltbox,
    /// The Sandbox checkout, `<server_appdata_path>/sandbox`.
    Sandbox,
}

impl Checkout {
    /// Its name in labels, headings, the migration table and errors
    /// (sb-go's).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Saltbox => "Saltbox",
            Self::Sandbox => "Sandbox",
        }
    }

    /// The checkout `--repo` names, exactly as sb-go spells it.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "Saltbox" => Some(Self::Saltbox),
            "Sandbox" => Some(Self::Sandbox),
            _ => None,
        }
    }

    /// Its branch command, for the runner's log record.
    #[must_use]
    pub fn command(self) -> &'static str {
        match self {
            Self::Saltbox => "sb branch",
            Self::Sandbox => "sb branch-sandbox",
        }
    }

    /// Its playbook repository for ansible (the tags cache, the
    /// migrations), at [`Checkout::path`].
    #[must_use]
    pub fn playbooks(self, root: &Path) -> sb_ansible::Repo {
        match self {
            Self::Saltbox => sb_ansible::Repo::saltbox(root),
            Self::Sandbox => {
                sb_ansible::Repo::sandbox(&config::appdata_path(&config::inventory_path(root)))
            }
        }
    }

    /// Where it is, under the Saltbox directory `root`: `root` itself, or
    /// the Sandbox under the inventory's appdata path (as sb finds its
    /// theme; cli-core's `appdata_path`).
    #[must_use]
    pub fn path(self, root: &Path) -> PathBuf {
        match self {
            Self::Saltbox => root.to_path_buf(),
            Self::Sandbox => config::appdata_path(&config::inventory_path(root)).join("sandbox"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_checkout_has_sb_gos_name_and_its_command() {
        assert_eq!(Checkout::Saltbox.name(), "Saltbox");
        assert_eq!(Checkout::Sandbox.name(), "Sandbox");
        assert_eq!(Checkout::Saltbox.command(), "sb branch");
        assert_eq!(Checkout::Sandbox.command(), "sb branch-sandbox");
        for checkout in [Checkout::Saltbox, Checkout::Sandbox] {
            assert_eq!(Checkout::from_name(checkout.name()), Some(checkout));
        }
        assert_eq!(Checkout::from_name("saltbox"), None);
    }

    #[test]
    fn the_saltbox_checkout_is_the_root_and_the_sandbox_is_under_appdata() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("saltbox");
        std::fs::create_dir_all(root.join("inventories/host_vars")).expect("inventory dir");
        std::fs::write(
            root.join("inventories/host_vars/localhost.yml"),
            "server_appdata_path: /data/appdata\n",
        )
        .expect("inventory");
        assert_eq!(Checkout::Saltbox.path(&root), root);
        assert_eq!(
            Checkout::Sandbox.path(&root),
            Path::new("/data/appdata/sandbox")
        );
        // ansible's playbooks are in the same two places.
        for checkout in [Checkout::Saltbox, Checkout::Sandbox] {
            assert_eq!(checkout.playbooks(&root).dir(), checkout.path(&root));
        }
        assert_eq!(
            Checkout::Sandbox.playbooks(&root).kind(),
            sb_ansible::Kind::Sandbox
        );
    }
}
