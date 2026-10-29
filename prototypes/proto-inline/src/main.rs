//! Prototype: the non-TUI look. tabled for tables, anstyle/anstream for
//! styled lines, dialoguer for prompts; on a TTY, piped, and with NO_COLOR.
//! Throwaway; see FINDINGS.md.

use std::collections::HashMap;
use std::io::IsTerminal;

use anstream::ColorChoice;
use anstyle::Style;
use anyhow::{Result, bail};
use bollard::Docker;
use bollard::models::{ContainerInspectResponse, PortMap};
use bollard::query_parameters::ListContainersOptions;
use clap::{Parser, Subcommand};
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, Password};
use futures::future::join_all;
use proto_theme::{BORDER, IDLE, MUTED, OK, PROBLEM, WARN};
use ratatui::style::Color;
use tabled::builder::Builder;
use tabled::settings::object::{Columns, Rows, Segment};
use tabled::settings::peaker::Priority;
use tabled::settings::style::BorderColor;
use tabled::settings::{Alignment, Style as TableStyle, Width};

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
    /// restore style prompts. Deferred: the restore backend is being
    /// rewritten, so its prompts take shape after that (FINDINGS.md).
    Prompt,
}

/// The palette's RGB value in each library's colour type. proto-theme's
/// colours are all `Color::Rgb` (`Color::from_u32`); anything else would be a
/// palette bug, shown as no colour rather than a named ANSI one. Why not
/// ratatui-core's `TryFrom<Color> for anstyle::RgbColor`: it covers anstyle
/// only, behind a feature the ratatui facade doesn't forward, and tabled and
/// console still need the RGB values; one match serves all three.
fn rgb(colour: Color) -> Option<(u8, u8, u8)> {
    match colour {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    }
}

fn style(colour: Color) -> Style {
    Style::new().fg_color(rgb(colour).map(|(r, g, b)| anstyle::RgbColor(r, g, b).into()))
}

fn table_colour(colour: Color) -> Option<tabled::settings::Color> {
    rgb(colour).map(|(r, g, b)| tabled::settings::Color::rgb_fg(r, g, b))
}

fn console_style(colour: Color) -> console::Style {
    let (r, g, b) = rgb(colour).unwrap_or((255, 255, 255));
    console::Style::new().for_stderr().true_color(r, g, b)
}

/// `--color` for every output path. colorchoice-clap sets anstream's
/// global choice, which `AutoStream::choice` reads first. No library links
/// that choice to console, which dialoguer's prompts go through, so `always`
/// and `never` are passed on to it here. `auto` leaves it to its own
/// detection (NO_COLOR, CLICOLOR_FORCE, a terminal or not). tabled writes
/// only the colours it is given, so the table follows `colours_on`.
fn apply_colour_choice(color: colorchoice_clap::Color) {
    color.write_global();
    let enabled = match color.color {
        clap::ColorChoice::Always => true,
        clap::ColorChoice::Never => false,
        clap::ColorChoice::Auto => return,
    };
    console::set_colors_enabled(enabled);
    console::set_colors_enabled_stderr(enabled);
}

/// One decision for every output path: anstream honours `--color`,
/// NO_COLOR, CLICOLOR_FORCE and non-terminal stdout.
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

