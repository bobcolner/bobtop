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
// `conn` is now a thin re-export of `gfb::sources` — the DB
// backend code physically lives in the gfb crate as of the Phase 4
// refactor (see docs/gtop-refactor.md). `bobtop-db` itself dissolves
// into gfb in Phase 6; this transitional shim keeps internal call
// sites and integration tests resolving against the same API.
pub mod conn {
    pub use gfb::sources::*;
    pub mod mock {
        pub use gfb::sources::db::mock::*;
    }
    // The `gfb = { features = ["all-sources"] }` line in Cargo.toml
    // forces both backends on, so these submodules always exist
    // here regardless of bobtop-db's own feature flags.
    pub mod pg {
        pub use gfb::sources::pg::*;
    }
    pub mod duck {
        pub use gfb::sources::duckdb::*;
    }
}
pub mod tree;

pub(crate) mod app;
pub(crate) mod keys;
pub(crate) mod ui;
