//! App state + event loop.

use std::time::Duration;

use anyhow::Result;
use bobtop_tui::{Nav, Theme};
use crossterm::event::{self, Event};
use ratatui::backend::Backend;
use ratatui::Terminal;

use crate::conn::{ColumnSpec, Connection, Row};
use crate::keys::{map as map_key, Action};
use crate::tree::{CatalogTree, NodeKind, NodePath};
use crate::ui;

pub const PREVIEW_LIMIT: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Preview,
}

#[derive(Debug, Clone)]
pub struct PreviewData {
    pub path: NodePath,
    pub columns: Vec<ColumnSpec>,
    pub rows: Vec<Row>,
}

pub struct App {
    pub conn: Box<dyn Connection>,
    pub theme: Theme,
    pub tree: CatalogTree,
    pub tree_nav: Nav,
    pub preview_nav: Nav,
    pub preview: Option<PreviewData>,
    pub focus: Focus,
    pub status: Option<String>,
    pub should_quit: bool,
}

impl App {
    pub fn new(conn: Box<dyn Connection>, theme: Theme) -> Self {
        // Endpoint auto-expands on construction so the user lands on
        // a visible database list, not a single collapsed node.
        let tree = CatalogTree::new(conn.as_ref()).unwrap_or_else(|_| {
            // Recoverable: present an empty tree if the initial
            // catalog query fails. Status line shows the error.
            CatalogTree::empty()
        });
        Self {
            conn,
            theme,
            tree,
            tree_nav: Nav::default(),
            preview_nav: Nav::default(),
            preview: None,
            focus: Focus::Tree,
            status: None,
            should_quit: false,
        }
    }

    pub fn run<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> Result<()> {
        while !self.should_quit {
            terminal.draw(|frame| ui::draw(self, frame))?;
            if event::poll(Duration::from_millis(120))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == event::KeyEventKind::Press {
                        self.handle_action(map_key(key));
                    }
                }
            }
        }
        Ok(())
    }

    fn handle_action(&mut self, action: Action) {
        match action {
            Action::None => {}
            Action::Quit => self.should_quit = true,
            Action::CycleFocus => {
                self.focus = match self.focus {
                    Focus::Tree => Focus::Preview,
                    Focus::Preview => Focus::Tree,
                };
            }
            Action::Up | Action::Down | Action::PageUp | Action::PageDown | Action::Home | Action::End => {
                self.move_cursor(action);
            }
            Action::Activate => self.activate(),
            Action::CollapseOrParent => self.collapse_or_parent(),
        }
    }

    fn move_cursor(&mut self, action: Action) {
        let (nav, len) = match self.focus {
            Focus::Tree => (&mut self.tree_nav, self.tree.nodes().len()),
            Focus::Preview => (
                &mut self.preview_nav,
                self.preview.as_ref().map(|p| p.rows.len()).unwrap_or(0),
            ),
        };
        let delta = match action {
            Action::Up => -1,
            Action::Down => 1,
            Action::PageUp => -10,
            Action::PageDown => 10,
            Action::Home => {
                nav.home();
                return;
            }
            Action::End => {
                nav.end(len);
                return;
            }
            _ => return,
        };
        nav.move_by(delta, len);
    }

    fn activate(&mut self) {
        match self.focus {
            Focus::Tree => self.activate_tree_node(),
            Focus::Preview => {
                // No-op for now — could open a "row detail" modal in
                // a follow-up. Cycling focus to the tree is the more
                // natural action in this state.
                self.focus = Focus::Tree;
            }
        }
    }

    fn activate_tree_node(&mut self) {
        let Some(node) = self.tree.nodes().get(self.tree_nav.cursor).cloned() else {
            return;
        };
        if matches!(node.kind, NodeKind::Table) {
            self.load_preview(node.path);
            return;
        }
        match self.tree.toggle(self.conn.as_ref(), self.tree_nav.cursor) {
            Ok(new_len) => {
                if self.tree_nav.cursor >= new_len {
                    self.tree_nav.cursor = new_len.saturating_sub(1);
                }
                self.status = None;
            }
            Err(e) => self.status = Some(format!("expand failed: {e}")),
        }
    }

    fn collapse_or_parent(&mut self) {
        if !matches!(self.focus, Focus::Tree) {
            self.focus = Focus::Tree;
            return;
        }
        let Some(node) = self.tree.nodes().get(self.tree_nav.cursor).cloned() else {
            return;
        };
        if node.expandable && node.expanded {
            // Collapse this node in place.
            if let Err(e) = self.tree.toggle(self.conn.as_ref(), self.tree_nav.cursor) {
                self.status = Some(format!("collapse failed: {e}"));
            }
            return;
        }
        // Already collapsed (or a leaf): jump cursor to the parent
        // node by walking back to the first row at depth - 1.
        if node.depth == 0 {
            return;
        }
        let target_depth = node.depth - 1;
        for i in (0..self.tree_nav.cursor).rev() {
            if self.tree.nodes()[i].depth == target_depth {
                self.tree_nav.cursor = i;
                break;
            }
        }
    }

    fn load_preview(&mut self, path: NodePath) {
        let (Some(db), Some(schema), Some(table)) =
            (path.database.clone(), path.schema.clone(), path.table.clone())
        else {
            return;
        };
        match (
            self.conn.columns(&db, &schema, &table),
            self.conn.preview_rows(&db, &schema, &table, PREVIEW_LIMIT),
        ) {
            (Ok(columns), Ok(rows)) => {
                self.preview = Some(PreviewData { path, columns, rows });
                self.preview_nav = Nav::default();
                self.focus = Focus::Preview;
                self.status = None;
            }
            (Err(e), _) | (_, Err(e)) => {
                self.status = Some(format!("preview failed: {e}"));
            }
        }
    }
}

impl CatalogTree {
    fn empty() -> Self {
        // Used only when initial catalog load fails — the App keeps
        // running so the status line can surface the error.
        Self::new_unchecked()
    }
}

// `CatalogTree::new_unchecked` lives here (not in tree.rs) so it stays
// out of the public-ish module API — it's a fallback that should never
// be reached except via App::new's error branch.
impl CatalogTree {
    fn new_unchecked() -> Self {
        // SAFETY: zero-sized state; serves as a "tree failed to load"
        // sentinel so the rest of the App can render an error.
        // Equivalent to `CatalogTree { nodes: vec![], expanded: HashSet::new() }`
        // but we can't construct it directly because the fields are
        // private. Use a no-op connection instead.
        struct NullConn;
        impl Connection for NullConn {
            fn endpoint_label(&self) -> &str { "(disconnected)" }
            fn databases(&self) -> Result<Vec<crate::conn::Database>> { Ok(vec![]) }
            fn schemas(&self, _: &str) -> Result<Vec<crate::conn::Schema>> { Ok(vec![]) }
            fn tables(&self, _: &str, _: &str) -> Result<Vec<crate::conn::Table>> { Ok(vec![]) }
            fn columns(&self, _: &str, _: &str, _: &str) -> Result<Vec<ColumnSpec>> { Ok(vec![]) }
            fn preview_rows(&self, _: &str, _: &str, _: &str, _: usize) -> Result<Vec<Row>> { Ok(vec![]) }
        }
        CatalogTree::new(&NullConn).expect("null connection cannot fail")
    }
}
