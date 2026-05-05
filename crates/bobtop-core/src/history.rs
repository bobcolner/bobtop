//! Bounded in-memory history of host metrics + per-pid top-N at three
//! resolution tiers. Backs retrospective agent queries ("what was eating
//! CPU 90 seconds ago?") without ever touching disk.
//!
//! ## Memory budget (worst case, fully populated)
//!
//! - 3 tiers × 60 entries × `HostMetrics` (~64 B) ≈ 12 KB
//! - 3 tiers × 60 entries × `TopProcs` (~1 KB each, 32 small refs) ≈ 180 KB
//! - **Total ≈ 200 KB**, regardless of process churn or runtime.
//!
//! ## CPU budget
//!
//! One wake per second. Per wake: read latest `HostSample`, scan the process
//! list four times (one per metric of interest) selecting top-N via partial
//! ordering, push into ring buffers. Sub-millisecond on a 500-pid host.

use std::collections::VecDeque;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::time::{interval, MissedTickBehavior};

use crate::sample::ProcessInfo;
use crate::store::{HostSample, SampleStore};

/// How many process refs to retain per metric per tick. Small by design —
/// agents asking "who was responsible" rarely care past the top handful, and
/// every additional slot is multiplied by `TIER_COUNT × CAPACITY`.
pub const TOP_N: usize = 8;

/// Entries per tier ring. 60 × 1s = 1 min; 60 × 5s = 5 min; 60 × 30s = 30 min.
pub const TIER_CAPACITY: usize = 60;

/// Compact, all-`Copy` host-level snapshot. One entry per ring slot; the
/// fixed size keeps the memory budget predictable independent of pid count.
#[derive(Debug, Clone, Copy, Default)]
pub struct HostMetrics {
    pub ts_unix_ms: u64,
    pub cpu_pct: f32,
    pub mem_used_bytes: u64,
    pub mem_total_bytes: u64,
    pub net_rx_bps: u64,
    pub net_tx_bps: u64,
    pub disk_r_bps: u64,
    pub disk_w_bps: u64,
    pub n_procs: u32,
}

/// One ranked process entry within a `TopProcs` row. Carries enough context
/// for an agent to answer "what was it" without joining back to the live
/// process table.
#[derive(Debug, Clone)]
pub struct ProcRef {
    pub pid: u32,
    pub name: String,
    /// The metric value (e.g. cpu fraction, mem bytes, net bps) at sample time.
    pub value: f64,
}

/// Top-N processes by each tracked metric, captured at one tick boundary.
#[derive(Debug, Clone, Default)]
pub struct TopProcs {
    pub ts_unix_ms: u64,
    pub by_cpu: Vec<ProcRef>,
    pub by_mem: Vec<ProcRef>,
    pub by_net_tx: Vec<ProcRef>,
    pub by_net_rx: Vec<ProcRef>,
}

/// Three-tier ring of host metrics + per-pid top-N. Single-writer (the
/// history task) / multi-reader (agent queries) — `RwLock` is the right
/// granularity since reads are rare and writes are 1 Hz.
#[derive(Debug, Default)]
pub struct HistoryRing {
    pub host_1s: VecDeque<HostMetrics>,
    pub host_5s: VecDeque<HostMetrics>,
    pub host_30s: VecDeque<HostMetrics>,
    pub procs_1s: VecDeque<TopProcs>,
    pub procs_5s: VecDeque<TopProcs>,
    pub procs_30s: VecDeque<TopProcs>,
    tick_count: u64,
}

impl HistoryRing {
    fn push(&mut self, host: HostMetrics, procs: TopProcs) {
        push_capped(&mut self.host_1s, host);
        push_capped(&mut self.procs_1s, procs.clone());
        self.tick_count = self.tick_count.wrapping_add(1);
        // Coarser tiers are sampled (not aggregated) — the entry that lands
        // in the 5s ring is the 1s entry that happened to fall on a 5s
        // boundary. Cheap and "good enough" for retrospective queries.
        if self.tick_count.is_multiple_of(5) {
            push_capped(&mut self.host_5s, host);
            push_capped(&mut self.procs_5s, procs.clone());
        }
        if self.tick_count.is_multiple_of(30) {
            push_capped(&mut self.host_30s, host);
            push_capped(&mut self.procs_30s, procs);
        }
    }

