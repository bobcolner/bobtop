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
use bobtop_tui::widgets::ProcessSort;
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

#[derive(Debug)]
pub struct App {
    pub theme: Theme,
    pub layout_preset: LayoutPreset,
    pub tty_graphs: bool,
    pub show_virtual_net: bool,

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
        sort_processes(&mut joined, self.proc_sort, self.proc_sort_descending);
        if !joined.is_empty() && self.selected_proc >= joined.len() {
            self.selected_proc = joined.len() - 1;
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
        self.proc_sort = cycle[new_idx as usize];
        // Re-sort the already-joined processes_sorted in place rather than
        // calling rebuild_sorted (avoids re-cloning + re-joining). The net
        // join state is the same; only the sort key changed.
        sort_processes(&mut self.processes_sorted, self.proc_sort, self.proc_sort_descending);
    }

    /// Switch the layout preset and mirror the new visible-box set into
    /// the shared `BoxesEnabled` so collectors for hidden boxes stop
    /// doing work on their next wake.
    pub fn set_layout(&mut self, preset: LayoutPreset) {
        self.layout_preset = preset;
        self.boxes.replace(preset.enabled_boxes());
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
        let visible_rows = self.processes_sorted.len();
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => ControlFlow::Quit,
            KeyCode::Char('1') => {
                self.set_layout(LayoutPreset::Full);
                ControlFlow::Continue
            }
            KeyCode::Char('m') => {
                self.set_layout(LayoutPreset::Minimal);
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
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected_proc > 0 {
                    self.selected_proc -= 1;
                    if self.selected_proc < self.scroll_offset {
                        self.scroll_offset = self.selected_proc;
                    }
                }
                ControlFlow::Continue
            }
            KeyCode::Down | KeyCode::Char('j') => {
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
