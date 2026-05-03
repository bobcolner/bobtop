//! `~/.config/bobtop/bobtop.toml` — sticky preferences.
//!
//! The config file is the user's "what I want by default" surface. CLI
//! flags always win over file values (and over hardcoded defaults). The
//! file is intentionally optional: a missing or malformed file is logged
//! at warn level and treated as empty — bobtop never fails to start
//! because of a bad config.
//!
//! Each field is `Option<T>` so the file can express "leave this alone"
//! by simply omitting the key. `effective()` in `main.rs` walks
//! (CLI > file > default) per-field using clap's `value_source` to
//! distinguish a user-provided CLI value from a clap default.
//!
//! Write-back (the in-app `O` options menu — B11) lands in a follow-up.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::cli::LayoutChoice;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub theme: Option<String>,
    pub tick_ms: Option<u64>,
    pub layout: Option<LayoutChoice>,
    pub no_ebpf: Option<bool>,
    pub no_pcap: Option<bool>,
    pub tty: Option<bool>,
    pub show_virtual_net: Option<bool>,
}

impl Config {
    /// Resolved file path: `$XDG_CONFIG_HOME/bobtop/bobtop.toml`, falling
    /// back to `$HOME/.config/bobtop/bobtop.toml`. Returns `None` when no
    /// home directory is discoverable (extremely unusual — sandboxed daemons
    /// without `$HOME` set), in which case the loader treats it as "no file."
    pub fn default_path() -> Option<PathBuf> {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            if !xdg.is_empty() {
                return Some(PathBuf::from(xdg).join("bobtop").join("bobtop.toml"));
            }
        }
        let home = std::env::var("HOME").ok()?;
        Some(PathBuf::from(home).join(".config").join("bobtop").join("bobtop.toml"))
    }

    /// Try to load from the canonical path. A missing file is silent;
    /// malformed TOML / unknown keys log a warning and return `Default`.
    pub fn load_or_default() -> Self {
        let Some(path) = Self::default_path() else {
            return Self::default();
        };
        Self::load_from(&path).unwrap_or_default()
    }

    /// Load + parse a specific path. `Ok(Default)` when the file is absent;
    /// `Ok(Default)` (after a logged warning) when parse fails.
    pub fn load_from(path: &std::path::Path) -> std::io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(contents) => match toml::from_str::<Self>(&contents) {
                Ok(c) => {
                    tracing::info!(path = %path.display(), "loaded config");
                    Ok(c)
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "config parse failed; using defaults",
                    );
                    Ok(Self::default())
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "config read failed; using defaults",
                );
                Ok(Self::default())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_round_trips() {
        let c = Config::default();
        let s = toml::to_string(&c).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert!(back.theme.is_none());
        assert!(back.tick_ms.is_none());
    }

    #[test]
    fn parses_partial_file() {
        let src = r#"
            theme = "nord"
            tick_ms = 2000
            no_ebpf = true
        "#;
        let c: Config = toml::from_str(src).unwrap();
        assert_eq!(c.theme.as_deref(), Some("nord"));
        assert_eq!(c.tick_ms, Some(2000));
        assert_eq!(c.no_ebpf, Some(true));
        assert!(c.no_pcap.is_none());
        assert!(c.tty.is_none());
    }

    #[test]
    fn unknown_keys_rejected() {
        let src = r#"theme = "nord"
totally_made_up = 42"#;
        // deny_unknown_fields means unknown key is a parse error — we want
        // typos surfaced rather than silently ignored.
        assert!(toml::from_str::<Config>(src).is_err());
    }

    #[test]
    fn missing_file_yields_default() {
        let path = std::path::Path::new("/no/such/file/anywhere.toml");
        let c = Config::load_from(path).unwrap();
        assert!(c.theme.is_none());
    }

    #[test]
    fn malformed_file_yields_default_not_error() {
        // Use a temp file so we exercise the read+parse path, not just the
        // "file missing" branch. Malformed TOML must not fail to load.
        let tmp = std::env::temp_dir().join(format!(
            "bobtop_malformed_{}.toml",
            std::process::id()
        ));
        std::fs::write(&tmp, "this is = not valid toml at all = ===").unwrap();
        let c = Config::load_from(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);
        assert!(c.theme.is_none());
    }
}
