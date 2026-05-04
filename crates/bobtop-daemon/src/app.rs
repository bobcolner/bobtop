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
use bobtop_tui::widgets::{CornerStyle, ProcessSort};
use bobtop_tui::{LayoutPreset, Theme};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use crate::cli::{MAX_TICK_MS, MIN_TICK_MS, TICK_STEP_MS};

/// Maximum historical samples kept for graphing. 600 = 10 min at 1 Hz.
pub const HISTORY_CAP: usize = 600;
pub const CPU_HISTORY_CAP: usize = HISTORY_CAP;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlFlow {
    Continue,
    Quit,
}

/// Editable working copy of `Config`, plus a cursor identifying which
/// field is currently being edited. The fields here intentionally mirror
/// the on-disk Config — when the user hits Enter we Serialize this back
/// out via `Config::save`. On Esc we throw the snapshot away.
#[derive(Debug, Clone)]
pub struct OptionsState {
    pub cursor: usize,
    pub theme: String,
    pub tick_ms: u64,
    pub layout: crate::cli::LayoutChoice,
    pub corners: crate::cli::CornerChoice,
    pub no_ebpf: bool,
    pub no_pcap: bool,
    pub tty: bool,
    pub show_virtual_net: bool,
    /// Snapshot of the available theme names so cycling is just an index
    /// step rather than refetching the registry on every keystroke.
    pub themes: Vec<String>,
    /// Theme name that was active when the overlay opened. Used to revert
    /// the live-applied theme when the user cancels with Esc.
    pub original_theme: String,
}

impl OptionsState {
    pub const FIELD_COUNT: usize = 8;

    /// Cycle the field at `self.cursor` by `delta` (-1 = previous,
    /// +1 = next). For booleans, any non-zero delta toggles. For
    /// numerics (tick_ms), step is ±100 ms clamped to the global
    /// MIN/MAX bounds.
    pub fn cycle_field(&mut self, delta: i32) {
        use crate::cli::{CornerChoice, LayoutChoice};
        match self.cursor {
            0 => {
                // theme
                if self.themes.is_empty() {
                    return;
                }
                let cur = self
                    .themes
                    .iter()
                    .position(|n| n == &self.theme)
                    .unwrap_or(0) as i32;
                let n = self.themes.len() as i32;
                let next = ((cur + delta) % n + n) % n;
                self.theme = self.themes[next as usize].clone();
            }
            1 => {
                // tick_ms ±100 within bounds
                let step: i64 = 100 * delta as i64;
                let new = (self.tick_ms as i64 + step)
                    .clamp(crate::cli::MIN_TICK_MS as i64, crate::cli::MAX_TICK_MS as i64);
                self.tick_ms = new as u64;
            }
            2 => {
                self.layout = match self.layout {
                    LayoutChoice::Full => LayoutChoice::Minimal,
                    LayoutChoice::Minimal => LayoutChoice::Full,
                };
            }
            3 => {
                self.corners = match self.corners {
                    CornerChoice::Rounded => CornerChoice::Square,
                    CornerChoice::Square => CornerChoice::Rounded,
                };
            }
            4 => self.no_ebpf = !self.no_ebpf,
            5 => self.no_pcap = !self.no_pcap,
            6 => self.tty = !self.tty,
            7 => self.show_virtual_net = !self.show_virtual_net,
            _ => {}
        }
    }

    pub fn to_config(&self) -> crate::config::Config {
        crate::config::Config {
            theme: Some(self.theme.clone()),
            tick_ms: Some(self.tick_ms),
            layout: Some(self.layout),
            corners: Some(self.corners),
            no_ebpf: Some(self.no_ebpf),
            no_pcap: Some(self.no_pcap),
            tty: Some(self.tty),
            show_virtual_net: Some(self.show_virtual_net),
        }
    }
}

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
    pub sort: ProcessSort,
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
        sort: ProcessSort::Cpu,
        descending: true,
    },
    Preset {
        label: "all panels, sort by MEM",
        layout: LayoutPreset::Full,
        sort: ProcessSort::Mem,
        descending: true,
    },
    Preset {
        label: "all panels, sort by NET RX",
        layout: LayoutPreset::Full,
        sort: ProcessSort::NetRx,
        descending: true,
    },
    Preset {
        label: "minimal (CPU + processes only)",
        layout: LayoutPreset::Minimal,
        sort: ProcessSort::Cpu,
        descending: true,
    },
];

