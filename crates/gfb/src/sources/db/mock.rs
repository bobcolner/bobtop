//! Hardcoded demo connection — useful for developing the UI without
//! hooking up Postgres or DuckDB. `--connect mock` (the default).

use anyhow::Result;

use super::{ColumnSpec, Connection, Database, Row, Schema, Table, TableMeta};

pub struct MockConnection {
    label: String,
    catalog: Vec<MockDatabase>,
}

struct MockDatabase {
    name: &'static str,
    schemas: Vec<MockSchema>,
}

struct MockSchema {
    name: &'static str,
    tables: Vec<MockTable>,
}

struct MockTable {
    name: &'static str,
    columns: Vec<ColumnSpec>,
    rows: Vec<Vec<&'static str>>,
}

impl MockConnection {
    pub fn demo() -> Self {
        Self {
            label: "mock://demo".into(),
            catalog: vec![
                MockDatabase {
                    name: "shop",
                    schemas: vec![
                        MockSchema {
                            name: "public",
                            tables: vec![
                                MockTable {
                                    name: "products",
                                    columns: vec![
                                        col("id", "int", false),
                                        col("name", "text", false),
                                        col("price", "numeric(10,2)", false),
                                        col("in_stock", "boolean", false),
                                    ],
                                    rows: vec![
                                        vec!["1", "Coffee Beans", "12.50", "true"],
                                        vec!["2", "French Press", "29.99", "true"],
                                        vec!["3", "Espresso Cups", "8.00", "false"],
                                        vec!["4", "Burr Grinder", "199.00", "true"],
                                        vec!["5", "Pour-Over Kit", "45.00", "true"],
                                    ],
                                },
                                MockTable {
                                    name: "orders",
                                    columns: vec![
                                        col("id", "int", false),
                                        col("customer_id", "int", false),
                                        col("total", "numeric(10,2)", false),
                                        col("placed_at", "timestamptz", false),
                                    ],
                                    rows: vec![
                                        vec!["1001", "42", "54.50", "2026-05-01 09:14:22+00"],
                                        vec!["1002", "13", "199.00", "2026-05-02 11:00:01+00"],
                                        vec!["1003", "42", "37.99", "2026-05-04 16:48:55+00"],
                                    ],
                                },
                                MockTable {
                                    name: "customers",
                                    columns: vec![
                                        col("id", "int", false),
                                        col("email", "text", false),
                                        col("created_at", "timestamptz", false),
                                    ],
                                    rows: vec![
                                        vec!["13", "alice@example.com", "2025-11-12 08:00:00+00"],
                                        vec!["42", "bob@example.com", "2025-12-03 14:22:10+00"],
                                    ],
                                },
                            ],
                        },
                        MockSchema {
                            name: "auth",
                            tables: vec![
                                MockTable {
                                    name: "users",
                                    columns: vec![
                                        col("id", "int", false),
                                        col("username", "text", false),
                                        col("password_hash", "text", false),
                                    ],
                                    rows: vec![
                                        vec!["1", "alice", "$argon2id$v=19$..."],
                                        vec!["2", "bob", "$argon2id$v=19$..."],
                                    ],
                                },
                                MockTable {
                                    name: "sessions",
                                    columns: vec![
                                        col("id", "uuid", false),
                                        col("user_id", "int", false),
                                        col("expires_at", "timestamptz", false),
                                    ],
                                    rows: vec![vec![
                                        "f47ac10b-58cc-4372-a567-0e02b2c3d479",
                                        "1",
                                        "2026-06-01 00:00:00+00",
                                    ]],
                                },
                            ],
                        },
                    ],
                },
                MockDatabase {
                    name: "analytics",
                    schemas: vec![
                        MockSchema {
                            name: "events",
                            tables: vec![
                                MockTable {
                                    name: "page_views",
                                    columns: vec![
                                        col("id", "bigint", false),
                                        col("path", "text", false),
                                        col("ts", "timestamptz", false),
                                    ],
                                    rows: vec![
                                        vec!["1", "/", "2026-05-07 12:00:00+00"],
                                        vec!["2", "/products", "2026-05-07 12:00:03+00"],
                                        vec!["3", "/products/4", "2026-05-07 12:00:11+00"],
                                    ],
                                },
                                MockTable {
                                    name: "click_events",
                                    columns: vec![
                                        col("id", "bigint", false),
                                        col("element", "text", false),
                                        col("ts", "timestamptz", false),
                                    ],
                                    rows: vec![],
                                },
                            ],
                        },
                        MockSchema {
                            name: "rollup",
                            tables: vec![MockTable {
                                name: "daily_revenue",
                                columns: vec![
                                    col("day", "date", false),
                                    col("total", "numeric(14,2)", false),
                                ],
                                rows: vec![
                                    vec!["2026-05-05", "1245.00"],
                                    vec!["2026-05-06", "892.50"],
                                ],
                            }],
                        },
                    ],
                },
            ],
        }
    }

