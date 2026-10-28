//! Prototype: the non-TUI look. comfy-table for tables, anstyle/anstream for
//! styled lines, dialoguer for prompts; on a TTY, piped, and with NO_COLOR.
//! Throwaway; see FINDINGS.md.

use std::io::IsTerminal;

use anstream::ColorChoice;
use anstyle::{AnsiColor, Style};
use anyhow::{Result, bail};
use bollard::Docker;
use bollard::query_parameters::ListContainersOptions;
use clap::{Parser, Subcommand};
use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::{Cell, Color, ContentArrangement, Table};
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, Password};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// docker ps style table.
    Ps {
        /// Only containers whose name contains this text.
        #[arg(long, default_value = "")]
        filter: String,
    },
    /// restore style prompts.
    Prompt,
}

const OK: Style = AnsiColor::Green.on_default().bold();
const WARN: Style = AnsiColor::Yellow.on_default();
const ERR: Style = AnsiColor::Red.on_default().bold();

/// One decision for every output path: anstream honours NO_COLOR,
/// CLICOLOR_FORCE and non-terminal stdout; comfy-table does not read NO_COLOR,
/// so it is told explicitly.
fn colours_on() -> bool {
    !matches!(
        anstream::AutoStream::choice(&std::io::stdout()),
        ColorChoice::Never
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().action {
        Action::Ps { filter } => ps(&filter).await,
        Action::Prompt => prompt(),
    }
}

async fn ps(filter: &str) -> Result<()> {
    let docker = Docker::connect_with_local_defaults()?;
    let opts = ListContainersOptions {
        all: true,
        ..Default::default()
    };
    let containers = docker.list_containers(Some(opts)).await?;

    let mut table = Table::new();
    table
        .load_style(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(["NAME", "STATE", "STATUS", "PORTS"]);
    if !colours_on() {
        table.force_no_tty();
    }
    let mut shown = 0;
    for c in containers {
        let name = c
            .names
            .unwrap_or_default()
            .first()
            .map(|n| n.trim_start_matches('/').to_string())
            .unwrap_or_default();
        if !name.contains(filter) {
            continue;
        }
        let state = c.state.map(|s| s.to_string()).unwrap_or_default();
        let colour = match state.as_str() {
            "running" => Color::Green,
            "exited" | "dead" => Color::Red,
            _ => Color::Yellow,
        };
        let ports = c
            .ports
            .unwrap_or_default()
            .iter()
            .filter_map(|p| {
                Some(format!(
                    "{}:{}→{}/{}",
                    p.ip.as_deref()?,
                    p.public_port?,
                    p.private_port,
                    p.typ.map(|t| t.to_string()).unwrap_or_default()
                ))
            })
            .collect::<Vec<_>>()
            .join(", ");
        table.add_row([
            Cell::new(name),
            Cell::new(&state).fg(colour),
            Cell::new(c.status.unwrap_or_default()),
            Cell::new(ports),
        ]);
        shown += 1;
    }
    println!("{table}");
    anstream::println!("{OK}✓{OK:#} {shown} container(s)");
    Ok(())
}

fn prompt() -> Result<()> {
    // dialoguer needs a terminal on stderr for prompts; fail clearly otherwise.
    if !std::io::stderr().is_terminal() || !std::io::stdin().is_terminal() {
        anstream::eprintln!(
            "{ERR}✗{ERR:#} prompts need an interactive terminal; pass --user/--password instead"
        );
        bail!("not a terminal");
    }
    let theme = ColorfulTheme::default();
    let user: String = Input::with_theme(&theme)
        .with_prompt("Saltbox username")
        .validate_with(|s: &String| -> Result<(), &str> {
            if s.trim().is_empty() {
                Err("username cannot be empty")
            } else if s.contains(char::is_whitespace) {
                Err("username cannot contain spaces")
            } else {
                Ok(())
            }
        })
        .interact_text()?;
    let password = Password::with_theme(&theme)
        .with_prompt("Password")
        .validate_with(|s: &String| -> Result<(), &str> {
            if s.len() < 8 {
                Err("at least 8 characters")
            } else {
                Ok(())
            }
        })
        .interact()?;
    let go = Confirm::with_theme(&theme)
        .with_prompt(format!("Restore backup for {user}?"))
        .default(false)
        .interact()?;
    if go {
        anstream::println!(
            "{OK}✓{OK:#} would restore for {user} ({} char password)",
            password.len()
        );
    } else {
        anstream::println!("{WARN}●{WARN:#} cancelled");
    }
    Ok(())
}
