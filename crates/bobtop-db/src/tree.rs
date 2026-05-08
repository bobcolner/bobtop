//! Catalog tree state — flattens
//! `connection > database > schema > table` into a list of visible
//! rows the left pane renders. The root has no synthetic node;
//! every connection is its own depth-0 endpoint, so multi-`--connect`
//! sessions show all endpoints stacked. Expansion is lazy: schemas
//! and tables are fetched the first time a parent expands.

use std::collections::HashSet;

use anyhow::Result;

use crate::conn::Connection;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Endpoint,
    Database,
    Schema,
    Table,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodePath {
    /// Index into the `App::conns` vector. Lets the App route
    /// queries (preview load, etc.) to the right backend.
    pub conn: usize,
    pub database: Option<String>,
    pub schema: Option<String>,
    pub table: Option<String>,
}

impl NodePath {
    /// Stable string key used for the expanded-set. The connection
    /// index goes first so two endpoints with the same `ml_momo`
    /// database name don't collide. Nesting separator is `\0`.
    pub fn key(&self) -> String {
        let mut out = String::new();
        out.push_str(&self.conn.to_string());
        if let Some(db) = &self.database {
            out.push('\0');
            out.push_str(db);
        }
        if let Some(s) = &self.schema {
            out.push('\0');
            out.push_str(s);
        }
        if let Some(t) = &self.table {
            out.push('\0');
            out.push_str(t);
        }
        out
    }
}

#[derive(Debug, Clone)]
pub struct CatalogNode {
    pub kind: NodeKind,
    pub label: String,
    /// 0 = endpoint, 1 = database, 2 = schema, 3 = table.
    pub depth: u8,
    pub ancestor_continues: Vec<bool>,
    pub is_last_sibling: bool,
    pub path: NodePath,
    /// Whether this node has a chevron — endpoint/database/schema yes,
    /// table no (tables are leaves in the tree pane).
    pub expandable: bool,
    pub expanded: bool,
}

pub struct CatalogTree {
    /// Flattened list of currently-visible nodes. Rebuilt from the
    /// connections + `expanded` set whenever expansion changes.
    nodes: Vec<CatalogNode>,
    expanded: HashSet<String>,
}

impl CatalogTree {
    pub fn new(conns: &[Box<dyn Connection>]) -> Result<Self> {
        let mut tree = Self {
            nodes: Vec::new(),
            expanded: HashSet::new(),
        };
        // Auto-expand every endpoint so the first thing the user sees
        // is the database list under each connection. With multiple
        // connections, this surfaces them all at once rather than
        // hiding everything behind a press of `Enter`.
        for i in 0..conns.len() {
            tree.expanded.insert(NodePath { conn: i, ..Default::default() }.key());
        }
        tree.rebuild(conns)?;
        Ok(tree)
    }

    pub fn nodes(&self) -> &[CatalogNode] {
        &self.nodes
    }

    #[allow(dead_code)] // exposed for future tree-state queries (jump-to-parent etc.)
    pub fn is_expanded(&self, path: &NodePath) -> bool {
        self.expanded.contains(&path.key())
    }

    /// Toggle expansion at `idx`. Tables are leaves, so toggling them
    /// is a no-op. Returns the new visible-row list length so callers
    /// can clamp cursor positions.
    pub fn toggle(&mut self, conns: &[Box<dyn Connection>], idx: usize) -> Result<usize> {
        let Some(node) = self.nodes.get(idx).cloned() else {
            return Ok(self.nodes.len());
        };
        if !node.expandable {
            return Ok(self.nodes.len());
        }
        let key = node.path.key();
        if self.expanded.contains(&key) {
            self.expanded.remove(&key);
        } else {
            self.expanded.insert(key);
        }
        self.rebuild(conns)?;
        Ok(self.nodes.len())
    }