/// sb-go's `sb docker ps` columns: the container, its status, the port
/// Traefik routes to, and the host port bindings. Each container is
/// inspected (in parallel, as proto-logs does) for health, exit code, labels
/// and bindings, which the list endpoint doesn't all carry.
async fn ps(filter: &str) -> Result<()> {
    let docker = Docker::connect_with_local_defaults()?;
    let opts = ListContainersOptions {
        all: true,
        ..Default::default()
    };
    let names: Vec<String> = docker
        .list_containers(Some(opts))
        .await?
        .into_iter()
        .filter_map(|c| Some(c.names?.first()?.trim_start_matches('/').to_string()))
        .filter(|name| name.contains(filter))
        .collect();
    let inspected = join_all(names.iter().map(|n| docker.inspect_container(n, None))).await;
    let mut rows = Vec::new();
    let mut failed = Vec::new();
    for (name, info) in names.into_iter().zip(inspected) {
        match info {
            Ok(info) => rows.push(row(name, info)),
            Err(e) => failed.push(format!("{name}: {e}")),
        }
    }
    rows.sort_by(|a, b| a.name.cmp(&b.name));

    // Fit the terminal when there is one, as comfy-table did; piped output
    // keeps its natural width. tabled doesn't detect the terminal itself;
    // console's size_checked is None when stdout isn't a terminal
    // (console 0.16.6 `unix_term.rs`).
    let width = console::Term::stdout()
        .size_checked()
        .map(|(_, cols)| usize::from(cols));
    println!("{}", table(&rows, colours_on(), width));
    if !failed.is_empty() {
        let err = style(PROBLEM).bold();
        for f in &failed {
            anstream::eprintln!("{err}✗{err:#} could not inspect {f}");
        }
        bail!("{} container(s) could not be inspected", failed.len());
    }
    Ok(())
}

struct Row {
    name: String,
    status: String,
    colour: Color,
    traefik: Vec<String>,
    bindings: Vec<String>,
}

fn row(name: String, info: ContainerInspectResponse) -> Row {
    let state = info.state.unwrap_or_default();
    let (status, colour) = status(
        &state.status.map(|s| s.to_string()).unwrap_or_default(),
        state.exit_code.unwrap_or_default(),
        state.oom_killed.unwrap_or_default(),
        state
            .health
            .and_then(|h| h.status)
            .map(|s| s.to_string())
            .as_deref(),
    );
    Row {
        name,
        status,
        colour,
        traefik: traefik_ports(&info.config.and_then(|c| c.labels).unwrap_or_default()),
        bindings: port_bindings(
            &info
                .network_settings
                .and_then(|n| n.ports)
                .unwrap_or_default(),
        ),
    }
}

/// Status text and palette colour. sb-go's wording ("running (healthy)"),
/// but an exit code decides whether a stopped container is a problem, as in
/// proto-logs. States: https://docs.docker.com/reference/api/engine/version/v1.53/#tag/Container/operation/ContainerInspect
fn status(state: &str, exit_code: i64, oom_killed: bool, health: Option<&str>) -> (String, Color) {
    match (state, health) {
        ("running", Some("healthy")) => ("running (healthy)".into(), OK),
        ("running", Some("unhealthy")) => ("running (unhealthy)".into(), PROBLEM),
        ("running", Some("starting")) => ("running (starting)".into(), WARN),
        ("running", _) => ("running".into(), OK),
        ("exited", _) if oom_killed => ("exited (out of memory)".into(), PROBLEM),
        ("exited", _) if exit_code != 0 => (format!("exited (code {exit_code})"), PROBLEM),
        ("exited" | "created" | "paused", _) => (state.into(), IDLE),
        ("restarting" | "dead", _) => (state.into(), PROBLEM),
        (other, _) => (other.into(), WARN),
    }
}

/// The ports Traefik routes to, from Saltbox's labels: sb-go's rule
/// (`traefik.http.services.<name>.loadbalancer.server.port`, skipping the
/// `-http` redirect services), deduplicated and sorted so the order is
/// stable (Go's map order made sb-go's vary).
fn traefik_ports(labels: &HashMap<String, String>) -> Vec<String> {
    let mut ports: Vec<String> = labels
        .iter()
        .filter(|(key, _)| {
            key.contains("traefik.http.services")
                && key.contains(".loadbalancer.server.port")
                && !key.contains("-http")
                && key.split('.').count() > 3
        })
        .map(|(_, port)| port.clone())
        .collect();
    ports.sort_by_key(|p| (p.parse::<u32>().unwrap_or(u32::MAX), p.clone()));
    ports.dedup();
    ports
}

