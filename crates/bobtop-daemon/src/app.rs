//! Shared application state.
//!
//! Lives behind an `Arc<Mutex<App>>`. Collector tasks lock it briefly to
//! apply samples; the render loop locks it to read for drawing. Locks are
//! never held across `.await`, so `std::sync::Mutex` is the right choice.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use std::collections::HashMap;

use bobtop_core::sample::{
    CpuSample, DiskSample, MemorySample, NetworkSample, ProcessInfo, ProcessSample,
};
use bobtop_core::{BoxesEnabled, MetricEvent};
use bobtop_net::{AttributorTier, ProcessNetSample};
use bobtop_tui::widgets::{CornerStyle, ProcessTableSort as TableSort};
use bobtop_tui::{LayoutPreset, Theme};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use crate::cli::{MAX_TICK_MS, MIN_TICK_MS, TICK_STEP_MS};
use crate::options_editor::{OptionsEditor, OptionsOutcome};
use crate::state::UiState;

/// Maximum historical samples kept for graphing. 600 = 10 min at 1 Hz.
pub const HISTORY_CAP: usize = 600;
pub const CPU_HISTORY_CAP: usize = HISTORY_CAP;
/// Inline chart style for per-core CPU and per-mount disk rows. Picks which
/// reusable widget renders the row's chart cell — only one shows at a time.
/// Default is `Sparkline`; `Bar` keeps the original gauge available so the
/// chart library stays interchangeable instead of one mode being hard-coded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrackChartStyle {
    #[default]
    Sparkline,
    Bar,
}

/// Per-core sparkline history depth — 80 samples is enough to fill ~40 cells
/// of braille (2 dot-cols per cell). Kept small so high-core hosts (64+) don't
/// pay a HISTORY_CAP × N memory bill for a meter that only renders a few cells.
pub const PER_TRACK_HISTORY_CAP: usize = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlFlow {
    Continue,
    Quit,
}

/// Editable working copy of `Config`, plus a cursor identifying which
/// field is currently being edited. The fields here intentionally mirror
/// the on-disk Config — when the user hits Enter we Serialize this back
/// out via `Config::save`. On Esc we throw the snapshot away.
/// Snapshot of /proc data shown in the detail modal (B3d). Captured
/// once when the user presses `Enter`; no live refresh — the modal is
/// for inspection, not monitoring. Fields that fail to read (perm denied,
/// process gone) get an explanatory placeholder rather than aborting.
#[derive(Debug, Clone)]
pub struct ProcessDetail {
    pub pid: u32,
    pub name: String,
    pub cmdline: String,
    pub status_lines: Vec<String>,
    pub fd_count: Result<usize, String>,
    pub io_lines: Vec<String>,
}

