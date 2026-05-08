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

pub(crate) mod app;
pub(crate) mod conn;
pub(crate) mod keys;
pub(crate) mod tree;
pub(crate) mod ui;
