//! `gfb` — TUI file browser, with optional Postgres / DuckDB /
//! DuckLake browsing.
//!
//! This crate ships as a binary (`gfb`); the library exists mainly so
//! the binary can live in `src/bin/` and so integration tests can drive
//! the app without re-spawning a process. External callers should go
//! through [`cli::run`] — everything else is crate-internal plumbing.
//!
//! # Features
//!
//! - `postgres` — Postgres backend via `tokio-postgres` (browse a live DB).
//! - `duckdb`   — DuckDB backend, with DuckLake attach support.
//! - `all-sources` (default) — enables both `postgres` and `duckdb`.
//!
//! Without any DB features the file-browser surface still works; the
//! `--connect` flag will reject postgres/duckdb URLs at runtime.

#![forbid(unsafe_code)]

pub mod cli;
pub mod sources;

pub(crate) mod app;
pub(crate) mod config;
pub(crate) mod editor;
pub(crate) mod options;
pub(crate) mod find;
pub(crate) mod fs;
pub(crate) mod keys;
pub(crate) mod preview;
pub(crate) mod state;
pub(crate) mod tree;
pub(crate) mod ui;
