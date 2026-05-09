//! [`Catalog`] + [`BrowserShell`] demo: a synthetic two-level
//! catalog rendered as a tree on the left, with a placeholder
//! preview area on the right. Renders to a `TestBackend` and dumps
//! the result so it works headlessly in CI.
//!
//! Run with: `cargo run -p gtui --example two_pane_browser`

use std::collections::HashSet;

use gtui::browser::BrowserShell;
use gtui::tree::{flatten, Catalog, TreeRow};
use gtui::widgets::live_table::{
    Align, Cell, ColumnDef, TableEntry, TableRowExt, WidthSpec,
};
use gtui::Theme;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::Terminal;

/// Tiny synthetic catalog: three "folders" each with two "files".
/// The NodeId is the path-as-string; that's it.
struct Demo;

impl Catalog for Demo {
    type NodeId = String;
    type Row = String;

    fn roots(&self) -> Vec<(Self::NodeId, Self::Row)> {
        vec![
            ("docs".into(),    "docs".into()),
            ("src".into(),     "src".into()),
            ("examples".into(), "examples".into()),
        ]
    }

    fn children(&self, node: &Self::NodeId) -> Vec<(Self::NodeId, Self::Row)> {
        // One layer of "files" per "folder". Anything past depth 1
        // is a leaf.
        if node.contains('/') {
            return vec![];
        }
        let leaves = match node.as_str() {
            "docs"     => &["intro.md", "api.md"][..],
            "src"      => &["lib.rs", "main.rs"][..],
            "examples" => &["one.rs", "two.rs"][..],
            _ => &[][..],
        };
        leaves
            .iter()
            .map(|f| {
                let id = format!("{node}/{f}");
                let label = id.clone();
                (id, label)
            })
            .collect()
    }

    fn is_expandable(&self, node: &Self::NodeId) -> bool {
        !node.contains('/')
    }
}

/// Column id used by `LiveTable` — single-column tree view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Col {
    Name,
}

/// Adapter that adds the `TableRowExt` impl for our flattened rows.
/// Wrapping the toolkit's `TreeRow<String, String>` is the conventional
/// pattern when you want app-specific glyphs / coloring.
struct RowView<'a> {
    inner: &'a TreeRow<String, String>,
}

impl<'a> TableRowExt<Col> for RowView<'a> {
    fn cell(&self, _col: Col) -> Cell {
        let glyph = if self.inner.expandable {
            if self.inner.expanded { "▼ " } else { "▶ " }
        } else {
            "  "
        };
        Cell::plain(format!("{glyph}{}", self.inner.row))
    }

    fn tree_depth(&self) -> u8 {
        self.inner.depth
    }

    fn ancestor_continues(&self) -> &[bool] {
        &self.inner.ancestor_continues
    }

    fn is_last_sibling(&self) -> bool {
        self.inner.is_last_sibling
    }
}

fn main() {
    let theme = Theme::fallback();

    // Expand `src` so the rendered tree has at least one open
    // subtree to show off the depth glyphs.
    let mut expanded = HashSet::new();
    expanded.insert("src".to_string());

    let rows = flatten(&Demo, &expanded);
    let views: Vec<TableEntry<RowView<'_>, ()>> = rows
        .iter()
        .map(|r| TableEntry::Item(RowView { inner: r }))
        .collect();

    let columns = vec![ColumnDef {
        id: Col::Name,
        label: "Name",
        width: WidthSpec::Flex,
        align: Align::Left,
        sortable: false,
    }];

    let backend = TestBackend::new(60, 12);
    let mut terminal = Terminal::new(backend).expect("test backend");
    terminal
        .draw(|frame| {
            let area = Rect::new(0, 0, 60, 12);
            let preview_rect = BrowserShell::<Col>::new()
                .with_title("demo browser")
                .with_accent(Color::Cyan)
                .with_focused(true)
                .render(
                    frame,
                    area,
                    &views,
                    &columns,
                    &theme,
                    Col::Name,
                    /* cursor */ 1,
                );
            // Caller fills the preview rect — for this demo we
            // just paint a single hint string at its top-left.
            let buf = frame.buffer_mut();
            let hint = " preview pane (caller-rendered) ";
            let mut x = preview_rect.x;
            for ch in hint.chars() {
                if x >= preview_rect.x + preview_rect.width {
                    break;
                }
                buf[(x, preview_rect.y)].set_symbol(&ch.to_string());
                x += 1;
            }
        })
        .expect("draw");

    println!("BrowserShell — tree on the left, preview rect handed back");
    println!();
    print_buffer(terminal.backend().buffer());
}

fn print_buffer(buf: &ratatui::buffer::Buffer) {
    let area = buf.area();
    for y in 0..area.height {
        let mut line = String::new();
        for x in 0..area.width {
            line.push_str(buf[(x, y)].symbol());
        }
        println!("│{}│", line.trim_end());
    }
}
