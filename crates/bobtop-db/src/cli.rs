//! CLI entry — argv parsing, terminal setup, app loop, teardown.

use std::io;

use anyhow::Result;
use clap::Parser;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use bobtop_tui::{load_theme, DEFAULT_THEME_NAME};

use crate::app::App;
use crate::conn;

#[derive(Debug, Parser)]
#[command(
    name = "bobtop-db",
    bin_name = "bobtop-db",
    about = "TUI database browser (Postgres + DuckDB / DuckLake)"
)]
pub struct Cli {
    /// Connection target. Examples:
    ///   `postgres://user:pass@host:5432/dbname`
    ///   `duckdb:///path/to/file.db`
    ///   `mock` (built-in demo, default)
    #[arg(long, default_value = "mock")]
    pub connect: String,

    /// Theme name from the bundled bobtop registry.
    #[arg(long, default_value = DEFAULT_THEME_NAME)]
    pub theme: String,
}

pub fn run(args: Vec<String>) -> Result<()> {
    let cli = match Cli::try_parse_from(args) {
        Ok(c) => c,
        Err(e) => e.exit(),
    };
    let theme = load_theme(&cli.theme);
    let connection = conn::open(&cli.connect)?;
    let mut app = App::new(connection, theme);

    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        SetTitle("bobtop-db")
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = app.run(&mut terminal);

    disable_raw_mode().ok();
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .ok();
    terminal.show_cursor().ok();

    result
}