    fn find_db(&self, name: &str) -> Option<&MockDatabase> {
        self.catalog.iter().find(|d| d.name == name)
    }

    fn find_schema(&self, db: &str, schema: &str) -> Option<&MockSchema> {
        self.find_db(db)?.schemas.iter().find(|s| s.name == schema)
    }

    fn find_table(&self, db: &str, schema: &str, table: &str) -> Option<&MockTable> {
        self.find_schema(db, schema)?
            .tables
            .iter()
            .find(|t| t.name == table)
    }
}

fn col(name: &str, ty: &str, nullable: bool) -> ColumnSpec {
    ColumnSpec {
        name: name.into(),
        data_type: ty.into(),
        nullable,
    }
}

impl Connection for MockConnection {
    fn endpoint_label(&self) -> &str {
        &self.label
    }

    fn databases(&self) -> Result<Vec<Database>> {
        Ok(self
            .catalog
            .iter()
            .map(|d| Database { name: d.name.into() })
            .collect())
    }

    fn schemas(&self, db: &str) -> Result<Vec<Schema>> {
        Ok(self
            .find_db(db)
            .map(|d| {
                d.schemas
                    .iter()
                    .map(|s| Schema { name: s.name.into() })
                    .collect()
            })
            .unwrap_or_default())
    }

    fn tables(&self, db: &str, schema: &str) -> Result<Vec<Table>> {
        Ok(self
            .find_schema(db, schema)
            .map(|s| {
                s.tables
                    .iter()
                    .map(|t| Table {
                        name: t.name.into(),
                        estimated_rows: Some(t.rows.len() as u64),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    fn columns(&self, db: &str, schema: &str, table: &str) -> Result<Vec<ColumnSpec>> {
        Ok(self
            .find_table(db, schema, table)
            .map(|t| t.columns.clone())
            .unwrap_or_default())
    }

    fn preview_rows(
        &self,
        db: &str,
        schema: &str,
        table: &str,
        limit: usize,
    ) -> Result<Vec<Row>> {
        Ok(self
            .find_table(db, schema, table)
            .map(|t| {
                t.rows
                    .iter()
                    .take(limit)
                    .map(|r| Row {
                        cells: r.iter().map(|s| (*s).to_string()).collect(),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    fn execute_query(&self, _sql: &str) -> Result<(Vec<String>, Vec<Row>)> {
        Ok((
            vec!["result".to_string(), "value".to_string()],
            vec![
                Row { cells: vec!["row1".to_string(), "42".to_string()] },
                Row { cells: vec!["row2".to_string(), "99".to_string()] },
            ],
        ))
    }

    fn drop_object(&self, _db: &str, _schema: &str, _table: Option<&str>, _cascade: bool) -> Result<()> {
        anyhow::bail!("mock connection does not support drop")
    }

    fn table_metadata(&self, db: &str, schema: &str, table: &str) -> Result<TableMeta> {
        let Some(t) = self.find_table(db, schema, table) else {
            return Ok(TableMeta::default());
        };
        let rows = t.rows.len() as u64;
        // Pretend each row averages the byte width of the demo data.
        let bytes: u64 = t.rows.iter()
            .map(|r| r.iter().map(|c| c.len() as u64).sum::<u64>())
            .sum();
        // Stable demo "modified" derived from the table-name hash so
        // rows render consistently between frames without storing real
        // mtimes anywhere.
        let mut h: u64 = 0xcbf29ce484222325;
        for b in table.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        let secs_ago = h % (60 * 60 * 24 * 30); // up to 30 days ago
        let modified = std::time::SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(secs_ago));
        Ok(TableMeta {
            row_count: Some(rows),
            size_bytes: if bytes > 0 { Some(bytes) } else { None },
            modified,
            modified_kind: super::ModifiedKind::Table,
        })
    }
}
