//! Per-connection worker thread.
//!
//! `duckdb::Connection` is `!Send` (raw pointers internally), so we
//! can't move a `Box<dyn Connection>` across threads or run a query
//! from `tokio::spawn_blocking` and recover the result. The worker
//! pattern here puts each connection on a dedicated `std::thread`
//! that owns it for its lifetime; the main UI thread interacts via
//! a request channel.
//!
//! Two interaction shapes:
//!
//! - **Async**: `request_children(path, reply_tx)` — fire-and-forget
//!   from the catalog's `children()` (called during a flatten). The
//!   worker runs the query, sends the result on `reply_tx`. The
//!   main loop polls `reply_tx`'s receiver, populates the cache,
//!   and marks the UI dirty.
//! - **Sync**: `columns()` / `preview_rows()` — block on a one-shot
//!   reply channel. Used by the table-preview pane on user-driven
//!   `Enter`. A brief stall there is acceptable; the catalog walk
//!   that fires per keystroke can't afford it.

use std::sync::mpsc;
use std::thread::JoinHandle;

use anyhow::Result;

use super::db::{ColumnSpec, Connection, DuckLakeAttach, NodeData, NodeKind, NodePath, Row, TableMeta};

/// Result of an async children load. The catalog populates the
/// cache in `Loading` state when it spawns the request; the main
/// loop transitions to `Ready` or `Failed` on receive.
#[derive(Debug)]
pub(crate) struct LoadResult {
    pub path: NodePath,
    pub result: Result<Vec<(NodePath, NodeData)>>,
}

/// Result of an async per-table metadata fetch. Carried back to the
/// main loop so it can populate the metadata cache without blocking
/// any keystroke. `path.level()` is always `NodeKind::Table` here.
#[derive(Debug)]
pub(crate) struct TableMetaResult {
    pub path: NodePath,
    pub result: Result<TableMeta>,
}

/// Result of an async SQL query execution.
#[derive(Debug)]
pub(crate) struct QueryExecResult {
    /// Generation token for stale-result discard.
    pub gen: u64,
    pub columns: Vec<String>,
    pub rows: Vec<super::db::Row>,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

enum Request {
    LoadChildren {
        path: NodePath,
        reply: mpsc::Sender<LoadResult>,
    },
    Columns {
        db: String,
        schema: String,
        table: String,
        reply: mpsc::Sender<Result<Vec<ColumnSpec>>>,
    },
    PreviewRows {
        db: String,
        schema: String,
        table: String,
        limit: usize,
        reply: mpsc::Sender<Result<Vec<Row>>>,
    },
    RowCount {
        db: String,
        schema: String,
        table: String,
        reply: mpsc::Sender<Option<u64>>,
    },
    TableMetaAsync {
        path: NodePath,
        reply: mpsc::Sender<TableMetaResult>,
    },
    DropObject {
        db: String,
        schema: String,
        table: Option<String>,
        cascade: bool,
        reply: mpsc::Sender<Result<()>>,
    },
    ExecQuery {
        sql: String,
        gen: u64,
        reply: mpsc::SyncSender<QueryExecResult>,
    },
    Shutdown,
}

pub(crate) struct ConnectionWorker {
    label: String,
    tx: mpsc::Sender<Request>,
    /// Kept so the worker thread joins on drop. Worker exits its
    /// loop when the request channel closes (all senders dropped) or
    /// on an explicit Shutdown request.
    _handle: JoinHandle<()>,
}

impl std::fmt::Debug for ConnectionWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionWorker")
            .field("label", &self.label)
            .finish()
    }
}

