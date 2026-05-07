//! Text preview backed by syntect.
//!
//! Path → bytes → lines → syntax-highlighted ratatui `Line`s.
//!
//! The `SyntaxSet` and `ThemeSet` are loaded once on first use (a few MB
//! of regex tables) and reused for the rest of the process. The active
//! syntect theme is hardcoded to `base16-ocean.dark` for v1; tying it to
//! the bobtop theme's luminance is a milestone-3 polish item.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::OnceLock;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Style as SynStyle, Theme as SynTheme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

use super::{Preview, PreviewBody, PreviewKind, PreviewLimits};

/// Lazy-loaded syntax tables (default-syntaxes feature). Roughly 2 MB of
/// regex DFAs — building them per file would dominate every preview.
fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme() -> &'static SynTheme {
    static T: OnceLock<SynTheme> = OnceLock::new();
    T.get_or_init(|| {
        let ts = ThemeSet::load_defaults();
        ts.themes
            .get("base16-ocean.dark")
            .cloned()
            .unwrap_or_else(|| ts.themes.values().next().cloned().expect("syntect ships defaults"))
    })
}

pub fn render_text(path: &Path, limits: PreviewLimits) -> Result<Preview, String> {
    let mut file = File::open(path).map_err(|e| format!("open: {e}"))?;
    let metadata = file.metadata().map_err(|e| format!("stat: {e}"))?;
    if metadata.len() == 0 {
        return Ok(Preview::empty());
    }

    let mut buf = Vec::with_capacity((metadata.len().min(limits.max_bytes)) as usize);
    let limit = limits.max_bytes;
    let mut handle = file.by_ref().take(limit);
    handle.read_to_end(&mut buf).map_err(|e| format!("read: {e}"))?;
    let truncated = metadata.len() > limit;

    if looks_binary(&buf) {
        return Ok(render_binary_summary(&buf, metadata.len()));
    }

    let content = String::from_utf8_lossy(&buf);
    let syntax = syntaxes();
    let theme = theme();
    let syn = match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => syntax
            .find_syntax_by_extension(ext)
            .or_else(|| syntax.find_syntax_by_first_line(&content))
            .unwrap_or_else(|| syntax.find_syntax_plain_text()),
        None => syntax
            .find_syntax_by_first_line(&content)
            .unwrap_or_else(|| syntax.find_syntax_plain_text()),
    };

    let mut hl = HighlightLines::new(syn, theme);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut total = 0usize;
    for raw in LinesWithEndings::from(&content) {
        total += 1;
        if lines.len() >= limits.max_lines {
            continue;
        }
        let regions = hl
            .highlight_line(raw, syntax)
            .map_err(|e| format!("highlight: {e}"))?;
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(regions.len());
        for (style, text) in regions {
            // syntect emits the trailing newline as part of the last
            // region. Strip it — ratatui `Line`s are newline-implicit
            // and a literal `\n` in a span draws a glyph in some fonts.
            let stripped = text.trim_end_matches(|c| c == '\n' || c == '\r');
            if stripped.is_empty() {
                continue;
            }
            spans.push(Span::styled(stripped.to_string(), to_ratatui(style)));
        }
        lines.push(Line::from(spans));
    }

    let note = match (truncated, total > limits.max_lines) {
        (true, _) => Some(format!(
            "(truncated — first {} of {} bytes)",
            limit, metadata.len()
        )),
        (false, true) => Some(format!(
            "(showing first {} of {} lines)",
            limits.max_lines, total
        )),
        _ => None,
    };

    Ok(Preview {
        kind: PreviewKind::Text,
        body: PreviewBody::Lines(lines),
        source_lines: total,
        note,
    })
}

pub fn render_directory(path: &Path) -> Result<Preview, String> {
    let mut entries: Vec<String> = std::fs::read_dir(path)
        .map_err(|e| format!("read_dir: {e}"))?
        .filter_map(|r| r.ok())
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let kind = e
                .file_type()
                .map(|t| {
                    if t.is_dir() {
                        "📁 "
                    } else if t.is_symlink() {
                        "🔗 "
                    } else {
                        "   "
                    }
                })
                .unwrap_or("   ");
            format!("{}{}", kind, name)
        })
        .collect();
    entries.sort();
    let lines: Vec<Line<'static>> = entries
        .iter()
        .map(|s| Line::from(s.clone()))
        .collect();
    let total = lines.len();
    Ok(Preview {
        kind: PreviewKind::Directory,
        body: PreviewBody::Lines(lines),
        source_lines: total,
        note: None,
    })
}

