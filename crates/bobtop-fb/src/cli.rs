//! `bobtop-fb`'s CLI entry — separated from the `main!` so the
//! daemon can dispatch into us via a `bobtop fb …` subcommand
//! without spawning a second binary. `bin/bobtop-fb.rs` is a thin
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

use bobtop_tui::{load_theme, DEFAULT_THEME_NAME};

use crate::{state, App, ImageBackendChoice};

#[derive(Debug, Parser)]
#[command(
    name = "bobtop-fb",
    bin_name = "bobtop-fb",
    about = "TUI file browser"
)]
pub struct Cli {
    /// Directory to start in. Defaults to $PWD.
    pub path: Option<PathBuf>,
    /// Theme name from the bundled bobtop registry.
    #[arg(long, default_value = DEFAULT_THEME_NAME)]
    pub theme: String,
    /// Image preview backend. `auto` detects kitty/iTerm/sixel and
    /// falls back to sextant blocks; `native` forces viuer; `sextant`
    /// forces our internal rasterizer.
    #[arg(long, value_parser = ["auto", "native", "sextant"], default_value = "auto")]
    pub image_backend: String,
}

/// Parse args (clap), set up the terminal, and run the App. `args`
/// is the full argv-style vector — the first element is the program
/// name, used by clap for error reporting only. Both the standalone
/// `bobtop-fb` binary and the bundled `bobtop fb …` subcommand call
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
    //      trips from bobtop, or repeated standalone launches).
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
    let mut app = App::new_with(start, theme, backend)?;

    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        SetTitle("bobtop-fb")
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
