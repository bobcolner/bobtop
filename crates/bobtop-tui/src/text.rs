//! Generic text and formatting helpers for TUI composition.
//!
//! These are intentionally app-agnostic so future TUIs can reuse the same
//! cell-writing and human-readable formatting logic without depending on the
//! bobtop daemon.

use ratatui::buffer::Buffer;
use ratatui::style::Style;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Terminal display width of `s` in cells.
///
/// Use this — not `chars().count()` — for any layout math (column fitting,
/// right-alignment, truncation). `chars().count()` gives one per scalar, but
/// CJK and emoji glyphs take two cells while combining marks take zero.
pub fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

pub fn write_str_at(buf: &mut Buffer, x: u16, y: u16, s: &str, style: Style) {
    let mut col = x;
    let right = buf.area.right();
    for ch in s.chars() {
        if col >= right {
            break;
        }
        let cell = &mut buf[(col, y)];
        cell.set_char(ch);
        cell.set_style(style);
        // Advance by the glyph's display width so wide chars (CJK, emoji)
        // claim two cells and the next char starts past them.
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0).max(1) as u16;
        col = col.saturating_add(cw);
    }
}

pub fn bool_label(b: bool) -> String {
    if b { "yes".into() } else { "no".into() }
}

pub fn format_bytes(b: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    const TIB: u64 = GIB * 1024;
    if b >= TIB {
        format!("{:.2} TiB", b as f64 / TIB as f64)
    } else if b >= GIB {
        format!("{:.2} GiB", b as f64 / GIB as f64)
    } else if b >= MIB {
        format!("{:.0} MiB", b as f64 / MIB as f64)
    } else if b >= KIB {
        format!("{:.0} KiB", b as f64 / KIB as f64)
    } else {
        format!("{b} B")
    }
}

/// Compact byte formatter for narrow columns (≤ 6 chars): "256M", "1.5G", "1.0T".
pub fn format_bytes_compact(b: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    const TIB: u64 = GIB * 1024;
    if b >= TIB {
        format!("{:.1}T", b as f64 / TIB as f64)
    } else if b >= GIB {
        format!("{:.1}G", b as f64 / GIB as f64)
    } else if b >= MIB {
        format!("{:.0}M", b as f64 / MIB as f64)
    } else if b >= KIB {
        format!("{:.0}K", b as f64 / KIB as f64)
    } else {
        format!("{}B", b)
    }
}

pub fn format_rate(bps: f64) -> String {
    if bps >= 1024.0 * 1024.0 {
        format!("{:.1}M", bps / (1024.0 * 1024.0))
    } else if bps >= 1024.0 {
        format!("{:.0}K", bps / 1024.0)
    } else {
        format!("{:.0}B", bps)
    }
}

/// Truncate `s` to at most `max_cells` terminal cells, accounting for
/// wide glyphs (CJK, emoji = 2 cells) and zero-width combining marks.
/// Truncates whole grapheme-equivalent units; never splits a char.
pub fn truncate_chars(s: &str, max_cells: usize) -> String {
    if display_width(s) <= max_cells {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut w = 0usize;
    for ch in s.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > max_cells {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    #[test]
    fn formats_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1024), "1 KiB");
    }

    #[test]
    fn formats_rates() {
        assert_eq!(format_rate(0.0), "0B");
        assert_eq!(format_rate(1024.0), "1K");
    }

    #[test]
    fn bool_labels_are_short() {
        assert_eq!(bool_label(true), "yes");
        assert_eq!(bool_label(false), "no");
    }

    #[test]
    fn writes_string_into_buffer() {
        let area = Rect::new(0, 0, 5, 1);
        let mut buf = Buffer::empty(area);
        write_str_at(&mut buf, 0, 0, "hello world", Style::default());
        let row: String = (0..area.width).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert_eq!(row, "hello");
    }

    #[test]
    fn truncates_by_character_count() {
        assert_eq!(truncate_chars("abcdef", 4), "abcd");
        assert_eq!(truncate_chars("abc", 4), "abc");
    }

    #[test]
    fn display_width_counts_cell_columns_not_codepoints() {
        // Latin: 1 cell each.
        assert_eq!(display_width("hello"), 5);
        // CJK: 2 cells each.
        assert_eq!(display_width("测试"), 4);
        // Mixed.
        assert_eq!(display_width("测a试"), 5);
    }

    #[test]
    fn truncate_keeps_whole_wide_chars() {
        // Cells = 2 each. Budget 3 fits one wide char (2) + nothing else.
        assert_eq!(truncate_chars("测试名", 3), "测");
        // Budget 4 fits two wide chars (4 cells exactly).
        assert_eq!(truncate_chars("测试名", 4), "测试");
        // Budget 5 still fits two wide (4 cells) — third would overflow.
        assert_eq!(truncate_chars("测试名", 5), "测试");
    }

    #[test]
    fn write_str_at_advances_two_cells_for_wide_chars() {
        let area = Rect::new(0, 0, 6, 1);
        let mut buf = Buffer::empty(area);
        // CJK uses 2 cells per glyph; "测a" should write 测 at x=0..1, a at x=2.
        write_str_at(&mut buf, 0, 0, "测a", Style::default());
        assert_eq!(buf[(0, 0)].symbol(), "测");
        // The cell at x=2 (after the wide glyph) holds the 'a'.
        assert_eq!(buf[(2, 0)].symbol(), "a");
    }
}
