//! `top` verb implementation: process filtering, aggregation, and ranking.
//!
//! Performance shape: filtering is a single linear scan with a precompiled
//! pattern; aggregation walks the filtered set once into a `HashMap`;
//! ranking is bounded by `n` via partial-sort. On a 500-pid host this runs
//! in well under a millisecond.

use std::collections::HashMap;
use std::time::Duration;

use bobtop_core::sample::ProcessInfo;
use bobtop_core::HostSample;

use super::schema::{rfc3339_now_pub, Row, TopResponse, SCHEMA_VERSION};

/// Default `n` when a request omits it. Small enough to fit comfortably in
/// an agent context window; clients that want more must ask explicitly.
pub const DEFAULT_N: usize = 10;
/// Hard upper bound on `n` so a malformed/over-eager client can't force a
/// massive response.
pub const MAX_N: usize = 100;
/// Cap on member pids returned in an aggregated row.
pub const PIDS_PER_ROW: usize = 50;

/// Sort metric. The wire form is a string (`"cpu"`, `"mem"`, `"net.tx"`,
/// `"net.rx"`, `"disk.r"`, `"disk.w"`); parse it once at the boundary.
#[derive(Debug, Clone, Copy)]
pub enum Metric {
    Cpu,
    Mem,
    NetTx,
    NetRx,
    DiskR,
    DiskW,
}

impl Metric {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "cpu" => Metric::Cpu,
            "mem" => Metric::Mem,
            "net.tx" | "net_tx" => Metric::NetTx,
            "net.rx" | "net_rx" => Metric::NetRx,
            "disk.r" | "disk_r" => Metric::DiskR,
            "disk.w" | "disk_w" => Metric::DiskW,
            _ => return None,
        })
    }

    pub fn wire_name(self) -> &'static str {
        match self {
            Metric::Cpu => "cpu",
            Metric::Mem => "mem",
            Metric::NetTx => "net.tx",
            Metric::NetRx => "net.rx",
            Metric::DiskR => "disk.r",
            Metric::DiskW => "disk.w",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Flat,
    Exec,
    Cgroup,
    Tree,
}

impl Group {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "flat" => Group::Flat,
            "exec" => Group::Exec,
            "cgroup" => Group::Cgroup,
            "tree" => Group::Tree,
            _ => return None,
        })
    }

    pub fn wire_name(self) -> &'static str {
        match self {
            Group::Flat => "flat",
            Group::Exec => "exec",
            Group::Cgroup => "cgroup",
            Group::Tree => "tree",
        }
    }
}

/// Compiled match predicate. Cheap to evaluate per pid (case-insensitive
/// glob or substring), cheap to construct per request.
#[derive(Debug, Clone)]
pub struct MatchPattern {
    raw_lower: String,
    is_glob: bool,
}

impl MatchPattern {
    pub fn new(raw: &str) -> Self {
        let is_glob = raw.contains('*') || raw.contains('?');
        Self {
            raw_lower: raw.to_ascii_lowercase(),
            is_glob,
        }
    }

    /// Returns `Some(field)` when the pattern matches name or cmdline,
    /// where `field` is `"name"` or `"cmdline"`. `None` on miss.
    pub fn check(&self, p: &ProcessInfo) -> Option<&'static str> {
        let name_lower = p.name.to_ascii_lowercase();
        if self.is_glob {
            if glob_match(&self.raw_lower, &name_lower) {
                return Some("name");
            }
            let cmd_lower = p.cmdline.to_ascii_lowercase();
            if glob_match(&self.raw_lower, &cmd_lower) {
                return Some("cmdline");
            }
        } else {
            if name_lower.contains(&self.raw_lower) {
                return Some("name");
            }
            let cmd_lower = p.cmdline.to_ascii_lowercase();
            if cmd_lower.contains(&self.raw_lower) {
                return Some("cmdline");
            }
        }
        None
    }
}

