//! sb-rust's truecolor palette, designed for dark terminal backgrounds.
//! Colours are named by role, not hue, so screens share one meaning per
//! colour. Truecolor is the default (capability map, decision 10): crossterm
//! writes `Color::Rgb` as 24-bit SGR without consulting the terminal
//! (crossterm 0.29 `style/types/colored.rs`). The fallback for terminals
//! without truecolor is planned through libraries, not built here.
//!
//! Explicit darker tones replace the DIM modifier, which terminals render
//! inconsistently.

use ratatui::style::{Color, Modifier, Style};

const fn rgb(hex: u32) -> Color {
    let [_, r, g, b] = hex.to_be_bytes();
    Color::Rgb(r, g, b)
}

// Text, from most to least prominent.
pub const TEXT: Color = rgb(0xDCE3EC);
pub const MUTED: Color = rgb(0x8B95A7);
pub const FAINT: Color = rgb(0x586174);
pub const BORDER: Color = rgb(0x3A4558);

/// Brand accent: title bars and key hints.
pub const ACCENT: Color = rgb(0x5CCFE6);
/// Text drawn on an ACCENT, OK or WARN background.
pub const ON_ACCENT: Color = rgb(0x0B1A24);

// Health of a source, and the words that describe it.
pub const OK: Color = rgb(0x6FD39A);
pub const PROBLEM: Color = rgb(0xFF6E7A);
pub const WARN: Color = rgb(0xF5B55B);
pub const IDLE: Color = rgb(0x84A9C0);

// Kinds of source and picker sections, each with a darker tone for rules.
pub const SERVICE: Color = rgb(0x7EA6FF);
pub const SERVICE_RULE: Color = rgb(0x34466E);
pub const CONTAINER: Color = rgb(0xC49BFF);
pub const CONTAINER_RULE: Color = rgb(0x4F3D6E);
pub const ATTENTION_RULE: Color = rgb(0x6A2E36);

/// Active filter and search text.
pub const HIGHLIGHT: Color = rgb(0xE8C77A);
/// Background of log lines matching the search.
pub const SEARCH_HIT: Color = rgb(0x4A3F12);

/// The selected row or menu item: one solid bar. ratatui paints a List's
/// `highlight_style` over the whole row after drawing it (ratatui-widgets
/// 0.3.2 `list/rendering.rs`), so both colours replace every span's own.
pub const SELECTED: Style = Style::new()
    .fg(rgb(0xFFFFFF))
    .bg(rgb(0x24405E))
    .add_modifier(Modifier::BOLD)
    .remove_modifier(Modifier::DIM);
