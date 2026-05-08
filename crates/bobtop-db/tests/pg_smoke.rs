//! Live-Postgres smoke tests. Skipped by default (no DB assumed in
//! CI). Run with:
//!
//! ```text
//! BOBTOP_PG_URL=postgresql://root@localhost/ml_momo \
//!     cargo test -p bobtop-db --test pg_smoke -- --ignored --nocapture
//! ```

use bobtop_db::conn::mock::MockConnection;
use bobtop_db::conn::pg::PgConnection;
use bobtop_db::conn::Connection;
use bobtop_db::tree::{CatalogTree, NodeKind};

fn pg_url() -> Option<String> {
    std::env::var("BOBTOP_PG_URL").ok()
}

#[test]
#[ignore]
fn lists_schemas_and_a_table_preview() {
    let Some(url) = pg_url() else {
        eprintln!("BOBTOP_PG_URL not set — skipping");
        return;
    };
    let conn = PgConnection::open(&url).expect("connect");
    let dbs = conn.databases().expect("databases");
    println!("endpoint: {}", conn.endpoint_label());
    println!("databases: {dbs:?}");
    assert!(!dbs.is_empty(), "expected at least one database");

    let db = &dbs[0].name;
    let schemas = conn.schemas(db).expect("schemas");
    println!("schemas in {db}: {:?}", schemas.iter().map(|s| &s.name).collect::<Vec<_>>());
    if schemas.is_empty() {
        eprintln!("(no user schemas — skipping table preview)");
        return;
    }

    // Walk schemas until we find one with tables.
    let mut found = None;
    for sch in &schemas {
        let tables = conn.tables(db, &sch.name).expect("tables");
        if let Some(t) = tables.first() {
            found = Some((sch.name.clone(), t.name.clone()));
            break;
        }
    }
    let Some((schema, table)) = found else {
        eprintln!("(no tables found across any schema)");
        return;
    };
    println!("schema={schema} table={table}");

    let cols = conn.columns(db, &schema, &table).expect("columns");
    println!(
        "columns: {:?}",
        cols.iter().map(|c| (&c.name, &c.data_type)).collect::<Vec<_>>()
    );
    assert!(!cols.is_empty(), "expected at least one column");

    let rows = conn.preview_rows(db, &schema, &table, 5).expect("preview");
    println!("first {} row(s):", rows.len());
    for r in &rows {
        println!("  {:?}", r.cells);
    }

    // If `bars_1m` exists (ml_momo has it), verify a populated table
    // round-trips actual data through the preview path.
    let bars = conn.tables(db, "public").unwrap_or_default();
    if let Some(t) = bars.iter().find(|t| t.name == "bars_1m") {
        let cols = conn.columns(db, "public", &t.name).unwrap();
        let rows = conn.preview_rows(db, "public", &t.name, 3).unwrap();
        println!(
            "bars_1m columns ({}): {:?}",
            cols.len(),
            cols.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
        println!("bars_1m first {} rows:", rows.len());
        for r in &rows {
            println!("  {:?}", r.cells);
        }
        assert!(!rows.is_empty(), "expected populated bars_1m");
    }
}

#[test]
#[ignore]
fn tree_expands_through_pg_to_tables() {
    let Some(url) = pg_url() else {
        eprintln!("BOBTOP_PG_URL not set — skipping");
        return;
    };
    let conn = PgConnection::open(&url).expect("connect");
    let conns: Vec<Box<dyn Connection>> = vec![Box::new(conn)];
    let mut tree = CatalogTree::new(&conns).expect("tree init");

    // After construction the endpoint is auto-expanded, so we should
    // see endpoint + at least one database.
    let nodes = tree.nodes();
    println!("after init ({} nodes):", nodes.len());
    for n in nodes {
        println!("  d{} {:?}: {}", n.depth, n.kind, n.label);
    }
    let db_idx = nodes
        .iter()
        .position(|n| n.kind == NodeKind::Database)
        .expect("database node");

    tree.toggle(&conns, db_idx).expect("toggle db");
    let nodes = tree.nodes();
    println!("\nafter expand database ({} nodes):", nodes.len());
    for n in nodes {
        println!("  d{} {:?}: {}", n.depth, n.kind, n.label);
    }
    let schema_idx = nodes
        .iter()
        .position(|n| n.kind == NodeKind::Schema)
        .expect("schema node after expanding db");

    tree.toggle(&conns, schema_idx).expect("toggle schema");
    let nodes = tree.nodes();
    println!("\nafter expand schema ({} nodes):", nodes.len());
    for n in nodes {
        println!("  d{} {:?}: {}", n.depth, n.kind, n.label);
    }
    let tables: Vec<&str> = nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Table)
        .map(|n| n.label.as_str())
        .collect();
    assert!(
        !tables.is_empty(),
        "expected at least one table after expanding schema"
    );
    println!("\ntables visible: {tables:?}");
}

#[test]
#[ignore]
fn two_endpoints_appear_side_by_side() {
    let Some(url) = pg_url() else {
        eprintln!("BOBTOP_PG_URL not set — skipping");
        return;
    };
    // PG endpoint + mock endpoint — exercises a multi-conn tree
    // without needing two live DBs.
    let conns: Vec<Box<dyn Connection>> = vec![
        Box::new(PgConnection::open(&url).expect("pg connect")),
        Box::new(MockConnection::demo()),
    ];
    let tree = CatalogTree::new(&conns).expect("tree init");
    let endpoints: Vec<&str> = tree
        .nodes()
        .iter()
        .filter(|n| n.kind == NodeKind::Endpoint)
        .map(|n| n.label.as_str())
        .collect();
    println!("endpoints: {endpoints:?}");
    assert_eq!(endpoints.len(), 2, "expected two endpoints");
    assert!(endpoints[0].starts_with("pg://"));
    assert!(endpoints[1].starts_with("mock://"));

    // Both auto-expanded → at least one DB row per endpoint.
    let dbs_under_endpoint_0 = tree
        .nodes()
        .iter()
        .filter(|n| n.kind == NodeKind::Database && n.path.conn == 0)
        .count();
    let dbs_under_endpoint_1 = tree
        .nodes()
        .iter()
        .filter(|n| n.kind == NodeKind::Database && n.path.conn == 1)
        .count();
    println!("conn 0 dbs: {dbs_under_endpoint_0}, conn 1 dbs: {dbs_under_endpoint_1}");
    assert!(dbs_under_endpoint_0 >= 1);
    assert!(dbs_under_endpoint_1 >= 1);
}