impl ConnectionWorker {
    /// Spawn a worker thread that constructs the `Connection` on
    /// its own thread (the !Send constraint forbids moving an
    /// already-open connection across thread boundaries) and runs
    /// the request loop. Blocks the caller until the connection has
    /// either succeeded or failed to open — matches the old
    /// synchronous `gfb::sources::open()` UX where `--connect`
    /// failures print up front.
    pub fn spawn(
        target: String,
        ducklake: Option<DuckLakeAttach>,
        read_only: bool,
    ) -> Result<Self> {
        let (req_tx, req_rx) = mpsc::channel::<Request>();
        let (open_tx, open_rx) = mpsc::channel::<Result<String>>();
        let thread_name = format!("gfb-conn-{}", short_label(&target));
        let handle = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || worker_main(target, ducklake, read_only, open_tx, req_rx))?;
        let label = match open_rx.recv() {
            Ok(Ok(l)) => l,
            Ok(Err(e)) => return Err(e),
            Err(_) => anyhow::bail!("connection thread died during open"),
        };
        Ok(Self {
            label,
            tx: req_tx,
            _handle: handle,
        })
    }

    pub fn endpoint_label(&self) -> &str {
        &self.label
    }

    /// Async children load. Fires the request and returns; the
    /// worker sends a `LoadResult` on `reply` once the query
    /// completes. A send error means the worker is gone — the
    /// caller's cache stays in `Loading` state until refresh.
    pub fn request_children(&self, path: NodePath, reply: mpsc::Sender<LoadResult>) {
        let _ = self.tx.send(Request::LoadChildren { path, reply });
    }

    /// Sync column fetch — blocks the caller until the worker
    /// replies. Acceptable on the user-initiated table-preview path
    /// where a brief stall is expected.
    pub fn columns(&self, db: &str, schema: &str, table: &str) -> Result<Vec<ColumnSpec>> {
        let (rtx, rrx) = mpsc::channel();
        self.tx.send(Request::Columns {
            db: db.into(),
            schema: schema.into(),
            table: table.into(),
            reply: rtx,
        })?;
        rrx.recv()?
    }

    pub fn drop_object(&self, db: &str, schema: &str, table: Option<&str>, cascade: bool) -> Result<()> {
        let (rtx, rrx) = mpsc::channel();
        self.tx.send(Request::DropObject {
            db: db.into(),
            schema: schema.into(),
            table: table.map(String::from),
            cascade,
            reply: rtx,
        })?;
        rrx.recv()?
    }

    /// Catalog-stats row count estimate — no table scan. Returns `None`
    /// when the backend can't provide a cheap estimate.
    pub fn approximate_row_count(&self, db: &str, schema: &str, table: &str) -> Option<u64> {
        let (rtx, rrx) = mpsc::channel();
        self.tx.send(Request::RowCount {
            db: db.into(),
            schema: schema.into(),
            table: table.into(),
            reply: rtx,
        }).ok()?;
        rrx.recv().ok()?
    }

    /// Async query execution. Fires the request and returns; the worker
    /// runs the SQL, records elapsed time, and sends a `QueryExecResult`
    /// on `reply`. The main loop drains the reply channel each tick.
    pub fn exec_query_async(&self, sql: String, gen: u64, reply: mpsc::SyncSender<QueryExecResult>) {
        let _ = self.tx.send(Request::ExecQuery { sql, gen, reply });
    }

    /// Async per-table metadata fetch (rows / size / mtime). Caller
    /// receives a `TableMetaResult` on `reply` once the worker runs
    /// the catalog query. `path.level()` must be `NodeKind::Table`.
    pub fn request_table_meta(&self, path: NodePath, reply: mpsc::Sender<TableMetaResult>) {
        let _ = self.tx.send(Request::TableMetaAsync { path, reply });
    }

    /// Sync preview-rows fetch, same blocking shape as `columns`.
    #[allow(dead_code)]
    pub fn preview_rows(
        &self,
        db: &str,
        schema: &str,
        table: &str,
        limit: usize,
    ) -> Result<Vec<Row>> {
        let (rtx, rrx) = mpsc::channel();
        self.tx.send(Request::PreviewRows {
            db: db.into(),
            schema: schema.into(),
            table: table.into(),
            limit,
            reply: rtx,
        })?;
        rrx.recv()?
    }
}

impl Drop for ConnectionWorker {
    fn drop(&mut self) {
        // Best-effort shutdown signal — if the channel is already
        // closed (worker died), the send fails silently and the
        // thread will have exited on its own.
        let _ = self.tx.send(Request::Shutdown);
    }
}

