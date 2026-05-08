//! Command-line argument parsing.

use std::time::Duration;

use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, ValueEnum, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LayoutChoice {
    #[default]
    Full,
    Minimal,
}

#[derive(Debug, Clone, Copy, ValueEnum, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CornerChoice {
    #[default]
    Rounded,
    Square,
}

impl From<CornerChoice> for gtui::widgets::CornerStyle {
    fn from(c: CornerChoice) -> Self {
        match c {
            CornerChoice::Rounded => gtui::widgets::CornerStyle::Rounded,
            CornerChoice::Square => gtui::widgets::CornerStyle::Square,
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "gtop", version, about = "A best-in-class terminal system monitor")]
pub struct Cli {
    /// Theme name to load. Searches built-ins, then ~/.config/gtop/themes/,
    /// then ~/.config/btop/themes/.
    #[arg(long, default_value = "dracula")]
    pub theme: String,

    /// Global update tick in milliseconds. Every collector — CPU, memory,
    /// network, disk, processes — samples on this single cadence (matches
    /// btop's `update_ms` model). Live-tunable in the TUI with `+` / `-`.
    /// Default 1500 ms matches btop's recommended cadence — calm by default,
    /// tune down for finer-grained traces.
    #[arg(long, alias = "interval-ms", default_value_t = 1500)]
    pub tick_ms: u64,

    /// Layout preset.
    #[arg(long, value_enum, default_value_t = LayoutChoice::Full)]
    pub layout: LayoutChoice,

    /// Disable eBPF tier of network attribution even if available.
    #[arg(long)]
    pub no_ebpf: bool,

    /// Disable libpcap tier of network attribution even if available.
    #[arg(long)]
    pub no_pcap: bool,

    /// Force the block-character TTY fallback for graphs (use on Linux VTs
    /// or terminals without braille support).
    #[arg(long)]
    pub tty: bool,

    /// Include virtual / container / VPN interfaces in the network panel
    /// aggregate (lo, docker*, veth*, virbr*, tun*, tap*, br-*). Default is
    /// to exclude them so the graph reflects "real" external traffic.
    #[arg(long)]
    pub show_virtual_net: bool,

    /// Border corner style: `rounded` (default, btop signature look) or
    /// `square` for fonts/terminals where rounded glyphs render poorly.
    #[arg(long, value_enum, default_value_t = CornerChoice::Rounded)]
    pub corners: CornerChoice,

    /// Print every available theme name and exit.
    #[arg(long)]
    pub list_themes: bool,

    /// Print the keybind list and exit. Uses the same source-of-truth
    /// table the in-app `?` overlay reads, so the two cannot drift.
    #[arg(long)]
    pub help_keys: bool,

    /// Run in headless daemon mode: start the sampler engine + agent
    /// socket, skip the TUI. Useful on servers / CI runners where you
    /// want agents to query the host without a terminal attached. Exits
    /// on SIGINT/SIGTERM.
    #[arg(long)]
    pub daemon: bool,
}

impl Cli {
    pub fn tick(&self) -> Duration {
        Duration::from_millis(self.tick_ms.clamp(MIN_TICK_MS, MAX_TICK_MS))
    }
}

/// Lower clamp on the global update tick. Below this, /proc parsing starts to
/// dominate CPU on busy systems.
pub const MIN_TICK_MS: u64 = 50;
/// Upper clamp on the global update tick. Above this and the TUI feels stale.
pub const MAX_TICK_MS: u64 = 10_000;
/// Single ±step applied by the `+` / `-` keys.
pub const TICK_STEP_MS: u64 = 100;
