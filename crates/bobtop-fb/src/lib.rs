//! `bobtop-fb` — TUI file browser library.
//!
//! Public surface intentionally narrow: callers (the `bobtop-fb` binary
//! today, possibly bobtop later) construct an [`App`], hand it a terminal
//! and an event stream, and let `App::run` drive. All rendering goes
//! through `bobtop-tui` widgets — this crate does not import ratatui
//! drawing primitives directly.

#![forbid(unsafe_code)]

pub mod app;
pub mod fs;
pub mod keys;
pub mod nav;
pub mod preview;
pub mod ui;

pub use app::App;
pub use fs::entry::{EntryKind, FsEntry};
