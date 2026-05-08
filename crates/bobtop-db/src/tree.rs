//! Catalog tree state — flattens `endpoint > database > schema > table`
//! into a list of visible rows the left pane renders. Expansion is
//! lazy: schemas/tables are fetched the first time a parent expands.

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
    pub database: Option<String>,
    pub schema: Option<String>,
    pub table: Option<String>,
}

impl NodePath {
    /// Stable string key used for the expanded-set. Nesting separator
    /// is `\0` so it can't collide with a database name.
    pub fn key(&self) -> String {
        let mut out = String::new();
        if let Some(db) = &self.database {
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
    /// connection + `expanded` set whenever expansion changes.
    nodes: Vec<CatalogNode>,
    expanded: HashSet<String>,
}

impl CatalogTree {
    pub fn new(conn: &dyn Connection) -> Result<Self> {
        let mut tree = Self {
            nodes: Vec::new(),
            expanded: HashSet::new(),
        };
        // Auto-expand the endpoint so the first thing the user sees
        // is the database list.
        tree.expanded.insert(String::new());
        tree.rebuild(conn)?;
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
    pub fn toggle(&mut self, conn: &dyn Connection, idx: usize) -> Result<usize> {
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
        self.rebuild(conn)?;
        Ok(self.nodes.len())
    }

    fn rebuild(&mut self, conn: &dyn Connection) -> Result<()> {
        let mut out = Vec::new();
        let endpoint_path = NodePath::default();
        let endpoint_expanded = self.expanded.contains(&endpoint_path.key());
        out.push(CatalogNode {
            kind: NodeKind::Endpoint,
            label: conn.endpoint_label().to_string(),
            depth: 0,
            ancestor_continues: Vec::new(),
            is_last_sibling: true,
            path: endpoint_path,
            expandable: true,
            expanded: endpoint_expanded,
        });

        if !endpoint_expanded {
            self.nodes = out;
            return Ok(());
        }

        let dbs = conn.databases()?;
        for (i, db) in dbs.iter().enumerate() {
            let is_last_db = i == dbs.len() - 1;
            let db_path = NodePath {
                database: Some(db.name.clone()),
                ..Default::default()
            };
            let db_expanded = self.expanded.contains(&db_path.key());
            out.push(CatalogNode {
                kind: NodeKind::Database,
                label: db.name.clone(),
                depth: 1,
                ancestor_continues: vec![false], // endpoint is always last sibling
                is_last_sibling: is_last_db,
                path: db_path.clone(),
                expandable: true,
                expanded: db_expanded,
            });

            if !db_expanded {
                continue;
            }
            let schemas = conn.schemas(&db.name)?;
            for (j, sch) in schemas.iter().enumerate() {
                let is_last_schema = j == schemas.len() - 1;
                let sch_path = NodePath {
                    database: Some(db.name.clone()),
                    schema: Some(sch.name.clone()),
                    ..Default::default()
                };
                let sch_expanded = self.expanded.contains(&sch_path.key());
                out.push(CatalogNode {
                    kind: NodeKind::Schema,
                    label: sch.name.clone(),
                    depth: 2,
                    ancestor_continues: vec![false, !is_last_db],
                    is_last_sibling: is_last_schema,
                    path: sch_path.clone(),
                    expandable: true,
                    expanded: sch_expanded,
                });

                if !sch_expanded {
                    continue;
                }
                let tables = conn.tables(&db.name, &sch.name)?;
                for (k, tbl) in tables.iter().enumerate() {
                    let is_last_tbl = k == tables.len() - 1;
                    out.push(CatalogNode {
                        kind: NodeKind::Table,
                        label: tbl.name.clone(),
                        depth: 3,
                        ancestor_continues: vec![false, !is_last_db, !is_last_schema],
                        is_last_sibling: is_last_tbl,
                        path: NodePath {
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

        self.nodes = out;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conn::mock::MockConnection;

    #[test]
    fn endpoint_auto_expands() {
        let conn = MockConnection::demo();
        let tree = CatalogTree::new(&conn).unwrap();
        assert!(tree.nodes()[0].kind == NodeKind::Endpoint);
        // Endpoint expanded → both databases visible.
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
        let conn = MockConnection::demo();
        let mut tree = CatalogTree::new(&conn).unwrap();
        // Find the index of "shop".
        let shop_idx = tree
            .nodes()
            .iter()
            .position(|n| n.kind == NodeKind::Database && n.label == "shop")
            .unwrap();
        tree.toggle(&conn, shop_idx).unwrap();
        let schemas: Vec<&str> = tree
            .nodes()
            .iter()
            .filter(|n| n.kind == NodeKind::Schema)
            .map(|n| n.label.as_str())
            .collect();
        assert_eq!(schemas, vec!["public", "auth"]);
    }
}
