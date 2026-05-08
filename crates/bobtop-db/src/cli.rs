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
use crate::conn::{self, DuckLakeAttach};

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
    ///   `duckdb://memory` (in-memory; useful with --ducklake-*)
    ///   `mock` (built-in demo, default)
    #[arg(long, default_value = "mock")]
    pub connect: String,

    /// Theme name from the bundled bobtop registry.
    #[arg(long, default_value = DEFAULT_THEME_NAME)]
    pub theme: String,

    /// DuckLake catalog Postgres URL. When set together with
    /// `--ducklake-path`, runs `ATTACH 'ducklake:postgres:URL' AS
    /// <name> (DATA_PATH '...')` after opening the DuckDB connection.
    /// Only valid with `--connect duckdb://...`.
    #[arg(long)]
    pub ducklake_catalog: Option<String>,

    /// DuckLake DATA_PATH (directory containing the lake's Parquet files).
    #[arg(long)]
    pub ducklake_path: Option<String>,

    /// Name to attach the DuckLake under. Shows up as a database in
    /// the tree pane. Defaults to `lake`.
    #[arg(long, default_value = "lake")]
    pub ducklake_name: String,
}

pub fn run(args: Vec<String>) -> Result<()> {
    let cli = match Cli::try_parse_from(args) {
        Ok(c) => c,
        Err(e) => e.exit(),
    };
    let theme = load_theme(&cli.theme);
    let ducklake = match (cli.ducklake_catalog.as_ref(), cli.ducklake_path.as_ref()) {
        (Some(url), Some(path)) => Some(DuckLakeAttach {
            name: cli.ducklake_name.clone(),
            catalog_pg_url: url.clone(),
            data_path: path.clone(),
        }),
        (Some(_), None) | (None, Some(_)) => {
            anyhow::bail!("--ducklake-catalog and --ducklake-path must be set together")
        }
        (None, None) => None,
    };
    let connection = conn::open(&cli.connect, ducklake)?;
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
