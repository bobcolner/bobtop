//! User-editable defaults persisted to disk.
//!
//! Stored at `$XDG_CONFIG_HOME/gfb/config.toml` (default
//! `~/.config/gfb/config.toml`). Loaded once at startup, written
//! whenever the options menu commits. Best-effort I/O — missing or
//! unwritable files never block startup or commit, matching the
//! conventions in [`crate::state`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Name of a theme bundled with `gtui` (e.g. `dracula`, `gruvbox`).
    /// Falls back to the gtui default if missing or unknown.
    pub theme: Option<String>,
    /// Whether to enable crossterm mouse capture on startup. Off by
    /// default — see `cli.rs` for why.
    pub mouse_capture: Option<bool>,
    /// Whether the file browser shows dotfiles by default.
    pub show_hidden: Option<bool>,
    /// Default file-browser sort: `name` | `size` | `mtime`.
    pub sort: Option<String>,
    /// Saved DB connection URLs. Used when neither `--connect` nor
    /// `$GFB_CONNECT` is set so reopening the browser auto-attaches.
    #[serde(default)]
    pub connections: Vec<ConnectionEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionEntry {
    pub url: String,
    /// Optional human label for the options-menu list. Not used at
    /// connect time — the worker derives its own label from the URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

pub fn config_file() -> PathBuf {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| {
                let mut p = PathBuf::from(h);
                p.push(".config");
                p
            })
        })
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let mut path = dir;
    path.push("gfb");
    path.push("config.toml");
    path
}

pub fn load() -> Config {
    let path = config_file();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return Config::default(),
    };
    toml::from_str(&text).unwrap_or_default()
}

/// Persist `config` to `config_file()`. Returns the resolved path on
/// success so callers can surface it in a status flash.
pub fn save(config: &Config) -> std::io::Result<PathBuf> {
    let path = config_file();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(config)
        .map_err(|e| std::io::Error::other(format!("toml: {e}")))?;
    atomic_write(&path, &text)?;
    Ok(path)
}

fn atomic_write(path: &Path, text: &str) -> std::io::Result<()> {
    let mut tmp = path.to_path_buf();
    let mut name = path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    name.push(".tmp");
    tmp.set_file_name(name);
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_keeps_all_fields() {
        let c = Config {
            theme: Some("dracula".into()),
            mouse_capture: Some(true),
            show_hidden: Some(false),
            sort: Some("size".into()),
            connections: vec![ConnectionEntry {
                url: "postgres://localhost/test".into(),
                label: Some("test".into()),
            }],
        };
        let text = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.theme.as_deref(), Some("dracula"));
        assert_eq!(back.mouse_capture, Some(true));
        assert_eq!(back.connections.len(), 1);
        assert_eq!(back.connections[0].url, "postgres://localhost/test");
    }

    #[test]
    fn empty_file_loads_default() {
        let c: Config = toml::from_str("").unwrap();
        assert!(c.theme.is_none());
        assert!(c.connections.is_empty());
    }
}