/// Heuristic — a file is "binary" if its first 8 KiB contain a NUL byte
/// or > 5 % non-printable, non-whitespace bytes. UTF-8 multibyte
/// sequences are explicitly allowed (continuation bytes 0x80–0xBF and
/// lead bytes 0xC0+ are counted as printable).
fn looks_binary(buf: &[u8]) -> bool {
    let head = &buf[..buf.len().min(8192)];
    if head.is_empty() {
        return false;
    }
    let mut bad = 0usize;
    for &b in head {
        if b == 0 {
            return true;
        }
        let printable = matches!(b, 0x09 | 0x0A | 0x0D | 0x20..=0x7E | 0x80..=0xFF);
        if !printable {
            bad += 1;
        }
    }
    bad * 20 > head.len()
}

fn render_binary_summary(buf: &[u8], total: u64) -> Preview {
    let head = &buf[..buf.len().min(256)];
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(format!("(binary file — {total} bytes)")));
    lines.push(Line::from(""));
    for (row, chunk) in head.chunks(16).enumerate() {
        let offset = row * 16;
        let hex: String = chunk
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<Vec<_>>()
            .join(" ");
        let ascii: String = chunk
            .iter()
            .map(|&b| if (0x20..=0x7E).contains(&b) { b as char } else { '.' })
            .collect();
        lines.push(Line::from(format!("{:08x}  {:<47}  {}", offset, hex, ascii)));
    }
    Preview {
        kind: PreviewKind::Binary,
        body: PreviewBody::Lines(lines),
        source_lines: 0,
        note: None,
    }
}

fn to_ratatui(s: SynStyle) -> Style {
    let mut style = Style::default().fg(ratatui::style::Color::Rgb(
        s.foreground.r,
        s.foreground.g,
        s.foreground.b,
    ));
    if s.font_style.contains(FontStyle::BOLD) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if s.font_style.contains(FontStyle::ITALIC) {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if s.font_style.contains(FontStyle::UNDERLINE) {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp_with(contents: &[u8], ext: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "bobtop-fb-preview-{}-{}.{}",
            std::process::id(),
            rand_suffix(),
            ext
        ));
        let mut f = File::create(&p).unwrap();
        f.write_all(contents).unwrap();
        p
    }

    fn rand_suffix() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    }

    #[test]
    fn empty_file_returns_empty_kind() {
        let path = tmp_with(b"", "txt");
        let p = render_text(&path, PreviewLimits::default()).unwrap();
        assert_eq!(p.kind, PreviewKind::Empty);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn binary_file_renders_hex_summary() {
        let mut bytes = vec![0u8; 64];
        bytes[3] = 0xff;
        let path = tmp_with(&bytes, "bin");
        let p = render_text(&path, PreviewLimits::default()).unwrap();
        assert_eq!(p.kind, PreviewKind::Binary);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn text_file_highlights_with_extension() {
        let path = tmp_with(b"fn main() { println!(\"hi\"); }\n", "rs");
        let p = render_text(&path, PreviewLimits::default()).unwrap();
        assert_eq!(p.kind, PreviewKind::Text);
        assert_eq!(p.source_lines, 1);
        let lines = match &p.body {
            PreviewBody::Lines(v) => v,
            _ => panic!("expected Lines body"),
        };
        assert_eq!(lines.len(), 1);
        // Multiple spans → highlighter actually ran (would be 1 span for plain).
        assert!(lines[0].spans.len() > 1, "expected multi-span highlight");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn line_cap_truncates_with_note() {
        let content: String = (0..50).map(|i| format!("line {i}\n")).collect();
        let path = tmp_with(content.as_bytes(), "txt");
        let limits = PreviewLimits {
            max_bytes: 10_000,
            max_lines: 10,
        };
        let p = render_text(&path, limits).unwrap();
        let lines = match &p.body {
            PreviewBody::Lines(v) => v,
            _ => panic!("expected Lines body"),
        };
        assert_eq!(lines.len(), 10);
        assert_eq!(p.source_lines, 50);
        assert!(p.note.as_deref().unwrap().contains("first 10 of 50"));
        let _ = std::fs::remove_file(&path);
    }
}
