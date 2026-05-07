//! `bobtop-fb` binary entry point.

use std::io;
use std::path::PathBuf;

use anyhow::Result;
use bobtop_fb::{App, ImageBackendChoice};
use bobtop_tui::{load_theme, DEFAULT_THEME_NAME};
use clap::Parser;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

#[derive(Debug, Parser)]
#[command(name = "bobtop-fb", about = "TUI file browser")]
struct Cli {
    /// Directory to start in. Defaults to $PWD.
    path: Option<PathBuf>,
    /// Theme name from the bundled bobtop registry.
    #[arg(long, default_value = DEFAULT_THEME_NAME)]
    theme: String,
    /// Image preview backend. `auto` detects kitty/iTerm/sixel and
    /// falls back to sextant blocks; `native` forces viuer; `sextant`
    /// forces our internal rasterizer.
    #[arg(long, value_parser = ["auto", "native", "sextant"], default_value = "auto")]
    image_backend: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let start = match cli.path {
        Some(p) => p,
        None => std::env::current_dir()?,
    };
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
