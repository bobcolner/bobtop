//! Agent query surface — wire format + handlers + Unix-socket server.
//!
//! The wire format is documented in the README. Every response
//! carries `"schema": "bobtop/v1"` so clients can detect a version skew.
//! Embedders (the daemon, MCP shims, etc.) call [`server::spawn`] to bring
//! the listener up alongside the engine, and use [`client`] to issue
//! requests over the socket.

pub mod client;
pub(crate) mod query;
pub(crate) mod schema;
pub mod server;

pub use server::spawn;
