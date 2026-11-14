//! Saltbox and Sandbox checkouts for `sb install`'s tests (ansible spec,
//! step 5): each a git repository with one commit, so sb-repo reads its
//! HEAD, and a tag cache valid for both, so no listing runs unless a test
//! asks for one. Real git on temp dirs; no Ansible, nothing under `/srv`.

use std::path::Path;
use std::process::Command;

/// The tags each seeded cache holds.
pub const SALTBOX_TAGS: [&str; 3] = ["always", "plex", "sonarr"];
pub const SANDBOX_TAGS: [&str; 3] = ["always", "b", "tautulli"];

/// `git <args>` in `dir`, which must succeed; its stdout, trimmed.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=sb", "-c", "user.email=sb"])
        .args(["-c", "init.defaultBranch=master"])
        .args(args)
        .current_dir(dir)
        .env("LC_ALL", "C")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// `dir` (created if missing) as a repository with one commit; its HEAD.
pub fn checkout(dir: &Path) -> String {
    std::fs::create_dir_all(dir).expect("checkout");
    git(dir, &["init", "--quiet"]);
    std::fs::write(dir.join("roles.txt"), "one").expect("write");
    git(dir, &["add", "roles.txt"]);
    git(dir, &["commit", "--quiet", "-m", "one"]);
    git(dir, &["rev-parse", "HEAD"])
}

/// Both checkouts, and `<saltbox>/cache.json` valid for their HEADs with
/// [`SALTBOX_TAGS`] and [`SANDBOX_TAGS`], keyed by path as sb-go writes it.
pub fn seed(saltbox: &Path, sandbox: &Path) {
    let (saltbox_head, sandbox_head) = (checkout(saltbox), checkout(sandbox));
    let cache = serde_json::json!({
        saltbox.display().to_string(): {"commit": saltbox_head, "tags": SALTBOX_TAGS},
        sandbox.display().to_string(): {"commit": sandbox_head, "tags": SANDBOX_TAGS},
    });
    std::fs::write(saltbox.join("cache.json"), cache.to_string()).expect("cache");
}

/// `dir`'s listing for the fake's `--list-tags`, as a file it reads in the
/// repository it runs in (untracked: HEAD doesn't change).
pub fn listing(dir: &Path, tags: &[&str]) {
    std::fs::create_dir_all(dir.join(".fake-ansible")).expect("settings");
    std::fs::write(
        dir.join(".fake-ansible/FAKE_ANSIBLE_LISTING"),
        format!("playbook: x.yml\n      TASK TAGS: [{}]\n", tags.join(", ")),
    )
    .expect("listing");
}