/// Host bindings, one per line, as sb-go: `host-ip:host-port->port/proto`
/// with the ports padded so the arrows line up, an unset host IP shown as
/// 0.0.0.0 and IPv6's `::` as `[::]`. Sorted by container port, IPv4 first.
fn port_bindings(ports: &PortMap) -> Vec<String> {
    let mut parts: Vec<(String, String, String)> = ports
        .iter()
        .flat_map(|(internal, bindings)| {
            bindings.iter().flatten().filter_map(move |b| {
                let host_port = b.host_port.clone().filter(|p| !p.is_empty())?;
                let ip = match b.host_ip.as_deref() {
                    None | Some("") => "0.0.0.0".to_string(),
                    Some("::") => "[::]".to_string(),
                    Some(ip) => ip.to_string(),
                };
                Some((ip, host_port, internal.clone()))
            })
        })
        .collect();
    let port_number = |p: &str| {
        p.split('/')
            .next()
            .and_then(|n| n.parse::<u32>().ok())
            .unwrap_or(u32::MAX)
    };
    parts.sort_by(|a, b| {
        (port_number(&a.2), &a.2, &a.0, &a.1).cmp(&(port_number(&b.2), &b.2, &b.0, &b.1))
    });
    let host_width = parts.iter().map(|p| p.1.len()).max().unwrap_or(0);
    let internal_width = parts.iter().map(|p| p.2.len()).max().unwrap_or(0);
    parts
        .iter()
        .map(|(ip, host, internal)| {
            format!("{ip}:{host:>host_width$}->{internal:>internal_width$}")
        })
        .collect()
}

