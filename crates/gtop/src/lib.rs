//! Library facade so examples and tests can construct an [`app::App`] and
//! call [`ui::draw`] without going through the binary's `tokio::main`. The
//! binary `main.rs` re-uses the same modules.

// Internal plumbing — formerly the standalone bobtop-core /
// bobtop-collectors / bobtop-pid-attr / bobtop-engine crates. Folded
// into gtop in the Phase 1 refactor (see docs/gtop-refactor.md). The
// module names match the old crate names with hyphens dropped, so a
// `bobtop_core::sample::ProcessInfo` import becomes
// `crate::core::sample::ProcessInfo`.
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
