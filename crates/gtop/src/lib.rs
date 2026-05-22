//! Library facade so examples and tests can construct an [`app::App`] and
//! call [`ui::draw`] without going through the binary's `tokio::main`. The
//! binary `main.rs` re-uses the same modules.

pub mod collectors;
pub mod core;
pub mod engine;
pub mod pid_attr;

pub mod agent;
pub mod app;
pub mod cli;
pub mod config;
pub mod cpuinfo;
pub mod group;
pub mod keys;
pub mod kill;
pub mod monitor_theme;
pub mod options_editor;
pub mod presets;
pub mod proc_sort;
pub mod process_detail;
pub mod state;
pub mod tui;
pub mod ui;
pub mod widgets;