/// The table: rounded, a rule between rows (tabled's `modern_rounded`), bold
/// centred headings, port columns right-aligned, as sb-go. With colours on,
/// the lines take the palette's BORDER colour (tabled `BorderColor`, which
/// comfy-table has no equivalent of) and each status its colour. `width`
/// shrinks the widest column first, wrapping at word boundaries.
fn table(rows: &[Row], colours: bool, width: Option<usize>) -> String {
    let mut builder = Builder::default();
    builder.push_record(["Container", "Status", "Traefik Port", "Port Bindings"]);
    for row in rows {
        builder.push_record([
            row.name.clone(),
            row.status.clone(),
            row.traefik.join(", "),
            row.bindings.join("\n"),
        ]);
    }
    let mut table = builder.build();
    table
        .with(TableStyle::modern_rounded())
        .modify(Columns::new(2..), Alignment::right())
        .modify(Rows::first(), Alignment::center());
    if colours {
        if let Some(c) = table_colour(BORDER) {
            table.modify(Segment::all(), BorderColor::filled(c));
        }
        table.modify(Rows::first(), tabled::settings::Color::BOLD);
        for (i, row) in rows.iter().enumerate() {
            if let Some(c) = table_colour(row.colour) {
                table.modify((i + 1, 1), c);
            }
        }
    }
    if let Some(width) = width {
        table.with(
            Width::wrap(width)
                .priority(Priority::max(true))
                .keep_words(true),
        );
    }
    table.to_string()
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
    use bollard::models::PortBinding;

    fn rows() -> Vec<Row> {
        [
            ("running", 0, None),
            ("running", 0, Some("healthy")),
            ("running", 0, Some("unhealthy")),
            ("exited", 0, None),
            ("exited", 1, None),
            ("restarting", 0, None),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, (state, code, health))| {
            let (status, colour) = status(state, code, false, health);
            Row {
                name: format!("demo-{i}"),
                status,
                colour,
                traefik: vec!["8080".into()],
                bindings: vec!["0.0.0.0:80->80/tcp".into(), "   [::]:80->80/tcp".into()],
            }
        })
        .collect()
    }

    /// SGR parameter lists in `text`, e.g. `[[38, 2, 111, 211, 154]]`. A plain
    /// split is enough: the input is only tabled and console output, which
    /// write `ESC [ params m` with `;` separators. Why not anstyle-parse: its
    /// `Perform` boilerplate is longer than this. Why not ansitok (in the
    /// tree through tabled's `ansi` feature): a direct dev-dependency on a
    /// transitive crate for one test helper.
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

    fn rgb_code(colour: Color) -> Vec<u16> {
        let (r, g, b) = rgb(colour).expect("palette colour");
        vec![38, 2, r.into(), g.into(), b.into()]
    }

    /// Truecolor is the design target (capability map, decision 10): the
    /// border, headings and statuses use palette RGB, never named colours.
    #[test]
    fn coloured_table_uses_only_truecolor() {
        let text = table(&rows(), true, None);
        let codes = sgr(&text);
        for colour in [BORDER, OK, PROBLEM, IDLE] {
            assert!(codes.contains(&rgb_code(colour)), "{colour:?} missing");
        }
        assert!(!codes.iter().any(|c| sets_named_colour(c)), "{codes:?}");
    }

    #[test]
    fn colours_off_means_no_escape_codes() {
        let text = table(&rows(), false, None);
        assert!(!text.contains('\x1b'), "{text}");
        assert!(text.contains("demo-0"));
    }

    /// sb-go's layout: rounded corners and a rule between every row.
    #[test]
    fn rows_are_separated_by_rules() {
        let text = table(&rows(), false, None);
        assert!(text.starts_with('╭'), "{text}");
        let rules = text.lines().filter(|l| l.starts_with('├')).count();
        assert_eq!(rules, rows().len(), "{text}");
    }

    #[test]
    fn table_fits_the_given_width() {
        let text = table(&rows(), true, Some(50));
        let widest = text
            .lines()
            .map(console::measure_text_width)
            .max()
            .unwrap_or(0);
        assert!(widest <= 50, "{widest}\n{text}");
        assert!(
            table(&rows(), false, None)
                .lines()
                .any(|l| l.chars().count() > 50)
        );
    }

    #[test]
    fn status_wording_and_colours() {
        assert_eq!(
            status("running", 0, false, Some("healthy")),
            ("running (healthy)".into(), OK)
        );
        assert_eq!(status("running", 0, false, None), ("running".into(), OK));
        assert_eq!(status("running", 0, false, Some("unhealthy")).1, PROBLEM);
        assert_eq!(status("running", 0, false, Some("starting")).1, WARN);
        assert_eq!(status("exited", 0, false, None), ("exited".into(), IDLE));
        assert_eq!(
            status("exited", 137, false, None),
            ("exited (code 137)".into(), PROBLEM)
        );
        assert_eq!(
            status("exited", 137, true, None).0,
            "exited (out of memory)"
        );
        assert_eq!(status("dead", 0, false, None).1, PROBLEM);
        assert_eq!(status("removing", 0, false, None).1, WARN);
    }

    #[test]
    fn traefik_ports_follow_sb_go_rule_sorted_and_unique() {
        let labels: HashMap<String, String> = [
            ("traefik.http.services.app.loadbalancer.server.port", "9000"),
            ("traefik.http.services.api.loadbalancer.server.port", "8080"),
            ("traefik.http.services.dup.loadbalancer.server.port", "9000"),
            (
                "traefik.http.services.app-http.loadbalancer.server.port",
                "80",
            ),
            ("traefik.enable", "true"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        assert_eq!(traefik_ports(&labels), ["8080", "9000"]);
    }

    #[test]
    fn port_bindings_line_up_like_sb_go() {
        let binding = |ip: Option<&str>, port: &str| PortBinding {
            host_ip: ip.map(str::to_string),
            host_port: Some(port.to_string()),
        };
        let ports: PortMap = [
            (
                "8080/tcp".to_string(),
                Some(vec![
                    binding(Some("::"), "80"),
                    binding(Some("0.0.0.0"), "80"),
                ]),
            ),
            ("443/tcp".to_string(), Some(vec![binding(None, "4443")])),
            ("9999/tcp".to_string(), Some(vec![binding(None, "")])),
            ("53/udp".to_string(), None),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            port_bindings(&ports),
            [
                "0.0.0.0:4443-> 443/tcp",
                "0.0.0.0:  80->8080/tcp",
                "[::]:  80->8080/tcp",
            ]
        );
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
