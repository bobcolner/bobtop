//! Catalog tree state — flattens
//! `connection > database > schema > table` into a list of visible
//! rows the left pane renders. The root has no synthetic node;
//! every connection is its own depth-0 endpoint, so multi-`--connect`
//! sessions show all endpoints stacked. Expansion is lazy: schemas
//! and tables are fetched the first time a parent expands.
//!
//! As of the Phase 2 refactor the depth / ancestor-continues / last-
//! sibling computation lives in [`gtui::tree`]. This module supplies
//! the connection-rooted [`gtui::tree::Catalog`] impl and the
//! domain-shaped [`CatalogNode`] view the UI consumes.

use std::collections::HashSet;

use anyhow::Result;
use gtui::tree::{flatten, Catalog};

use crate::conn::Connection;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Endpoint,
    Database,
    Schema,
    Table,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct NodePath {
    /// Index into the `App::conns` vector. Lets the App route
    /// queries (preview load, etc.) to the right backend.
    pub conn: usize,
    pub database: Option<String>,
    pub schema: Option<String>,
    pub table: Option<String>,
}

impl NodePath {
    fn endpoint(conn: usize) -> Self {
        Self {
            conn,
            ..Default::default()
        }
    }

    fn database(conn: usize, db: &str) -> Self {
        Self {
            conn,
            database: Some(db.to_string()),
            ..Default::default()
        }
    }

    fn schema(conn: usize, db: &str, sch: &str) -> Self {
        Self {
            conn,
            database: Some(db.to_string()),
            schema: Some(sch.to_string()),
            ..Default::default()
        }
    }

    fn table(conn: usize, db: &str, sch: &str, tbl: &str) -> Self {
        Self {
            conn,
            database: Some(db.to_string()),
            schema: Some(sch.to_string()),
            table: Some(tbl.to_string()),
        }
    }

    fn level(&self) -> NodeKind {
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

/// Per-row payload the toolkit walker hands back. The full
/// [`CatalogNode`] is reconstructed from a [`gtui::tree::TreeRow`] +
/// this once flatten is done.
#[derive(Debug, Clone)]
struct NodeData {
    kind: NodeKind,
    label: String,
}

/// Connection-rooted [`Catalog`] impl. One catalog per rebuild — the
/// connections slice is borrowed for the lifetime of the walk.
struct ConnectionGroup<'a> {
    conns: &'a [Box<dyn Connection>],
}

impl<'a> Catalog for ConnectionGroup<'a> {
    type NodeId = NodePath;
    type Row = NodeData;

    fn roots(&self) -> Vec<(Self::NodeId, Self::Row)> {
        self.conns
            .iter()
            .enumerate()
            .map(|(i, c)| {
                (
                    NodePath::endpoint(i),
                    NodeData {
                        kind: NodeKind::Endpoint,
                        label: c.endpoint_label().to_string(),
                    },
                )
            })
            .collect()
    }

    fn children(&self, node: &Self::NodeId) -> Vec<(Self::NodeId, Self::Row)> {
        let Some(conn) = self.conns.get(node.conn) else {
            return Vec::new();
        };
        match node.level() {
            NodeKind::Endpoint => match conn.databases() {
                Ok(dbs) => dbs
                    .into_iter()
                    .map(|db| {
                        (
                            NodePath::database(node.conn, &db.name),
                            NodeData {
                                kind: NodeKind::Database,
                                label: db.name,
                            },
                        )
                    })
                    .collect(),
                Err(e) => {
                    tracing::warn!("databases() failed for conn {}: {e:#}", node.conn);
                    Vec::new()
                }
            },
            NodeKind::Database => {
                let db = node.database.as_deref().unwrap_or_default();
                match conn.schemas(db) {
                    Ok(schemas) => schemas
                        .into_iter()
                        .map(|s| {
                            (
                                NodePath::schema(node.conn, db, &s.name),
                                NodeData {
                                    kind: NodeKind::Schema,
                                    label: s.name,
                                },
                            )
                        })
                        .collect(),
                    Err(e) => {
                        tracing::warn!("schemas({db}) failed for conn {}: {e:#}", node.conn);
                        Vec::new()
                    }
                }
            }
            NodeKind::Schema => {
                let db = node.database.as_deref().unwrap_or_default();
                let sch = node.schema.as_deref().unwrap_or_default();
                match conn.tables(db, sch) {
                    Ok(tables) => tables
                        .into_iter()
                        .map(|t| {
                            (
                                NodePath::table(node.conn, db, sch, &t.name),
                                NodeData {
                                    kind: NodeKind::Table,
                                    label: t.name,
                                },
                            )
                        })
                        .collect(),
                    Err(e) => {
                        tracing::warn!(
                            "tables({db},{sch}) failed for conn {}: {e:#}",
                            node.conn
                        );
                        Vec::new()
                    }
                }
            }
            NodeKind::Table => Vec::new(),
        }
    }

    fn is_expandable(&self, node: &Self::NodeId) -> bool {
        !matches!(node.level(), NodeKind::Table)
    }
}

pub struct CatalogTree {
    /// Flattened list of currently-visible nodes. Rebuilt from the
    /// connections + `expanded` set whenever expansion changes.
    nodes: Vec<CatalogNode>,
    expanded: HashSet<NodePath>,
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
            tree.expanded.insert(NodePath::endpoint(i));
        }
        tree.rebuild(conns);
        Ok(tree)
    }

    pub fn nodes(&self) -> &[CatalogNode] {
        &self.nodes
    }

    #[allow(dead_code)] // exposed for future tree-state queries (jump-to-parent etc.)
    pub fn is_expanded(&self, path: &NodePath) -> bool {
        self.expanded.contains(path)
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
        if self.expanded.contains(&node.path) {
            self.expanded.remove(&node.path);
        } else {
            self.expanded.insert(node.path);
        }
        self.rebuild(conns);
        Ok(self.nodes.len())
    }

    fn rebuild(&mut self, conns: &[Box<dyn Connection>]) {
        let group = ConnectionGroup { conns };
        self.nodes = flatten(&group, &self.expanded)
            .into_iter()
            .map(|tr| CatalogNode {
                kind: tr.row.kind,
                label: tr.row.label,
                depth: tr.depth,
                ancestor_continues: tr.ancestor_continues,
                is_last_sibling: tr.is_last_sibling,
                path: tr.id,
                expandable: tr.expandable,
                expanded: tr.expanded,
            })
            .collect();
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
