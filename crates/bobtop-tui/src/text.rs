//! Generic text and formatting helpers for TUI composition.
//!
//! These are intentionally app-agnostic so future TUIs can reuse the same
//! cell-writing and human-readable formatting logic without depending on the
//! bobtop daemon.

use ratatui::buffer::Buffer;
use ratatui::style::Style;

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
        col = col.saturating_add(1);
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

pub fn format_rate(bps: f64) -> String {
    if bps >= 1024.0 * 1024.0 {
        format!("{:.1}M", bps / (1024.0 * 1024.0))
    } else if bps >= 1024.0 {
        format!("{:.0}K", bps / 1024.0)
    } else {
        format!("{:.0}B", bps)
    }
}

pub fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    s.chars().take(max_chars).collect()
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
}
