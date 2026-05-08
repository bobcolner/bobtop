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
    pub conns: Vec<Box<dyn Connection>>,
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
    pub fn new(conns: Vec<Box<dyn Connection>>, theme: Theme) -> Self {
        // All endpoints auto-expand on construction so the user lands
        // on visible database lists rather than a stack of collapsed
        // chevrons.
        let tree = CatalogTree::new(&conns).unwrap_or_else(|_| {
            // Recoverable: present an empty tree if the initial
            // catalog query fails. Status line surfaces the error.
            CatalogTree::empty()
        });
        Self {
            conns,
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
        let was_expanded = node.expanded;
        match self.tree.toggle(&self.conns, self.tree_nav.cursor) {
            Ok(new_len) => {
                // After expanding a parent the cursor would otherwise stay
                // on the parent — pressing Enter again would collapse it,
                // hiding the children we just revealed. Step into the
                // first child instead so successive Enters drill straight
                // down (database → schema → table). Collapse stays a
                // single-key action via `h` / `←` / Backspace.
                if !was_expanded {
                    let next = self.tree_nav.cursor.saturating_add(1);
                    if next < new_len
                        && self
                            .tree
                            .nodes()
                            .get(next)
                            .map(|child| child.depth > node.depth)
                            .unwrap_or(false)
                    {
                        self.tree_nav.cursor = next;
                    }
                }
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
            if let Err(e) = self.tree.toggle(&self.conns, self.tree_nav.cursor) {
                self.status = Some(format!("collapse failed: {e}"));
            }
            return;
        }
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
        let Some(conn) = self.conns.get(path.conn) else {
            self.status = Some(format!("preview: missing connection #{}", path.conn));
            return;
        };
        match (
            conn.columns(&db, &schema, &table),
            conn.preview_rows(&db, &schema, &table, PREVIEW_LIMIT),
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
    /// Empty fallback for when the initial catalog load fails — the
    /// App keeps running so the status line can surface the error.
    fn empty() -> Self {
        let conns: Vec<Box<dyn Connection>> = Vec::new();
        CatalogTree::new(&conns).expect("empty conn list cannot fail")
    }
}
