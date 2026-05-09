//! Postgres backend (`tokio-postgres`).
//!
//! v1 scope: browse the database the user connected to. `databases()`
//! returns just the connected database — switching databases means
//! reconnecting with a different URL. A follow-up can connect to the
//! `postgres` maintenance DB to enumerate all databases and lazily
//! open per-DB connections for schema/table queries.
//!
//! Schema/table discovery uses `information_schema` so it works on
//! any Postgres-compatible target. Preview rows go through a
//! dynamic `SELECT col::text` so the renderer stays type-agnostic —
//! NULLs come back as `None` and become `—`.

use std::sync::Mutex;

use anyhow::{Context, Result};
use tokio::runtime::Runtime;
use tokio_postgres::{Client, NoTls};

use super::db::{ColumnSpec, Connection, Database, Row, Schema, Table};

/// Holds a single-threaded Tokio runtime alongside the wire client.
/// Trait methods are sync; each call `block_on`s the appropriate
/// async query. Cheap because the connection is already established.
pub struct PgConnection {
    label: String,
    db_name: String,
    rt: Runtime,
    /// Wrapped in a Mutex because `&self` methods on the trait need
    /// mutable access to the underlying client. tokio-postgres
    /// `Client` is `Send + Sync` but query() takes `&self`, so the
    /// Mutex is really protecting the runtime context invocation
    /// rather than the client itself — but it's the simplest shape.
    client: Mutex<Client>,
}

impl PgConnection {
    pub fn open(url: &str) -> Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("build tokio runtime for postgres")?;

        let (client, connection) = rt
            .block_on(async {
                let (c, conn) = tokio_postgres::connect(url, NoTls).await?;
                Ok::<_, tokio_postgres::Error>((c, conn))
            })
            .with_context(|| format!("connect to {url}"))?;

        // Spawn the connection driver onto the runtime so the wire
        // protocol pumps. Errors here are surfaced as next-query
        // failures (the client returns "connection closed").
        rt.spawn(async move {
            if let Err(e) = connection.await {
                tracing::warn!(error = %e, "postgres connection driver exited");
            }
        });

        let db_name: String = rt
            .block_on(async {
                let row = client.query_one("SELECT current_database()", &[]).await?;
                Ok::<_, tokio_postgres::Error>(row.get::<_, String>(0))
            })
            .context("read current_database()")?;

        let label = format!("pg://{}", short_host_from_url(url, &db_name));
        Ok(Self {
            label,
            db_name,
            rt,
            client: Mutex::new(client),
        })
    }

    fn block<F, T>(&self, fut: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        self.rt.block_on(fut)
    }
}

impl Connection for PgConnection {
    fn endpoint_label(&self) -> &str {
        &self.label
    }

    fn databases(&self) -> Result<Vec<Database>> {
        // v1: surface the database we're connected to. Listing all
        // databases would require a maintenance connection — see the
        // module docs.
        Ok(vec![Database {
            name: self.db_name.clone(),
        }])
    }

    fn schemas(&self, db: &str) -> Result<Vec<Schema>> {
        if db != self.db_name {
            return Ok(Vec::new());
        }
        let client = self.client.lock().expect("client mutex poisoned");
        let rows = self
            .block(client.query(
                "SELECT schema_name \
                 FROM information_schema.schemata \
                 WHERE schema_name NOT IN ('pg_catalog', 'pg_toast', 'information_schema') \
                   AND schema_name NOT LIKE 'pg_temp_%' \
                   AND schema_name NOT LIKE 'pg_toast_temp_%' \
                 ORDER BY schema_name",
                &[],
            ))
            .context("query information_schema.schemata")?;
        Ok(rows
            .into_iter()
            .map(|r| Schema { name: r.get::<_, String>(0) })
            .collect())
    }