/// Iterative glob match supporting `*` (zero or more) and `?` (any one).
/// Linear in `pattern.len() + s.len()` amortized.
pub fn glob_match(pattern: &str, s: &str) -> bool {
    let p = pattern.as_bytes();
    let t = s.as_bytes();
    let (mut pi, mut si) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while si < t.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == t[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, si));
            pi += 1;
        } else if let Some((sp, ss)) = star {
            pi = sp + 1;
            si = ss + 1;
            star = Some((sp, si));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// Parse a window string like `30s`, `1m`, `5m`, `30m`. Returns the duration
/// or an error message suitable for embedding in an `ErrorResponse`.
pub fn parse_window(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("window cannot be empty".into());
    }
    let (num_part, unit) = s
        .find(|c: char| c.is_ascii_alphabetic())
        .map(|i| (&s[..i], &s[i..]))
        .ok_or_else(|| format!("window '{s}' missing unit (s|m)"))?;
    let n: u64 = num_part
        .parse()
        .map_err(|_| format!("window '{s}' has non-numeric quantity"))?;
    let secs = match unit {
        "s" => n,
        "m" => n.saturating_mul(60),
        other => return Err(format!("window unit '{other}' not supported (use s|m)")),
    };
    if secs == 0 {
        return Err("window must be > 0".into());
    }
    if secs > 1800 {
        return Err("window must be ≤ 30m (history retention limit)".into());
    }
    Ok(Duration::from_secs(secs))
}

/// History-tracked metrics. Subset of `Metric` — disk read/write aren't
/// retained in the ring buffer (yet).
pub fn history_metric_for(m: Metric) -> Result<bobtop_core::Metric, String> {
    Ok(match m {
        Metric::Cpu => bobtop_core::Metric::Cpu,
        Metric::Mem => bobtop_core::Metric::Mem,
        Metric::NetTx => bobtop_core::Metric::NetTx,
        Metric::NetRx => bobtop_core::Metric::NetRx,
        Metric::DiskR | Metric::DiskW => {
            return Err("disk metrics are not retained in history yet".into())
        }
    })
}

/// Aggregate stats over a matched pid set, used by the `summary` verb
/// when scope is `match` or `pid`. Returns `None` when no pids match.
pub fn match_summary(
    snap: &HostSample,
    match_pat: Option<&MatchPattern>,
) -> Option<MatchAggregate> {
    let procs = snap.processes.as_ref()?;
    let mut count = 0u32;
    let mut cpu_pct = 0.0f32;
    let mut mem = 0u64;
    let mut net_rx = 0.0f64;
    let mut net_tx = 0.0f64;
    let mut disk_r = 0.0f64;
    let mut disk_w = 0.0f64;
    for p in &procs.processes {
        if let Some(m) = match_pat {
            if m.check(p).is_none() {
                continue;
            }
        }
        count += 1;
        cpu_pct += p.cpu_fraction * 100.0;
        mem = mem.saturating_add(p.mem_rss_bytes);
        net_rx += p.net_rx_bytes_per_sec.unwrap_or(0.0);
        net_tx += p.net_tx_bytes_per_sec.unwrap_or(0.0);
        disk_r += p.disk_read_bytes_per_sec.unwrap_or(0.0);
        disk_w += p.disk_write_bytes_per_sec.unwrap_or(0.0);
    }
    if count == 0 {
        return None;
    }
    Some(MatchAggregate {
        pid_count: count,
        cpu_pct,
        mem_bytes: mem,
        net_rx_bps: net_rx as u64,
        net_tx_bps: net_tx as u64,
        disk_r_bps: disk_r as u64,
        disk_w_bps: disk_w as u64,
    })
}

#[derive(Debug, Clone, Default)]
pub struct MatchAggregate {
    pub pid_count: u32,
    pub cpu_pct: f32,
    pub mem_bytes: u64,
    pub net_rx_bps: u64,
    pub net_tx_bps: u64,
    pub disk_r_bps: u64,
    pub disk_w_bps: u64,
}

/// Locate a single `ProcessInfo` by pid. Cheap linear scan — typical
/// snapshots are <2k pids.
pub fn find_pid(snap: &HostSample, pid: u32) -> Option<&ProcessInfo> {
    snap.processes
        .as_ref()
        .and_then(|s| s.processes.iter().find(|p| p.pid == pid))
}

/// Resolve a `match` pattern to a single pid, or report ambiguity.
/// Returns `Ok(Some(pid))` on a unique match, `Ok(None)` when no pid
/// matches, `Err(matched_count)` when multiple pids match.
pub fn resolve_pid_by_match(
    snap: &HostSample,
    pat: &MatchPattern,
) -> std::result::Result<Option<u32>, usize> {
    let procs = match snap.processes.as_ref() {
        Some(s) => &s.processes,
        None => return Ok(None),
    };
    let mut found: Option<u32> = None;
    let mut count = 0usize;
    for p in procs {
        if pat.check(p).is_some() {
            count += 1;
            if count == 1 {
                found = Some(p.pid);
            }
            if count > 1 {
                return Err(count);
            }
        }
    }
    Ok(if count == 0 { None } else { found })
}

