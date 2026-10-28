//! Prototype: dynamic completion of comma-separated Ansible tags with
//! clap_complete's runtime engine (`unstable-dynamic`). Throwaway; see FINDINGS.md.

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};
use clap_complete::env::CompleteEnv;

#[derive(Parser)]
#[command(name = "proto-complete")]
struct Cli {
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Run playbooks for the given comma-separated tags.
    Install {
        #[arg(add = ArgValueCompleter::new(complete_tags))]
        tags: String,
    },
    /// A second command so subcommand completion is visible too.
    Update,
}

/// Stand-in for the tag cache (sb-go reads Saltbox's cache.json).
const TAGS: &[&str] = &[
    "core", "plex", "radarr", "sonarr", "sabnzbd", "settings", "traefik",
];
const SANDBOX_TAGS: &[&str] = &["jellyseerr", "komga", "sonarr4k"];

/// Completes the last element of a comma-separated list, keeping the prefix,
/// and skipping tags already chosen.
fn complete_tags(current: &std::ffi::OsStr) -> Vec<CompletionCandidate> {
    let Some(current) = current.to_str() else {
        return Vec::new();
    };
    let (done, partial) = current
        .rsplit_once(',')
        .map_or(("", current), |(d, p)| (d, p));
    let chosen: Vec<&str> = done.split(',').filter(|s| !s.is_empty()).collect();
    let prefix = if done.is_empty() {
        String::new()
    } else {
        format!("{done},")
    };
    TAGS.iter()
        .map(|t| (*t).to_string())
        .chain(SANDBOX_TAGS.iter().map(|t| format!("sandbox-{t}")))
        .filter(|t| t.starts_with(partial) && !chosen.contains(&t.as_str()))
        .map(|t| CompletionCandidate::new(format!("{prefix}{t}")))
        .collect()
}

fn main() {
    // Answers completion requests when the shell calls us with COMPLETE=<shell>.
    CompleteEnv::with_factory(Cli::command).complete();
    match Cli::parse().action {
        Action::Install { tags } => println!("would install: {tags}"),
        Action::Update => println!("would update"),
    }
}
