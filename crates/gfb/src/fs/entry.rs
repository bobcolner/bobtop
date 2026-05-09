//! Directory entry record.
//!
//! Captures the small set of fields the UI shows: name, kind, size, mtime
//! (as a `SystemTime`; formatting happens at draw time), and a symlink
//! target hint. Permissions/ownership are deferred — they're not in the
//! v1 column set.

use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
    Symlink,
    Other,
}

#[derive(Debug, Clone)]
pub struct FsEntry {
    pub path: PathBuf,
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    pub mtime: Option<SystemTime>,
}

impl FsEntry {
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, EntryKind::Dir)
    }
}
