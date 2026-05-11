//! Backend-agnostic connection abstraction.
//!
//! The TUI talks to a single [`Connection`] trait; concrete impls
//! (Postgres, DuckDB, DuckLake, mock) plug in below. Methods are
//! synchronous — async DB clients (tokio-postgres) wrap their futures
//! with `block_on` at the call site so the render loop never sees a
//! `.await`. Volume per call is bounded by `LIMIT` clauses, so the
//! latency hit is acceptable for browse-only access.

pub mod mock;

use anyhow::Result;

/// Tree node kind for the catalog browser. `Endpoint` is the
/// connection root, `Database`/`Schema`/`Table` are the natural
/// catalog levels Postgres + DuckDB share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    Endpoint,
    Database,
    Schema,
    Table,
}

/// Path through the catalog tree, identifying a specific node. The
/// `conn` index disambiguates between two endpoints with the same
/// database name (e.g. two `--connect`s both exposing `ml_momo`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct NodePath {
    pub conn: usize,
    pub database: Option<String>,
    pub schema: Option<String>,
    pub table: Option<String>,
}

impl NodePath {
    pub fn endpoint(conn: usize) -> Self {
        Self {
            conn,
            ..Default::default()
        }
    }

    pub fn database(conn: usize, db: &str) -> Self {
        Self {
            conn,
            database: Some(db.to_string()),
            ..Default::default()
        }
    }

    pub fn schema(conn: usize, db: &str, sch: &str) -> Self {
        Self {
            conn,
            database: Some(db.to_string()),
            schema: Some(sch.to_string()),
            ..Default::default()
        }
    }

    pub fn table(conn: usize, db: &str, sch: &str, tbl: &str) -> Self {
        Self {
            conn,
            database: Some(db.to_string()),
            schema: Some(sch.to_string()),
            table: Some(tbl.to_string()),
        }
    }

    /// What level of the catalog this path identifies.
    pub fn level(&self) -> NodeKind {
        match (
            self.database.is_some(),
            self.schema.is_some(),
            self.table.is_some(),
        ) {
            (false, _, _) => NodeKind::Endpoint,
            (true, false, _) => NodeKind::Database,
            (true, true, false) => NodeKind::Schema,
            (true, true, true) => NodeKind::Table,
        }
    }
}

/// Per-node payload the tree renderer carries. Captured separately
/// from [`NodePath`] so the toolkit's [`crate::tree::Catalog`] impl
/// can return `(NodePath, NodeData)` tuples without forcing the
/// renderer to walk the path each time.
#[derive(Debug, Clone)]
pub struct NodeData {
    pub kind: NodeKind,
    pub label: String,
}

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

    /// Cheaply-estimated row count from catalog statistics — no full
    /// scan. Returns `None` when a fast estimate isn't available for
    /// this backend. Postgres reads `pg_class.reltuples` (updated by
    /// VACUUM/ANALYZE); other backends may return `None`.
    fn approximate_row_count(&self, _db: &str, _schema: &str, _table: &str) -> Option<u64> {
        None
    }

    /// Execute an arbitrary SQL statement and return the result set.
    /// Column names are returned separately so the renderer can label
    /// the table header without a schema lookup. Cells are stringified
    /// by the backend — the renderer stays type-agnostic.
    fn execute_query(&self, sql: &str) -> Result<(Vec<String>, Vec<Row>)>;

    /// Drop a table (`table = Some`) or schema (`table = None`).
    /// If `cascade` is true, appends `CASCADE` to drop dependent objects too.
    fn drop_object(&self, db: &str, schema: &str, table: Option<&str>, cascade: bool) -> Result<()>;
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
            #[cfg(feature = "postgres")]
            {
                Ok(Box::new(super::pg::PgConnection::open(s)?))
            }
            #[cfg(not(feature = "postgres"))]
            {
                let _ = s;
                anyhow::bail!(
                    "postgres backend not compiled in — rebuild gfb with --features postgres"
                )
            }
        }
        s if s.starts_with("duckdb://") => {
            let path = s.strip_prefix("duckdb://").unwrap_or("");
            #[cfg(feature = "duckdb")]
            {
                let mut conn = super::duckdb::DuckConnection::open_with(path, read_only)?;
                if let Some(attach) = ducklake {
                    conn.attach_ducklake(&attach.name, &attach.catalog_pg_url, &attach.data_path)?;
                }
                Ok(Box::new(conn))
            }
            #[cfg(not(feature = "duckdb"))]
            {
                let _ = (path, ducklake, read_only);
                anyhow::bail!(
                    "duckdb backend not compiled in — rebuild gfb with --features duckdb"
                )
            }
        }
        other => anyhow::bail!("unrecognized connection target: {other}"),
    }
}