fn worker_main(
    target: String,
    ducklake: Option<DuckLakeAttach>,
    read_only: bool,
    open_tx: mpsc::Sender<Result<String>>,
    req_rx: mpsc::Receiver<Request>,
) {
    let conn = match super::db::open(&target, ducklake, read_only) {
        Ok(c) => c,
        Err(e) => {
            let _ = open_tx.send(Err(e));
            return;
        }
    };
    let _ = open_tx.send(Ok(conn.endpoint_label().to_string()));
    drop(open_tx); // hint: open completed
    for req in req_rx.iter() {
        match req {
            Request::LoadChildren { path, reply } => {
                let result = load_children_for(&*conn, &path);
                let _ = reply.send(LoadResult { path, result });
            }
            Request::Columns {
                db,
                schema,
                table,
                reply,
            } => {
                let _ = reply.send(conn.columns(&db, &schema, &table));
            }
            Request::PreviewRows {
                db,
                schema,
                table,
                limit,
                reply,
            } => {
                let _ = reply.send(conn.preview_rows(&db, &schema, &table, limit));
            }
            Request::RowCount { db, schema, table, reply } => {
                let _ = reply.send(conn.approximate_row_count(&db, &schema, &table));
            }
            Request::TableMetaAsync { path, reply } => {
                let result = match (path.database.as_deref(), path.schema.as_deref(), path.table.as_deref()) {
                    (Some(db), Some(schema), Some(table)) => conn.table_metadata(db, schema, table),
                    _ => Ok(super::db::TableMeta::default()),
                };
                let _ = reply.send(TableMetaResult { path, result });
            }
            Request::DropObject { db, schema, table, cascade, reply } => {
                let _ = reply.send(conn.drop_object(&db, &schema, table.as_deref(), cascade));
            }
            Request::ExecQuery { sql, gen, reply } => {
                let t0 = std::time::Instant::now();
                let result = conn.execute_query(&sql);
                let elapsed_ms = t0.elapsed().as_millis() as u64;
                let (columns, rows, error) = match result {
                    Ok((cols, rows)) => (cols, rows, None),
                    Err(e) => {
                        let msg = format!("{e:#}");
                        let _ = std::fs::write("/tmp/gfb-query-error.log", &msg);
                        (Vec::new(), Vec::new(), Some(msg))
                    }
                };
                let send_result = reply.try_send(QueryExecResult { gen, columns, rows, elapsed_ms, error });
                if send_result.is_err() {
                    let _ = std::fs::write("/tmp/gfb-debug.log", "worker: reply channel full or closed\n");
                }
            }
            Request::Shutdown => return,
        }
    }
}

fn load_children_for(
    conn: &dyn Connection,
    path: &NodePath,
) -> Result<Vec<(NodePath, NodeData)>> {
    let root = path.conn;
    match path.level() {
        NodeKind::Endpoint => {
            let dbs = conn.databases()?;
            Ok(dbs
                .into_iter()
                .map(|db| {
                    (
                        NodePath::database(root, &db.name),
                        NodeData {
                            kind: NodeKind::Database,
                            label: db.name,
                        },
                    )
                })
                .collect())
        }
        NodeKind::Database => {
            let db = path.database.as_deref().unwrap_or_default();
            let schemas = conn.schemas(db)?;
            Ok(schemas
                .into_iter()
                .map(|s| {
                    (
                        NodePath::schema(root, db, &s.name),
                        NodeData {
                            kind: NodeKind::Schema,
                            label: s.name,
                        },
                    )
                })
                .collect())
        }
        NodeKind::Schema => {
            let db = path.database.as_deref().unwrap_or_default();
            let sch = path.schema.as_deref().unwrap_or_default();
            let tables = conn.tables(db, sch)?;
            Ok(tables
                .into_iter()
                .map(|t| {
                    (
                        NodePath::table(root, db, sch, &t.name),
                        NodeData {
                            kind: NodeKind::Table,
                            label: t.name,
                        },
                    )
                })
                .collect())
        }
        NodeKind::Table => Ok(Vec::new()),
    }
}

/// Trim a `--connect` URL down to a 12-char-ish thread name so
/// `top -H` / strace output stays readable.
fn short_label(target: &str) -> String {
    target
        .split(['/', '@'])
        .last()
        .unwrap_or(target)
        .chars()
        .take(16)
        .collect()
}