    fn rebuild(&mut self, conns: &[Box<dyn Connection>]) -> Result<()> {
        let mut out = Vec::new();
        for (i, conn) in conns.iter().enumerate() {
            let is_last_endpoint = i == conns.len() - 1;
            let endpoint_path = NodePath { conn: i, ..Default::default() };
            let endpoint_expanded = self.expanded.contains(&endpoint_path.key());
            out.push(CatalogNode {
                kind: NodeKind::Endpoint,
                label: conn.endpoint_label().to_string(),
                depth: 0,
                ancestor_continues: Vec::new(),
                is_last_sibling: is_last_endpoint,
                path: endpoint_path,
                expandable: true,
                expanded: endpoint_expanded,
            });

            if !endpoint_expanded {
                continue;
            }

            let dbs = conn.databases()?;
            for (j, db) in dbs.iter().enumerate() {
                let is_last_db = j == dbs.len() - 1;
                let db_path = NodePath {
                    conn: i,
                    database: Some(db.name.clone()),
                    ..Default::default()
                };
                let db_expanded = self.expanded.contains(&db_path.key());
                out.push(CatalogNode {
                    kind: NodeKind::Database,
                    label: db.name.clone(),
                    depth: 1,
                    ancestor_continues: vec![!is_last_endpoint],
                    is_last_sibling: is_last_db,
                    path: db_path.clone(),
                    expandable: true,
                    expanded: db_expanded,
                });

                if !db_expanded {
                    continue;
                }
                let schemas = conn.schemas(&db.name)?;
                for (k, sch) in schemas.iter().enumerate() {
                    let is_last_schema = k == schemas.len() - 1;
                    let sch_path = NodePath {
                        conn: i,
                        database: Some(db.name.clone()),
                        schema: Some(sch.name.clone()),
                        ..Default::default()
                    };
                    let sch_expanded = self.expanded.contains(&sch_path.key());
                    out.push(CatalogNode {
                        kind: NodeKind::Schema,
                        label: sch.name.clone(),
                        depth: 2,
                        ancestor_continues: vec![!is_last_endpoint, !is_last_db],
                        is_last_sibling: is_last_schema,
                        path: sch_path.clone(),
                        expandable: true,
                        expanded: sch_expanded,
                    });

                    if !sch_expanded {
                        continue;
                    }
                    let tables = conn.tables(&db.name, &sch.name)?;
                    for (m, tbl) in tables.iter().enumerate() {
                        let is_last_tbl = m == tables.len() - 1;
                        out.push(CatalogNode {
                            kind: NodeKind::Table,
                            label: tbl.name.clone(),
                            depth: 3,
                            ancestor_continues: vec![
                                !is_last_endpoint,
                                !is_last_db,
                                !is_last_schema,
                            ],
                            is_last_sibling: is_last_tbl,
                            path: NodePath {
                                conn: i,
                                database: Some(db.name.clone()),
                                schema: Some(sch.name.clone()),
                                table: Some(tbl.name.clone()),
                            },
                            expandable: false,
                            expanded: false,
                        });
                    }
                }
            }
        }

        self.nodes = out;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conn::mock::MockConnection;

    fn boxed_mock() -> Box<dyn Connection> {
        Box::new(MockConnection::demo())
    }

    #[test]
    fn endpoint_auto_expands() {
        let conns: Vec<Box<dyn Connection>> = vec![boxed_mock()];
        let tree = CatalogTree::new(&conns).unwrap();
        assert!(tree.nodes()[0].kind == NodeKind::Endpoint);
        let dbs: Vec<&str> = tree
            .nodes()
            .iter()
            .filter(|n| n.kind == NodeKind::Database)
            .map(|n| n.label.as_str())
            .collect();
        assert_eq!(dbs, vec!["shop", "analytics"]);
    }

    #[test]
    fn toggling_database_reveals_schemas() {
        let conns: Vec<Box<dyn Connection>> = vec![boxed_mock()];
        let mut tree = CatalogTree::new(&conns).unwrap();
        let shop_idx = tree
            .nodes()
            .iter()
            .position(|n| n.kind == NodeKind::Database && n.label == "shop")
            .unwrap();
        tree.toggle(&conns, shop_idx).unwrap();
        let schemas: Vec<&str> = tree
            .nodes()
            .iter()
            .filter(|n| n.kind == NodeKind::Schema)
            .map(|n| n.label.as_str())
            .collect();
        assert_eq!(schemas, vec!["public", "auth"]);
    }

    #[test]
    fn two_endpoints_render_at_depth_zero() {
        let conns: Vec<Box<dyn Connection>> = vec![boxed_mock(), boxed_mock()];
        let tree = CatalogTree::new(&conns).unwrap();
        let endpoints: Vec<&str> = tree
            .nodes()
            .iter()
            .filter(|n| n.kind == NodeKind::Endpoint)
            .map(|n| n.label.as_str())
            .collect();
        assert_eq!(endpoints.len(), 2, "expected two endpoints");
        // Each endpoint expanded → each shows two databases. Path
        // disambiguation by conn index keeps the expansion sets
        // distinct.
        let db_count = tree
            .nodes()
            .iter()
            .filter(|n| n.kind == NodeKind::Database)
            .count();
        assert_eq!(db_count, 4);
    }
}
