//! Prototype: the non-TUI look. comfy-table for tables, anstyle/anstream for
//! styled lines, dialoguer for prompts; on a TTY, piped, and with NO_COLOR.
//! Throwaway; see FINDINGS.md.

use std::io::IsTerminal;

use anstream::ColorChoice;
use anstyle::Style;
use anyhow::{Result, bail};
use bollard::Docker;
use bollard::query_parameters::ListContainersOptions;
use clap::{Parser, Subcommand};
use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::{Attribute, Cell, ContentArrangement, Table};
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, Password};
use proto_theme::{IDLE, MUTED, OK, PROBLEM, WARN};
use ratatui::style::Color;

#[derive(Parser)]
struct Cli {
    /// `--color=auto|always|never` (capability map, decision 10), from the
    /// anstream project's clap mixin
    /// (https://docs.rs/colorchoice-clap/1.0.8/colorchoice_clap/struct.Color.html).
    #[command(flatten)]
    color: colorchoice_clap::Color,
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

/// The palette's RGB value in each library's colour type. proto-theme's
/// colours are all `Color::Rgb` (`Color::from_u32`); anything else would be a
/// palette bug, shown as no colour rather than a named ANSI one. Why not
/// ratatui-core's `TryFrom<Color> for anstyle::RgbColor`: it covers anstyle
/// only, behind a feature the ratatui facade doesn't forward, and comfy-table
/// and console still need the RGB values; one match serves all three.
fn rgb(colour: Color) -> Option<(u8, u8, u8)> {
    match colour {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    }
}

fn style(colour: Color) -> Style {
    Style::new().fg_color(rgb(colour).map(|(r, g, b)| anstyle::RgbColor(r, g, b).into()))
}

fn cell_colour(colour: Color) -> Option<comfy_table::Color> {
    rgb(colour).map(|(r, g, b)| comfy_table::Color::Rgb { r, g, b })
}

fn console_style(colour: Color) -> console::Style {
    let (r, g, b) = rgb(colour).unwrap_or((255, 255, 255));
    console::Style::new().for_stderr().true_color(r, g, b)
}

/// `--color` for every output path. colorchoice-clap sets anstream's
/// global choice, which `AutoStream::choice` reads first. No library links
/// that choice to console, which dialoguer's prompts go through, or to
/// crossterm, which comfy-table styles through and which drops colours on its
/// own when NO_COLOR is set (crossterm 0.29 `style/types/colored.rs`), so
/// `always` and `never` are passed on to both here. `auto` leaves them to
/// their own detection (NO_COLOR, CLICOLOR_FORCE, a terminal or not).
fn apply_colour_choice(color: colorchoice_clap::Color) {
    color.write_global();
    let enabled = match color.color {
        clap::ColorChoice::Always => true,
        clap::ColorChoice::Never => false,
        clap::ColorChoice::Auto => return,
    };
    console::set_colors_enabled(enabled);
    console::set_colors_enabled_stderr(enabled);
    crossterm::style::Colored::set_ansi_color_disabled(!enabled);
}

/// One decision for every output path: anstream honours `--color`,
/// NO_COLOR, CLICOLOR_FORCE and non-terminal stdout; comfy-table doesn't read
/// NO_COLOR, so the table follows this instead.
fn colours_on() -> bool {
    !matches!(
        anstream::AutoStream::choice(&std::io::stdout()),
        ColorChoice::Never
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    apply_colour_choice(cli.color);
    match cli.action {
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

    let rows: Vec<Row> = containers
        .into_iter()
        .filter_map(|c| {
            let name = c
                .names
                .unwrap_or_default()
                .first()
                .map(|n| n.trim_start_matches('/').to_string())
                .unwrap_or_default();
            if !name.contains(filter) {
                return None;
            }
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
            Some(Row {
                name,
                state: c.state.map(|s| s.to_string()).unwrap_or_default(),
                status: c.status.unwrap_or_default(),
                ports,
            })
        })
        .collect();
    let colours = colours_on();
    println!("{}", table(&rows, colours));
    let ok = style(OK).bold();
    anstream::println!("{ok}✓{ok:#} {} container(s)", rows.len());
    Ok(())
}

struct Row {
    name: String,
    state: String,
    status: String,
    ports: String,
}

/// The state's palette colour. The list endpoint gives the state but not the
/// exit code, so a failed exit can't be told from a clean stop here; the
/// real table would reuse proto-logs' classification (inspect gives the exit
/// code). States: https://docs.docker.com/reference/api/engine/version/v1.53/#tag/Container/operation/ContainerInspect
fn state_colour(state: &str) -> Color {
    match state {
        "running" => OK,
        "restarting" | "dead" => PROBLEM,
        "exited" | "created" | "paused" => IDLE,
        _ => WARN,
    }
}

/// The table, styled or not. Colour is decided here per cell rather than
/// with `force_no_tty`, which would also switch off comfy-table's terminal
/// width lookup and with it the wrapping. `enforce_styling` keeps the
/// colours when `--color=always` output is piped.
fn table(rows: &[Row], colours: bool) -> Table {
    let paint = |cell: Cell, colour: Color| match cell_colour(colour) {
        Some(c) if colours => cell.fg(c),
        _ => cell,
    };
    let mut table = Table::new();
    table
        .load_style(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(["NAME", "STATE", "STATUS", "PORTS"].map(|h| {
            let cell = paint(Cell::new(h), MUTED);
            if colours {
                cell.add_attribute(Attribute::Bold)
            } else {
                cell
            }
        }));
    if colours {
        table.enforce_styling();
    }
    for row in rows {
        table.add_row([
            Cell::new(&row.name),
            paint(Cell::new(&row.state), state_colour(&row.state)),
            Cell::new(&row.status),
            Cell::new(&row.ports),
        ]);
    }
    table
}

/// dialoguer's ColorfulTheme with the palette in place of its named ANSI
/// colours. The theme's fields are public for this
/// (https://docs.rs/dialoguer/0.12.0/dialoguer/theme/struct.ColorfulTheme.html).
fn prompt_theme() -> ColorfulTheme {
    let styled = |text: &str, colour: Color| console_style(colour).apply_to(text.to_string());
    ColorfulTheme {
        defaults_style: console_style(MUTED),
        prompt_prefix: styled("?", proto_theme::ACCENT),
        prompt_suffix: styled("›", MUTED),
        success_prefix: styled("✔", OK),
        success_suffix: styled("·", MUTED),
        error_prefix: styled("✘", PROBLEM),
        error_style: console_style(PROBLEM),
        hint_style: console_style(MUTED),
        values_style: console_style(OK),
        active_item_style: console_style(proto_theme::ACCENT),
        active_item_prefix: styled("❯", proto_theme::ACCENT),
        checked_item_prefix: styled("✔", OK),
        unchecked_item_prefix: styled("⬚", MUTED),
        picked_item_prefix: styled("❯", proto_theme::ACCENT),
        ..ColorfulTheme::default()
    }
}

fn prompt() -> Result<()> {
    // dialoguer needs a terminal on stderr for prompts; fail clearly otherwise.
    if !std::io::stderr().is_terminal() || !std::io::stdin().is_terminal() {
        let err = style(PROBLEM).bold();
        anstream::eprintln!(
            "{err}✗{err:#} prompts need an interactive terminal; pass --user/--password instead"
        );
        bail!("not a terminal");
    }
    let theme = prompt_theme();
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
    let (ok, warn) = (style(OK).bold(), style(WARN));
    if go {
        anstream::println!(
            "{ok}✓{ok:#} would restore for {user} ({} char password)",
            password.len()
        );
    } else {
        anstream::println!("{warn}●{warn:#} cancelled");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<Row> {
        [
            "running",
            "exited",
            "restarting",
            "created",
            "paused",
            "dead",
        ]
        .iter()
        .map(|state| Row {
            name: format!("demo-{state}"),
            state: (*state).to_string(),
            status: "Up 2 hours".to_string(),
            ports: "0.0.0.0:80→80/tcp".to_string(),
        })
        .collect()
    }

    /// SGR parameter lists in `text`, e.g. `[[38, 2, 111, 211, 154]]`. A plain
    /// split is enough: the input is only comfy-table, crossterm and console
    /// output, which write `ESC [ params m` with `;` separators. Why not
    /// anstyle-parse: its `Perform` boilerplate is longer than this.
    fn sgr(text: &str) -> Vec<Vec<u16>> {
        text.split("\x1b[")
            .skip(1)
            .filter_map(|rest| rest.split_once('m'))
            .map(|(params, _)| params.split(';').filter_map(|p| p.parse().ok()).collect())
            .collect()
    }

    /// Named ANSI foreground or background colours (30–37, 40–47, 90–97,
    /// 100–107), skipping the RGB triplets of 38;2 and 48;2.
    fn sets_named_colour(codes: &[u16]) -> bool {
        let mut i = 0;
        while i < codes.len() {
            match codes[i] {
                38 | 48 if codes.get(i + 1) == Some(&2) => i += 5,
                38 | 48 => i += 3,
                30..=37 | 40..=47 | 90..=97 | 100..=107 => return true,
                _ => i += 1,
            }
        }
        false
    }

    /// Truecolor is the design target (capability map, decision 10).
    #[test]
    fn coloured_table_uses_only_truecolor() {
        let text = table(&rows(), true).to_string();
        let codes = sgr(&text);
        assert!(
            codes.iter().any(|c| c.starts_with(&[38, 2])),
            "no truecolor: {text}"
        );
        assert!(!codes.iter().any(|c| sets_named_colour(c)), "{codes:?}");
    }

    /// With colour off no cell carries a style, so even a terminal (which
    /// comfy-table styles for; forced here) gets none.
    #[test]
    fn colours_off_means_no_escape_codes() {
        let text = table(&rows(), false).enforce_styling().to_string();
        assert!(!text.contains('\x1b'), "{text}");
        assert!(text.contains("demo-running"));
    }

    #[test]
    fn states_use_their_palette_colours() {
        assert_eq!(state_colour("running"), OK);
        assert_eq!(state_colour("dead"), PROBLEM);
        assert_eq!(state_colour("restarting"), PROBLEM);
        assert_eq!(state_colour("exited"), IDLE);
        assert_eq!(state_colour("removing"), WARN);
    }

    /// Every styled part of the prompt theme is truecolor.
    #[test]
    fn prompt_theme_uses_only_truecolor() {
        console::set_colors_enabled_stderr(true);
        let theme = prompt_theme();
        for part in [
            theme.prompt_prefix.to_string(),
            theme.success_prefix.to_string(),
            theme.error_prefix.to_string(),
            theme.error_style.apply_to("x").to_string(),
            theme.values_style.apply_to("x").to_string(),
            theme.hint_style.apply_to("x").to_string(),
        ] {
            let codes = sgr(&part);
            assert!(!codes.is_empty(), "unstyled: {part:?}");
            assert!(!codes.iter().any(|c| sets_named_colour(c)), "{part:?}");
        }
    }
}
