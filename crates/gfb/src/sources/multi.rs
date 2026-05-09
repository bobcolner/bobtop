//! Multi-root catalog: filesystem + N database connections.
//!
//! Built in Phase 5 of the gtop refactor (see `docs/gtop-refactor.md`).
//! `gfb` becomes the unified browser by stacking the cwd's filesystem
//! children and every `--connect`'d DB endpoint as siblings at depth 0
//! of one tree. The umbrella [`AnyNodeId`] / [`AnyRow`] enums let the
//! shared [`gtui::tree::Catalog`] / [`gtui::tree::flatten`] machinery
//! drive both source kinds without bifurcating the renderer.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;

use gtui::tree::Catalog;

use super::db::{NodeData, NodeKind, NodePath};
use super::worker::{ConnectionWorker, LoadResult};
use crate::fs::entry::FsEntry;
use crate::tree::FsCatalog;

/// State of a single DB-children cache entry.
#[derive(Debug, Clone)]
pub(crate) enum ChildrenState {
    /// Worker thread is running the query — render the parent as
    /// "expanded but empty" until the result lands.
    Loading,
    /// Query succeeded; serve from this Vec.
    Ready(Vec<(NodePath, NodeData)>),
    /// Query failed; we surface the error in the status bar and
    /// treat the subtree as empty so the rest of the tree stays
    /// usable.
    #[allow(dead_code)]
    Failed(String),
}

/// Per-session cache of DB-query children. Lives on `App` and is
/// borrowed into a [`MultiRootCatalog`] for each flatten. Cache miss
/// inserts `Loading` and fires an async query against the worker
/// thread; the main loop transitions to `Ready` / `Failed` when the
/// reply arrives. Hits return a clone — fast even on busy
/// keystrokes that re-flatten several times.
pub(crate) type DbCache = RefCell<HashMap<NodePath, ChildrenState>>;

pub(crate) fn new_db_cache() -> DbCache {
    RefCell::new(HashMap::new())
}

/// Per-session cache of filesystem scans. Same shape as [`DbCache`]
/// but keyed on the directory path. Each `scan_dir` call is a
/// readdir + stat-per-entry — cheap individually, but a tree-mode
/// keystroke can trigger 5-10 scans across an expanded subtree.
/// Cache lives until refresh (`r`) or an `fs_dirty` notification
/// from the watcher.
pub(crate) type FsCache = RefCell<HashMap<PathBuf, Vec<FsEntry>>>;

pub(crate) fn new_fs_cache() -> FsCache {
    RefCell::new(HashMap::new())
}

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
    conns: &'a [ConnectionWorker],
    cache: &'a DbCache,
    /// Reply channel for async children loads. Cloned per spawn.
    /// `None` disables async — the catalog returns empty rather than
    /// firing a request, useful for callers that don't have a
    /// running render loop to drain results (e.g. tests, headless
    /// snapshots). Most call sites pass `Some(app.db_load_tx())`.
    load_tx: Option<&'a mpsc::Sender<LoadResult>>,
}

impl<'a> MultiRootCatalog<'a> {
    pub(crate) fn new(
        fs: FsCatalog<'a>,
        conns: &'a [ConnectionWorker],
        cache: &'a DbCache,
        load_tx: Option<&'a mpsc::Sender<LoadResult>>,
    ) -> Self {
        Self {
            fs,
            conns,
            cache,
            load_tx,
        }
    }

    /// Look up `path`'s children in the cache. On hit, return the
    /// stored Vec (cloned). On miss, insert `Loading`, fire an
    /// async request to the worker, return empty.
    fn db_children(&self, path: &NodePath) -> Vec<(NodePath, NodeData)> {
        match self.cache.borrow().get(path).cloned() {
            Some(ChildrenState::Ready(v)) => return v,
            Some(ChildrenState::Loading) | Some(ChildrenState::Failed(_)) => return Vec::new(),
            None => {}
        }
        // Cache miss — install Loading and dispatch a query.
        self.cache
            .borrow_mut()
            .insert(path.clone(), ChildrenState::Loading);
        if let Some(tx) = self.load_tx {
            if let Some(worker) = self.conns.get(path.conn) {
                worker.request_children(path.clone(), tx.clone());
            }
        }
        Vec::new()
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
            AnyNodeId::Db { root, path } => self
                .db_children(path)
                .into_iter()
                .map(|(p, d)| {
                    (
                        AnyNodeId::Db {
                            root: *root,
                            path: p,
                        },
                        AnyRow::Db(d),
                    )
                })
                .collect(),
        }
    }

    fn is_expandable(&self, node: &Self::NodeId) -> bool {
        match node {
            AnyNodeId::Fs(p) => self.fs.is_expandable(p),
            AnyNodeId::Db { path, .. } => path.level() != NodeKind::Table,
        }
    }
}
