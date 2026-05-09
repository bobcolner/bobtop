//! Tree-pane data sources for `gfb`.
//!
//! Phase 4 of the gtop refactor (see `docs/gtop-refactor.md`) folded
//! the database backends here. Each subsource is feature-gated so a
//! default `cargo install gfb` doesn't pull `tokio-postgres` /
//! `duckdb` (and their transitive ~30 MB of build cost):
//!
//! - [`db`] — backend-agnostic [`Connection`] trait + the always-on
//!   `MockConnection`. Required by every consumer.
//! - [`pg`] — Postgres backend (`tokio-postgres`). Feature
//!   `postgres`.
//! - [`duckdb`] — DuckDB / DuckLake backend. Feature `duckdb`.
//!
//! A filesystem `Catalog` impl is planned for this module in Phase 5
//! (where `gfb`'s tree pane goes multi-root); for now, `gfb`'s
//! filesystem tree-walk lives in `crate::tree`.
//!
//! Re-exports below let callers write `gfb::sources::Connection`
//! without spelling the inner module path.

pub mod db;
pub mod multi;
pub(crate) mod worker;

pub use db::{
    open, ColumnSpec, Connection, Database, DuckLakeAttach, NodeData, NodeKind, NodePath, Row,
    Schema, Table,
};
pub use multi::{AnyNodeId, AnyRow};
pub(crate) use multi::MultiRootCatalog;
pub(crate) use worker::{ConnectionWorker, LoadResult};

#[cfg(feature = "postgres")]
pub mod pg;

#[cfg(feature = "duckdb")]
pub mod duckdb;
