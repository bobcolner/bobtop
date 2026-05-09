//! Sortable [`LiveTable`] demo. Renders a synthetic process list to
//! a `TestBackend` Buffer (no TTY required) and dumps the resulting
//! frame to stdout.
//!
//! Run with: `cargo run -p gtui --example process_table`

use gtui::widgets::live_table::{
    Align, Cell, ColumnDef, LiveTable, TableEntry, TableRowExt, WidthSpec,
};
use gtui::Theme;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Col {
    Pid,
    Name,
    Cpu,
    Mem,
}

#[derive(Debug, Clone)]
struct Proc {
    pid: u32,
    name: &'static str,
    cpu: f32,
    mem_mb: u32,
}

impl TableRowExt<Col> for Proc {
    fn cell(&self, col: Col) -> Cell {
        match col {
            Col::Pid => Cell::plain(self.pid.to_string()),
            Col::Name => Cell::plain(self.name.to_string()),
            Col::Cpu => Cell::plain(format!("{:.1}%", self.cpu)),
            Col::Mem => Cell::plain(format!("{} MB", self.mem_mb)),
        }
    }
}

fn main() {
    let theme = Theme::fallback();

    // Sort by CPU descending — the cell rendering keeps using the
    // Rust-native order; the LiveTable only paints the sort arrow
    // on the matching header.
    let mut rows = vec![
        Proc { pid: 1234, name: "chrome",     cpu: 18.4, mem_mb: 1320 },
        Proc { pid: 8821, name: "rustc",      cpu: 92.0, mem_mb: 880 },
        Proc { pid: 22,   name: "systemd",    cpu: 0.1,  mem_mb: 12 },
        Proc { pid: 4001, name: "node",       cpu: 6.7,  mem_mb: 240 },
        Proc { pid: 4096, name: "postgres",   cpu: 1.2,  mem_mb: 110 },
        Proc { pid: 5102, name: "ssh-agent",  cpu: 0.0,  mem_mb: 4 },
        Proc { pid: 6310, name: "tmux",       cpu: 0.4,  mem_mb: 18 },
    ];
    rows.sort_by(|a, b| b.cpu.partial_cmp(&a.cpu).unwrap());

    let entries: Vec<TableEntry<Proc, ()>> =
        rows.into_iter().map(TableEntry::Item).collect();

    let columns = vec![
        ColumnDef {
            id: Col::Pid,
            label: "PID",
            width: WidthSpec::Fixed(6),
            align: Align::Right,
            sortable: true,
        },
        ColumnDef {
            id: Col::Name,
            label: "Name",
            width: WidthSpec::Flex,
            align: Align::Left,
            sortable: true,
        },
        ColumnDef {
            id: Col::Cpu,
            label: "CPU",
            width: WidthSpec::Fixed(7),
            align: Align::Right,
            sortable: true,
        },
        ColumnDef {
            id: Col::Mem,
            label: "MEM",
            width: WidthSpec::Fixed(8),
            align: Align::Right,
            sortable: true,
        },
    ];

    let backend = TestBackend::new(60, 10);
    let mut terminal = Terminal::new(backend).expect("test backend");
    terminal
        .draw(|frame| {
            let table = LiveTable::new(&entries, &columns, &theme, Col::Name)
                .with_selection(Some(1), 0)
                .with_sort(Some(Col::Cpu), /* descending */ true)
                .with_fade(false);
            frame.render_widget(&table, Rect::new(0, 0, 60, 10));
        })
        .expect("draw");

    println!("LiveTable<Proc, (), Col> — sorted by CPU desc, row 1 selected");
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
        // Strip trailing whitespace so the dump reads cleanly when a
        // column doesn't fully consume its declared width.
        println!("│{}│", line.trim_end());
    }
}
