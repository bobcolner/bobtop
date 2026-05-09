//! `gfb`'s CLI entry — separated from the `main!` so the
//! daemon can dispatch into us via a `gtop fb …` subcommand
//! without spawning a second binary. `bin/gfb.rs` is a thin
//! shell that calls [`run`] with the process arg vector.

use std::io;
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use gtui::{load_theme, DEFAULT_THEME_NAME};

use crate::app::{App, ImageBackendChoice};
use crate::state;

#[derive(Debug, Parser)]
#[command(
    name = "gfb",
    bin_name = "gfb",
    about = "TUI file (and database) browser"
)]
pub struct Cli {
    /// Directory to start in. Defaults to $PWD.
    pub path: Option<PathBuf>,
    /// Theme name from the bundled gtop registry.
    #[arg(long, default_value = DEFAULT_THEME_NAME)]
    pub theme: String,
    /// Image preview backend. `auto` detects kitty/iTerm/sixel and
    /// falls back to sextant blocks; `native` forces viuer; `sextant`
    /// forces our internal rasterizer.
    #[arg(long, value_parser = ["auto", "native", "sextant"], default_value = "auto")]
    pub image_backend: String,
    /// Database connection URL. Repeat to attach multiple endpoints —
    /// each shows up as a sibling of the filesystem at depth 0 of the
    /// tree. Accepted schemes: `mock` (built-in demo data),
    /// `postgres://...` (needs `--features postgres`), `duckdb:///path`
    /// (needs `--features duckdb`).
    #[arg(long = "connect")]
    pub connections: Vec<String>,
    /// DuckLake catalog Postgres URL — used by the `duckdb://` backend
    /// to attach a lake. Requires `--ducklake-path` and applies to
    /// every `--connect duckdb://...` in this invocation.
    #[arg(long)]
    pub ducklake_catalog: Option<String>,
    /// DuckLake data directory (Parquet files).
    #[arg(long)]
    pub ducklake_path: Option<PathBuf>,
    /// Display name for the attached DuckLake catalog. Defaults to
    /// `lake`.
    #[arg(long, default_value = "lake")]
    pub ducklake_name: String,
}

/// Parse args (clap), set up the terminal, and run the App. `args`
/// is the full argv-style vector — the first element is the program
/// name, used by clap for error reporting only. Both the standalone
/// `gfb` binary and the bundled `gtop fb …` subcommand call
/// this with their respective slices.
pub fn run(args: Vec<String>) -> Result<()> {
    let cli = match Cli::try_parse_from(args) {
        Ok(c) => c,
        Err(e) => {
            // `--help` / `--version` are reported as Errors by clap;
            // print them to stdout and exit success so they don't
            // look like a parse failure.
            e.exit();
        }
    };
    // Path resolution priority:
    //   1. Explicit `--path` arg → exactly that directory.
    //   2. No arg → persisted `last_cwd` from the previous session
    //      (resumes the user where they left off across `b` round
    //      trips from gtop, or repeated standalone launches).
    //   3. No arg + no persisted state → $PWD.
    let persisted = state::load();
    let start = cli
        .path
        .or_else(|| {
            persisted
                .last_cwd
                .clone()
                .filter(|p| p.is_dir())
        })
        .map_or_else(|| std::env::current_dir(), Ok)?;
    let theme = load_theme(&cli.theme);
    let backend = match cli.image_backend.as_str() {
        "native" => ImageBackendChoice::Native,
        "sextant" => ImageBackendChoice::Sextant,
        _ => ImageBackendChoice::Auto,
    };

    // Build the DB connection list. Each `--connect` URL goes through
    // gfb::sources::open(); the DuckLake attach params apply to every
    // `duckdb://` URL in the same invocation. A failed connect prints
    // to stderr and continues — partial setups are useful for browse.
    let mut connections: Vec<Box<dyn crate::sources::Connection>> = Vec::new();
    for url in &cli.connections {
        let ducklake = match (&cli.ducklake_catalog, &cli.ducklake_path) {
            (Some(catalog), Some(path)) if url.starts_with("duckdb://") => {
                Some(crate::sources::DuckLakeAttach {
                    name: cli.ducklake_name.clone(),
                    catalog_pg_url: catalog.clone(),
                    data_path: path.to_string_lossy().into_owned(),
                })
            }
            _ => None,
        };
        match crate::sources::open(url, ducklake, /* read_only */ true) {
            Ok(c) => connections.push(c),
            Err(e) => eprintln!("gfb: --connect {url} failed: {e:#}"),
        }
    }

    let mut app = App::new_with(start, theme, backend, connections)?;

    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        SetTitle("gfb")
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = app.run(&mut terminal);

    // Snapshot the cwd on exit so the next launch can resume here.
    // Best-effort — a write failure shouldn't tank the whole exit.
    let final_cwd = std::path::PathBuf::from(app.cwd_display());
    state::save(&state::PersistedState {
        last_cwd: Some(final_cwd),
    });

    // Always tear down the alt screen even if `run` errored.
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
