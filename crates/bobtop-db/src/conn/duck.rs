//! DuckDB backend (and DuckLake-attach helper).
//!
//! Opens a DuckDB connection — file path or in-memory — and exposes
//! the catalog through the same [`Connection`] trait the Postgres
//! backend uses. DuckDB conveniently exposes `information_schema` so
//! the schema/table/column queries stay symmetric with PG's.
//!
//! DuckLake support: [`DuckConnection::attach_ducklake`] runs
//! `INSTALL ducklake; LOAD ducklake; ATTACH 'ducklake:postgres:URL'
//! AS <name> (DATA_PATH '...')` which makes the lake show up as
//! another database in the tree pane. Combined with an in-memory
//! DuckDB connection, that's the canonical way to browse a lake
//! whose catalog lives in Postgres and whose data lives in Parquet.

use anyhow::{Context, Result};
use duckdb::{params, AccessMode, Config, Connection as DuckConn};

use super::{ColumnSpec, Connection, Database, Row, Schema, Table};

pub struct DuckConnection {
    conn: DuckConn,
    label: String,
}

impl DuckConnection {
    /// Open a DuckDB file (or `:memory:`) in read-only mode.
    /// In-memory paths fall back to default access — they're RAM-only,
    /// so the access-mode distinction doesn't matter and read-only
    /// would block the ATTACH path.
    pub fn open(path: &str) -> Result<Self> {
        Self::open_with(path, true)
    }

    /// Open with explicit read-only / read-write choice. The CLI's
    /// `--write` flag routes here so users who actually want to mutate
    /// can opt in; everything else (browser access, lake attach via an
    /// already-busy file) sticks with the read-only default.
    pub fn open_with(path: &str, read_only: bool) -> Result<Self> {
        let in_memory = path.is_empty() || path == ":memory:" || path == "memory";
        let conn = if in_memory {
            // In-memory: read-only would block the ATTACH path that
            // makes this connection useful (DuckLake mounting the lake
            // catalog). Always read-write for `:memory:`.
            DuckConn::open_in_memory().context("open in-memory duckdb")?
        } else if read_only {
            let cfg = Config::default()
                .access_mode(AccessMode::ReadOnly)
                .context("set read-only access mode")?;
            DuckConn::open_with_flags(path, cfg)
                .with_context(|| format!("open duckdb (read-only): {path}"))?
        } else {
            DuckConn::open(path).with_context(|| format!("open duckdb: {path}"))?
        };
        let label = if in_memory {
            "duckdb://memory".to_string()
        } else if read_only {
            format!("duckdb://{path} [ro]")
        } else {
            format!("duckdb://{path}")
        };
        Ok(Self { conn, label })
    }

    /// `INSTALL ducklake; LOAD ducklake; ATTACH 'ducklake:postgres:URL'
    /// AS <name> (DATA_PATH '...')`. Idempotent — DuckDB makes
    /// INSTALL/LOAD safe to repeat. Errors here are surfaced to the
    /// caller so the CLI can decide whether to abort or carry on
    /// without the lake (currently aborts).
    pub fn attach_ducklake(
        &mut self,
        name: &str,
        catalog_pg_url: &str,
        data_path: &str,
    ) -> Result<()> {
        // ducklake is a core DuckDB extension on recent builds. We
        // skip the `FROM community` qualifier so DuckDB picks up an
        // already-installed core copy (the ml_momo dev box has it
        // pinned from the core repo, and forcing `community` errors
        // out with "extension installed from a different repository").
        // If the extension isn't installed at all, plain `INSTALL`
        // resolves to the user's default repo (core).
        self.conn
            .execute_batch("INSTALL ducklake; LOAD ducklake;")
            .context("install/load ducklake extension")?;
        // Identifiers (the AS name) need quoting; the `ducklake:...`
        // string is a value, not an identifier — it goes inside single
        // quotes. We escape any embedded quotes defensively.
        let attach_target = format!(
            "ducklake:postgres:{}",
            catalog_pg_url.replace('\'', "''")
        );
        let data_path_escaped = data_path.replace('\'', "''");
        let sql = format!(
            "ATTACH '{attach_target}' AS {} (DATA_PATH '{data_path_escaped}')",
            quote_ident(name)
        );
        self.conn
            .execute(&sql, params![])
            .with_context(|| format!("attach ducklake as {name}"))?;
        // Append the lake to the label so the status bar reflects it.
        self.label = format!("{}+ducklake://{}", self.label, name);
        Ok(())
    }

