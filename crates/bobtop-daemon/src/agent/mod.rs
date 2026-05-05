//! Agent query layer — a tiny Unix-socket JSON-RPC interface that exposes
//! the running daemon's [`SampleStore`] (and, in later phases, [`History`])
//! to read-only consumers.
//!
//! The wire format is documented in `docs/agent-schema.md`. Every response
//! carries `"schema": "bobtop/v1"` so clients can detect a version skew.
//!
//! Phase 2 (this commit) ships only the `snapshot` verb. Subsequent phases
//! add `top`, `peak`, `responsible_for`, etc., behind the same socket and
//! response envelope.

pub mod client;
pub mod query;
pub mod schema;
pub mod server;

pub use server::spawn;