#[derive(Debug)]
pub struct App {
    pub theme: Theme,
    pub layout_preset: LayoutPreset,
    pub tty_graphs: bool,
    pub show_virtual_net: bool,
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
    /// Memory used-fraction history for the small mem time-series graph.
    pub mem_history: VecDeque<f64>,
    pub latest_cpu: Option<CpuSample>,
    pub latest_mem: Option<MemorySample>,
    pub latest_processes: Option<ProcessSample>,
    pub latest_network: Option<NetworkSample>,
    pub latest_disk: Option<DiskSample>,
    /// Per-tick aggregate of "real" interface bandwidth, suitable for the
    /// dual-trace network graph. `(rx_bytes_per_sec, tx_bytes_per_sec)`.
    pub net_history: VecDeque<(f64, f64)>,
    pub net_samples: Vec<ProcessNetSample>,
    pub net_tier: AttributorTier,

    /// Sorted-by-CPU process list, ready to render.
    pub processes_sorted: Vec<ProcessInfo>,

    pub selected_proc: usize,
    pub scroll_offset: usize,

    /// Active sort column for the process table — driven by `←` / `→`.
    pub proc_sort: ProcessSort,
    /// Sort direction for `proc_sort`. Toggled by `r`.
    pub proc_sort_descending: bool,

    /// `?` toggles the centered help overlay listing keybinds (B2).
    pub show_help: bool,

    /// `B` toggles the boxes overlay — show/hide individual panels (B5).
    pub show_boxes_overlay: bool,
    /// Cursor row inside the boxes overlay (index into `bobtop_core::Box::ALL`).
    pub boxes_overlay_cursor: usize,

    /// `f` opens the process filter input (B3b). While `filter_active`,
    /// keystrokes append to `filter_text`; rebuild_sorted hides processes
    /// whose name and cmdline both miss the substring (case-insensitive).
    /// `filter_text` persists across edit-mode toggles so the user can
    /// briefly switch focus and come back without re-typing.
    pub filter_active: bool,
    pub filter_text: String,

    /// `k` (SIGTERM) / `K` (SIGKILL) opens a confirm dialog targeting the
    /// currently-selected process (B3c). `Some(req)` = modal showing.
    /// Enter sends the signal via libc::kill; Esc cancels.
    pub pending_kill: Option<KillRequest>,
    /// Most recent kill outcome — drives a brief one-line toast in the
    /// status bar so the user gets feedback (success / errno).
    pub last_kill_msg: Option<String>,

    /// `Enter` opens a read-only detail modal for the selected pid (B3d).
    /// Lazy-populated from /proc on open; not refreshed every tick because
    /// most fields (cmdline, environ, fd count) don't churn at human speed.
    pub detail: Option<ProcessDetail>,

    /// `O` opens the options overlay (B11b). When `Some`, the user is
    /// editing a snapshot of the persisted Config; ←/→ cycles the field
    /// at the cursor, ↑/↓ moves cursor, Enter saves to disk + applies
    /// live, Esc closes without saving.
    pub options: Option<OptionsState>,
    /// Status line shown in the header strip after a save attempt
    /// (success path or error). Cleared on next user action.
    pub last_options_msg: Option<String>,

