//! `bobtop-db` — TUI database browser.
//!
//! Browses Postgres + DuckDB / DuckLake catalogs with a tree-pane on
//! the left (connection → database → schema → table) and a row-preview
//! pane on the right. Read-only at first; query editing is a follow-up.
//!
//! Public surface intentionally narrow: external callers go through
//! [`cli::run`].

#![forbid(unsafe_code)]

pub mod cli;
// `conn` and `tree` are public because integration tests need to
// drive backends directly without spinning up a TUI. The rest of
// the modules stay crate-internal.
pub mod conn;
pub mod tree;

pub(crate) mod app;
pub(crate) mod keys;
pub(crate) mod ui;
