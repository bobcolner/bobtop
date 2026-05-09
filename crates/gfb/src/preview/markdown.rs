//! Markdown preview via pulldown-cmark.
//!
//! Streams events and folds them into ratatui `Line`s. The renderer is
//! a TUI approximation, not a perfect document renderer — the goal is
//! that a user reading a README in the preview pane gets enough
//! visual structure (headings, code, lists, emphasis) to navigate. We
//! deliberately don't try to wrap text or honor terminal width here:
//! `ScrollableText` already handles soft-wrap when configured to.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::{Preview, PreviewBody, PreviewKind, PreviewLimits};

const CODE_FG: Color = Color::Rgb(0xc5, 0xc8, 0xc6);
const CODE_BG: Color = Color::Rgb(0x1d, 0x1f, 0x21);
const HEADING_FG: Color = Color::Rgb(0xf0, 0xc6, 0x74);
const LINK_FG: Color = Color::Rgb(0x81, 0xa2, 0xbe);
const QUOTE_FG: Color = Color::Rgb(0xb5, 0xbd, 0x68);

pub fn render_markdown(path: &Path, limits: PreviewLimits) -> Result<Preview, String> {
    let mut buf = Vec::new();
    File::open(path)
        .map_err(|e| format!("open: {e}"))?
        .take(limits.max_bytes)
        .read_to_end(&mut buf)
        .map_err(|e| format!("read: {e}"))?;
    if buf.is_empty() {
        return Ok(Preview::empty());
    }
    let content = String::from_utf8_lossy(&buf);
    let parser = Parser::new(&content);

    let mut state = MdState::default();
    for event in parser {
        if state.lines.len() >= limits.max_lines {
            state.note_truncated = true;
            break;
        }
        state.feed(event);
    }
    state.flush_line();

    let total = state.lines.len();
    let note = if state.note_truncated {
        Some(format!("(showing first {} lines)", limits.max_lines))
    } else {
        None
    };
    Ok(Preview {
        kind: PreviewKind::Markdown,
        body: PreviewBody::Lines(state.lines),
        source_lines: total,
        note,
    })
}

#[derive(Default)]
struct MdState {
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    style_stack: Vec<Style>,
    /// Stack of pending list markers — one per nesting level.
    /// `Some(n)` for ordered lists, `None` for unordered.
    list_stack: Vec<Option<u64>>,
    in_code_block: bool,
    in_heading: Option<HeadingLevel>,
    in_quote: usize,
    note_truncated: bool,
}

impl MdState {
    fn current_style(&self) -> Style {
        self.style_stack.last().copied().unwrap_or_default()
    }

    fn push_style(&mut self, base: Style) {
        let merged = self.current_style().patch(base);
        self.style_stack.push(merged);
    }

    fn pop_style(&mut self) {
        self.style_stack.pop();
    }

    fn flush_line(&mut self) {
        if self.current.is_empty() {
            // Preserve intentional blank lines (paragraph separators).
            self.lines.push(Line::from(""));
        } else {
            let spans = std::mem::take(&mut self.current);
            self.lines.push(Line::from(spans));
        }
    }

    /// End a block element. Adds a single trailing blank line for
    /// breathing room, but collapses runs of blanks so deeply-nested
    /// markdown doesn't produce huge gaps.
    fn break_block(&mut self) {
        if !self.current.is_empty() {
            let spans = std::mem::take(&mut self.current);
            self.lines.push(Line::from(spans));
        }
        if self
            .lines
            .last()
            .map(|l| l.spans.iter().all(|s| s.content.trim().is_empty()))
            .unwrap_or(false)
        {
            return;
        }
        self.lines.push(Line::from(""));
    }

    fn write_indent(&mut self) {
        let depth = self.list_stack.len();
        if depth > 0 {
            let pad = "  ".repeat(depth - 1);
            self.current.push(Span::raw(pad));
        }
        if self.in_quote > 0 {
            let bars = "│ ".repeat(self.in_quote);
            self.current.push(Span::styled(
                bars,
                Style::default().fg(QUOTE_FG),
            ));
        }
    }