impl ProcessDetail {
    /// Read the relevant /proc entries for `pid`. All errors are folded
    /// into placeholders inside the returned struct — the modal opens
    /// even when individual fields are unavailable (kthreads, perms).
    /// All strings are scrubbed of control characters (especially the
    /// tabs in /proc/[pid]/{status,io}) so the renderer can call
    /// `set_char` on every byte without producing terminal artifacts.
    pub fn read(pid: u32, name: &str) -> Self {
        let base = format!("/proc/{pid}");
        let cmdline = std::fs::read(format!("{base}/cmdline"))
            .map(|bytes| {
                // /proc cmdline uses NUL separators between argv entries.
                bytes
                    .split(|b| *b == 0)
                    .filter(|s| !s.is_empty())
                    .map(|s| String::from_utf8_lossy(s).into_owned())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .map(|s| sanitize_for_display(&s))
            .unwrap_or_else(|e| format!("(cmdline unavailable: {e})"));

        let status_lines = std::fs::read_to_string(format!("{base}/status"))
            .map(|s| {
                // Pull the rows users actually care about; full status is 50+ lines.
                s.lines()
                    .filter(|l| {
                        let key = l.split(':').next().unwrap_or("");
                        matches!(
                            key,
                            "Name"
                                | "State"
                                | "Tgid"
                                | "Pid"
                                | "PPid"
                                | "Uid"
                                | "Gid"
                                | "Threads"
                                | "VmRSS"
                                | "VmSize"
                                | "voluntary_ctxt_switches"
                                | "nonvoluntary_ctxt_switches"
                        )
                    })
                    .map(sanitize_for_display)
                    .collect()
            })
            .unwrap_or_else(|e| vec![format!("(status unavailable: {e})")]);

        let fd_count = std::fs::read_dir(format!("{base}/fd"))
            .map(|it| it.count())
            .map_err(|e| e.to_string());

        let io_lines = std::fs::read_to_string(format!("{base}/io"))
            .map(|s| s.lines().map(sanitize_for_display).collect())
            .unwrap_or_else(|e| vec![format!("(io unavailable: {e})")]);

        Self {
            pid,
            name: name.to_string(),
            cmdline,
            status_lines,
            fd_count,
            io_lines,
        }
    }
}

/// Replace tabs with a single space and drop other ASCII control bytes.
/// /proc files use literal tabs as key/value separators
/// (e.g. `Name:\tbobtop`), and writing them straight into a terminal
/// cell via `Cell::set_char('\t')` produces unpredictable cursor jumps
/// or weird filler glyphs depending on the terminal — the source of
/// the modal-render artifacts. Collapse runs of whitespace too so
/// `Name:\t\tbobtop` doesn't render as a wide gap. Allows non-ASCII
/// (UTF-8 in cmdline arguments survives intact).
/// Read the CPU model name from `/proc/cpuinfo`. Returns the first
/// `model name` line's value, trimmed and de-whitespaced. Strips the
/// common AMD/Intel marketing tail (`Processor`, `CPU @ 3.40GHz`,
/// `XX-Core Processor`) so the panel title doesn't blow past 30 chars.
fn read_cpu_model() -> Option<String> {
    let body = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    let raw = body
        .lines()
        .find_map(|l| l.strip_prefix("model name").and_then(|r| r.split_once(':')))
        .map(|(_, v)| v.trim().to_string())?;
    // Trim noisy suffixes for compactness.
    let cleaned = raw
        .replace("(R)", "")
        .replace("(TM)", "")
        .replace("CPU ", "")
        .split('@')
        .next()
        .unwrap_or(&raw)
        .split("-Core")
        .next()
        .unwrap_or(&raw)
        .trim()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    // Drop trailing "<digits>" leftover from "96-Core Processor" splits.
    let cleaned = cleaned.trim_end_matches(char::is_whitespace).to_string();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn sanitize_for_display<S: AsRef<str>>(s: S) -> String {
    let mut out = String::with_capacity(s.as_ref().len());
    let mut prev_was_space = false;
    for ch in s.as_ref().chars() {
        let ch = match ch {
            '\t' => ' ',
            // Strip the rest of the C0 control range (NUL, BEL, BS, ESC, etc.)
            // and DEL. These are the bytes that confuse terminals when set
            // directly into a buffer cell.
            c if c.is_control() => continue,
            c => c,
        };
        if ch == ' ' {
            if prev_was_space {
                continue;
            }
            prev_was_space = true;
        } else {
            prev_was_space = false;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod sanitize_tests {
    use super::sanitize_for_display;

    #[test]
    fn tabs_become_single_space() {
        assert_eq!(sanitize_for_display("Name:\tbobtop"), "Name: bobtop");
    }

    #[test]
    fn runs_of_whitespace_collapse() {
        assert_eq!(sanitize_for_display("a\t\t  b"), "a b");
    }

    #[test]
    fn other_controls_dropped() {
        assert_eq!(sanitize_for_display("hi\x07\x1b\x00there"), "hithere");
    }

    #[test]
    fn unicode_survives() {
        assert_eq!(sanitize_for_display("café — utf"), "café — utf");
    }
}

/// Pending kill confirmation — when `App.pending_kill` is `Some`, the
/// kill modal is showing and the user is one keypress away from sending
/// the signal. `name` is captured at request time so the modal still
/// reads sensibly if the process disappears between request + confirm.
#[derive(Debug, Clone)]
pub struct KillRequest {
    pub pid: u32,
    pub name: String,
    pub signal: KillSignal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillSignal {
    /// SIGTERM — polite "please exit." Process can catch and clean up.
    Term,
    /// SIGKILL — immediate, uncatchable. Last resort.
    Kill,
}

impl KillSignal {
    pub fn label(self) -> &'static str {
        match self {
            KillSignal::Term => "SIGTERM",
            KillSignal::Kill => "SIGKILL",
        }
    }
    pub fn libc_value(self) -> i32 {
        match self {
            KillSignal::Term => libc::SIGTERM,
            KillSignal::Kill => libc::SIGKILL,
        }
    }
}

/// One "preset" — a complete saved view that the user can recall with a
/// single keystroke. btop calls these "presets" and binds them to 1–4;
/// we ship the same 4-slot scheme. Each preset bundles the layout, the
/// process-table sort key, and the sort direction. (Theme is intentionally
/// *not* in here — it's a per-session choice that survives preset swaps.)
#[derive(Debug, Clone, Copy)]
pub struct Preset {
    pub label: &'static str,
    pub layout: LayoutPreset,
    pub sort: TableSort,
    pub descending: bool,
}

/// Default 4-slot preset bank. Slot 0 (key `1`) is the "everything-on"
/// view; slots 1–3 (keys `2`/`3`/`4`) sharpen focus on memory, network,
/// and minimal layouts respectively. Once B10 (config file) lands these
/// will be user-overridable; for now they're a sensible fixed bank.
pub const DEFAULT_PRESETS: [Preset; 4] = [
    Preset {
        label: "all panels, sort by CPU",
        layout: LayoutPreset::Full,
        sort: TableSort::Cpu,
        descending: true,
    },
    Preset {
        label: "all panels, sort by MEM",
        layout: LayoutPreset::Full,
        sort: TableSort::Mem,
        descending: true,
    },
    Preset {
        label: "all panels, sort by NET RX",
        layout: LayoutPreset::Full,
        sort: TableSort::NetRx,
        descending: true,
    },
    Preset {
        label: "minimal (CPU + processes only)",
        layout: LayoutPreset::Minimal,
        sort: TableSort::Cpu,
        descending: true,
    },
];

#[derive(Debug)]
pub struct App {
    pub theme: Theme,
    pub layout_preset: LayoutPreset,
    pub tty_graphs: bool,
    pub show_virtual_net: bool,
    /// Interface focused by the network panel. `None` = aggregate view
    /// (sum of all visible interfaces — current default behavior). When
    /// set, the panel renders only that interface's rates + history. `b`
    /// (back) / `n` (next) keybinds cycle through visible interfaces with
    /// `None` as the first slot in the cycle ("All").
    pub selected_iface: Option<String>,
    /// Chart style for per-core CPU rows and per-mount disk rows. Default
    /// is `Sparkline` (1-row time-series). `Bar` falls back to the gauge
    /// MiniMeter / Meter — kept as a peer option so the chart-type lib
    /// stays interchangeable instead of having one mode hard-coded.
    pub track_chart_style: TrackChartStyle,
    /// CPU model name from `/proc/cpuinfo "model name"`, read once at App
    /// init. Used in the cores subpanel title alongside the live frequency.
    /// `None` on hosts that don't expose it (uncommon — most VMs / WSL do).
    pub cpu_model: Option<String>,
    /// Mirror of btop's `theme_background`. When false, theme's `main_bg`
    /// is suppressed so the terminal default (translucency / wallpaper)
    /// shows through. Applied at theme-load time in `apply_color_options`.
    pub theme_background: bool,
    /// Mirror of btop's `truecolor`. When false, theme RGB is downsampled
    /// to the 256-color cube + greyscale palette via the same algorithm
    /// btop uses (btop_theme.cpp:155). Applied at theme-load time so the
    /// downsample only runs once per theme change, not per cell.
    pub truecolor: bool,
    /// Border corner style applied to every BoxedPanel. Sourced from
    /// the config file (B12). Default rounded matches btop's signature.
    pub corner_style: CornerStyle,

    /// Per-box visibility shared with collectors. When a box is disabled,
    /// its collector tick wakes, checks the bit, and goes back to sleep
    /// without doing the (expensive) sample. Mirrors `layout_preset`'s
    /// `enabled_boxes()` set whenever the preset changes.
    pub boxes: BoxesEnabled,

    #[allow(dead_code)] // Used by future "uptime" / session-duration overlays.
    pub started_at: Instant,
    /// Live-tunable global tick in milliseconds. Shared with every collector
    /// task — they re-read it each iteration so `+` / `-` take effect on the
    /// next sample. `Ordering::Relaxed` is fine: we only need monotonic
    /// visibility, not synchronization with anything else.
    pub tick_ms: Arc<AtomicU64>,

    /// Aggregate CPU utilization history for the BrailleGraph (0.0..=1.0).
    pub cpu_history: VecDeque<f64>,
    /// Per-core utilization history (0..=1), keyed by core id. Lazily grown
    /// to fit the current CPU sample's core count — handles both hot-plug
    /// (cores appear) and the common fixed-N case without explicit init.
    pub core_history: std::collections::HashMap<u32, VecDeque<f64>>,
    /// Per-mount disk-IO history (read_rate, write_rate) keyed by mount label.
    /// Same lazy-grow pattern as `core_history` so removable mounts don't need
    /// special handling.
    pub disk_history: std::collections::HashMap<String, VecDeque<(f64, f64)>>,
    /// Memory used-fraction history for the small mem time-series graph.
    pub mem_history: VecDeque<f64>,
    /// Memory pressure-stall history — `some_avg10` percentage per sample,
    /// 0..=100. Drives the PSI sparkline. Empty on hosts without
    /// `/proc/pressure/memory`.
    pub mem_pressure_history: VecDeque<f64>,
    pub latest_cpu: Option<CpuSample>,
    pub latest_mem: Option<MemorySample>,
    pub latest_processes: Option<ProcessSample>,
    pub latest_network: Option<NetworkSample>,
    pub latest_disk: Option<DiskSample>,
    /// Per-tick aggregate of "real" interface bandwidth, suitable for the
    /// dual-trace network graph. `(rx_bytes_per_sec, tx_bytes_per_sec)`.
    pub net_history: VecDeque<(f64, f64)>,
    /// Per-interface (rx, tx) bytes/sec history, capped at HISTORY_CAP per
    /// iface. Populated alongside the aggregate `net_history`. Used when
    /// `selected_iface` scopes the network panel to a single interface.
    pub iface_history: std::collections::HashMap<String, VecDeque<(f64, f64)>>,
    pub net_samples: Vec<ProcessNetSample>,
    pub net_tier: AttributorTier,

    /// Sorted-by-CPU process list, ready to render.
    pub processes_sorted: Vec<ProcessInfo>,

    pub selected_proc: usize,
    pub scroll_offset: usize,
    /// Stable identity for the selected process row.
    selected_proc_pid: Option<u32>,
    /// Keep the highlighted process attached to the same pid as it moves.
    pub sticky_proc_selection: bool,

    /// Active sort column for the process table — driven by `←` / `→`.
    pub proc_sort: TableSort,
    /// Sort direction for `proc_sort`. Toggled by `r`.
    pub proc_sort_descending: bool,

    /// Process grouping mode (the "intelligent clustering" feature).
    /// `g` cycles flat → exec → cgroup → tree → flat.
    pub group_mode: crate::group::GroupMode,
    /// Expand/collapse state. For grouped modes (exec/cgroup), entries
    /// are header keys that are EXPANDED (default = collapsed). For tree
    /// mode, entries are pids stringified that are COLLAPSED (default =
    /// expanded). The two semantics share one set because they never
    /// coexist — switching modes resets the meaning.
    pub expanded: std::collections::HashSet<String>,

    pub ui: UiState,
}

impl App {
    pub fn new(
        theme: Theme,
        layout_preset: LayoutPreset,
        tick_ms: Arc<AtomicU64>,
        tty_graphs: bool,
        show_virtual_net: bool,
    ) -> Self {
        let boxes = BoxesEnabled::with(layout_preset.enabled_boxes());
        Self {
            theme,
            layout_preset,
            tty_graphs,
            show_virtual_net,
            selected_iface: None,
            track_chart_style: TrackChartStyle::default(),
            cpu_model: read_cpu_model(),
            theme_background: true,
            truecolor: true,
            corner_style: CornerStyle::default(),
            boxes,
            started_at: Instant::now(),
            tick_ms,
            cpu_history: VecDeque::with_capacity(CPU_HISTORY_CAP),
            core_history: std::collections::HashMap::new(),
            disk_history: std::collections::HashMap::new(),
            mem_history: VecDeque::with_capacity(HISTORY_CAP),
            mem_pressure_history: VecDeque::with_capacity(PER_TRACK_HISTORY_CAP),
            latest_cpu: None,
            latest_mem: None,
            latest_processes: None,
            latest_network: None,
            latest_disk: None,
            net_history: VecDeque::with_capacity(HISTORY_CAP),
            iface_history: std::collections::HashMap::new(),
            net_samples: Vec::new(),
            net_tier: AttributorTier::Unavailable,
            processes_sorted: Vec::new(),
            selected_proc: 0,
            scroll_offset: 0,
            selected_proc_pid: None,
            sticky_proc_selection: false,
            proc_sort: TableSort::Cpu,
            proc_sort_descending: true,
            ui: UiState::default(),
            group_mode: crate::group::GroupMode::Flat,
            expanded: std::collections::HashSet::new(),
        }
    }

    /// Atomically read-and-clear the dirty flag. The render loop calls this
    /// to decide whether to skip `terminal.draw()` this iteration.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::replace(&mut self.ui.dirty, false)
    }

    /// Mark state as observably changed. Anything that mutates a field
    /// `ui::draw` reads should call this (apply_event, handle_input
    /// branches that change selection/sort/layout/tick).
    pub fn mark_dirty(&mut self) {
        self.ui.dirty = true;
    }

    pub fn apply_event(&mut self, ev: MetricEvent) {
        self.ui.dirty = true;
        match ev {
            MetricEvent::Cpu(s) => {
                if self.cpu_history.len() == CPU_HISTORY_CAP {
                    self.cpu_history.pop_front();
                }
                self.cpu_history.push_back(s.aggregate_utilization as f64);
                // Per-core sparkline history. Only push values for cores in
                // this sample — never auto-zero an absent core, since that
                // would draw a misleading "down to 0" stair into the graph.
                for core in &s.cores {
                    let h = self
                        .core_history
                        .entry(core.id)
                        .or_insert_with(|| VecDeque::with_capacity(PER_TRACK_HISTORY_CAP));
                    if h.len() == PER_TRACK_HISTORY_CAP {
                        h.pop_front();
                    }
                    h.push_back(core.utilization as f64);
                }
                self.latest_cpu = Some(s);
            }
            MetricEvent::Memory(s) => {
                if s.total_bytes > 0 {
                    let used_frac = s.used_bytes as f64 / s.total_bytes as f64;
                    if self.mem_history.len() == HISTORY_CAP {
                        self.mem_history.pop_front();
                    }
                    self.mem_history.push_back(used_frac.clamp(0.0, 1.0));
                }
                // Push the most-responsive PSI window (`some_avg10`) into the
                // sparkline ring. Stored as 0..=1 fraction of "100% stalled
                // for the entire 10s window" — kernel value `100.00` means
                // every task waited on memory.
                if let Some(p) = s.pressure {
                    if self.mem_pressure_history.len() == PER_TRACK_HISTORY_CAP {
                        self.mem_pressure_history.pop_front();
                    }
                    self.mem_pressure_history
                        .push_back((p.some_avg10 as f64 / 100.0).clamp(0.0, 1.0));
                }
                self.latest_mem = Some(s);
            }
            MetricEvent::Process(s) => {
                self.latest_processes = Some(s);
                self.rebuild_sorted();
            }
            MetricEvent::Network(s) => {
                let (rx, tx) = aggregate_net_rates(&s, self.show_virtual_net);
                if self.net_history.len() == HISTORY_CAP {
                    self.net_history.pop_front();
                }
                self.net_history.push_back((rx, tx));
                // Per-interface history powers the `b`/`n` scope cycle on
                // the net panel. Each iface gets its own ring; absent
                // interfaces never show in the cycle.
                for iface in &s.interfaces {
                    let h = self
                        .iface_history
                        .entry(iface.name.clone())
                        .or_insert_with(|| VecDeque::with_capacity(HISTORY_CAP));
                    if h.len() == HISTORY_CAP {
                        h.pop_front();
                    }
                    h.push_back((iface.rx_bytes_per_sec, iface.tx_bytes_per_sec));
                }
                self.latest_network = Some(s);
            }
            MetricEvent::Disk(s) => {
                // Per-mount sparkline history. Normalize each rate against a
                // rolling per-mount peak at render time; the deque stores raw
                // bytes/sec so the renderer can compute its own scale ceiling.
                for fs in &s.filesystems {
                    let r = fs.read_bytes_per_sec.unwrap_or(0.0);
                    let w = fs.write_bytes_per_sec.unwrap_or(0.0);
                    let h = self
                        .disk_history
                        .entry(fs.label.clone())
                        .or_insert_with(|| VecDeque::with_capacity(PER_TRACK_HISTORY_CAP));
                    if h.len() == PER_TRACK_HISTORY_CAP {
                        h.pop_front();
                    }
                    h.push_back((r, w));
                }
                self.latest_disk = Some(s);
            }
            MetricEvent::Gpu(_) => {
                // Not yet wired into the UI; receiving is fine.
            }
        }
    }

    /// Most recent normalized (rx, tx) pair for the BrailleGraph, in 0..=1.
    pub fn net_normalized_history(&self) -> Vec<(f64, f64)> {
        let scale = self.net_scale_bps().max(1.0);
        self.net_history
            .iter()
            .map(|(rx, tx)| ((rx / scale).clamp(0.0, 1.0), (tx / scale).clamp(0.0, 1.0)))
            .collect()
    }

    /// Auto-scale ceiling for the network graph: rolling-max across both
    /// rx and tx in the entire visible `net_history` (same bound as CPU
    /// history — `HISTORY_CAP`), plus 20% headroom, floored at 1 KiB/s.
    /// Aligning the scale window with the visible-graph window means the
    /// scale label always reflects what's actually on screen.
    pub fn net_scale_bps(&self) -> f64 {
        let max_recent = self
            .net_history
            .iter()
            .flat_map(|(rx, tx)| [*rx, *tx])
            .fold(0.0_f64, |a, b| a.max(b));
        (max_recent * 1.2).max(1024.0)
    }

    /// Rolling-max for download direction only across the visible window.
    pub fn net_peak_rx(&self) -> f64 {
        self.net_history
            .iter()
            .map(|(rx, _)| *rx)
            .fold(0.0_f64, |a, b| a.max(b))
    }

    pub fn net_peak_tx(&self) -> f64 {
        self.net_history
            .iter()
            .map(|(_, tx)| *tx)
            .fold(0.0_f64, |a, b| a.max(b))
    }

    pub fn apply_net(&mut self, samples: Vec<ProcessNetSample>, tier: AttributorTier) {
        self.net_samples = samples;
        self.net_tier = tier;
        self.rebuild_sorted();
        self.ui.dirty = true;
    }

    /// Rebuild `processes_sorted` from `latest_processes`, joining in
    /// per-pid net samples (so sorting by NetRx/NetTx works against real
    /// values rather than `None`s). Always called when either a Process
    /// sample, a net attributor sample, or the active sort changes.
    fn rebuild_sorted(&mut self) {
        let mut joined: Vec<ProcessInfo> = self
            .latest_processes
            .as_ref()
            .map(|s| s.processes.clone())
            .unwrap_or_default();
        let net_idx: HashMap<u32, &ProcessNetSample> =
            self.net_samples.iter().map(|s| (s.pid, s)).collect();
        let has_bw = self.net_tier.has_bandwidth();
        for p in joined.iter_mut() {
            if let Some(n) = net_idx.get(&p.pid) {
                p.net_rx_bytes_per_sec = n.rx_bytes_per_sec;
                p.net_tx_bytes_per_sec = n.tx_bytes_per_sec;
            } else if has_bw {
                p.net_rx_bytes_per_sec = Some(0.0);
                p.net_tx_bytes_per_sec = Some(0.0);
            }
        }
        // Apply text filter (B3b) if any. Match name OR cmdline,
        // case-insensitive. Empty filter passes everything.
        if !self.ui.filter_text.is_empty() {
            let needle = self.ui.filter_text.to_lowercase();
            joined.retain(|p| {
                p.name.to_lowercase().contains(&needle)
                    || p.cmdline.to_lowercase().contains(&needle)
            });
        }
        sort_processes(&mut joined, self.proc_sort, self.proc_sort_descending);
        self.processes_sorted = joined;
        let rows = self.display_rows();
        self.sync_selected_process_position_from_rows(&rows);
    }

    /// Current global update tick in milliseconds.
    pub fn tick_ms(&self) -> u64 {
        self.tick_ms.load(Ordering::Relaxed)
    }

    /// Adjust the global tick by `delta_ms` (positive = slower, negative =
    /// faster), clamped to `[MIN_TICK_MS, MAX_TICK_MS]`. Returns the new value.
    pub fn nudge_tick(&self, delta_ms: i64) -> u64 {
        let cur = self.tick_ms() as i64;
        let new = (cur + delta_ms).clamp(MIN_TICK_MS as i64, MAX_TICK_MS as i64) as u64;
        self.tick_ms.store(new, Ordering::Relaxed);
        new
    }

    /// Step the active sort column by `delta` (-1 = previous, +1 = next),
    /// wrapping around. Re-sorts the existing process list in place so the
    /// change is visible immediately rather than waiting for the next sample.
    pub fn cycle_sort(&mut self, delta: i32) {
        let cycle = TableSort::cycle();
        let cur_idx = cycle.iter().position(|&s| s == self.proc_sort).unwrap_or(0) as i32;
        let len = cycle.len() as i32;
        let new_idx = ((cur_idx + delta) % len + len) % len;
        self.set_sort(cycle[new_idx as usize]);
    }

    /// Set the sort column directly (used by the `n`/`m`/`p`/`c` direct-sort
    /// keybinds). Re-sorts in place — the join state hasn't changed, only
    /// the comparator has.
    pub fn set_sort(&mut self, sort: TableSort) {
        self.proc_sort = sort;
        sort_processes(&mut self.processes_sorted, self.proc_sort, self.proc_sort_descending);
        let rows = self.display_rows();
        self.sync_selected_process_position_from_rows(&rows);
    }

    /// Switch the layout preset and mirror the new visible-box set into
    /// the shared `BoxesEnabled` so collectors for hidden boxes stop
    /// doing work on their next wake.
    pub fn set_layout(&mut self, preset: LayoutPreset) {
        self.layout_preset = preset;
        self.boxes.replace(preset.enabled_boxes());
    }

    /// Apply a full preset (layout + sort + direction) in one shot.
    /// Re-sorts in place if the new sort matches the existing one, otherwise
    /// re-applies via `set_sort` (which re-sorts the joined list).
    pub fn apply_preset(&mut self, p: &Preset) {
        self.set_layout(p.layout);
        self.proc_sort = p.sort;
        self.proc_sort_descending = p.descending;
        sort_processes(&mut self.processes_sorted, self.proc_sort, self.proc_sort_descending);
        let rows = self.display_rows();
        self.sync_selected_process_position_from_rows(&rows);
    }

    /// Open the options overlay (B11b) with a snapshot of the current
    /// settings. Theme list is queried from the bobtop-tui builtin
    /// registry once at open time.
    pub fn open_options(&mut self) {
        let editor = OptionsEditor::from_app(self);
        self.ui.options = Some(editor);
    }

    /// Re-apply `theme_background` and `truecolor` to `self.theme`. Should
    /// be called after every theme load so a parsed theme reflects the
    /// user's color preferences before it reaches the renderer.
    /// - `theme_background = false` clears `main_bg` so panels render on
    ///   the terminal default (transparency / wallpaper shows through).
    /// - `truecolor = false` walks every theme color and downsamples to
    ///   the 256-palette (btop's algorithm — `bobtop_tui::downsample_256`).
    pub fn apply_color_options(&mut self) {
        if !self.theme_background {
            self.theme.main_bg = None;
        }
        if !self.truecolor {
            bobtop_tui::downsample_theme_to_256(&mut self.theme);
        }
    }

    /// Materialize the current display rows. Cheap to call (clones
    /// ProcessInfo entries today; can be cached on App later if it
    /// shows up in the bench). The renderer and the keybind handlers
    /// both use this so they always agree on what's visible.
    pub fn display_rows(&self) -> Vec<crate::group::TableRow> {
        crate::group::build_display(
            &self.processes_sorted,
            self.group_mode,
            &self.expanded,
            self.proc_sort,
            self.proc_sort_descending,
        )
    }

    fn row_pid(row: Option<&crate::group::TableRow>) -> Option<u32> {
        match row? {
            crate::group::TableRow::Item(p) => Some(p.info.pid),
            crate::group::TableRow::Header(_) => None,
        }
    }

    /// Keep the cursor index as-is and refresh the remembered pid.
    fn sync_selected_process_identity_from_rows(&mut self, rows: &[crate::group::TableRow]) {
        if rows.is_empty() {
            self.selected_proc = 0;
            self.selected_proc_pid = None;
            return;
        }
        self.selected_proc = self.selected_proc.min(rows.len() - 1);
        self.selected_proc_pid = Self::row_pid(rows.get(self.selected_proc));
    }

    /// Keep the cursor on the same pid if possible after rows re-order.
    fn sync_selected_process_position_from_rows(&mut self, rows: &[crate::group::TableRow]) {
        if rows.is_empty() {
            self.selected_proc = 0;
            self.selected_proc_pid = None;
            return;
        }
        let mut idx = self.selected_proc.min(rows.len() - 1);
        if self.sticky_proc_selection {
            if let Some(pid) = self.selected_proc_pid {
                if let Some(found) = rows
                    .iter()
                    .position(|row| matches!(row, crate::group::TableRow::Item(p) if p.info.pid == pid))
                {
                    idx = found;
                }
            }
        }
        self.selected_proc = idx;
        self.selected_proc_pid = Self::row_pid(rows.get(idx));
    }

    /// Cycle the focused network interface for the net panel. `direction`
    /// is +1 for next (`n`), -1 for back (`b`). The cycle is `None` (All)
    /// → iface[0] → iface[1] → … → iface[N-1] → None, so users always
    /// have a way back to the aggregate view. Hidden virtual interfaces
    /// are skipped when `show_virtual_net = false`, matching the rest of
    /// the panel's filter.
    pub fn cycle_selected_iface(&mut self, direction: i32) {
        let Some(s) = &self.latest_network else { return };
        let mut names: Vec<String> = s
            .interfaces
            .iter()
            .filter(|i| {
                self.show_virtual_net || !bobtop_collectors::is_virtual_interface(&i.name)
            })
            .map(|i| i.name.clone())
            .collect();
        if names.is_empty() {
            self.selected_iface = None;
            return;
        }
        names.sort();
        // Slot 0 = None (All); slots 1..=N = interfaces. Map current state
        // into that index space, step, then map back.
        let cur = match &self.selected_iface {
            None => 0i32,
            Some(name) => names.iter().position(|n| n == name).map_or(0, |i| i as i32 + 1),
        };
        let total = names.len() as i32 + 1;
        let next = ((cur + direction) % total + total) % total;
        self.selected_iface = if next == 0 {
            None
        } else {
            Some(names[(next - 1) as usize].clone())
        };
        self.ui.dirty = true;
    }

    /// Cycle the grouping mode (`g` keybind). Resets `expanded` because
    /// the key namespace differs between modes (header keys for
    /// grouped views, pids for tree view).
    pub fn cycle_group_mode(&mut self) {
        self.group_mode = self.group_mode.next();
        self.expanded.clear();
        let rows = self.display_rows();
        self.sync_selected_process_position_from_rows(&rows);
    }

    /// Toggle expand/collapse for the row currently under the selection
    /// cursor. For grouped headers this expands the group; for tree
    /// processes this collapses/uncollapses the subtree rooted at that
    /// pid. No-op for Flat mode (no children to hide).
    pub fn toggle_selected_expand(&mut self) {
        let rows = self.display_rows();
        let Some(row) = rows.get(self.selected_proc) else { return };
        let key = match (self.group_mode, row) {
            (crate::group::GroupMode::Flat, _) => return,
            (_, crate::group::TableRow::Header(h)) => h.key.clone(),
            (crate::group::GroupMode::ByParent, crate::group::TableRow::Item(p)) => {
                p.info.pid.to_string()
            }
            (_, crate::group::TableRow::Item(_)) => return,
        };
        if self.expanded.contains(&key) {
            self.expanded.remove(&key);
        } else {
            self.expanded.insert(key);
        }
        let rows = self.display_rows();
        self.sync_selected_process_position_from_rows(&rows);
    }

    /// Stage a kill request for the currently-selected process. No signal
    /// is sent yet — the user has to confirm via Enter in the modal. If
    /// the selection isn't on a process row (e.g. on a group header)
    /// this is a silent no-op.
    pub fn request_kill(&mut self, signal: KillSignal) {
        let rows = self.display_rows();
        let Some(row) = rows.get(self.selected_proc) else { return };
        let crate::group::TableRow::Item(p) = row else { return };
        self.ui.pending_kill = Some(KillRequest {
            pid: p.info.pid,
            name: p.info.name.clone(),
            signal,
        });
    }

    pub fn toggle_sort_direction(&mut self) {
        self.proc_sort_descending = !self.proc_sort_descending;
        sort_processes(&mut self.processes_sorted, self.proc_sort, self.proc_sort_descending);
        let rows = self.display_rows();
        self.sync_selected_process_position_from_rows(&rows);
    }

    /// Toggle sticky selection follow for the process list.
    pub fn toggle_sticky_proc_selection(&mut self) {
        self.sticky_proc_selection = !self.sticky_proc_selection;
        let rows = self.display_rows();
        self.sync_selected_process_position_from_rows(&rows);
    }

    pub fn handle_input(&mut self, ev: Event) -> ControlFlow {
        let Event::Key(k) = ev else { return ControlFlow::Continue };
        if k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('c')) {
            return ControlFlow::Quit;
        }
        // Every recognized key either quits or mutates rendered state
        // (selection, sort, layout, tick). Cheaper to mark unconditionally
        // here than to thread `mark_dirty()` through each branch.
        let flow = self.handle_key(k);
        if matches!(flow, ControlFlow::Continue) {
            self.ui.dirty = true;
        }
        flow
    }

    fn handle_key(&mut self, k: KeyEvent) -> ControlFlow {
        // visible_rows = total render-row count (group headers + visible
        // processes). Bounding selection on this so cursor stops at the
        // bottom of what's actually painted, not the underlying process
        // count which excludes headers and includes hidden subtree members.
        let rows = self.display_rows();
        let visible_rows = rows.len();

        // Options overlay (B11b) — modal. ↑/↓ moves cursor, ←/→ cycles
        // the value at the cursor, Enter saves to disk + applies live,
        // Esc closes without saving. Routed before everything else so
        // the rest of the keybinds can't fire while the user is editing.
        if let Some(editor) = self.ui.options.take() {
            match editor.handle_key(k) {
                OptionsOutcome::KeepOpen(editor) => {
                    self.ui.options = Some(editor);
                }
                OptionsOutcome::Preview { editor, preview } => {
                    preview.apply_to(self);
                    self.ui.options = Some(editor);
                }
                OptionsOutcome::Cancel { preview } => {
                    preview.apply_to(self);
                }
                OptionsOutcome::Save { commit, message } => {
                    commit.apply_to(self);
                    self.ui.last_options_msg = Some(message);
                }
            }
            return ControlFlow::Continue;
        }

        // Detail modal (B3d) — Esc closes. While open the rest of the
        // UI is read-only so we don't lose the user's place if they
        // accidentally press a sort key.
        if self.ui.detail.is_some() {
            match k.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                    self.ui.detail = None;
                    return ControlFlow::Continue;
                }
                _ => return ControlFlow::Continue,
            }
        }

        // Kill confirm dialog (B3c) — modal. Enter sends the signal,
        // Esc/n cancels. We route this first so the user can't
        // accidentally take other action with the modal up.
        if let Some(req) = self.ui.pending_kill.clone() {
            match k.code {
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let outcome = send_signal(req.pid, req.signal);
                    self.ui.last_kill_msg = Some(outcome);
                    self.ui.pending_kill = None;
                    return ControlFlow::Continue;
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.ui.pending_kill = None;
                    return ControlFlow::Continue;
                }
                _ => return ControlFlow::Continue,
            }
        }

        // Filter input (B3b) — when active, every keystroke goes into the
        // filter text. Esc cancels the filter entirely; Enter commits and
        // exits edit mode (filter stays applied). This must come BEFORE
        // any other modal so e.g. `q` / `B` while typing don't quit/open menus.
        if self.ui.filter_active {
            match k.code {
                KeyCode::Esc => {
                    self.ui.filter_active = false;
                    self.ui.filter_text.clear();
                    self.rebuild_sorted();
                    return ControlFlow::Continue;
                }
                KeyCode::Enter => {
                    self.ui.filter_active = false;
                    return ControlFlow::Continue;
                }
                KeyCode::Backspace => {
                    self.ui.filter_text.pop();
                    self.rebuild_sorted();
                    return ControlFlow::Continue;
                }
                KeyCode::Char(c) => {
                    self.ui.filter_text.push(c);
                    self.rebuild_sorted();
                    return ControlFlow::Continue;
                }
                _ => return ControlFlow::Continue,
            }
        }

        // When the help overlay is open, only `?` / Esc / `q` are routed —
        // everything else is swallowed so the user doesn't accidentally
        // mutate state while reading the keybinds.
        if self.ui.show_help {
            match k.code {
                KeyCode::Char('?') | KeyCode::Esc => {
                    self.ui.show_help = false;
                    return ControlFlow::Continue;
                }
                KeyCode::Char('q') => return ControlFlow::Quit,
                _ => return ControlFlow::Continue,
            }
        }
        // Boxes overlay (B5): ↑/↓ moves cursor, space toggles, B/Esc closes.
        // Other keys pass through (so e.g. `+`/`-` still tunes tick while
        // the overlay is visible) — that matches btop's modal-but-permissive
        // boxes menu.
        if self.ui.show_boxes_overlay {
            use bobtop_core::Box as BoxKind;
            let n = BoxKind::ALL.len();
            match k.code {
                KeyCode::Char('B') | KeyCode::Char('b') | KeyCode::Esc => {
                    self.ui.show_boxes_overlay = false;
                    return ControlFlow::Continue;
                }
                KeyCode::Up => {
                    if self.ui.boxes_overlay_cursor > 0 {
                        self.ui.boxes_overlay_cursor -= 1;
                    }
                    return ControlFlow::Continue;
                }
                KeyCode::Down => {
                    if self.ui.boxes_overlay_cursor + 1 < n {
                        self.ui.boxes_overlay_cursor += 1;
                    }
                    return ControlFlow::Continue;
                }
                KeyCode::Char(' ') | KeyCode::Enter => {
                    let b = BoxKind::ALL[self.ui.boxes_overlay_cursor];
                    let cur = self.boxes.is_enabled(b);
                    self.boxes.set(b, !cur);
                    return ControlFlow::Continue;
                }
                _ => {} // fall through to normal handling
            }
        }
        match k.code {
            KeyCode::Char('?') => {
                self.ui.show_help = true;
                ControlFlow::Continue
            }
            KeyCode::Char('B') => {
                // Capital B opens the boxes overlay. Lowercase `b` is left
                // free for future use (btop's `b` cycles network interfaces).
                self.ui.show_boxes_overlay = true;
                self.ui.boxes_overlay_cursor = 0;
                ControlFlow::Continue
            }
            KeyCode::Char('f') => {
                // Open the filter input (B3b). If a filter is already applied
                // (text non-empty, but not in edit mode), `f` re-enters edit
                // mode so the user can refine it.
                self.ui.filter_active = true;
                ControlFlow::Continue
            }
            KeyCode::Char('k') => {
                self.request_kill(KillSignal::Term);
                ControlFlow::Continue
            }
            KeyCode::Char('K') => {
                self.request_kill(KillSignal::Kill);
                ControlFlow::Continue
            }
            KeyCode::Enter => {
                // Enter does double-duty: on a group header, expand/collapse;
                // on a process row, open the detail modal.
                if let Some(row) = rows.get(self.selected_proc) {
                    match row {
                        crate::group::TableRow::Header(_) => self.toggle_selected_expand(),
                        crate::group::TableRow::Item(p) => {
                            self.ui.detail = Some(ProcessDetail::read(p.info.pid, &p.info.name));
                        }
                    }
                }
                ControlFlow::Continue
            }
            KeyCode::Char('g') => {
                self.cycle_group_mode();
                ControlFlow::Continue
            }
            // `[` / `]` cycle the focused network interface in the net
            // panel. `n` was the obvious mnemonic but it's already bound
            // to sort-by-name in the proc panel.
            KeyCode::Char('[') => {
                self.cycle_selected_iface(-1);
                ControlFlow::Continue
            }
            KeyCode::Char(']') => {
                self.cycle_selected_iface(1);
                ControlFlow::Continue
            }
            KeyCode::Char(' ') => {
                // Space toggles expand on the row at cursor (header in
                // grouped modes, subtree root in tree mode).
                self.toggle_selected_expand();
                ControlFlow::Continue
            }
            KeyCode::Char('O') => {
                self.open_options();
                ControlFlow::Continue
            }
            KeyCode::Char('q') | KeyCode::Esc => ControlFlow::Quit,
            KeyCode::Char(c @ ('1' | '2' | '3' | '4')) => {
                let idx = (c as u8 - b'1') as usize;
                if let Some(preset) = DEFAULT_PRESETS.get(idx) {
                    self.apply_preset(preset);
                }
                ControlFlow::Continue
            }
            // Direct sort shortcuts (B3a). `m` used to alias preset 4
            // (minimal); that alias is dropped in favor of the more useful
            // sort-by-mem binding — preset 4 is still reachable via `4`.
            KeyCode::Char('p') => {
                self.set_sort(TableSort::Pid);
                ControlFlow::Continue
            }
            KeyCode::Char('n') => {
                self.set_sort(TableSort::Name);
                ControlFlow::Continue
            }
            KeyCode::Char('m') => {
                self.set_sort(TableSort::Mem);
                ControlFlow::Continue
            }
            KeyCode::Char('c') => {
                self.set_sort(TableSort::Cpu);
                ControlFlow::Continue
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                // `=` is the unshifted form of `+` on US keyboards — accept both.
                self.nudge_tick(TICK_STEP_MS as i64);
                ControlFlow::Continue
            }
            KeyCode::Char('-') | KeyCode::Char('_') => {
                self.nudge_tick(-(TICK_STEP_MS as i64));
                ControlFlow::Continue
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.cycle_sort(-1);
                ControlFlow::Continue
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.cycle_sort(1);
                ControlFlow::Continue
            }
            KeyCode::Char('r') => {
                self.toggle_sort_direction();
                ControlFlow::Continue
            }
            KeyCode::Char('s') => {
                self.toggle_sticky_proc_selection();
                ControlFlow::Continue
            }
            KeyCode::Up => {
                // Note: vim-style `k` for up was dropped after B3a/B3c —
                // `k` now sends SIGTERM and `j` `n` `m` etc. carry sort
                // shortcuts. Use the actual arrow keys for cursor movement.
                if self.selected_proc > 0 {
                    self.selected_proc -= 1;
                    if self.selected_proc < self.scroll_offset {
                        self.scroll_offset = self.selected_proc;
                    }
                }
                self.sync_selected_process_identity_from_rows(&rows);
                ControlFlow::Continue
            }
            KeyCode::Down => {
                if self.selected_proc + 1 < visible_rows {
                    self.selected_proc += 1;
                }
                self.sync_selected_process_identity_from_rows(&rows);
                ControlFlow::Continue
            }
            KeyCode::PageUp => {
                self.selected_proc = self.selected_proc.saturating_sub(10);
                if self.selected_proc < self.scroll_offset {
                    self.scroll_offset = self.selected_proc;
                }
                self.sync_selected_process_identity_from_rows(&rows);
                ControlFlow::Continue
            }
            KeyCode::PageDown => {
                self.selected_proc = (self.selected_proc + 10).min(visible_rows.saturating_sub(1));
                self.sync_selected_process_identity_from_rows(&rows);
                ControlFlow::Continue
            }
            KeyCode::Home => {
                self.selected_proc = 0;
                self.scroll_offset = 0;
                self.sync_selected_process_identity_from_rows(&rows);
                ControlFlow::Continue
            }
            KeyCode::End => {
                self.selected_proc = visible_rows.saturating_sub(1);
                self.sync_selected_process_identity_from_rows(&rows);
                ControlFlow::Continue
            }
            _ => ControlFlow::Continue,
        }
    }
}

fn sort_processes(rows: &mut [ProcessInfo], sort: TableSort, descending: bool) {
    use std::cmp::Ordering;
    rows.sort_by(|a, b| {
        let ord = match sort {
            TableSort::Pid => a.pid.cmp(&b.pid),
            TableSort::Name => a.name.cmp(&b.name),
            TableSort::User => a.user.cmp(&b.user),
            TableSort::Threads => a.threads.cmp(&b.threads),
            TableSort::Mem => a.mem_rss_bytes.cmp(&b.mem_rss_bytes),
            TableSort::Cpu => a
                .cpu_fraction
                .partial_cmp(&b.cpu_fraction)
                .unwrap_or(Ordering::Equal),
            TableSort::NetRx => a
                .net_rx_bytes_per_sec
                .unwrap_or(0.0)
                .partial_cmp(&b.net_rx_bytes_per_sec.unwrap_or(0.0))
                .unwrap_or(Ordering::Equal),
            TableSort::NetTx => a
                .net_tx_bytes_per_sec
                .unwrap_or(0.0)
                .partial_cmp(&b.net_tx_bytes_per_sec.unwrap_or(0.0))
                .unwrap_or(Ordering::Equal),
            TableSort::DiskRead => a
                .disk_read_bytes_per_sec
                .unwrap_or(0.0)
                .partial_cmp(&b.disk_read_bytes_per_sec.unwrap_or(0.0))
                .unwrap_or(Ordering::Equal),
            TableSort::DiskWrite => a
                .disk_write_bytes_per_sec
                .unwrap_or(0.0)
                .partial_cmp(&b.disk_write_bytes_per_sec.unwrap_or(0.0))
                .unwrap_or(Ordering::Equal),
        };
        if descending { ord.reverse() } else { ord }
    });
}

/// Send `signal` to `pid` via libc::kill. Returns a short human-readable
/// outcome ("sent SIGTERM to pid 1234" / "kill(1234) failed: EPERM") for
/// the status-bar toast. Errors are surfaced to the user but never
/// propagated up — kill failures are routine (perm denied, race with
/// process exit) and should not crash the TUI.
fn send_signal(pid: u32, signal: KillSignal) -> String {
    // SAFETY: libc::kill is async-signal-safe; we pass a valid (positive)
    // pid_t and a known constant signal number. Worst case the kernel
    // returns EINVAL/EPERM/ESRCH, which we surface as a string.
    let rc = unsafe { libc::kill(pid as libc::pid_t, signal.libc_value()) };
    if rc == 0 {
        format!("sent {} to pid {}", signal.label(), pid)
    } else {
        let err = std::io::Error::last_os_error();
        format!("kill(pid={pid}, {sig}) failed: {err}", sig = signal.label())
    }
}

fn aggregate_net_rates(s: &NetworkSample, include_virtual: bool) -> (f64, f64) {
    let mut rx = 0.0;
    let mut tx = 0.0;
    for iface in &s.interfaces {
        if !include_virtual && bobtop_collectors::is_virtual_interface(&iface.name) {
            continue;
        }
        rx += iface.rx_bytes_per_sec;
        tx += iface.tx_bytes_per_sec;
    }
    (rx, tx)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use bobtop_core::sample::{CpuSample, ProcessState};

    use super::*;

    fn theme() -> Theme {
        Theme::fallback()
    }

    fn fake_cpu(util: f32) -> CpuSample {
        CpuSample {
            timestamp: Instant::now(),
            aggregate_utilization: util,
            cores: vec![],
            load_average: None,
        }
    }

    fn fake_proc(pid: u32, name: &str, cpu: f32) -> ProcessInfo {
        ProcessInfo {
            pid,
            parent_pid: None,
            name: name.into(),
            cmdline: String::new(),
            user: "u".into(),
            state: ProcessState::Running,
            cpu_fraction: cpu,
            mem_rss_bytes: 0,
            mem_vsz_bytes: 0,
            threads: 1,
            net_rx_bytes_per_sec: None,
            net_tx_bytes_per_sec: None,
            disk_read_bytes_per_sec: None,
            disk_write_bytes_per_sec: None,
            cgroup: None,
        }
    }

    #[test]
    fn cpu_history_bounded_at_cap() {
        let mut app = App::new(theme(), LayoutPreset::Full, Arc::new(AtomicU64::new(500)), false, false);
        for _ in 0..(CPU_HISTORY_CAP + 50) {
            app.apply_event(MetricEvent::Cpu(fake_cpu(0.5)));
        }
        assert_eq!(app.cpu_history.len(), CPU_HISTORY_CAP);
    }

    #[test]
    fn process_event_sorts_by_cpu_descending() {
        let mut app = App::new(theme(), LayoutPreset::Full, Arc::new(AtomicU64::new(500)), false, false);
        let sample = ProcessSample {
            timestamp: Instant::now(),
            processes: vec![
                fake_proc(1, "low", 0.05),
                fake_proc(2, "high", 0.95),
                fake_proc(3, "mid", 0.50),
            ],
        };
        app.apply_event(MetricEvent::Process(sample));
        assert_eq!(app.processes_sorted[0].name, "high");
        assert_eq!(app.processes_sorted[2].name, "low");
    }

    #[test]
    fn esc_and_q_quit() {
        let mut app = App::new(theme(), LayoutPreset::Full, Arc::new(AtomicU64::new(500)), false, false);
        let q = Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.handle_input(q), ControlFlow::Quit);
        assert_eq!(app.handle_input(esc), ControlFlow::Quit);
    }