    /// Choose the smallest tier that covers `window`, returning its host
    /// entries. Older entries are ordered first.
    pub fn host_window(&self, window: Duration) -> Vec<HostMetrics> {
        let secs = window.as_secs();
        let ring = if secs <= 60 {
            &self.host_1s
        } else if secs <= 300 {
            &self.host_5s
        } else {
            &self.host_30s
        };
        ring.iter().copied().collect()
    }

    /// Top-N for a metric at a point in the past, expressed as an offset
    /// from "now." Picks the closest sample in the smallest tier that spans
    /// the offset. Returns `None` when the offset is older than the longest
    /// tier or no data has been captured yet.
    pub fn procs_at(&self, offset: Duration, by: Metric) -> Option<Vec<ProcRef>> {
        let secs = offset.as_secs();
        let ring = if secs <= 60 {
            &self.procs_1s
        } else if secs <= 300 {
            &self.procs_5s
        } else if secs <= 1800 {
            &self.procs_30s
        } else {
            return None;
        };
        // Most recent at the back; offset 0 == latest.
        let idx_from_back = match secs {
            0..=60 => secs as usize,
            61..=300 => (secs / 5) as usize,
            301..=1800 => (secs / 30) as usize,
            _ => return None,
        };
        if idx_from_back >= ring.len() {
            return None;
        }
        let entry = &ring[ring.len() - 1 - idx_from_back];
        Some(match by {
            Metric::Cpu => entry.by_cpu.clone(),
            Metric::Mem => entry.by_mem.clone(),
            Metric::NetTx => entry.by_net_tx.clone(),
            Metric::NetRx => entry.by_net_rx.clone(),
        })
    }
}

/// Ranked metrics the history tracks. Adding a new variant requires
/// extending `TopProcs` and `top_n_by`; intentionally kept small.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    Cpu,
    Mem,
    NetTx,
    NetRx,
}

/// Aggregate statistics over a window. All values are in the metric's
/// native unit (cpu pct, bytes, bps).
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowStats {
    pub avg: f64,
    pub peak: f64,
    pub p95: f64,
    pub samples: usize,
}

/// Result of a `peak` query: the highest value seen, when it occurred
/// (offset from "now"), and the top-N pids responsible at that moment.
#[derive(Debug, Clone)]
pub struct PeakResult {
    pub value: f64,
    pub offset_secs: u64,
    pub responsible: Vec<ProcRef>,
}

/// Cheap-to-clone handle to the history ring. Owns the writer task indirectly
/// (the task lives as long as the underlying `SampleStore` exists).
#[derive(Debug, Clone)]
pub struct History {
    inner: Arc<RwLock<HistoryRing>>,
}

impl History {
    /// Window stats for a host-level metric. Picks the smallest tier that
    /// covers `window` (matches `host_window`'s behavior).
    pub fn host_stats(&self, window: Duration, by: Metric) -> Option<WindowStats> {
        let entries = self.read(|r| r.host_window(window));
        if entries.is_empty() {
            return None;
        }
        let mut values: Vec<f64> = entries.iter().map(|h| host_metric(h, by)).collect();
        let mut sum = 0.0;
        let mut peak = f64::MIN;
        for &v in &values {
            sum += v;
            if v > peak {
                peak = v;
            }
        }
        let avg = sum / values.len() as f64;
        // p95 via in-place sort. 60 elements is trivial; no need for nth_element.
        values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let idx = ((values.len() as f64 - 1.0) * 0.95).round() as usize;
        let p95 = values.get(idx).copied().unwrap_or(peak);
        Some(WindowStats {
            avg,
            peak,
            p95,
            samples: entries.len(),
        })
    }

    /// Locate the peak of `by` within `window` and return the responsible
    /// processes captured at that tick. Returns `None` when no samples
    /// have been recorded yet.
    pub fn peak(&self, window: Duration, by: Metric) -> Option<PeakResult> {
        self.read(|r| {
            let (host_ring, procs_ring, step_secs) = ring_for_window(r, window);
            if host_ring.is_empty() {
                return None;
            }
            let mut peak_v = f64::MIN;
            let mut peak_idx = 0usize;
            for (i, h) in host_ring.iter().enumerate() {
                let v = host_metric(h, by);
                if v > peak_v {
                    peak_v = v;
                    peak_idx = i;
                }
            }
            let offset_idx = host_ring.len() - 1 - peak_idx;
            let offset_secs = (offset_idx as u64) * step_secs;
            // Pull the matching `TopProcs` entry from the same tier — same
            // index, since both rings advance together.
            let responsible = procs_ring
                .get(peak_idx)
                .map(|tp| match by {
                    Metric::Cpu => tp.by_cpu.clone(),
                    Metric::Mem => tp.by_mem.clone(),
                    Metric::NetTx => tp.by_net_tx.clone(),
                    Metric::NetRx => tp.by_net_rx.clone(),
                })
                .unwrap_or_default();
            Some(PeakResult {
                value: peak_v,
                offset_secs,
                responsible,
            })
        })
    }

