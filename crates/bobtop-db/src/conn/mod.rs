//! Backend-agnostic connection abstraction.
//!
//! The TUI talks to a single [`Connection`] trait; concrete impls
//! (Postgres, DuckDB, DuckLake, mock) plug in below. Methods are
//! synchronous — async DB clients (tokio-postgres) wrap their futures
//! with `block_on` at the call site so the render loop never sees a
//! `.await`. Volume per call is bounded by `LIMIT` clauses, so the
//! latency hit is acceptable for browse-only access.

pub mod duck;
pub mod mock;
pub mod pg;

use anyhow::Result;

/// Top-level handle: owns the database list for a single endpoint.
/// "Endpoint" = a Postgres server, a single DuckDB file, etc.
///
/// Not `Send`: the `duckdb` crate's `Connection` is `!Send` (raw
/// pointers internally), and the TUI is single-threaded anyway.
pub trait Connection {
    /// Display label for the endpoint (e.g. "postgres@db.example",
    /// "duckdb:/data/lake.db", "mock").
    fn endpoint_label(&self) -> &str;

    /// One row per logical database the endpoint exposes. Postgres
    /// returns its `pg_database` list; DuckDB returns `main` plus
    /// any `ATTACH`-ed catalogs.
    fn databases(&self) -> Result<Vec<Database>>;

    /// Schemas inside a single database.
    fn schemas(&self, db: &str) -> Result<Vec<Schema>>;

    /// Tables inside a single schema.
    fn tables(&self, db: &str, schema: &str) -> Result<Vec<Table>>;

    /// Column metadata for a single table — used by the preview pane
    /// to label columns and pick alignment per type.
    fn columns(&self, db: &str, schema: &str, table: &str) -> Result<Vec<ColumnSpec>>;

    /// First `limit` rows of a table, in declared column order.
    /// Cells are stringified at the connection level so the renderer
    /// stays type-agnostic.
    fn preview_rows(
        &self,
        db: &str,
        schema: &str,
        table: &str,
        limit: usize,
    ) -> Result<Vec<Row>>;
}

#[derive(Debug, Clone)]
pub struct Database {
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Schema {
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Table {
    pub name: String,
    /// Row count if cheaply available (DuckDB makes this easy via
    /// `pragma_show_tables_expanded`; Postgres needs a live `count(*)`
    /// which we don't want to issue from a browser, so this stays
    /// `None` there). Surfaced in the tree row label once the real
    /// backends are wired.
    #[allow(dead_code)]
    pub estimated_rows: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ColumnSpec {
    pub name: String,
    pub data_type: String,
    /// Surfaced in the DDL / column-detail view once that lands.
    #[allow(dead_code)]
    pub nullable: bool,
}

/// One result row. Length == `columns(...).len()`. Cells are already
/// stringified — the connection picks a representation appropriate
/// for the backend (NULL → "—", numerics formatted with their natural
/// precision, timestamps in ISO 8601, etc.).
#[derive(Debug, Clone)]
pub struct Row {
    pub cells: Vec<String>,
}

/// Resolve `target` to a concrete connection. `target` is the
/// `--connect` CLI value:
///
/// - `mock` → built-in demo data
/// - `postgres://...` → tokio-postgres backend (TODO: not wired yet)
/// - `duckdb:///path` → duckdb-rs backend (TODO: not wired yet)
/// DuckLake attach parameters. When `Some`, [`open`] opens the DuckDB
/// connection and immediately runs `INSTALL ducklake; LOAD ducklake;
/// ATTACH 'ducklake:postgres:URL' AS <name> (DATA_PATH '...')` so the
/// lake is browsable as another catalog in the tree.
#[derive(Debug, Clone)]
pub struct DuckLakeAttach {
    pub name: String,
    pub catalog_pg_url: String,
    pub data_path: String,
}

pub fn open(
    target: &str,
    ducklake: Option<DuckLakeAttach>,
    read_only: bool,
) -> Result<Box<dyn Connection>> {
    match target {
        "mock" => Ok(Box::new(mock::MockConnection::demo())),
        s if s.starts_with("postgres://") || s.starts_with("postgresql://") => {
            if ducklake.is_some() {
                anyhow::bail!("--ducklake-* flags only apply with --connect duckdb://...");
            }
            Ok(Box::new(pg::PgConnection::open(s)?))
        }
        s if s.starts_with("duckdb://") => {
            let path = s.strip_prefix("duckdb://").unwrap_or("");
            let mut conn = duck::DuckConnection::open_with(path, read_only)?;
            if let Some(attach) = ducklake {
                conn.attach_ducklake(&attach.name, &attach.catalog_pg_url, &attach.data_path)?;
            }
            Ok(Box::new(conn))
        }
        other => anyhow::bail!("unrecognized connection target: {other}"),
    }
}
