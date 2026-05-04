//! Library facade so examples and tests can construct an [`app::App`] and
//! call [`ui::draw`] without going through the binary's `tokio::main`. The
//! binary `main.rs` re-uses the same modules.

pub mod app;
pub mod cli;
pub mod config;
pub mod group;
pub mod tui;
pub mod ui;
pub mod state;
