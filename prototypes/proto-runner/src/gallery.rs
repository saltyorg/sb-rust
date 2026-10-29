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
    HORIZONTAL, LIGHT_QUADRUPLE_DASH_VERTICAL, ROUNDED_BOTTOM_LEFT, ROUNDED_BOTTOM_RIGHT,
    ROUNDED_TOP_LEFT, ROUNDED_TOP_RIGHT, VERTICAL,
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
            text: "System checked",
            elapsed: "1s",
        },
        Done {
            text: "Package lists updated",
            elapsed: "3s",
        },
    ],
    running: "Installing packages",
    running_elapsed: "7s",
    tail: [
        "Get: http://archive.example.org/ubuntu noble/main amd64 jq 1.7.1 [65.7 kB]",
        "Get: http://archive.example.org/ubuntu noble/main amd64 unzip 6.0 [174 kB]",
        "Progress: [ 40%] [########            ]",
    ],
    failed: "Installing packages",
    failed_code: 100,
    failed_elapsed: "9s",
    failure_tail: [
        "Building dependency tree... Done",
        "Reading state information... Done",
        "E: Unable to locate package demo-unavailable",
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
    (
        "Tree",
        "output as branches under its step, times in a column",
        tree,
    ),
    (
        "Tree, dot leaders",
        "the tree with dot leaders to the times",
        tree_dots,
    ),
    (
        "Tree, rounded",
        "the tree with a rounded last branch",
        tree_rounded,
    ),
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

/// The trees drawn by the tree formats, one `Vec` of lines per tree (a
/// tree ends at a blank line), for tests.
#[cfg(test)]
pub(crate) fn tree_blocks() -> Vec<Vec<String>> {
    let mut blocks = Vec::new();
    for (name, _, render) in FORMATS {
        if !name.starts_with("Tree") {
            continue;
        }
        let mut block = Vec::new();
        for line in render(&SAMPLE) {
            let line = console::strip_ansi_codes(&line).to_string();
            if line.trim().is_empty() {
                if !block.is_empty() {
                    blocks.push(std::mem::take(&mut block));
                }
            } else if !line.ends_with(':') {
                block.push(line);
            }
        }
        if !block.is_empty() {
            blocks.push(block);
        }
    }
    blocks
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
    leader_to(WIDTH, left, right)
}

/// `leader`, ending at `width` columns.
fn leader_to(width: usize, left: &str, right: &str) -> String {
    let dots_width = width.saturating_sub(measure_text_width(left) + measure_text_width(right) + 2);
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

/// How a tree format lays out its right-hand column.
#[derive(Clone, Copy)]
enum Fill {
    Spaces,
    Dots,
}

/// A step item at `depth`, with `right` ending at the time column whatever
/// the depth. The indent per level is what the palette's glyphs take
/// ("├" + "── "); console's pad_str and pad_str_with pad by display width.
fn tree_item(
    glyphs: &termtree::GlyphPalette,
    depth: usize,
    left: &str,
    right: &str,
    fill: Fill,
) -> String {
    let indent = measure_text_width(glyphs.middle_item) + measure_text_width(glyphs.item_indent);
    let width = WIDTH.saturating_sub(depth * indent);
    match fill {
        Fill::Spaces => {
            let room = width.saturating_sub(measure_text_width(right) + 1);
            format!("{} {right}", pad_str(left, room, Alignment::Left, None))
        }
        Fill::Dots => leader_to(width, left, right),
    }
}

/// Renders a termtree tree, then tones down its glyphs: termtree's
/// GlyphPalette takes fixed strings, so colour is applied afterwards to
/// each line's leading glyph run.
fn render_tree(tree: &termtree::Tree<String>) -> Vec<String> {
    tree.to_string()
        .lines()
        .map(|line| {
            let split = line
                .char_indices()
                .find(|(_, c)| !matches!(c, ' ' | '│' | '├' | '└' | '╰' | '─'))
                .map_or(line.len(), |(i, _)| i);
            let (glyphs, text) = line.split_at(split);
            format!("{}{text}", faint(glyphs))
        })
        .collect()
}

/// One run as two complete trees, "while running" and "if it fails", so
/// neither is broken by the other.
fn tree_variant(s: &Sample, fill: Fill, glyphs: termtree::GlyphPalette) -> Vec<String> {
    let finished = || -> Vec<termtree::Tree<String>> {
        s.done
            .iter()
            .map(|d| {
                termtree::Tree::new(tree_item(
                    &glyphs,
                    1,
                    &ok(format!("✓ {}", d.text)),
                    &muted(d.elapsed),
                    fill,
                ))
            })
            .collect()
    };
    let running = termtree::Tree::new(tree_item(
        &glyphs,
        1,
        &format!("{} {}", accent(spinner()), accent(s.running)),
        &muted(s.running_elapsed),
        fill,
    ))
    .with_leaves(s.tail.iter().map(muted));
    let failed = termtree::Tree::new(tree_item(
        &glyphs,
        1,
        &bad(format!("✗ {}", s.failed)),
        &bad(format!("exit {} · {}", s.failed_code, s.failed_elapsed)),
        fill,
    ))
    .with_leaves(s.failure_tail.iter().map(|l| (*l).to_string()));
    let mut out = vec![muted("while running:")];
    let mut tree = termtree::Tree::new(accent("Saltbox update")).with_glyphs(glyphs);
    tree.extend(finished());
    tree.push(running);
    out.extend(render_tree(&tree));
    out.push(String::new());
    out.push(muted("if it fails:"));
    let mut tree = termtree::Tree::new(accent("Saltbox update")).with_glyphs(glyphs);
    tree.extend(finished());
    tree.push(failed);
    out.extend(render_tree(&tree));
    out
}

/// Trees drawn by termtree (rust-cli), with termtree's default glyphs.
fn tree(s: &Sample) -> Vec<String> {
    tree_variant(s, Fill::Spaces, termtree::GlyphPalette::new())
}

fn tree_dots(s: &Sample) -> Vec<String> {
    tree_variant(s, Fill::Dots, termtree::GlyphPalette::new())
}

/// Rounded last branch, with ratatui's rounded corner glyph.
fn tree_rounded(s: &Sample) -> Vec<String> {
    let glyphs = termtree::GlyphPalette {
        last_item: ROUNDED_BOTTOM_LEFT,
        ..termtree::GlyphPalette::new()
    };
    tree_variant(s, Fill::Spaces, glyphs)
}
