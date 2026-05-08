//! Daemon-side glue around the agent layer.
//!
//! The wire format, query handlers, and Unix-socket server now live in
//! `bobtop_engine::agent`. This module is just the daemon-binary
//! pieces: the `gtop agent <subcommand>` CLI and a re-export of the
//! engine's `spawn` so `main.rs` can keep its existing call shape.

pub mod client;

pub use bobtop_engine::agent::server::spawn;