    /// Spawn a 1 Hz writer task that pulls from the store, extracts top-N
    /// per metric, and pushes into the ring. Cheap to call once at startup.
    pub fn spawn(store: SampleStore) -> Self {
        let inner = Arc::new(RwLock::new(HistoryRing::default()));
        let writer_inner = Arc::clone(&inner);
        tokio::spawn(async move {
            let mut tick = interval(Duration::from_secs(1));
            tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                let snap = store.latest();
                let host = snap_to_host_metrics(&snap);
                let procs = snap_to_top_procs(&snap);
                if let Ok(mut g) = writer_inner.write() {
                    g.push(host, procs);
                }
            }
        });
        Self { inner }
    }

    /// One-shot read snapshot via the closure. Avoids exposing the lock.
    pub fn read<R>(&self, f: impl FnOnce(&HistoryRing) -> R) -> R {
        let g = self.inner.read().expect("history lock poisoned");
        f(&g)
    }
}

/// Pick the smallest tier that covers `window`, returning its host ring,
/// matching procs ring, and the per-step duration in seconds.
fn ring_for_window(
    r: &HistoryRing,
    window: Duration,
) -> (&VecDeque<HostMetrics>, &VecDeque<TopProcs>, u64) {
    let secs = window.as_secs();
    if secs <= 60 {
        (&r.host_1s, &r.procs_1s, 1)
    } else if secs <= 300 {
        (&r.host_5s, &r.procs_5s, 5)
    } else {
        (&r.host_30s, &r.procs_30s, 30)
    }
}

fn host_metric(h: &HostMetrics, m: Metric) -> f64 {
    match m {
        Metric::Cpu => h.cpu_pct as f64,
        Metric::Mem => h.mem_used_bytes as f64,
        Metric::NetTx => h.net_tx_bps as f64,
        Metric::NetRx => h.net_rx_bps as f64,
    }
}