    /// All non-system catalogs visible to this connection. After
    /// `attach_ducklake`, includes the lake.
    fn list_databases(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT catalog_name \
             FROM information_schema.schemata \
             WHERE catalog_name NOT IN ('system', 'temp') \
             ORDER BY catalog_name",
        )?;
        let rows = stmt.query_map(params![], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

impl Connection for DuckConnection {
    fn endpoint_label(&self) -> &str {
        &self.label
    }

    fn databases(&self) -> Result<Vec<Database>> {
        Ok(self
            .list_databases()?
            .into_iter()
            .map(|name| Database { name })
            .collect())
    }

    fn schemas(&self, db: &str) -> Result<Vec<Schema>> {
        let mut stmt = self.conn.prepare(
            "SELECT schema_name \
             FROM information_schema.schemata \
             WHERE catalog_name = ? \
               AND schema_name NOT IN ('information_schema', 'pg_catalog') \
             ORDER BY schema_name",
        )?;
        let rows = stmt.query_map(params![db], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(Schema { name: r? });
        }
        Ok(out)
    }

    fn tables(&self, db: &str, schema: &str) -> Result<Vec<Table>> {
        let mut stmt = self.conn.prepare(
            "SELECT table_name \
             FROM information_schema.tables \
             WHERE table_catalog = ? AND table_schema = ? \
               AND table_type IN ('BASE TABLE', 'VIEW') \
             ORDER BY table_name",
        )?;
        let rows = stmt.query_map(params![db, schema], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(Table {
                name: r?,
                estimated_rows: None,
            });
        }
        Ok(out)
    }

    fn columns(&self, db: &str, schema: &str, table: &str) -> Result<Vec<ColumnSpec>> {
        let mut stmt = self.conn.prepare(
            "SELECT column_name, data_type, is_nullable \
             FROM information_schema.columns \
             WHERE table_catalog = ? AND table_schema = ? AND table_name = ? \
             ORDER BY ordinal_position",
        )?;
        let rows = stmt.query_map(params![db, schema, table], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (name, data_type, nullable) = r?;
            out.push(ColumnSpec {
                name,
                data_type,
                nullable: nullable == "YES",
            });
        }
        Ok(out)
    }

    fn preview_rows(
        &self,
        db: &str,
        schema: &str,
        table: &str,
        limit: usize,
    ) -> Result<Vec<Row>> {
        let columns = self.columns(db, schema, table)?;
        if columns.is_empty() {
            return Ok(Vec::new());
        }
        // Cast every column to VARCHAR so the renderer reads strings
        // regardless of underlying type. DuckDB's VARCHAR cast handles
        // STRUCT/LIST/MAP types by emitting their natural literal form.
        let select_list = columns
            .iter()
            .map(|c| format!("{}::VARCHAR", quote_ident(&c.name)))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT {select_list} FROM {}.{}.{} LIMIT {limit}",
            quote_ident(db),
            quote_ident(schema),
            quote_ident(table),
        );
        let mut stmt = self
            .conn
            .prepare(&sql)
            .with_context(|| format!("prepare preview for {schema}.{table}"))?;
        let n_cols = columns.len();
        let mut rows_iter = stmt.query(params![])?;
        let mut out = Vec::new();
        while let Some(row) = rows_iter.next()? {
            let cells: Vec<String> = (0..n_cols)
                .map(|i| {
                    row.get::<_, Option<String>>(i)
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| "—".to_string())
                })
                .collect();
            out.push(Row { cells });
        }
        Ok(out)
    }
}

fn quote_ident(ident: &str) -> String {
    let mut out = String::with_capacity(ident.len() + 2);
    out.push('"');
    for ch in ident.chars() {
        if ch == '"' {
            out.push('"');
        }
        out.push(ch);
    }
    out.push('"');
    out
}
