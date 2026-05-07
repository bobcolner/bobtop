//! Preview pipeline.
//!
//! Each preview is produced by a previewer that takes a path and returns a
//! `Preview` (lines + classification). The dispatcher today only handles
//! text — image/markdown/PDF land in later milestones, gated by the same
//! `Previewer` trait so adding them doesn't churn the App.
//!
//! The async surface is intentionally thin: callers spawn
//! `render_for(path)` on a tokio runtime, the result lands on an mpsc
//! channel tagged with a `generation` counter, and the App drops stale
//! results when the cursor has moved on.

pub mod cache;
pub mod image;
pub mod markdown;
pub mod text;

use std::path::PathBuf;
use std::sync::Arc;

use ::image::DynamicImage;
use ratatui::text::Line;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewKind {
    /// Plain or syntax-highlighted text.
    Text,
    /// Markdown rendered to styled lines.
    Markdown,
    /// Image (decoded; rasterized at draw time).
    Image,
    /// Empty file (0 bytes).
    Empty,
    /// File too large to preview — content shows a head + size note.
    TooLarge,
    /// Looks binary; content is a hex / printable summary.
    Binary,
    /// Directory — content lists immediate children.
    Directory,
}

#[derive(Clone)]
pub enum PreviewBody {
    /// Pre-styled lines for `ScrollableText`.
    Lines(Vec<Line<'static>>),
    /// Decoded image; rasterized to half-blocks at draw time when the
    /// caller knows the destination rect.
    Image(Arc<DynamicImage>),
}

impl std::fmt::Debug for PreviewBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PreviewBody::Lines(v) => write!(f, "Lines({})", v.len()),
            PreviewBody::Image(img) => {
                write!(f, "Image({}x{})", img.width(), img.height())
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Preview {
    pub kind: PreviewKind,
    pub body: PreviewBody,
    /// Total source-line count before truncation (for the title/footer
    /// hint when content was capped).
    pub source_lines: usize,
    pub note: Option<String>,
}

impl Preview {
    pub fn empty() -> Self {
        Self {
            kind: PreviewKind::Empty,
            body: PreviewBody::Lines(vec![Line::from("(empty file)")]),
            source_lines: 0,
            note: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum PreviewState {
    None,
    Loading(PathBuf),
    Ready {
        path: PathBuf,
        preview: Arc<Preview>,
    },
    Error {
        path: PathBuf,
        message: String,
    },
}

impl PreviewState {
    pub fn path(&self) -> Option<&PathBuf> {
        match self {
            PreviewState::None => None,
            PreviewState::Loading(p) | PreviewState::Error { path: p, .. } => Some(p),
            PreviewState::Ready { path, .. } => Some(path),
        }
    }
}

/// Result delivered back to the App from the spawned task.
#[derive(Debug)]
pub struct PreviewResult {
    pub generation: u64,
    pub path: PathBuf,
    pub outcome: Result<Preview, String>,
}

/// Hard limits enforced by every previewer. Worst-case work per file is
/// `bytes` * a small constant (highlighter overhead is roughly 10x raw
/// I/O for syntect; capping at 2 MiB caps per-file work at ~20 MiB).
#[derive(Debug, Clone, Copy)]
pub struct PreviewLimits {
    pub max_bytes: u64,
    pub max_lines: usize,
}

impl Default for PreviewLimits {
    fn default() -> Self {
        Self {
            max_bytes: 2 * 1024 * 1024,
            max_lines: 5_000,
        }
    }
}

/// Synchronous entry point — does the actual work. Spawn from a tokio
/// blocking task so file I/O, syntect highlighting, and image decoding
/// don't block the async runtime.
///
/// Dispatch is extension-first: cheap to evaluate and matches what the
/// user typed when they named the file. Magic-byte fallback can come
/// later (lots of code is named `Makefile`, no extension); for now an
/// unknown extension falls through to the text previewer, which has
/// its own binary heuristic.
pub fn render_blocking(path: &std::path::Path, limits: PreviewLimits) -> Result<Preview, String> {
    if path.is_dir() {
        return text::render_directory(path);
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("md" | "markdown") => markdown::render_markdown(path, limits),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp") => image::decode_image(path),
        _ => text::render_text(path, limits),
    }
}
