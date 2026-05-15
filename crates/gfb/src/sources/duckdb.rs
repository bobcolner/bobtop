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

use super::db::{ColumnSpec, Connection, Database, ModifiedKind, Row, Schema, Table, TableMeta};

pub struct DuckConnection {
    conn: DuckConn,
    label: String,
    /// Filesystem path to the underlying `.db` file, or `None` for
    /// `:memory:`. Surfaced as the table's `modified` timestamp so
    /// the preview pane shows *something* — DuckDB has no per-table
    /// mtime concept of its own.
    file_path: Option<std::path::PathBuf>,
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
        let file_path = if in_memory {
            None
        } else {
            Some(std::path::PathBuf::from(path))
        };
        Ok(Self { conn, label, file_path })
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

    fn execute_query(&self, sql: &str) -> Result<(Vec<String>, Vec<Row>)> {
        use std::convert::AsRef as _;
        let mut stmt = self.conn.prepare(sql)
            .with_context(|| format!("prepare: {}", sql.chars().take(80).collect::<String>()))?;
        let mut rows_iter = stmt.query(params![])
            .with_context(|| format!("execute: {}", sql.chars().take(80).collect::<String>()))?;
        // Column names require the statement to have been executed — get them
        // from the first Row's AsRef<Statement>, which shares the borrow without
        // conflicting with rows_iter.
        let mut col_names: Vec<String> = Vec::new();
        let mut rows = Vec::new();
        while let Some(row) = rows_iter.next()? {
            if col_names.is_empty() {
                let s = row.as_ref();
                let n = s.column_count();
                col_names = (0..n)
                    .map(|i| s.column_name(i).map(|s| s.as_str()).unwrap_or("?").to_string())
                    .collect();
            }
            let n_cols = col_names.len();
            let cells: Vec<String> = (0..n_cols)
                .map(|i| {
                    row.get::<_, duckdb::types::Value>(i)
                        .map(duck_value_to_string)
                        .unwrap_or_else(|_| "—".to_string())
                })
                .collect();
            rows.push(Row { cells });
        }
        // If there were no rows, get column names from the statement directly
        // (it has been executed, so this is now safe).
        if col_names.is_empty() {
            let n = stmt.column_count();
            col_names = (0..n)
                .map(|i| stmt.column_name(i).map(|s| s.as_str()).unwrap_or("?").to_string())
                .collect();
        }
        Ok((col_names, rows))
    }

    fn table_metadata(&self, db: &str, schema: &str, table: &str) -> Result<TableMeta> {
        // Rows: estimated_size from duckdb_tables(). Size: sum of
        // compressed block sizes from pragma_storage_info — only OK
        // here because the renderer fires this on demand (single
        // table the user opened), not for every visible row. DuckDB
        // doesn't track per-table mtime, so `modified` stays None.
        let mut stmt = self.conn.prepare(
            "SELECT estimated_size FROM duckdb_tables() \
             WHERE database_name = ? AND schema_name = ? AND table_name = ?",
        )?;
        let row_count: Option<u64> = stmt
            .query_row(params![db, schema, table], |row| {
                row.get::<_, Option<i64>>(0)
            })
            .ok()
            .flatten()
            .and_then(|n| if n > 0 { Some(n as u64) } else { None });

        let qualified = format!(
            "{}.{}.{}",
            quote_ident(db),
            quote_ident(schema),
            quote_ident(table)
        );
        let qualified_lit = qualified.replace('\'', "''");
        let size_sql = format!(
            "SELECT COALESCE(SUM(compressed_size), 0)::BIGINT \
             FROM pragma_storage_info('{qualified_lit}')"
        );
        let size_bytes = self
            .conn
            .query_row(&size_sql, params![], |r| r.get::<_, i64>(0))
            .ok()
            .and_then(|n| if n > 0 { Some(n as u64) } else { None });

        // DuckDB has no per-table mtime, so we fall back to the .db
        // file's mtime. Acceptable here because this method runs only
        // for the table the user explicitly opened — the misleading
        // "every row identical" symptom from the per-row Miller
        // column doesn't apply.
        let modified = self
            .file_path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok());

        Ok(TableMeta {
            row_count,
            size_bytes,
            modified,
            modified_kind: ModifiedKind::DbFile,
        })
    }

    fn drop_object(&self, db: &str, schema: &str, table: Option<&str>, cascade: bool) -> Result<()> {
        let cas = if cascade { " CASCADE" } else { "" };
        let sql = match table {
            Some(t) => format!(
                "DROP TABLE IF EXISTS {}.{}.{}{}",
                quote_ident(db), quote_ident(schema), quote_ident(t), cas
            ),
            None => format!(
                "DROP SCHEMA IF EXISTS {}.{}{}",
                quote_ident(db), quote_ident(schema), cas
            ),
        };
        self.conn.execute_batch(&sql)
            .with_context(|| format!("drop_object: {sql}"))
    }
}

fn duck_value_to_string(v: duckdb::types::Value) -> String {
    use duckdb::types::Value;
    match v {
        Value::Null => "—".to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::TinyInt(n) => n.to_string(),
        Value::SmallInt(n) => n.to_string(),
        Value::Int(n) => n.to_string(),
        Value::BigInt(n) => n.to_string(),
        Value::HugeInt(n) => n.to_string(),
        Value::UTinyInt(n) => n.to_string(),
        Value::USmallInt(n) => n.to_string(),
        Value::UInt(n) => n.to_string(),
        Value::UBigInt(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Double(f) => f.to_string(),
        Value::Decimal(d) => d.to_string(),
        Value::Text(s) => s,
        Value::Blob(b) => format!("\\x{}", b.iter().map(|byte| format!("{byte:02x}")).collect::<String>()),
        Value::Date32(days) => {
            // Days since 1970-01-01 — re-use the epoch helper from ui.rs is not
            // accessible here; do a minimal inline conversion.
            let secs = days as i64 * 86400;
            if secs >= 0 {
                let (y, mo, d, _, _, _) = epoch_ymd(secs as u64);
                format!("{y}-{mo:02}-{d:02}")
            } else {
                format!("(date:{days})")
            }
        }
        Value::Timestamp(_, us) => {
            let secs = us / 1_000_000;
            if secs >= 0 {
                let (y, mo, d, h, mi, s) = epoch_ymd(secs as u64);
                let frac = (us % 1_000_000).abs();
                if frac == 0 {
                    format!("{y}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
                } else {
                    format!("{y}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}.{frac:06}")
                }
            } else {
                format!("(ts:{us})")
            }
        }
        Value::Time64(_, us) => {
            let total_s = us / 1_000_000;
            let h = total_s / 3600;
            let m = (total_s % 3600) / 60;
            let s = total_s % 60;
            format!("{h:02}:{m:02}:{s:02}")
        }
        Value::Interval { months, days, nanos } => {
            format!("{months}mo {days}d {nanos}ns")
        }
        // Composite types — fall back to debug repr.
        other => format!("{other:?}"),
    }
}

fn epoch_ymd(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400;
    let z = days + 719468;
    let era = z / 146097;
    let doe = z % 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    (y as u32, mo as u32, d as u32, h as u32, m as u32, s as u32)
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
