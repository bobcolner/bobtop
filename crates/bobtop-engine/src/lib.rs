//! The bobtop sampling engine + agent query layer.
//!
//! Two layers in one crate:
//!
//! - [`engine`] — bus + collectors + per-pid attribution + latest-value
//!   store + retrospective ring buffer, packaged behind one
//!   [`Engine::start`](engine::Engine::start) call.
//! - [`agent`] — the queryable JSON-RPC surface (schema types, query
//!   handlers, Unix-socket server, client helpers). Wire format
//!   documented in `docs/agent-schema.md` and stable at `bobtop/v1`.
//!
//! Embedders (the daemon binary, future MCP shims, library bindings)
//! pull this crate to get a self-contained engine + agent surface
//! without dragging in TUI / clap / ratatui weight.

#![warn(missing_debug_implementations)]

pub mod agent;
pub mod engine;

// Top-level re-exports for the most common entry points.
pub use engine::{Engine, EngineConfig, EngineMeta};