/// Per-pid metric extractor. Returns 0.0 when the source is unavailable so
/// downstream sort/aggregation logic doesn't have to branch.
pub fn metric_value(p: &ProcessInfo, m: Metric) -> f64 {
    match m {
        Metric::Cpu => p.cpu_fraction as f64,
        Metric::Mem => p.mem_rss_bytes as f64,
        Metric::NetTx => p.net_tx_bytes_per_sec.unwrap_or(0.0),
        Metric::NetRx => p.net_rx_bytes_per_sec.unwrap_or(0.0),
        Metric::DiskR => p.disk_read_bytes_per_sec.unwrap_or(0.0),
        Metric::DiskW => p.disk_write_bytes_per_sec.unwrap_or(0.0),
    }
}

/// Top-N over the snapshot. Allocates one `Row` per output entry; processes
/// the filtered set in a single pass for `flat`, in two passes for `exec`
/// (group, then rank).
pub fn run_top(
    snap: &HostSample,
    metric: Metric,
    n: usize,
    group: Group,
    match_pat: Option<&MatchPattern>,
) -> TopResponse {
    let n = n.clamp(1, MAX_N);
    let empty = Vec::new();
    let procs = snap
        .processes
        .as_ref()
        .map(|p| &p.processes)
        .unwrap_or(&empty);
    let rows = match group {
        Group::Flat => top_flat(procs, metric, n, match_pat),
        Group::Exec => top_keyed(
            procs,
            metric,
            n,
            match_pat,
            "exec",
            |p| p.name.clone(),
        ),
        Group::Cgroup => top_keyed(
            procs,
            metric,
            n,
            match_pat,
            "cgroup",
            |p| p.cgroup.clone().unwrap_or_else(|| "(no cgroup)".into()),
        ),
        Group::Tree => top_tree(procs, metric, n, match_pat),
    };
    TopResponse {
        schema: SCHEMA_VERSION,
        ts: rfc3339_now_pub(),
        by: metric.wire_name().to_string(),
        group: group.wire_name(),
        rows,
    }
}

fn top_flat(
    procs: &[ProcessInfo],
    metric: Metric,
    n: usize,
    match_pat: Option<&MatchPattern>,
) -> Vec<Row> {
    // Bounded min-heap keyed on the metric so we never sort the full list.
    use std::cmp::Ordering;
    use std::collections::BinaryHeap;

    #[derive(PartialEq)]
    struct Entry {
        value: f64,
        idx: usize,
        matched_on: Option<&'static str>,
    }
    impl Eq for Entry {}
    impl PartialOrd for Entry {
        fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
            Some(self.cmp(o))
        }
    }
    impl Ord for Entry {
        fn cmp(&self, o: &Self) -> Ordering {
            // Reverse on value so the heap top is the smallest of the top-N.
            o.value.partial_cmp(&self.value).unwrap_or(Ordering::Equal)
        }
    }

    let mut heap: BinaryHeap<Entry> = BinaryHeap::with_capacity(n + 1);
    for (idx, p) in procs.iter().enumerate() {
        let matched_on = match match_pat {
            Some(m) => match m.check(p) {
                Some(field) => Some(field),
                None => continue,
            },
            None => None,
        };
        let v = metric_value(p, metric);
        if v <= 0.0 {
            continue;
        }
        if heap.len() < n {
            heap.push(Entry {
                value: v,
                idx,
                matched_on,
            });
        } else if let Some(top) = heap.peek() {
            if v > top.value {
                heap.pop();
                heap.push(Entry {
                    value: v,
                    idx,
                    matched_on,
                });
            }
        }
    }
    let mut entries: Vec<Entry> = heap.into_iter().collect();
    entries.sort_by(|a, b| b.value.partial_cmp(&a.value).unwrap_or(Ordering::Equal));
    entries
        .into_iter()
        .map(|e| {
            let p = &procs[e.idx];
            Row {
                id: p.pid.to_string(),
                kind: "flat",
                name: p.name.clone(),
                cmdline: Some(p.cmdline.clone()),
                pids: vec![p.pid],
                pids_truncated: false,
                cpu_pct: (p.cpu_fraction * 100.0),
                mem_bytes: p.mem_rss_bytes,
                net_rx_bps: p.net_rx_bytes_per_sec.unwrap_or(0.0) as u64,
                net_tx_bps: p.net_tx_bytes_per_sec.unwrap_or(0.0) as u64,
                disk_r_bps: p.disk_read_bytes_per_sec.unwrap_or(0.0) as u64,
                disk_w_bps: p.disk_write_bytes_per_sec.unwrap_or(0.0) as u64,
                matched_on: e.matched_on,
            }
        })
        .collect()
}

