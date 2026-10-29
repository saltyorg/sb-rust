//! sb-rust's truecolor palette, shared by the prototype screens and the seed
//! of the real `tui`/`term-ui` theme. Designed for dark terminal backgrounds.
//! Colours are named by role, not hue, so screens share one meaning per
//! colour. Truecolor is the default (capability map, decision 10): crossterm
//! writes `Color::Rgb` as 24-bit SGR without consulting the terminal
//! (crossterm 0.29 `style/types/colored.rs`). The fallback for terminals
//! without truecolor is planned through libraries, not built here.
//!
//! Explicit darker tones replace the DIM modifier, which terminals render
//! inconsistently.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

// Text, from most to least prominent.
pub const TEXT: Color = Color::from_u32(0xDCE3EC);
pub const MUTED: Color = Color::from_u32(0x8B95A7);
pub const FAINT: Color = Color::from_u32(0x586174);
pub const BORDER: Color = Color::from_u32(0x3A4558);

/// Brand accent: title bars and key hints.
pub const ACCENT: Color = Color::from_u32(0x5CCFE6);
/// Text drawn on an ACCENT, OK or WARN background.
pub const ON_ACCENT: Color = Color::from_u32(0x0B1A24);

// Health of a source, and the words that describe it.
pub const OK: Color = Color::from_u32(0x6FD39A);
pub const PROBLEM: Color = Color::from_u32(0xFF6E7A);
pub const WARN: Color = Color::from_u32(0xF5B55B);
pub const IDLE: Color = Color::from_u32(0x84A9C0);

// Kinds of source and picker sections, each with a darker tone for rules.
pub const SERVICE: Color = Color::from_u32(0x7EA6FF);
pub const SERVICE_RULE: Color = Color::from_u32(0x34466E);
pub const CONTAINER: Color = Color::from_u32(0xC49BFF);
pub const CONTAINER_RULE: Color = Color::from_u32(0x4F3D6E);
pub const ATTENTION_RULE: Color = Color::from_u32(0x6A2E36);

/// Active filter and search text.
pub const HIGHLIGHT: Color = Color::from_u32(0xE8C77A);
/// Links in rendered text.
pub const LINK: Color = Color::from_u32(0x7EA6FF);
/// Inline code and code blocks: sand text on a slightly raised background.
pub const CODE: Color = Color::from_u32(0xE8C77A);
pub const CODE_BG: Color = Color::from_u32(0x1C2430);
/// Second accent, for emphasis that isn't a warning (notes marked important).
pub const ACCENT_2: Color = Color::from_u32(0xC49BFF);

/// Buttons: an enabled button, and one that can't be used right now.
pub const BUTTON_BG: Color = Color::from_u32(0x2A3446);
pub const BUTTON_DISABLED_BG: Color = Color::from_u32(0x1A2029);

/// Background of log lines matching the search.
pub const SEARCH_HIT: Color = Color::from_u32(0x4A3F12);

/// The selected row or menu item: one solid bar. ratatui paints a List's
/// `highlight_style` over the whole row after drawing it (ratatui-widgets
/// 0.3.2 `list/rendering.rs`), so both colours replace every span's own.
pub const SELECTED: Style = Style::new()
    .fg(Color::from_u32(0xFFFFFF))
    .bg(Color::from_u32(0x24405E))
    .add_modifier(Modifier::BOLD)
    .remove_modifier(Modifier::DIM);

/// Footer key hints: keys in the accent colour, what they do muted.
pub fn key_hints(hints: &[(&str, &str)]) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (key, what) in hints {
        spans.push(Span::styled(
            (*key).to_string(),
            Style::new().fg(ACCENT).bold(),
        ));
        spans.push(Span::styled(format!(" {what}   "), Style::new().fg(MUTED)));
    }
    Line::from(spans)
}