    fn feed(&mut self, ev: Event<'_>) {
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => self.text(&t),
            Event::Code(t) => self.current.push(Span::styled(
                t.into_string(),
                Style::default().fg(CODE_FG).bg(CODE_BG),
            )),
            Event::Html(_) | Event::InlineHtml(_) => {
                // Strip raw HTML; rendering as text would be confusing.
            }
            Event::SoftBreak | Event::HardBreak => {
                self.flush_line();
                self.write_indent();
            }
            Event::Rule => {
                self.flush_line();
                self.lines.push(Line::from(Span::styled(
                    "─".repeat(40),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            Event::FootnoteReference(_) | Event::TaskListMarker(_) => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Heading { level, .. } => {
                self.flush_line_if_dirty();
                let prefix = match level {
                    HeadingLevel::H1 => "# ",
                    HeadingLevel::H2 => "## ",
                    HeadingLevel::H3 => "### ",
                    HeadingLevel::H4 => "#### ",
                    HeadingLevel::H5 => "##### ",
                    HeadingLevel::H6 => "###### ",
                };
                let style = Style::default()
                    .fg(HEADING_FG)
                    .add_modifier(Modifier::BOLD);
                self.current
                    .push(Span::styled(prefix.to_string(), style));
                self.push_style(style);
                self.in_heading = Some(level);
            }
            Tag::Paragraph => {
                self.write_indent();
            }
            Tag::CodeBlock(_) => {
                self.flush_line_if_dirty();
                self.in_code_block = true;
                self.push_style(Style::default().fg(CODE_FG).bg(CODE_BG));
            }
            Tag::List(start) => {
                self.flush_line_if_dirty();
                self.list_stack.push(start);
            }
            Tag::Item => {
                self.write_indent();
                let marker_style = Style::default().fg(HEADING_FG);
                let marker = match self.list_stack.last_mut() {
                    Some(Some(n)) => {
                        let s = format!("{}. ", n);
                        *n += 1;
                        s
                    }
                    Some(None) => "• ".to_string(),
                    None => "• ".to_string(),
                };
                self.current.push(Span::styled(marker, marker_style));
            }
            Tag::Emphasis => self.push_style(Style::default().add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.push_style(Style::default().add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => {
                self.push_style(Style::default().add_modifier(Modifier::CROSSED_OUT))
            }
            Tag::Link { dest_url, .. } => {
                self.push_style(Style::default().fg(LINK_FG).add_modifier(Modifier::UNDERLINED));
                // Save dest in a marker span — rendered after link text in `end`.
                self.current.push(Span::raw(""));
                self.current.push(Span::styled(
                    String::new(),
                    Style::default().fg(LINK_FG),
                ));
                // Stash url at the end of the link.
                self.current.push(Span::raw(format!("\u{0001}{}\u{0001}", dest_url)));
            }
            Tag::BlockQuote => {
                self.flush_line_if_dirty();
                self.in_quote += 1;
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) => {
                self.pop_style();
                self.in_heading = None;
                self.break_block();
            }
            TagEnd::Paragraph => self.break_block(),
            TagEnd::CodeBlock => {
                self.in_code_block = false;
                self.pop_style();
                self.break_block();
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
                self.break_block();
            }
            TagEnd::Item => {
                self.flush_line();
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => self.pop_style(),
            TagEnd::Link => {
                self.pop_style();
                // Find the most recent stashed dest_url ( url )
                // and inline it as visible " (url)" after the link text.
                if let Some(idx) = self
                    .current
                    .iter()
                    .rposition(|s| s.content.starts_with('\u{0001}'))
                {
                    let span = self.current.remove(idx);
                    let url = span
                        .content
                        .trim_matches('\u{0001}')
                        .to_string();
                    self.current.push(Span::styled(
                        format!(" ({})", url),
                        Style::default().fg(LINK_FG),
                    ));
                }
            }
            TagEnd::BlockQuote => {
                self.in_quote = self.in_quote.saturating_sub(1);
                self.break_block();
            }
            _ => {}
        }
    }

    fn text(&mut self, t: &str) {
        let style = self.current_style();
        if self.in_code_block {
            // Code blocks keep newlines as line separators.
            for (i, raw) in t.split_inclusive('\n').enumerate() {
                let stripped = raw.trim_end_matches('\n');
                if i > 0 {
                    self.flush_line();
                    self.write_indent();
                }
                if !stripped.is_empty() {
                    self.current
                        .push(Span::styled(stripped.to_string(), style));
                }
            }
            return;
        }
        if t.is_empty() {
            return;
        }
        self.current.push(Span::styled(t.to_string(), style));
    }

    fn flush_line_if_dirty(&mut self) {
        if !self.current.is_empty() {
            let spans = std::mem::take(&mut self.current);
            self.lines.push(Line::from(spans));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    fn tmp(name: &str, contents: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "gfb-md-{}-{}.md",
            std::process::id(),
            name
        ));
        let mut f = File::create(&p).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        p
    }

    fn lines_of(p: &Preview) -> &[Line<'static>] {
        match &p.body {
            PreviewBody::Lines(v) => v,
            _ => panic!("expected Lines body"),
        }
    }

    #[test]
    fn heading_is_bold_with_prefix() {
        let path = tmp("heading", "# Hello\n\ntext\n");
        let p = render_markdown(&path, PreviewLimits::default()).unwrap();
        let lines = lines_of(&p);
        let first = &lines[0];
        let s = first.spans.iter().map(|s| s.content.as_ref()).collect::<String>();
        assert!(s.starts_with("# Hello"), "got: {s:?}");
        assert!(first.spans.iter().any(|s| s.style.add_modifier.contains(Modifier::BOLD)));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn code_block_lines_carry_code_style() {
        let path = tmp(
            "code",
            "```\nlet x = 1;\nlet y = 2;\n```\n",
        );
        let p = render_markdown(&path, PreviewLimits::default()).unwrap();
        let lines = lines_of(&p);
        let with_code = lines
            .iter()
            .filter(|l| l.spans.iter().any(|s| s.style.fg == Some(CODE_FG)))
            .count();
        assert!(with_code >= 2, "expected ≥2 code-styled lines, got {with_code}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unordered_list_emits_bullets() {
        let path = tmp("ul", "- one\n- two\n");
        let p = render_markdown(&path, PreviewLimits::default()).unwrap();
        let lines = lines_of(&p);
        let bullets = lines
            .iter()
            .filter(|l| l.spans.iter().any(|s| s.content.contains('•')))
            .count();
        assert_eq!(bullets, 2, "expected two bullet lines");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn link_renders_with_inline_url() {
        let path = tmp("link", "[anthropic](https://anthropic.com)\n");
        let p = render_markdown(&path, PreviewLimits::default()).unwrap();
        let s = lines_of(&p)
            .iter()
            .flat_map(|l| l.spans.iter().map(|sp| sp.content.as_ref()))
            .collect::<String>();
        assert!(s.contains("anthropic"), "missing link text: {s:?}");
        assert!(s.contains("https://anthropic.com"), "missing url: {s:?}");
        let _ = std::fs::remove_file(&path);
    }
}