    #[test]
    fn ctrl_c_quits() {
        let mut app = App::new(theme(), LayoutPreset::Full, Arc::new(AtomicU64::new(500)), false, false);
        let ev = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(app.handle_input(ev), ControlFlow::Quit);
    }

    #[test]
    fn boxes_overlay_toggles_a_box_and_closes() {
        use bobtop_core::Box as BoxKind;
        let mut app = App::new(theme(), LayoutPreset::Full, Arc::new(AtomicU64::new(500)), false, false);
        // All boxes start enabled in Full.
        assert!(app.boxes.is_enabled(BoxKind::Cpu));

        // Open overlay with capital B.
        let open = Event::Key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::NONE));
        app.handle_input(open);
        assert!(app.ui.show_boxes_overlay);
        assert_eq!(app.ui.boxes_overlay_cursor, 0); // first row = CPU

        // Space toggles the cursor's box (CPU at index 0).
        let space = || Event::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        app.handle_input(space());
        assert!(!app.boxes.is_enabled(BoxKind::Cpu));

        // Down moves cursor to MEM, space disables it too.
        let down = Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_input(down);
        assert_eq!(app.ui.boxes_overlay_cursor, 1);
        app.handle_input(space());
        assert!(!app.boxes.is_enabled(BoxKind::Memory));

        // Esc closes; CPU/MEM stay disabled (overlay state was just visibility).
        let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_input(esc);
        assert!(!app.ui.show_boxes_overlay);
        assert!(!app.boxes.is_enabled(BoxKind::Cpu));
    }

    #[test]
    fn preset_keys_swap_layout_and_sort() {
        use bobtop_core::Box as BoxKind;
        let mut app = App::new(theme(), LayoutPreset::Full, Arc::new(AtomicU64::new(500)), false, false);
        // Seed with a Process sample so apply_preset has something to sort.
        let sample = ProcessSample {
            timestamp: Instant::now(),
            processes: vec![
                fake_proc(1, "a", 0.10),
                fake_proc(2, "b", 0.95),
            ],
        };
        app.apply_event(MetricEvent::Process(sample));

        // Preset 2 (key '2') = Full + sort by Mem.
        let two = Event::Key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));
        app.handle_input(two);
        assert_eq!(app.layout_preset, LayoutPreset::Full);
        assert_eq!(app.proc_sort, TableSort::Mem);

        // Preset 4 (key '4') = Minimal — should disable MEM/DISK/NET boxes.
        let four = Event::Key(KeyEvent::new(KeyCode::Char('4'), KeyModifiers::NONE));
        app.handle_input(four);
        assert_eq!(app.layout_preset, LayoutPreset::Minimal);
        assert_eq!(app.proc_sort, TableSort::Cpu);
        assert!(!app.boxes.is_enabled(BoxKind::Memory));
        assert!(!app.boxes.is_enabled(BoxKind::Network));
        assert!(app.boxes.is_enabled(BoxKind::Cpu));
        assert!(app.boxes.is_enabled(BoxKind::Process));

        // After B3a, `m` is the direct sort-by-mem shortcut, not the
        // preset-4 alias. Verify it sorts (and does NOT change layout).
        app.apply_preset(&DEFAULT_PRESETS[0]); // back to slot 1 (sort: Cpu)
        assert_eq!(app.layout_preset, LayoutPreset::Full);
        assert_eq!(app.proc_sort, TableSort::Cpu);
        let m = Event::Key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        app.handle_input(m);
        assert_eq!(app.proc_sort, TableSort::Mem);
        assert_eq!(app.layout_preset, LayoutPreset::Full); // not changed
    }

    #[test]
    fn options_theme_cycle_live_previews_and_esc_reverts() {
        let mut app = App::new(theme(), LayoutPreset::Full, Arc::new(AtomicU64::new(500)), false, false);
        let original = app.theme.name.clone();

        // Open options. Cursor starts on theme (field 0).
        let press = |c: char| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        app.handle_input(press('O'));
        assert!(app.ui.options.is_some());
        assert_eq!(app.ui.options.as_ref().unwrap().cursor(), 0);

        // Press → to advance theme. Both the staged opts.theme and the
        // live app.theme must change in lockstep.
        let right = Event::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        app.handle_input(right);
        let after_cycle = app.theme.name.clone();
        assert_ne!(
            after_cycle, original,
            "live theme should change when cycling theme field"
        );
        assert_eq!(app.ui.options.as_ref().unwrap().theme_name(), after_cycle);

        // Esc closes the modal and reverts the theme.
        let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_input(esc);
        assert!(app.ui.options.is_none());
        assert_eq!(app.theme.name, original, "Esc should revert theme");
    }

    #[test]
    fn kill_key_stages_request_then_cancel_clears_it() {
        let mut app = App::new(theme(), LayoutPreset::Full, Arc::new(AtomicU64::new(500)), false, false);
        let sample = ProcessSample {
            timestamp: Instant::now(),
            processes: vec![fake_proc(4242, "victim", 0.0)],
        };
        app.apply_event(MetricEvent::Process(sample));
        app.selected_proc = 0;

        // Press `k` → SIGTERM staged for selected pid.
        let press = |c: char| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        app.handle_input(press('k'));
        let req = app.ui.pending_kill.as_ref().expect("kill staged");
        assert_eq!(req.pid, 4242);
        assert_eq!(req.signal, KillSignal::Term);

        // Esc cancels — no signal sent (no last_kill_msg), pending cleared.
        let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_input(esc);
        assert!(app.ui.pending_kill.is_none());
        assert!(app.ui.last_kill_msg.is_none());

        // Press `K` (capital) → SIGKILL staged.
        app.handle_input(press('K'));
        let req = app.ui.pending_kill.as_ref().expect("kill staged");
        assert_eq!(req.signal, KillSignal::Kill);
    }

    #[test]
    fn filter_typing_narrows_processes_sorted() {
        let mut app = App::new(theme(), LayoutPreset::Full, Arc::new(AtomicU64::new(500)), false, false);
        let sample = ProcessSample {
            timestamp: Instant::now(),
            processes: vec![
                fake_proc(1, "firefox", 0.10),
                fake_proc(2, "chrome", 0.20),
                fake_proc(3, "chromium", 0.30),
            ],
        };
        app.apply_event(MetricEvent::Process(sample));
        assert_eq!(app.processes_sorted.len(), 3);

        // Open filter, type "chrom" → should keep "chrome" + "chromium".
        let press = |c: char| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        app.handle_input(press('f'));
        assert!(app.ui.filter_active);
        for c in "chrom".chars() {
            app.handle_input(press(c));
        }
        assert_eq!(app.ui.filter_text, "chrom");
        assert_eq!(app.processes_sorted.len(), 2);
        assert!(app.processes_sorted.iter().all(|p| p.name.contains("chrom")));

        // Esc clears the filter and exits edit mode.
        let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_input(esc);
        assert!(!app.ui.filter_active);
        assert_eq!(app.ui.filter_text, "");
        assert_eq!(app.processes_sorted.len(), 3);
    }

    #[test]
    fn direct_sort_keys_set_sort_columns() {
        let mut app = App::new(theme(), LayoutPreset::Full, Arc::new(AtomicU64::new(500)), false, false);
        let press = |c: char| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        app.handle_input(press('p'));
        assert_eq!(app.proc_sort, TableSort::Pid);
        app.handle_input(press('n'));
        assert_eq!(app.proc_sort, TableSort::Name);
        app.handle_input(press('m'));
        assert_eq!(app.proc_sort, TableSort::Mem);
        app.handle_input(press('c'));
        assert_eq!(app.proc_sort, TableSort::Cpu);
    }

    #[test]
    fn sticky_selection_follows_process_across_resort() {
        let mut app = App::new(theme(), LayoutPreset::Full, Arc::new(AtomicU64::new(500)), false, false);
        let sample = ProcessSample {
            timestamp: Instant::now(),
            processes: vec![
                fake_proc(10, "alpha", 0.20),
                fake_proc(20, "beta", 0.90),
                fake_proc(30, "gamma", 0.50),
            ],
        };
        app.apply_event(MetricEvent::Process(sample));
        assert!(!app.sticky_proc_selection);

        app.toggle_sticky_proc_selection();
        assert!(app.sticky_proc_selection);

        let rows = app.display_rows();
        let selected_idx = rows
            .iter()
            .position(|row| matches!(row, crate::group::TableRow::Item(p) if p.info.pid == 30))
            .expect("selected pid should be visible");
        app.selected_proc = selected_idx;
        app.sync_selected_process_identity_from_rows(&rows);
        assert_eq!(app.selected_proc_pid, Some(30));

        app.set_sort(TableSort::Pid);
        let rows = app.display_rows();
        let selected_row = rows.get(app.selected_proc).expect("selection should stay visible");
        let pid = match selected_row {
            crate::group::TableRow::Item(p) => p.info.pid,
            crate::group::TableRow::Header(_) => 0,
        };
        assert_eq!(pid, 30);
        assert_eq!(app.selected_proc_pid, Some(30));
    }

    #[test]
    fn layout_switch_updates_boxes_enabled() {
        use bobtop_core::Box as BoxKind;
        let mut app = App::new(theme(), LayoutPreset::Full, Arc::new(AtomicU64::new(500)), false, false);
        for b in BoxKind::ALL {
            assert!(app.boxes.is_enabled(b), "{b:?} should start enabled in Full");
        }
        app.set_layout(LayoutPreset::Minimal);
        assert!(app.boxes.is_enabled(BoxKind::Cpu));
        assert!(app.boxes.is_enabled(BoxKind::Process));
        assert!(!app.boxes.is_enabled(BoxKind::Memory));
        assert!(!app.boxes.is_enabled(BoxKind::Disk));
        assert!(!app.boxes.is_enabled(BoxKind::Network));
        app.set_layout(LayoutPreset::Full);
        for b in BoxKind::ALL {
            assert!(app.boxes.is_enabled(b), "{b:?} should re-enable on Full");
        }
    }

    #[test]
    fn dirty_flag_set_by_event_and_input_cleared_by_take() {
        let mut app = App::new(theme(), LayoutPreset::Full, Arc::new(AtomicU64::new(500)), false, false);
        // Starts dirty so the first frame paints.
        assert!(app.take_dirty());
        assert!(!app.take_dirty(), "consecutive take_dirty should clear");

        app.apply_event(MetricEvent::Cpu(fake_cpu(0.3)));
        assert!(app.take_dirty(), "apply_event must mark dirty");

        let down = Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_input(down);
        assert!(app.take_dirty(), "handle_input on a recognized key must mark dirty");

        // Quit-path keys don't matter for dirty (we exit immediately).
        let _ = app.take_dirty();
        let unrelated = Event::Key(KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE));
        app.handle_input(unrelated);
        // F12 hits the `_ => Continue` branch — that branch still marks dirty.
        // We accept that as a small over-paint cost in exchange for not
        // threading mark_dirty into every match arm.
        assert!(app.take_dirty());
    }

    #[test]
    fn down_advances_selection_and_keeps_in_bounds() {
        let mut app = App::new(theme(), LayoutPreset::Full, Arc::new(AtomicU64::new(500)), false, false);
        let sample = ProcessSample {
            timestamp: Instant::now(),
            processes: vec![fake_proc(1, "a", 0.1), fake_proc(2, "b", 0.2)],
        };
        app.apply_event(MetricEvent::Process(sample));
        let down = Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.selected_proc, 0);
        app.handle_input(down.clone());
        assert_eq!(app.selected_proc, 1);
        // Saturates at last row.
        app.handle_input(down);
        assert_eq!(app.selected_proc, 1);
    }
}
