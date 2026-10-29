//! `--gallery`: the same moments of a run (finished steps, a running step with
//! its live tail, a failed step with its last lines) in several formats, as
//! static snapshots, to compare designs side by side. Colours come from the
//! shared palette through `palette`; widths and padding from console
//! (`measure_text_width`, `pad_str`), so wide text lines up.

use console::{Alignment, measure_text_width, pad_str, pad_str_with};
use proto_theme as theme;
// Frame and branch glyphs from ratatui's symbol sets, so the mock-ups match
// the frames the full-screen prototypes draw.
use ratatui::symbols::line::{
    BOTTOM_LEFT, HORIZONTAL, LIGHT_QUADRUPLE_DASH_VERTICAL, ROUNDED_BOTTOM_LEFT,
    ROUNDED_BOTTOM_RIGHT, ROUNDED_TOP_LEFT, ROUNDED_TOP_RIGHT, VERTICAL, VERTICAL_RIGHT,
};

use crate::palette;

struct Done {
    text: &'static str,
    elapsed: &'static str,
}

/// One run's worth of sample data, shown in every format.
struct Sample {
    done: [Done; 2],
    running: &'static str,
    running_elapsed: &'static str,
    tail: [&'static str; 3],
    failed: &'static str,
    failed_code: u32,
    failed_elapsed: &'static str,
    failure_tail: [&'static str; 3],
    total: usize,
}

const SAMPLE: Sample = Sample {
    done: [
        Done {
            text: "Ansible available",
            elapsed: "1s",
        },
        Done {
            text: "Packages installed",
            elapsed: "4s",
        },
    ],
    running: "Configuring service",
    running_elapsed: "7s",
    tail: [
        "TASK [docker : Create network] ****",
        "ok: [localhost]",
        "TASK [docker : Start container] ****",
    ],
    failed: "Configuring service",
    failed_code: 2,
    failed_elapsed: "9s",
    failure_tail: [
        "task line 13",
        "task line 14",
        "fatal: [localhost]: FAILED! => unreachable",
    ],
    total: 4,
};

/// A format: its name, a one-line description, and its rendering.
type Format = (&'static str, &'static str, fn(&Sample) -> Vec<String>);

const FORMATS: &[Format] = &[
    (
        "Current",
        "symbol + text, muted elapsed, indented muted tail",
        current,
    ),
    (
        "Dot leaders",
        "elapsed right-aligned behind dot leaders",
        dot_leaders,
    ),
    ("Step counter", "[n/total] on every line", step_counter),
    ("Badges", "coloured status labels in front", badges),
    ("Timeline", "a vertical rail with a node per step", timeline),
    (
        "Grouped",
        "a light frame around each step and its output",
        grouped,
    ),
    (
        "Minimal",
        "words instead of symbols, no decoration",
        minimal,
    ),
    ("Tree", "output as branches under its step", tree),
];

/// Every format's title and snapshot, ready to print.
pub(crate) fn lines() -> Vec<String> {
    let mut out = Vec::new();
    for (n, (name, about, render)) in FORMATS.iter().enumerate() {
        let title = palette(theme::ACCENT)
            .bold()
            .apply_to(format!("{}. {name}", n + 1));
        out.push(format!(
            "{title}  {}",
            palette(theme::MUTED).apply_to(about)
        ));
        out.push(String::new());
        out.extend(render(&SAMPLE).into_iter().map(|l| format!("  {l}")));
        out.push(String::new());
    }
    out
}

pub(crate) fn print() {
    for line in lines() {
        eprintln!("{line}");
    }
}

/// A frame of the spinner the real runner draws: indicatif's default tick
/// strings (indicatif 0.18.6 style.rs, ProgressStyle::default_spinner).
fn spinner() -> String {
    indicatif::ProgressStyle::default_spinner()
        .get_tick_str(3)
        .to_string()
}
const WIDTH: usize = 52;

fn ok(s: impl std::fmt::Display) -> String {
    palette(theme::OK).apply_to(s).to_string()
}
fn bad(s: impl std::fmt::Display) -> String {
    palette(theme::PROBLEM).apply_to(s).to_string()
}
fn accent(s: impl std::fmt::Display) -> String {
    palette(theme::ACCENT).apply_to(s).to_string()
}
fn muted(s: impl std::fmt::Display) -> String {
    palette(theme::MUTED).apply_to(s).to_string()
}
fn faint(s: impl std::fmt::Display) -> String {
    palette(theme::FAINT).apply_to(s).to_string()
}

/// Left text, then dots out to `WIDTH`, then right text (console's
/// pad_str_with does the padding by display width).
fn leader(left: &str, right: &str) -> String {
    let dots_width = WIDTH.saturating_sub(measure_text_width(left) + measure_text_width(right) + 2);
    let dots = pad_str_with("", dots_width, Alignment::Left, None, '.');
    format!("{left} {} {right}", faint(dots))
}

fn current(s: &Sample) -> Vec<String> {
    let mut out: Vec<String> = s
        .done
        .iter()
        .map(|d| format!("{} {}", ok(format!("✓ {}", d.text)), muted(d.elapsed)))
        .collect();
    out.push(format!(
        "{} {} {}",
        accent(spinner()),
        accent(s.running),
        muted(s.running_elapsed)
    ));
    out.extend(s.tail.iter().map(|l| format!("    {}", muted(l))));
    out.push(String::new());
    out.push(bad(format!(
        "✗ {}: failed (exit {})",
        s.failed, s.failed_code
    )));
    out.extend(s.failure_tail.iter().map(|l| format!("    {l}")));
    out
}

fn dot_leaders(s: &Sample) -> Vec<String> {
    let mut out: Vec<String> = s
        .done
        .iter()
        .map(|d| leader(&ok(format!("✓ {}", d.text)), &muted(d.elapsed)))
        .collect();
    out.push(leader(
        &format!("{} {}", accent(spinner()), accent(s.running)),
        &muted(s.running_elapsed),
    ));
    out.extend(s.tail.iter().map(|l| format!("    {}", muted(l))));
    out.push(String::new());
    out.push(leader(
        &bad(format!("✗ {}", s.failed)),
        &bad(format!("exit {} · {}", s.failed_code, s.failed_elapsed)),
    ));
    out.extend(s.failure_tail.iter().map(|l| format!("    {l}")));
    out
}

fn step_counter(s: &Sample) -> Vec<String> {
    let n = |i: usize| faint(format!("[{i}/{}]", s.total));
    let mut out: Vec<String> = s
        .done
        .iter()
        .enumerate()
        .map(|(i, d)| {
            format!(
                "{} {} {}",
                n(i + 1),
                ok(format!("✓ {}", d.text)),
                muted(format!("({})", d.elapsed))
            )
        })
        .collect();
    out.push(format!(
        "{} {} {} {}",
        n(3),
        accent(spinner()),
        accent(s.running),
        muted(format!("({})", s.running_elapsed))
    ));
    out.extend(s.tail.iter().map(|l| format!("        {}", muted(l))));
    out.push(String::new());
    out.push(format!(
        "{} {}",
        n(3),
        bad(format!("✗ {}: failed (exit {})", s.failed, s.failed_code))
    ));
    out.extend(s.failure_tail.iter().map(|l| format!("        {l}")));
    out
}

fn badges(s: &Sample) -> Vec<String> {
    // Background colours through console's dotted syntax (`on_#RRGGBB`,
    // console 0.16.6 Style::from_dotted_str), centred by pad_str.
    let badge = |text: &str, background: ratatui::style::Color| {
        console::Style::from_dotted_str(&format!("{}.on_{background}.bold", theme::ON_ACCENT))
            .for_stderr()
            .apply_to(pad_str(text, 6, Alignment::Center, None))
            .to_string()
    };
    let mut out: Vec<String> = s
        .done
        .iter()
        .map(|d| {
            format!(
                "{} {} {}",
                badge("DONE", theme::OK),
                d.text,
                muted(d.elapsed)
            )
        })
        .collect();
    out.push(format!(
        "{} {} {}",
        badge("RUN", theme::ACCENT),
        s.running,
        muted(s.running_elapsed)
    ));
    out.extend(s.tail.iter().map(|l| format!("       {}", muted(l))));
    out.push(String::new());
    out.push(format!(
        "{} {} {}",
        badge("FAIL", theme::PROBLEM),
        s.failed,
        bad(format!("exit {}", s.failed_code))
    ));
    out.extend(s.failure_tail.iter().map(|l| format!("       {l}")));
    out
}

/// Drawn by hand, like the tree: a static mock-up.
fn timeline(s: &Sample) -> Vec<String> {
    let rail = || faint(VERTICAL);
    let mut out = Vec::new();
    for d in &s.done {
        out.push(format!("{} {} {}", ok("●"), d.text, muted(d.elapsed)));
        out.push(rail());
    }
    out.push(format!(
        "{} {} {}",
        accent(spinner()),
        accent(s.running),
        muted(s.running_elapsed)
    ));
    out.extend(s.tail.iter().map(|l| format!("{}   {}", rail(), muted(l))));
    out.push(String::new());
    out.push(format!(
        "{} {} {}",
        bad("●"),
        bad(s.failed),
        bad(format!("· failed (exit {})", s.failed_code))
    ));
    out.extend(
        s.failure_tail
            .iter()
            .map(|l| format!("{}   {l}", bad(LIGHT_QUADRUPLE_DASH_VERTICAL))),
    );
    out
}

/// Custom frame: comfy-table draws tables, not a titled box around free
/// text, and ratatui's Block would need a backend to turn into inline text.
/// This is a mock-up to judge, not a component.
fn grouped(s: &Sample) -> Vec<String> {
    let frame = |title: String, right: String, body: &[&str], colour: fn(&str) -> String| {
        // "╭─ " + title + " " + dashes + " " + right + " ╮": 7 columns besides
        // title, right and dashes, so the top edge matches the bottom.
        let head_used = measure_text_width(&title) + measure_text_width(&right) + 7;
        let mut lines = vec![format!(
            "{} {title} {} {right} {}",
            colour(&format!("{ROUNDED_TOP_LEFT}{HORIZONTAL}")),
            colour(&HORIZONTAL.repeat(WIDTH.saturating_sub(head_used))),
            colour(ROUNDED_TOP_RIGHT)
        )];
        lines.extend(
            body.iter()
                .map(|l| format!("{} {}", colour(VERTICAL), muted(l))),
        );
        lines.push(colour(&format!(
            "{ROUNDED_BOTTOM_LEFT}{}{ROUNDED_BOTTOM_RIGHT}",
            HORIZONTAL.repeat(WIDTH.saturating_sub(2))
        )));
        lines
    };
    let border = |t: &str| faint(t);
    let red = |t: &str| bad(t);
    let mut out: Vec<String> = s
        .done
        .iter()
        .map(|d| format!("{} {} {}", ok("✓"), d.text, muted(d.elapsed)))
        .collect();
    out.extend(frame(
        format!("{} {}", accent(spinner()), accent(s.running)),
        muted(s.running_elapsed),
        &s.tail,
        border,
    ));
    out.push(String::new());
    out.extend(frame(
        bad(format!("✗ {}", s.failed)),
        bad(format!("exit {}", s.failed_code)),
        &s.failure_tail,
        red,
    ));
    out
}

fn minimal(s: &Sample) -> Vec<String> {
    let mut out: Vec<String> = s
        .done
        .iter()
        .map(|d| format!("{}  {}  {}", ok("done"), d.text, muted(d.elapsed)))
        .collect();
    out.push(format!(
        "{}  {}  {}",
        accent("...."),
        s.running,
        muted(s.running_elapsed)
    ));
    out.extend(s.tail.iter().map(|l| format!("      {}", faint(l))));
    out.push(String::new());
    out.push(format!(
        "{}  {}  {}",
        bad("fail"),
        s.failed,
        muted(format!("exit {} after {}", s.failed_code, s.failed_elapsed))
    ));
    out.extend(s.failure_tail.iter().map(|l| format!("      {l}")));
    out
}

/// Drawn by hand: a fixed two-level mock-up. If this format is chosen for the
/// real runner, use a tree crate (termtree) instead; none is in the tree yet.
fn tree(s: &Sample) -> Vec<String> {
    let tee = format!("{VERTICAL_RIGHT}{HORIZONTAL}");
    let elbow = format!("{BOTTOM_LEFT}{HORIZONTAL}");
    let mut out = vec![accent("Saltbox update")];
    for d in &s.done {
        out.push(format!(
            "{} {} {}",
            faint(&tee),
            ok(format!("✓ {}", d.text)),
            muted(d.elapsed)
        ));
    }
    out.push(format!(
        "{} {} {} {}",
        faint(&tee),
        accent(spinner()),
        accent(s.running),
        muted(s.running_elapsed)
    ));
    let last = s.tail.len() - 1;
    for (i, l) in s.tail.iter().enumerate() {
        let branch = if i == last { &elbow } else { &tee };
        out.push(format!(
            "{}  {} {}",
            faint(VERTICAL),
            faint(branch),
            muted(l)
        ));
    }
    out.push(String::new());
    out.push(format!(
        "{} {}",
        faint(&elbow),
        bad(format!("✗ {}: failed (exit {})", s.failed, s.failed_code))
    ));
    let last = s.failure_tail.len() - 1;
    for (i, l) in s.failure_tail.iter().enumerate() {
        let branch = if i == last { &elbow } else { &tee };
        out.push(format!("   {} {l}", bad(branch)));
    }
    out
}
