//! Live-DuckDB smoke tests. Skipped by default. Run with:
//!
//! ```text
//! BOBTOP_DUCKDB_PATH=/root/repos/ml_momo/lake_data/ml_momo.duckdb \
//! BOBTOP_DUCKLAKE_PG=postgresql://root@localhost/ml_momo \
//! BOBTOP_DUCKLAKE_PATH=/root/repos/ml_momo/lake_data \
//!     cargo test -p bobtop-db --test duck_smoke -- --ignored --nocapture
//! ```
//!
//! `BOBTOP_DUCKDB_PATH` is required; `BOBTOP_DUCKLAKE_*` are optional
//! and exercise the ATTACH path when set.

use bobtop_db::conn::duck::DuckConnection;
use bobtop_db::conn::Connection;

#[test]
#[ignore]
fn opens_duckdb_and_optionally_attaches_lake() {
    let path =
        std::env::var("BOBTOP_DUCKDB_PATH").expect("BOBTOP_DUCKDB_PATH must be set for this test");
    let mut conn = DuckConnection::open(&path).expect("open");

    if let (Ok(pg), Ok(data)) = (
        std::env::var("BOBTOP_DUCKLAKE_PG"),
        std::env::var("BOBTOP_DUCKLAKE_PATH"),
    ) {
        conn.attach_ducklake("lake", &pg, &data).expect("attach lake");
        println!("attached ducklake (catalog={pg}, data={data})");
    } else {
        println!("(skipping ATTACH — set BOBTOP_DUCKLAKE_PG and BOBTOP_DUCKLAKE_PATH)");
    }
    println!("endpoint: {}", conn.endpoint_label());

    let dbs = conn.databases().expect("databases");
    println!("databases: {dbs:?}");
    assert!(!dbs.is_empty(), "expected at least one database");

    // Walk every database/schema looking for a populated table; preview it.
    let mut showed = 0;
    for db in &dbs {
        for sch in conn.schemas(&db.name).expect("schemas") {
            let tables = conn.tables(&db.name, &sch.name).expect("tables");
            if let Some(t) = tables.first() {
                let cols = conn.columns(&db.name, &sch.name, &t.name).expect("columns");
                let rows = conn
                    .preview_rows(&db.name, &sch.name, &t.name, 3)
                    .unwrap_or_default();
                println!(
                    "{}.{}.{}  cols={} rows={}",
                    db.name,
                    sch.name,
                    t.name,
                    cols.len(),
                    rows.len()
                );
                for r in &rows {
                    println!("    {:?}", r.cells);
                }
                showed += 1;
                if showed >= 3 {
                    return;
                }
            }
        }
    }
}