/// Bucket processes by an arbitrary string key (executable name, cgroup
/// path, etc.), aggregate metrics within each bucket, and return the
/// top-N buckets by the requested metric. Shared by `exec` and `cgroup`
/// groupings.
fn top_keyed<F>(
    procs: &[ProcessInfo],
    metric: Metric,
    n: usize,
    match_pat: Option<&MatchPattern>,
    kind: &'static str,
    key_fn: F,
) -> Vec<Row>
where
    F: Fn(&ProcessInfo) -> String,
{
    #[derive(Default)]
    struct Agg {
        cpu_pct: f32,
        mem_bytes: u64,
        net_rx: f64,
        net_tx: f64,
        disk_r: f64,
        disk_w: f64,
        pids: Vec<u32>,
        pids_truncated: bool,
        sample_cmdline: String,
        matched_on: Option<&'static str>,
    }
    let mut groups: HashMap<String, Agg> = HashMap::new();
    for p in procs {
        let matched_on = match match_pat {
            Some(m) => match m.check(p) {
                Some(field) => Some(field),
                None => continue,
            },
            None => None,
        };
        let g = groups.entry(key_fn(p)).or_default();
        agg_acc(g, p);
        if g.pids.len() < PIDS_PER_ROW {
            g.pids.push(p.pid);
        } else {
            g.pids_truncated = true;
        }
        if g.sample_cmdline.is_empty() {
            g.sample_cmdline = p.cmdline.clone();
        }
        if g.matched_on.is_none() {
            g.matched_on = matched_on;
        }
    }
    fn key_value(a: &Agg, m: Metric) -> f64 {
        match m {
            Metric::Cpu => a.cpu_pct as f64,
            Metric::Mem => a.mem_bytes as f64,
            Metric::NetTx => a.net_tx,
            Metric::NetRx => a.net_rx,
            Metric::DiskR => a.disk_r,
            Metric::DiskW => a.disk_w,
        }
    }
    fn agg_acc(g: &mut Agg, p: &ProcessInfo) {
        g.cpu_pct += p.cpu_fraction * 100.0;
        g.mem_bytes = g.mem_bytes.saturating_add(p.mem_rss_bytes);
        g.net_rx += p.net_rx_bytes_per_sec.unwrap_or(0.0);
        g.net_tx += p.net_tx_bytes_per_sec.unwrap_or(0.0);
        g.disk_r += p.disk_read_bytes_per_sec.unwrap_or(0.0);
        g.disk_w += p.disk_write_bytes_per_sec.unwrap_or(0.0);
    }
    let mut rows: Vec<(String, Agg)> = groups.into_iter().collect();
    rows.retain(|(_, a)| key_value(a, metric) > 0.0);
    rows.sort_by(|(_, a), (_, b)| {
        key_value(b, metric)
            .partial_cmp(&key_value(a, metric))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    rows.truncate(n);
    rows.into_iter()
        .map(|(name, a)| Row {
            id: name.clone(),
            kind,
            name,
            cmdline: Some(a.sample_cmdline),
            pids: a.pids,
            pids_truncated: a.pids_truncated,
            cpu_pct: a.cpu_pct,
            mem_bytes: a.mem_bytes,
            net_rx_bps: a.net_rx as u64,
            net_tx_bps: a.net_tx as u64,
            disk_r_bps: a.disk_r as u64,
            disk_w_bps: a.disk_w as u64,
            matched_on: a.matched_on,
        })
        .collect()
}

/// Aggregate by parent-rooted subtree: each row is the set of processes
/// reachable by walking `parent_pid` upward to a root (a pid whose parent
/// is missing from the snapshot, has parent 0, or has parent_pid `None`).
/// The row's `id` is the root pid, `name` is the root's executable name,
/// and aggregates sum across the entire subtree. Most useful for
/// "what did `cargo test` actually spawn?" queries.
fn top_tree(
    procs: &[ProcessInfo],
    metric: Metric,
    n: usize,
    match_pat: Option<&MatchPattern>,
) -> Vec<Row> {
    if procs.is_empty() {
        return Vec::new();
    }
    // Index by pid for O(1) parent lookups.
    let by_pid: HashMap<u32, &ProcessInfo> = procs.iter().map(|p| (p.pid, p)).collect();
    // Walk from each pid to its root, memoizing roots so a long chain
    // doesn't re-traverse for every leaf.
    let mut root_of: HashMap<u32, u32> = HashMap::with_capacity(procs.len());
    for p in procs {
        if root_of.contains_key(&p.pid) {
            continue;
        }
        let mut cur = p.pid;
        let mut chain: Vec<u32> = Vec::new();
        loop {
            chain.push(cur);
            let info = match by_pid.get(&cur) {
                Some(i) => *i,
                None => break, // shouldn't happen; safe-guard
            };
            match info.parent_pid {
                Some(pp) if pp != 0 && by_pid.contains_key(&pp) => {
                    if let Some(&already) = root_of.get(&pp) {
                        for c in &chain {
                            root_of.insert(*c, already);
                        }
                        chain.clear();
                        break;
                    }
                    cur = pp;
                }
                _ => {
                    // `cur` is the root.
                    let root = cur;
                    for c in &chain {
                        root_of.insert(*c, root);
                    }
                    chain.clear();
                    break;
                }
            }
        }
    }
    // Aggregate per root, applying the match filter at the leaf level so
    // a tree containing any matched pid surfaces.
    #[derive(Default)]
    struct TreeAgg {
        cpu_pct: f32,
        mem_bytes: u64,
        net_rx: f64,
        net_tx: f64,
        disk_r: f64,
        disk_w: f64,
        pids: Vec<u32>,
        pids_truncated: bool,
        matched_on: Option<&'static str>,
    }
    let mut roots: HashMap<u32, TreeAgg> = HashMap::new();
    let mut any_match_seen: bool = match_pat.is_none();
    for p in procs {
        let matched_on = match match_pat {
            Some(m) => m.check(p),
            None => None,
        };
        if match_pat.is_some() && matched_on.is_none() {
            continue;
        }
        any_match_seen = any_match_seen || matched_on.is_some();
        let root_pid = match root_of.get(&p.pid).copied() {
            Some(r) => r,
            None => continue,
        };
        let g = roots.entry(root_pid).or_default();
        g.cpu_pct += p.cpu_fraction * 100.0;
        g.mem_bytes = g.mem_bytes.saturating_add(p.mem_rss_bytes);
        g.net_rx += p.net_rx_bytes_per_sec.unwrap_or(0.0);
        g.net_tx += p.net_tx_bytes_per_sec.unwrap_or(0.0);
        g.disk_r += p.disk_read_bytes_per_sec.unwrap_or(0.0);
        g.disk_w += p.disk_write_bytes_per_sec.unwrap_or(0.0);
        if g.pids.len() < PIDS_PER_ROW {
            g.pids.push(p.pid);
        } else {
            g.pids_truncated = true;
        }
        if g.matched_on.is_none() {
            g.matched_on = matched_on;
        }
    }
    if !any_match_seen {
        return Vec::new();
    }
    fn val(a: &TreeAgg, m: Metric) -> f64 {
        match m {
            Metric::Cpu => a.cpu_pct as f64,
            Metric::Mem => a.mem_bytes as f64,
            Metric::NetTx => a.net_tx,
            Metric::NetRx => a.net_rx,
            Metric::DiskR => a.disk_r,
            Metric::DiskW => a.disk_w,
        }
    }
    let mut rows: Vec<(u32, TreeAgg)> = roots.into_iter().collect();
    rows.retain(|(_, a)| val(a, metric) > 0.0);
    rows.sort_by(|(_, a), (_, b)| {
        val(b, metric)
            .partial_cmp(&val(a, metric))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    rows.truncate(n);
    rows.into_iter()
        .map(|(root_pid, a)| {
            let root_info = by_pid.get(&root_pid);
            Row {
                id: root_pid.to_string(),
                kind: "tree",
                name: root_info.map(|p| p.name.clone()).unwrap_or_default(),
                cmdline: root_info.map(|p| p.cmdline.clone()),
                pids: a.pids,
                pids_truncated: a.pids_truncated,
                cpu_pct: a.cpu_pct,
                mem_bytes: a.mem_bytes,
                net_rx_bps: a.net_rx as u64,
                net_tx_bps: a.net_tx as u64,
                disk_r_bps: a.disk_r as u64,
                disk_w_bps: a.disk_w as u64,
                matched_on: a.matched_on,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bobtop_core::sample::{ProcessSample, ProcessState};
    use std::time::Instant;

    fn pi(pid: u32, name: &str, cmdline: &str, cpu: f32, mem: u64) -> ProcessInfo {
        ProcessInfo {
            pid,
            parent_pid: None,
            name: name.into(),
            cmdline: cmdline.into(),
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

    fn snap(procs: Vec<ProcessInfo>) -> HostSample {
        HostSample {
            processes: Some(ProcessSample {
                timestamp: Instant::now(),
                processes: procs,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn glob_matches_prefix_suffix_contains() {
        assert!(glob_match("node*", "node"));
        assert!(glob_match("node*", "node-foo"));
        assert!(!glob_match("node*", "myfoo"));
        assert!(glob_match("*chrome*", "google-chrome-helper"));
        assert!(glob_match("c?ome", "chome"));
    }

    #[test]
    fn substring_match_is_case_insensitive() {
        let p = MatchPattern::new("Node");
        let proc = pi(1, "node", "node next-server", 0.1, 0);
        assert_eq!(p.check(&proc), Some("name"));
    }

    #[test]
    fn flat_top_n_sorts_descending_and_filters_zero() {
        let s = snap(vec![
            pi(1, "a", "a", 0.1, 0),
            pi(2, "b", "b", 0.5, 0),
            pi(3, "c", "c", 0.0, 0),
            pi(4, "d", "d", 0.9, 0),
        ]);
        let r = run_top(&s, Metric::Cpu, 10, Group::Flat, None);
        assert_eq!(r.rows.len(), 3);
        assert_eq!(r.rows[0].id, "4");
        assert_eq!(r.rows[1].id, "2");
        assert_eq!(r.rows[2].id, "1");
    }

    #[test]
    fn match_pattern_filters_before_ranking() {
        let s = snap(vec![
            pi(1, "node", "node a", 0.1, 0),
            pi(2, "redis", "redis", 0.9, 0),
            pi(3, "node", "node b", 0.5, 0),
        ]);
        let m = MatchPattern::new("node*");
        let r = run_top(&s, Metric::Cpu, 10, Group::Flat, Some(&m));
        assert_eq!(r.rows.len(), 2);
        assert_eq!(r.rows[0].id, "3");
        assert_eq!(r.rows[1].id, "1");
        assert!(r.rows.iter().all(|r| r.matched_on == Some("name")));
    }

    #[test]
    fn exec_groups_by_name_and_aggregates() {
        let s = snap(vec![
            pi(1, "node", "node a", 0.1, 100),
            pi(2, "node", "node b", 0.2, 200),
            pi(3, "redis", "redis", 0.5, 50),
        ]);
        let r = run_top(&s, Metric::Cpu, 10, Group::Exec, None);
        // node group sums to 0.3 cpu, redis 0.5 — redis ranks higher.
        assert_eq!(r.rows.len(), 2);
        assert_eq!(r.rows[0].id, "redis");
        assert_eq!(r.rows[1].id, "node");
        let node = &r.rows[1];
        assert_eq!(node.pids.len(), 2);
        assert_eq!(node.mem_bytes, 300);
    }

    #[test]
    fn window_parses_seconds_and_minutes() {
        assert_eq!(parse_window("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_window("1m").unwrap(), Duration::from_secs(60));
        assert_eq!(parse_window("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_window("30m").unwrap(), Duration::from_secs(1800));
    }

    #[test]
    fn window_rejects_missing_unit_or_oversize() {
        assert!(parse_window("60").is_err());
        assert!(parse_window("0s").is_err());
        assert!(parse_window("31m").is_err());
        assert!(parse_window("1h").is_err());
        assert!(parse_window("").is_err());
    }

    fn pi_with(
        pid: u32,
        parent: Option<u32>,
        name: &str,
        cmdline: &str,
        cpu: f32,
        mem: u64,
        cgroup: Option<&str>,
    ) -> ProcessInfo {
        ProcessInfo {
            pid,
            parent_pid: parent,
            name: name.into(),
            cmdline: cmdline.into(),
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
            cgroup: cgroup.map(String::from),
        }
    }

    #[test]
    fn cgroup_groups_by_cgroup_path_with_no_cgroup_bucket() {
        let s = snap(vec![
            pi_with(1, None, "a", "a", 0.1, 0, Some("docker.scope")),
            pi_with(2, None, "b", "b", 0.2, 0, Some("docker.scope")),
            pi_with(3, None, "c", "c", 0.5, 0, Some("user.slice")),
            pi_with(4, None, "d", "d", 0.05, 0, None),
        ]);
        let r = run_top(&s, Metric::Cpu, 10, Group::Cgroup, None);
        // user.slice (0.5) > docker.scope (0.3) > (no cgroup) (0.05)
        assert_eq!(r.rows[0].id, "user.slice");
        assert_eq!(r.rows[0].kind, "cgroup");
        assert_eq!(r.rows[1].id, "docker.scope");
        assert_eq!(r.rows[1].pids.len(), 2);
        assert_eq!(r.rows[2].id, "(no cgroup)");
    }

    #[test]
    fn tree_aggregates_subtree_under_root() {
        // pid 1 (root) → pid 2, pid 3
        // pid 4 (root, no parent in snapshot) → pid 5
        let s = snap(vec![
            pi_with(1, None, "cargo", "cargo build", 0.1, 100, None),
            pi_with(2, Some(1), "rustc", "rustc", 0.5, 200, None),
            pi_with(3, Some(1), "ld", "ld", 0.2, 50, None),
            pi_with(4, None, "node", "node", 0.05, 1000, None),
            pi_with(5, Some(4), "child", "child", 0.0, 500, None),
        ]);
        let r = run_top(&s, Metric::Cpu, 10, Group::Tree, None);
        // cargo subtree cpu = 0.1 + 0.5 + 0.2 = 0.8 → 80%
        // node subtree cpu = 0.05 + 0.0 = 0.05 → 5%
        assert_eq!(r.rows[0].id, "1");
        assert_eq!(r.rows[0].kind, "tree");
        assert_eq!(r.rows[0].name, "cargo");
        assert_eq!(r.rows[0].pids.len(), 3);
        assert!((r.rows[0].cpu_pct - 80.0).abs() < 0.001);
        assert_eq!(r.rows[1].id, "4");
        assert_eq!(r.rows[1].pids.len(), 2);
    }

    #[test]
    fn match_summary_aggregates_filtered_pids() {
        let s = snap(vec![
            pi_with(1, None, "node", "node a", 0.1, 100, None),
            pi_with(2, None, "node", "node b", 0.2, 200, None),
            pi_with(3, None, "redis", "redis", 0.5, 50, None),
        ]);
        let pat = MatchPattern::new("node");
        let agg = match_summary(&s, Some(&pat)).unwrap();
        assert_eq!(agg.pid_count, 2);
        assert_eq!(agg.mem_bytes, 300);
        assert!((agg.cpu_pct - 30.0).abs() < 0.001);
    }

    #[test]
    fn resolve_pid_by_match_unique_vs_ambiguous() {
        let s = snap(vec![
            pi_with(1, None, "node", "node a", 0.0, 0, None),
            pi_with(2, None, "redis", "redis", 0.0, 0, None),
            pi_with(3, None, "node", "node b", 0.0, 0, None),
        ]);
        // Unique: redis matches one pid.
        let pat = MatchPattern::new("redis");
        assert_eq!(resolve_pid_by_match(&s, &pat).unwrap(), Some(2));
        // Ambiguous: node matches two pids.
        let pat = MatchPattern::new("node");
        assert!(resolve_pid_by_match(&s, &pat).is_err());
        // Miss.
        let pat = MatchPattern::new("nothing");
        assert_eq!(resolve_pid_by_match(&s, &pat).unwrap(), None);
    }

    #[test]
    fn n_is_clamped_to_max() {
        let s = snap((0..200).map(|i| pi(i + 1, "p", "p", 0.001 * (i + 1) as f32, 0)).collect());
        let r = run_top(&s, Metric::Cpu, 10_000, Group::Flat, None);
        assert!(r.rows.len() <= MAX_N);
    }
}