fn push_capped<T>(q: &mut VecDeque<T>, v: T) {
    if q.len() == TIER_CAPACITY {
        q.pop_front();
    }
    q.push_back(v);
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn snap_to_host_metrics(snap: &HostSample) -> HostMetrics {
    let cpu_pct = snap
        .cpu
        .as_ref()
        .map(|s| s.aggregate_utilization * 100.0)
        .unwrap_or(0.0);
    let (mem_used, mem_total) = snap
        .memory
        .as_ref()
        .map(|m| (m.used_bytes, m.total_bytes))
        .unwrap_or((0, 0));
    let (net_rx, net_tx) = snap
        .network
        .as_ref()
        .map(|n| {
            let rx: f64 = n.interfaces.iter().map(|i| i.rx_bytes_per_sec).sum();
            let tx: f64 = n.interfaces.iter().map(|i| i.tx_bytes_per_sec).sum();
            (rx as u64, tx as u64)
        })
        .unwrap_or((0, 0));
    let (disk_r, disk_w) = snap
        .disk
        .as_ref()
        .map(|d| {
            let r: f64 = d.devices.iter().map(|x| x.read_bytes_per_sec).sum();
            let w: f64 = d.devices.iter().map(|x| x.write_bytes_per_sec).sum();
            (r as u64, w as u64)
        })
        .unwrap_or((0, 0));
    let n_procs = snap
        .processes
        .as_ref()
        .map(|p| p.processes.len() as u32)
        .unwrap_or(0);
    HostMetrics {
        ts_unix_ms: now_unix_ms(),
        cpu_pct,
        mem_used_bytes: mem_used,
        mem_total_bytes: mem_total,
        net_rx_bps: net_rx,
        net_tx_bps: net_tx,
        disk_r_bps: disk_r,
        disk_w_bps: disk_w,
        n_procs,
    }
}

fn snap_to_top_procs(snap: &HostSample) -> TopProcs {
    let mut t = TopProcs {
        ts_unix_ms: now_unix_ms(),
        ..Default::default()
    };
    let Some(ps) = snap.processes.as_ref() else {
        return t;
    };
    t.by_cpu = top_n_by(&ps.processes, |p| p.cpu_fraction as f64);
    t.by_mem = top_n_by(&ps.processes, |p| p.mem_rss_bytes as f64);
    t.by_net_tx = top_n_by(&ps.processes, |p| p.net_tx_bytes_per_sec.unwrap_or(0.0));
    t.by_net_rx = top_n_by(&ps.processes, |p| p.net_rx_bytes_per_sec.unwrap_or(0.0));
    t
}

/// Single-pass top-N selection via a bounded min-heap. O(n log N) with N == 8,
/// so effectively linear in pid count.
fn top_n_by(procs: &[ProcessInfo], key: impl Fn(&ProcessInfo) -> f64) -> Vec<ProcRef> {
    use std::cmp::Ordering;
    use std::collections::BinaryHeap;

    // BinaryHeap is a max-heap; invert ordering to keep the smallest of the
    // top-N at the top so we can evict it cheaply.
    #[derive(PartialEq)]
    struct Rev(f64, u32, String);
    impl Eq for Rev {}
    impl PartialOrd for Rev {
        fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
            Some(self.cmp(o))
        }
    }
    impl Ord for Rev {
        fn cmp(&self, o: &Self) -> Ordering {
            // Reverse on value so the heap's top is the *minimum*.
            o.0.partial_cmp(&self.0).unwrap_or(Ordering::Equal)
        }
    }

    let mut heap: BinaryHeap<Rev> = BinaryHeap::with_capacity(TOP_N + 1);
    for p in procs {
        let v = key(p);
        if v <= 0.0 {
            continue;
        }
        if heap.len() < TOP_N {
            heap.push(Rev(v, p.pid, p.name.clone()));
        } else if let Some(top) = heap.peek() {
            if v > top.0 {
                heap.pop();
                heap.push(Rev(v, p.pid, p.name.clone()));
            }
        }
    }
    let mut out: Vec<ProcRef> = heap
        .into_iter()
        .map(|Rev(v, pid, name)| ProcRef {
            pid,
            name,
            value: v,
        })
        .collect();
    // Largest value first.
    out.sort_by(|a, b| b.value.partial_cmp(&a.value).unwrap_or(Ordering::Equal));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::{ProcessSample, ProcessState};
    use std::time::Instant;

    fn pi(pid: u32, name: &str, cpu: f32, mem: u64) -> ProcessInfo {
        ProcessInfo {
            pid,
            parent_pid: None,
            name: name.into(),
            cmdline: name.into(),
            user: "u".into(),
            state: ProcessState::Sleeping,
            cpu_fraction: cpu,
            mem_rss_bytes: mem,
            mem_vsz_bytes: mem,
            threads: 1,
            net_rx_bytes_per_sec: None,
            net_tx_bytes_per_sec: None,
            disk_read_bytes_per_sec: None,
            disk_write_bytes_per_sec: None,
            cgroup: None,
        }
    }

    #[test]
    fn top_n_picks_largest_and_skips_zero() {
        let ps = vec![
            pi(1, "a", 0.1, 100),
            pi(2, "b", 0.5, 50),
            pi(3, "c", 0.0, 200),
            pi(4, "d", 0.9, 10),
        ];
        let top = top_n_by(&ps, |p| p.cpu_fraction as f64);
        assert_eq!(top.len(), 3); // c skipped (zero)
        assert_eq!(top[0].pid, 4); // 0.9
        assert_eq!(top[1].pid, 2); // 0.5
        assert_eq!(top[2].pid, 1); // 0.1
    }

    #[test]
    fn top_n_caps_at_top_n() {
        let ps: Vec<_> = (0..50).map(|i| pi(i + 1, "p", (i + 1) as f32 * 0.01, 0)).collect();
        let top = top_n_by(&ps, |p| p.cpu_fraction as f64);
        assert_eq!(top.len(), TOP_N);
        // pid 50 had the largest cpu (0.50)
        assert_eq!(top[0].pid, 50);
    }

    #[test]
    fn ring_caps_to_capacity() {
        let mut r = HistoryRing::default();
        for _ in 0..(TIER_CAPACITY + 5) {
            r.push(HostMetrics::default(), TopProcs::default());
        }
        assert_eq!(r.host_1s.len(), TIER_CAPACITY);
    }

    #[test]
    fn coarser_tiers_advance_on_boundaries() {
        let mut r = HistoryRing::default();
        for _ in 0..30 {
            r.push(HostMetrics::default(), TopProcs::default());
        }
        // 30 ticks → 5s ring should have 6 entries (30/5), 30s ring should have 1 (30/30).
        assert_eq!(r.host_5s.len(), 6);
        assert_eq!(r.host_30s.len(), 1);
    }

    #[test]
    fn host_window_picks_smallest_covering_tier() {
        let mut r = HistoryRing::default();
        for _ in 0..30 {
            r.push(HostMetrics::default(), TopProcs::default());
        }
        assert_eq!(r.host_window(Duration::from_secs(30)).len(), 30); // 1s tier
        assert_eq!(r.host_window(Duration::from_secs(120)).len(), 6); // 5s tier
        assert_eq!(r.host_window(Duration::from_secs(600)).len(), 1); // 30s tier
    }

    #[test]
    fn procs_at_returns_none_for_unavailable_offsets() {
        let r = HistoryRing::default();
        assert!(r.procs_at(Duration::from_secs(5), Metric::Cpu).is_none());
    }

    #[test]
    fn host_stats_computes_avg_peak_p95() {
        let mut r = HistoryRing::default();
        for i in 0..10 {
            let h = HostMetrics {
                cpu_pct: (i + 1) as f32 * 10.0,
                ..Default::default()
            };
            r.push(h, TopProcs::default());
        }
        let h = History {
            inner: Arc::new(RwLock::new(r)),
        };
        let stats = h
            .host_stats(Duration::from_secs(60), Metric::Cpu)
            .unwrap();
        assert_eq!(stats.samples, 10);
        assert_eq!(stats.peak, 100.0);
        assert!((stats.avg - 55.0).abs() < 0.001);
        // p95 of [10,20,30,40,50,60,70,80,90,100] at idx round(9 * 0.95) = 9 → 100
        assert_eq!(stats.p95, 100.0);
    }

    #[test]
    fn peak_locates_max_and_returns_responsible() {
        let mut r = HistoryRing::default();
        for i in 0..5 {
            let h = HostMetrics {
                cpu_pct: if i == 2 { 99.0 } else { 10.0 },
                ..Default::default()
            };
            let mut tp = TopProcs::default();
            if i == 2 {
                tp.by_cpu.push(ProcRef {
                    pid: 999,
                    name: "guilty".into(),
                    value: 0.99,
                });
            }
            r.push(h, tp);
        }
        let h = History {
            inner: Arc::new(RwLock::new(r)),
        };
        let p = h.peak(Duration::from_secs(60), Metric::Cpu).unwrap();
        assert_eq!(p.value, 99.0);
        // Most recent push was i=4; peak was i=2 → offset_secs = (4 - 2) * 1 = 2
        assert_eq!(p.offset_secs, 2);
        assert_eq!(p.responsible.first().map(|r| r.pid), Some(999));
    }

    #[test]
    fn peak_returns_none_when_empty() {
        let h = History {
            inner: Arc::new(RwLock::new(HistoryRing::default())),
        };
        assert!(h.peak(Duration::from_secs(60), Metric::Cpu).is_none());
    }

    #[tokio::test]
    async fn history_writer_appends_from_store() {
        // Drive the store manually: build a HostSample and feed it via a bus.
        let bus = crate::DataBus::default();
        let store = SampleStore::spawn(&bus);
        let history = History::spawn(store.clone());

        // Publish one process sample so top-N has something to pick.
        let mut rx = store.subscribe();
        bus.publish(ProcessSample {
            timestamp: Instant::now(),
            processes: vec![pi(1, "node", 0.5, 1024), pi(2, "rg", 0.1, 512)],
        });
        rx.changed().await.unwrap();

        // Wait one history tick (~1s).
        tokio::time::sleep(Duration::from_millis(1100)).await;
        history.read(|r| {
            assert!(!r.procs_1s.is_empty(), "1s ring should have an entry");
            let top = &r.procs_1s.back().unwrap().by_cpu;
            assert_eq!(top.first().map(|p| p.pid), Some(1));
        });
    }
}
