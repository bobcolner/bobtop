//! `gfb` — TUI file browser library.
//!
//! Public surface intentionally narrow: external callers go through
//! [`cli::run`] and nothing else. All other modules are crate-internal
//! plumbing — they were previously `pub` so the binary in `src/bin/`
//! could reach them, but the binary only ever called `cli::run`, so
//! exposing the rest was bloat.

#![forbid(unsafe_code)]

pub mod cli;

pub(crate) mod app;
pub(crate) mod editor;
pub(crate) mod find;
pub(crate) mod fs;
pub(crate) mod keys;
pub(crate) mod preview;
pub(crate) mod state;
pub(crate) mod tree;
pub(crate) mod ui;