    /// Process grouping mode (the "intelligent clustering" feature).
    /// `g` cycles flat → exec → cgroup → tree → flat.
    pub group_mode: crate::group::GroupMode,
    /// Expand/collapse state. For grouped modes (exec/cgroup), entries
    /// are header keys that are EXPANDED (default = collapsed). For tree
    /// mode, entries are pids stringified that are COLLAPSED (default =
    /// expanded). The two semantics share one set because they never
    /// coexist — switching modes resets the meaning.
    pub expanded: std::collections::HashSet<String>,

    /// Set by `apply_event` and `handle_input` whenever something
    /// observable to the renderer has changed. Cleared by `take_dirty`
    /// after the render loop calls `terminal.draw()`. Lets us render
    /// on change instead of on a 60Hz heartbeat — see `tui::run`.
    dirty: bool,
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
            corner_style: CornerStyle::default(),
            boxes,
            started_at: Instant::now(),
            tick_ms,
            cpu_history: VecDeque::with_capacity(CPU_HISTORY_CAP),
            mem_history: VecDeque::with_capacity(HISTORY_CAP),
            latest_cpu: None,
            latest_mem: None,
            latest_processes: None,
            latest_network: None,
            latest_disk: None,
            net_history: VecDeque::with_capacity(HISTORY_CAP),
            net_samples: Vec::new(),
            net_tier: AttributorTier::Unavailable,
            processes_sorted: Vec::new(),
            selected_proc: 0,
            scroll_offset: 0,
            proc_sort: ProcessSort::Cpu,
            proc_sort_descending: true,
            show_help: false,
            show_boxes_overlay: false,
            boxes_overlay_cursor: 0,
            filter_active: false,
            filter_text: String::new(),
            pending_kill: None,
            last_kill_msg: None,
            detail: None,
            options: None,
            last_options_msg: None,
            group_mode: crate::group::GroupMode::Flat,
            expanded: std::collections::HashSet::new(),
            // Start dirty so the very first frame paints something rather
            // than a blank alt-screen until the first sample lands.
            dirty: true,
        }
    }

    /// Atomically read-and-clear the dirty flag. The render loop calls this
    /// to decide whether to skip `terminal.draw()` this iteration.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::replace(&mut self.dirty, false)
    }

    /// Mark state as observably changed. Anything that mutates a field
    /// `ui::draw` reads should call this (apply_event, handle_input
    /// branches that change selection/sort/layout/tick).
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn apply_event(&mut self, ev: MetricEvent) {
        self.dirty = true;
        match ev {
            MetricEvent::Cpu(s) => {
                if self.cpu_history.len() == CPU_HISTORY_CAP {
                    self.cpu_history.pop_front();
                }
                self.cpu_history.push_back(s.aggregate_utilization as f64);
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
                self.latest_network = Some(s);
            }
            MetricEvent::Disk(s) => self.latest_disk = Some(s),
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
        self.dirty = true;
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
        if !self.filter_text.is_empty() {
            let needle = self.filter_text.to_lowercase();
            joined.retain(|p| {
                p.name.to_lowercase().contains(&needle)
                    || p.cmdline.to_lowercase().contains(&needle)
            });
        }
        sort_processes(&mut joined, self.proc_sort, self.proc_sort_descending);
        if !joined.is_empty() && self.selected_proc >= joined.len() {
            self.selected_proc = joined.len() - 1;
        } else if joined.is_empty() {
            self.selected_proc = 0;
        }
        self.processes_sorted = joined;
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
        let cycle = ProcessSort::cycle();
        let cur_idx = cycle.iter().position(|&s| s == self.proc_sort).unwrap_or(0) as i32;
        let len = cycle.len() as i32;
        let new_idx = ((cur_idx + delta) % len + len) % len;
        self.set_sort(cycle[new_idx as usize]);
    }

    /// Set the sort column directly (used by the `n`/`m`/`p`/`c` direct-sort
    /// keybinds). Re-sorts in place — the join state hasn't changed, only
    /// the comparator has.
    pub fn set_sort(&mut self, sort: ProcessSort) {
        self.proc_sort = sort;
        sort_processes(&mut self.processes_sorted, self.proc_sort, self.proc_sort_descending);
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
    }

    /// Open the options overlay (B11b) with a snapshot of the current
    /// settings. Theme list is queried from the bobtop-tui builtin
    /// registry once at open time.
    pub fn open_options(&mut self) {
        let themes: Vec<String> =
            bobtop_tui::builtin_names().map(|s| s.to_string()).collect();
        let current_theme = self.theme.name.clone();
        self.options = Some(OptionsState {
            cursor: 0,
            theme: current_theme.clone(),
            tick_ms: self.tick_ms(),
            layout: match self.layout_preset {
                LayoutPreset::Full => crate::cli::LayoutChoice::Full,
                LayoutPreset::Minimal => crate::cli::LayoutChoice::Minimal,
            },
            corners: match self.corner_style {
                CornerStyle::Rounded => crate::cli::CornerChoice::Rounded,
                CornerStyle::Square => crate::cli::CornerChoice::Square,
            },
            // Sticky bools — we don't have these on App today, default to
            // false. After first save they'll round-trip correctly.
            no_ebpf: false,
            no_pcap: false,
            tty: self.tty_graphs,
            show_virtual_net: self.show_virtual_net,
            themes,
            original_theme: current_theme,
        });
    }

    /// Apply the staged theme to the live App immediately, so the user
    /// sees the colors change as they cycle through themes in the
    /// options overlay. Other staged options stay snapshot-only — they
    /// only take effect on Enter (save_options).
    pub fn preview_theme(&mut self) {
        let Some(opts) = &self.options else { return };
        // Use load_theme so the search path + parse + name-tracking are
        // handled in one place. Earlier code tried to call find_source +
        // from_source manually and got the tuple destructure backwards
        // (find_source returns (source, origin), not (name, source)),
        // which assigned the raw source text to Theme.name.
        self.theme = bobtop_tui::load_theme(&opts.theme);
    }

    /// Discard a pending options edit and restore the originally-active
    /// theme. Called when the user dismisses the overlay with Esc.
    pub fn cancel_options(&mut self) {
        let Some(opts) = self.options.take() else { return };
        self.theme = bobtop_tui::load_theme(&opts.original_theme);
    }

    /// Apply the staged options to the live App and persist to disk. The
    /// returned message is for the header toast — Ok = "saved to PATH"
    /// or "save failed: …".
    pub fn save_options(&mut self) -> String {
        let Some(opts) = self.options.take() else {
            return "no options to save".into();
        };
        // Live-apply the changes that mutate render state immediately so
        // the user sees the effect before we even disk-write. Theme,
        // corners, tick_ms, layout, tty_graphs, show_virtual_net.
        self.theme = bobtop_tui::load_theme(&opts.theme);
        self.corner_style = opts.corners.into();
        self.tick_ms.store(opts.tick_ms, Ordering::Relaxed);
        self.set_layout(match opts.layout {
            crate::cli::LayoutChoice::Full => LayoutPreset::Full,
            crate::cli::LayoutChoice::Minimal => LayoutPreset::Minimal,
        });
        self.tty_graphs = opts.tty;
        self.show_virtual_net = opts.show_virtual_net;
        // Then persist.
        match opts.to_config().save() {
            Ok(p) => format!("saved {}", p.display()),
            Err(e) => format!("save failed: {e}"),
        }
    }

    /// Materialize the current display rows. Cheap to call (clones
    /// ProcessInfo entries today; can be cached on App later if it
    /// shows up in the bench). The renderer and the keybind handlers
    /// both use this so they always agree on what's visible.
    pub fn display_rows(&self) -> Vec<crate::group::DisplayRow> {
        crate::group::build_display(
            &self.processes_sorted,
            self.group_mode,
            &self.expanded,
            self.proc_sort,
            self.proc_sort_descending,
        )
    }

    /// Cycle the grouping mode (`g` keybind). Resets `expanded` because
    /// the key namespace differs between modes (header keys for
    /// grouped views, pids for tree view).
    pub fn cycle_group_mode(&mut self) {
        self.group_mode = self.group_mode.next();
        self.expanded.clear();
        self.selected_proc = 0;
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
            (_, crate::group::DisplayRow::Header(h)) => h.key.clone(),
            (crate::group::GroupMode::ByParent, crate::group::DisplayRow::Process(p)) => {
                p.info.pid.to_string()
            }
            (_, crate::group::DisplayRow::Process(_)) => return,
        };
        if self.expanded.contains(&key) {
            self.expanded.remove(&key);
        } else {
            self.expanded.insert(key);
        }
    }

    /// Stage a kill request for the currently-selected process. No signal
    /// is sent yet — the user has to confirm via Enter in the modal. If
    /// the selection isn't on a process row (e.g. on a group header)
    /// this is a silent no-op.
    pub fn request_kill(&mut self, signal: KillSignal) {
        let rows = self.display_rows();
        let Some(row) = rows.get(self.selected_proc) else { return };
        let crate::group::DisplayRow::Process(p) = row else { return };
        self.pending_kill = Some(KillRequest {
            pid: p.info.pid,
            name: p.info.name.clone(),
            signal,
        });
    }

    pub fn toggle_sort_direction(&mut self) {
        self.proc_sort_descending = !self.proc_sort_descending;
        sort_processes(&mut self.processes_sorted, self.proc_sort, self.proc_sort_descending);
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
            self.dirty = true;
        }
        flow
    }

    fn handle_key(&mut self, k: KeyEvent) -> ControlFlow {
        // visible_rows = total render-row count (group headers + visible
        // processes). Bounding selection on this so cursor stops at the
        // bottom of what's actually painted, not the underlying process
        // count which excludes headers and includes hidden subtree members.
        let visible_rows = self.display_rows().len();

        // Options overlay (B11b) — modal. ↑/↓ moves cursor, ←/→ cycles
        // the value at the cursor, Enter saves to disk + applies live,
        // Esc closes without saving. Routed before everything else so
        // the rest of the keybinds can't fire while the user is editing.
        if self.options.is_some() {
            match k.code {
                KeyCode::Esc => {
                    // Revert any live previews (currently just theme).
                    self.cancel_options();
                    return ControlFlow::Continue;
                }
                KeyCode::Enter => {
                    let msg = self.save_options();
                    self.last_options_msg = Some(msg);
                    return ControlFlow::Continue;
                }
                KeyCode::Up => {
                    if let Some(o) = self.options.as_mut() {
                        if o.cursor > 0 {
                            o.cursor -= 1;
                        }
                    }
                    return ControlFlow::Continue;
                }
                KeyCode::Down => {
                    if let Some(o) = self.options.as_mut() {
                        if o.cursor + 1 < OptionsState::FIELD_COUNT {
                            o.cursor += 1;
                        }
                    }
                    return ControlFlow::Continue;
                }
                KeyCode::Left => {
                    let on_theme = self
                        .options
                        .as_mut()
                        .map(|o| {
                            o.cycle_field(-1);
                            o.cursor == 0
                        })
                        .unwrap_or(false);
                    if on_theme {
                        self.preview_theme();
                    }
                    return ControlFlow::Continue;
                }
                KeyCode::Right | KeyCode::Char(' ') => {
                    let on_theme = self
                        .options
                        .as_mut()
                        .map(|o| {
                            o.cycle_field(1);
                            o.cursor == 0
                        })
                        .unwrap_or(false);
                    if on_theme {
                        self.preview_theme();
                    }
                    return ControlFlow::Continue;
                }
                _ => return ControlFlow::Continue,
            }
        }

        // Detail modal (B3d) — Esc closes. While open the rest of the
        // UI is read-only so we don't lose the user's place if they
        // accidentally press a sort key.
        if self.detail.is_some() {
            match k.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                    self.detail = None;
                    return ControlFlow::Continue;
                }
                _ => return ControlFlow::Continue,
            }
        }

        // Kill confirm dialog (B3c) — modal. Enter sends the signal,
        // Esc/n cancels. We route this first so the user can't
        // accidentally take other action with the modal up.
        if let Some(req) = self.pending_kill.clone() {
            match k.code {
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let outcome = send_signal(req.pid, req.signal);
                    self.last_kill_msg = Some(outcome);
                    self.pending_kill = None;
                    return ControlFlow::Continue;
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.pending_kill = None;
                    return ControlFlow::Continue;
                }
                _ => return ControlFlow::Continue,
            }
        }

        // Filter input (B3b) — when active, every keystroke goes into the
        // filter text. Esc cancels the filter entirely; Enter commits and
        // exits edit mode (filter stays applied). This must come BEFORE
        // any other modal so e.g. `q` / `B` while typing don't quit/open menus.
        if self.filter_active {
            match k.code {
                KeyCode::Esc => {
                    self.filter_active = false;
                    self.filter_text.clear();
                    self.rebuild_sorted();
                    return ControlFlow::Continue;
                }
                KeyCode::Enter => {
                    self.filter_active = false;
                    return ControlFlow::Continue;
                }
                KeyCode::Backspace => {
                    self.filter_text.pop();
                    self.rebuild_sorted();
                    return ControlFlow::Continue;
                }
                KeyCode::Char(c) => {
                    self.filter_text.push(c);
                    self.rebuild_sorted();
                    return ControlFlow::Continue;
                }
                _ => return ControlFlow::Continue,
            }
        }

        // When the help overlay is open, only `?` / Esc / `q` are routed —
        // everything else is swallowed so the user doesn't accidentally
        // mutate state while reading the keybinds.
        if self.show_help {
            match k.code {
                KeyCode::Char('?') | KeyCode::Esc => {
                    self.show_help = false;
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
        if self.show_boxes_overlay {
            use bobtop_core::Box as BoxKind;
            let n = BoxKind::ALL.len();
            match k.code {
                KeyCode::Char('B') | KeyCode::Char('b') | KeyCode::Esc => {
                    self.show_boxes_overlay = false;
                    return ControlFlow::Continue;
                }
                KeyCode::Up => {
                    if self.boxes_overlay_cursor > 0 {
                        self.boxes_overlay_cursor -= 1;
                    }
                    return ControlFlow::Continue;
                }
                KeyCode::Down => {
                    if self.boxes_overlay_cursor + 1 < n {
                        self.boxes_overlay_cursor += 1;
                    }
                    return ControlFlow::Continue;
                }
                KeyCode::Char(' ') | KeyCode::Enter => {
                    let b = BoxKind::ALL[self.boxes_overlay_cursor];
                    let cur = self.boxes.is_enabled(b);
                    self.boxes.set(b, !cur);
                    return ControlFlow::Continue;
                }
                _ => {} // fall through to normal handling
            }
        }
        match k.code {
            KeyCode::Char('?') => {
                self.show_help = true;
                ControlFlow::Continue
            }
            KeyCode::Char('B') => {
                // Capital B opens the boxes overlay. Lowercase `b` is left
                // free for future use (btop's `b` cycles network interfaces).
                self.show_boxes_overlay = true;
                self.boxes_overlay_cursor = 0;
                ControlFlow::Continue
            }
            KeyCode::Char('f') => {
                // Open the filter input (B3b). If a filter is already applied
                // (text non-empty, but not in edit mode), `f` re-enters edit
                // mode so the user can refine it.
                self.filter_active = true;
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
                let rows = self.display_rows();
                if let Some(row) = rows.get(self.selected_proc) {
                    match row {
                        crate::group::DisplayRow::Header(_) => self.toggle_selected_expand(),
                        crate::group::DisplayRow::Process(p) => {
                            self.detail = Some(ProcessDetail::read(p.info.pid, &p.info.name));
                        }
                    }
                }
                ControlFlow::Continue
            }
            KeyCode::Char('g') => {
                self.cycle_group_mode();
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
                self.set_sort(ProcessSort::Pid);
                ControlFlow::Continue
            }
            KeyCode::Char('n') => {
                self.set_sort(ProcessSort::Name);
                ControlFlow::Continue
            }
            KeyCode::Char('m') => {
                self.set_sort(ProcessSort::Mem);
                ControlFlow::Continue
            }
            KeyCode::Char('c') => {
                self.set_sort(ProcessSort::Cpu);
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
                ControlFlow::Continue
            }
            KeyCode::Down => {
                if self.selected_proc + 1 < visible_rows {
                    self.selected_proc += 1;
                }
                ControlFlow::Continue
            }
            KeyCode::PageUp => {
                self.selected_proc = self.selected_proc.saturating_sub(10);
                if self.selected_proc < self.scroll_offset {
                    self.scroll_offset = self.selected_proc;
                }
                ControlFlow::Continue
            }
            KeyCode::PageDown => {
                self.selected_proc = (self.selected_proc + 10).min(visible_rows.saturating_sub(1));
                ControlFlow::Continue
            }
            KeyCode::Home => {
                self.selected_proc = 0;
                self.scroll_offset = 0;
                ControlFlow::Continue
            }
            KeyCode::End => {
                self.selected_proc = visible_rows.saturating_sub(1);
                ControlFlow::Continue
            }
            _ => ControlFlow::Continue,
        }
    }
}

fn sort_processes(rows: &mut [ProcessInfo], sort: ProcessSort, descending: bool) {
    use std::cmp::Ordering;
    rows.sort_by(|a, b| {
        let ord = match sort {
            ProcessSort::Pid => a.pid.cmp(&b.pid),
            ProcessSort::Name => a.name.cmp(&b.name),
            ProcessSort::User => a.user.cmp(&b.user),
            ProcessSort::Threads => a.threads.cmp(&b.threads),
            ProcessSort::Mem => a.mem_rss_bytes.cmp(&b.mem_rss_bytes),
            ProcessSort::Cpu => a
                .cpu_fraction
                .partial_cmp(&b.cpu_fraction)
                .unwrap_or(Ordering::Equal),
            ProcessSort::NetRx => a
                .net_rx_bytes_per_sec
                .unwrap_or(0.0)
                .partial_cmp(&b.net_rx_bytes_per_sec.unwrap_or(0.0))
                .unwrap_or(Ordering::Equal),
            ProcessSort::NetTx => a
                .net_tx_bytes_per_sec
                .unwrap_or(0.0)
                .partial_cmp(&b.net_tx_bytes_per_sec.unwrap_or(0.0))
                .unwrap_or(Ordering::Equal),
            ProcessSort::DiskRead => a
                .disk_read_bytes_per_sec
                .unwrap_or(0.0)
                .partial_cmp(&b.disk_read_bytes_per_sec.unwrap_or(0.0))
                .unwrap_or(Ordering::Equal),
            ProcessSort::DiskWrite => a
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
        assert!(app.show_boxes_overlay);
        assert_eq!(app.boxes_overlay_cursor, 0); // first row = CPU

        // Space toggles the cursor's box (CPU at index 0).
        let space = || Event::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        app.handle_input(space());
        assert!(!app.boxes.is_enabled(BoxKind::Cpu));

        // Down moves cursor to MEM, space disables it too.
        let down = Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_input(down);
        assert_eq!(app.boxes_overlay_cursor, 1);
        app.handle_input(space());
        assert!(!app.boxes.is_enabled(BoxKind::Memory));

        // Esc closes; CPU/MEM stay disabled (overlay state was just visibility).
        let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_input(esc);
        assert!(!app.show_boxes_overlay);
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
        assert_eq!(app.proc_sort, ProcessSort::Mem);

        // Preset 4 (key '4') = Minimal — should disable MEM/DISK/NET boxes.
        let four = Event::Key(KeyEvent::new(KeyCode::Char('4'), KeyModifiers::NONE));
        app.handle_input(four);
        assert_eq!(app.layout_preset, LayoutPreset::Minimal);
        assert_eq!(app.proc_sort, ProcessSort::Cpu);
        assert!(!app.boxes.is_enabled(BoxKind::Memory));
        assert!(!app.boxes.is_enabled(BoxKind::Network));
        assert!(app.boxes.is_enabled(BoxKind::Cpu));
        assert!(app.boxes.is_enabled(BoxKind::Process));

        // After B3a, `m` is the direct sort-by-mem shortcut, not the
        // preset-4 alias. Verify it sorts (and does NOT change layout).
        app.apply_preset(&DEFAULT_PRESETS[0]); // back to slot 1 (sort: Cpu)
        assert_eq!(app.layout_preset, LayoutPreset::Full);
        assert_eq!(app.proc_sort, ProcessSort::Cpu);
        let m = Event::Key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        app.handle_input(m);
        assert_eq!(app.proc_sort, ProcessSort::Mem);
        assert_eq!(app.layout_preset, LayoutPreset::Full); // not changed
    }

    #[test]
    fn options_theme_cycle_live_previews_and_esc_reverts() {
        let mut app = App::new(theme(), LayoutPreset::Full, Arc::new(AtomicU64::new(500)), false, false);
        let original = app.theme.name.clone();

        // Open options. Cursor starts on theme (field 0).
        let press = |c: char| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        app.handle_input(press('O'));
        assert!(app.options.is_some());
        assert_eq!(app.options.as_ref().unwrap().cursor, 0);

        // Press → to advance theme. Both the staged opts.theme and the
        // live app.theme must change in lockstep.
        let right = Event::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        app.handle_input(right);
        let after_cycle = app.theme.name.clone();
        assert_ne!(
            after_cycle, original,
            "live theme should change when cycling theme field"
        );
        assert_eq!(app.options.as_ref().unwrap().theme, after_cycle);

        // Esc closes the modal and reverts the theme.
        let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_input(esc);
        assert!(app.options.is_none());
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
        let req = app.pending_kill.as_ref().expect("kill staged");
        assert_eq!(req.pid, 4242);
        assert_eq!(req.signal, KillSignal::Term);

        // Esc cancels — no signal sent (no last_kill_msg), pending cleared.
        let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_input(esc);
        assert!(app.pending_kill.is_none());
        assert!(app.last_kill_msg.is_none());

        // Press `K` (capital) → SIGKILL staged.
        app.handle_input(press('K'));
        let req = app.pending_kill.as_ref().expect("kill staged");
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
        assert!(app.filter_active);
        for c in "chrom".chars() {
            app.handle_input(press(c));
        }
        assert_eq!(app.filter_text, "chrom");
        assert_eq!(app.processes_sorted.len(), 2);
        assert!(app.processes_sorted.iter().all(|p| p.name.contains("chrom")));

        // Esc clears the filter and exits edit mode.
        let esc = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.handle_input(esc);
        assert!(!app.filter_active);
        assert_eq!(app.filter_text, "");
        assert_eq!(app.processes_sorted.len(), 3);
    }

    #[test]
    fn direct_sort_keys_set_sort_columns() {
        let mut app = App::new(theme(), LayoutPreset::Full, Arc::new(AtomicU64::new(500)), false, false);
        let press = |c: char| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        app.handle_input(press('p'));
        assert_eq!(app.proc_sort, ProcessSort::Pid);
        app.handle_input(press('n'));
        assert_eq!(app.proc_sort, ProcessSort::Name);
        app.handle_input(press('m'));
        assert_eq!(app.proc_sort, ProcessSort::Mem);
        app.handle_input(press('c'));
        assert_eq!(app.proc_sort, ProcessSort::Cpu);
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