    fn tables(&self, db: &str, schema: &str) -> Result<Vec<Table>> {
        if db != self.db_name {
            return Ok(Vec::new());
        }
        let client = self.client.lock().expect("client mutex poisoned");
        let rows = self
            .block(client.query(
                "SELECT table_name \
                 FROM information_schema.tables \
                 WHERE table_schema = $1 \
                   AND table_type IN ('BASE TABLE', 'VIEW', 'FOREIGN TABLE') \
                 ORDER BY table_name",
                &[&schema],
            ))
            .context("query information_schema.tables")?;
        Ok(rows
            .into_iter()
            .map(|r| Table {
                name: r.get::<_, String>(0),
                estimated_rows: None,
            })
            .collect())
    }

    fn columns(&self, db: &str, schema: &str, table: &str) -> Result<Vec<ColumnSpec>> {
        if db != self.db_name {
            return Ok(Vec::new());
        }
        let client = self.client.lock().expect("client mutex poisoned");
        let rows = self
            .block(client.query(
                "SELECT column_name, data_type, is_nullable \
                 FROM information_schema.columns \
                 WHERE table_schema = $1 AND table_name = $2 \
                 ORDER BY ordinal_position",
                &[&schema, &table],
            ))
            .context("query information_schema.columns")?;
        Ok(rows
            .into_iter()
            .map(|r| ColumnSpec {
                name: r.get::<_, String>(0),
                data_type: r.get::<_, String>(1),
                nullable: r.get::<_, String>(2) == "YES",
            })
            .collect())
    }

    fn preview_rows(
        &self,
        db: &str,
        schema: &str,
        table: &str,
        limit: usize,
    ) -> Result<Vec<Row>> {
        if db != self.db_name {
            return Ok(Vec::new());
        }
        let columns = self.columns(db, schema, table)?;
        if columns.is_empty() {
            return Ok(Vec::new());
        }

        // Build a `SELECT "col1"::text, "col2"::text, ... FROM "schema"."table" LIMIT $1`.
        // Casting to text means we can read everything as Option<String>
        // regardless of the underlying type, and Postgres formats the
        // value in its natural representation (numerics, timestamps,
        // arrays, jsonb — all already-pretty strings).
        let select_list = columns
            .iter()
            .map(|c| format!("{}::text", quote_ident(&c.name)))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT {select_list} FROM {}.{} LIMIT $1",
            quote_ident(schema),
            quote_ident(table),
        );
        let client = self.client.lock().expect("client mutex poisoned");
        let rows = self
            .block(client.query(&sql, &[&(limit as i64)]))
            .with_context(|| format!("preview {schema}.{table}"))?;

        Ok(rows
            .into_iter()
            .map(|r| Row {
                cells: (0..columns.len())
                    .map(|i| {
                        r.try_get::<_, Option<String>>(i)
                            .ok()
                            .flatten()
                            .unwrap_or_else(|| "—".to_string())
                    })
                    .collect(),
            })
            .collect())
    }
}

/// Quote a SQL identifier — wrap in double quotes and escape embedded
/// quotes by doubling them. Required so schema/table/column names with
/// reserved words, mixed case, or punctuation work correctly.
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

/// Build a compact endpoint label for the status line.
/// Examples: `pg://localhost/ml_momo`, `pg:///ml_momo`.
fn short_host_from_url(url: &str, db: &str) -> String {
    // Cheap and stable: extract the host between `@` and `/db`. We
    // don't fully URL-parse because the label is informational only.
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let after_at = after_scheme.split_once('@').map(|(_, rest)| rest).unwrap_or(after_scheme);
    let host = after_at.split('/').next().unwrap_or("?");
    if host.is_empty() {
        format!("/{db}")
    } else {
        format!("{host}/{db}")
    }
}

#[cfg(test)]
mod tests {
    use super::quote_ident;

    #[test]
    fn quotes_identifiers() {
        assert_eq!(quote_ident("public"), "\"public\"");
        assert_eq!(quote_ident("Mixed Case"), "\"Mixed Case\"");
        assert_eq!(quote_ident("with\"quote"), "\"with\"\"quote\"");
    }
}
