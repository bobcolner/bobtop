//! Multi-root catalog: filesystem + N database connections.
//!
//! Built in Phase 5 of the gtop refactor (see `docs/gtop-refactor.md`).
//! `gfb` becomes the unified browser by stacking the cwd's filesystem
//! children and every `--connect`'d DB endpoint as siblings at depth 0
//! of one tree. The umbrella [`AnyNodeId`] / [`AnyRow`] enums let the
//! shared [`gtui::tree::Catalog`] / [`gtui::tree::flatten`] machinery
//! drive both source kinds without bifurcating the renderer.

use std::path::PathBuf;

use gtui::tree::Catalog;

use super::db::{Connection, NodeData, NodeKind, NodePath};
use crate::fs::entry::FsEntry;
use crate::tree::FsCatalog;

/// Stable identifier for a row in the unified tree. Filesystem rows
/// key on their absolute path; DB rows key on the connection index
/// plus a [`NodePath`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AnyNodeId {
    Fs(PathBuf),
    Db { root: usize, path: NodePath },
}

impl AnyNodeId {
    /// True if this id points at a filesystem entry. Convenience for
    /// the file-ops gate in `App::handle_tree`.
    pub fn is_fs(&self) -> bool {
        matches!(self, AnyNodeId::Fs(_))
    }
}

/// Per-row payload. Carries the domain object the renderer needs to
/// produce a label, glyph, and trailing metadata cells.
#[derive(Debug, Clone)]
pub enum AnyRow {
    Fs(FsEntry),
    Db(NodeData),
}

/// Unified catalog over the filesystem source and a list of database
/// connections. Filesystem children of the cwd come first at depth 0,
/// then each connection's endpoint as a sibling.
///
/// `pub(crate)` because the underlying [`FsCatalog`] is — gfb owns
/// the wrapper end-to-end and external consumers haven't asked for
/// it (yet).
pub(crate) struct MultiRootCatalog<'a> {
    fs: FsCatalog<'a>,
    conns: &'a [Box<dyn Connection>],
}

impl<'a> MultiRootCatalog<'a> {
    pub(crate) fn new(fs: FsCatalog<'a>, conns: &'a [Box<dyn Connection>]) -> Self {
        Self { fs, conns }
    }
}

impl<'a> Catalog for MultiRootCatalog<'a> {
    type NodeId = AnyNodeId;
    type Row = AnyRow;

    fn roots(&self) -> Vec<(Self::NodeId, Self::Row)> {
        let mut out: Vec<(AnyNodeId, AnyRow)> = self
            .fs
            .roots()
            .into_iter()
            .map(|(p, e)| (AnyNodeId::Fs(p), AnyRow::Fs(e)))
            .collect();
        for (i, conn) in self.conns.iter().enumerate() {
            out.push((
                AnyNodeId::Db {
                    root: i,
                    path: NodePath::endpoint(i),
                },
                AnyRow::Db(NodeData {
                    kind: NodeKind::Endpoint,
                    label: conn.endpoint_label().to_string(),
                }),
            ));
        }
        out
    }

    fn children(&self, node: &Self::NodeId) -> Vec<(Self::NodeId, Self::Row)> {
        match node {
            AnyNodeId::Fs(p) => self
                .fs
                .children(p)
                .into_iter()
                .map(|(p, e)| (AnyNodeId::Fs(p), AnyRow::Fs(e)))
                .collect(),
            AnyNodeId::Db { root, path } => {
                let Some(conn) = self.conns.get(*root) else {
                    return Vec::new();
                };
                match path.level() {
                    NodeKind::Endpoint => match conn.databases() {
                        Ok(dbs) => dbs
                            .into_iter()
                            .map(|db| {
                                (
                                    AnyNodeId::Db {
                                        root: *root,
                                        path: NodePath::database(*root, &db.name),
                                    },
                                    AnyRow::Db(NodeData {
                                        kind: NodeKind::Database,
                                        label: db.name,
                                    }),
                                )
                            })
                            .collect(),
                        Err(e) => {
                            tracing::warn!("databases() failed for conn {root}: {e:#}");
                            Vec::new()
                        }
                    },
                    NodeKind::Database => {
                        let db = path.database.as_deref().unwrap_or_default();
                        match conn.schemas(db) {
                            Ok(schemas) => schemas
                                .into_iter()
                                .map(|s| {
                                    (
                                        AnyNodeId::Db {
                                            root: *root,
                                            path: NodePath::schema(*root, db, &s.name),
                                        },
                                        AnyRow::Db(NodeData {
                                            kind: NodeKind::Schema,
                                            label: s.name,
                                        }),
                                    )
                                })
                                .collect(),
                            Err(e) => {
                                tracing::warn!(
                                    "schemas({db}) failed for conn {root}: {e:#}"
                                );
                                Vec::new()
                            }
                        }
                    }
                    NodeKind::Schema => {
                        let db = path.database.as_deref().unwrap_or_default();
                        let sch = path.schema.as_deref().unwrap_or_default();
                        match conn.tables(db, sch) {
                            Ok(tables) => tables
                                .into_iter()
                                .map(|t| {
                                    (
                                        AnyNodeId::Db {
                                            root: *root,
                                            path: NodePath::table(*root, db, sch, &t.name),
                                        },
                                        AnyRow::Db(NodeData {
                                            kind: NodeKind::Table,
                                            label: t.name,
                                        }),
                                    )
                                })
                                .collect(),
                            Err(e) => {
                                tracing::warn!(
                                    "tables({db},{sch}) failed for conn {root}: {e:#}"
                                );
                                Vec::new()
                            }
                        }
                    }
                    NodeKind::Table => Vec::new(),
                }
            }
        }
    }

    fn is_expandable(&self, node: &Self::NodeId) -> bool {
        match node {
            AnyNodeId::Fs(p) => self.fs.is_expandable(p),
            AnyNodeId::Db { path, .. } => path.level() != NodeKind::Table,
        }
    }
}
