//! Wire-format types for the agent socket.
//!
//! Kept deliberately separate from the internal sample types in
//! `bobtop-core::sample` — the wire format is a public contract that ages
//! independently of internal struct shapes. Convert at the boundary.

use std::time::{SystemTime, UNIX_EPOCH};

use bobtop_core::{HostSample, PeakResult, ProcRef, WindowStats};
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: &str = "gtop/v1";

/// Inbound request envelope. Every query is a single JSON object whose
/// `q` field selects the verb. Unknown or unrecognized fields are ignored
/// so additive schema growth is non-breaking.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Request {
    pub q: String,
    /// Sort/aggregate metric. Validation lives in the verb handler.
    #[serde(default)]
    pub by: Option<String>,
    /// Top-N. Defaults to 10 when omitted; clamped server-side.
    #[serde(default)]
    pub n: Option<usize>,
    /// Aggregation level: `flat` | `exec` | `cgroup` | `tree`.
    #[serde(default)]
    pub group: Option<String>,
    /// Process-name filter. A single string or an array of strings is
    /// accepted. Each item supports case-insensitive substring, glob, or
    /// `re:` regex matching against the union of `comm` + `cmdline`.
    #[serde(default, rename = "match")]
    pub match_: Option<MatchQuery>,
    /// Retrospective window (e.g. `1m`, `5m`, `30m`) for `peak` / `window`.
    #[serde(default)]
    pub window: Option<String>,
    /// Metric for retrospective verbs (independent of `by`, kept separate
    /// so a `top` query within a window can ask about a different metric
    /// later if we extend that direction).
    #[serde(default)]
    pub metric: Option<String>,
    /// Explicit pid for `summary` / `pid_inspect`.
    #[serde(default)]
    pub pid: Option<u32>,
    /// `summary` scope: `host` (default) | `match` | `pid`.
    #[serde(default)]
    pub scope: Option<String>,
    /// Point-in-time offset for `responsible_for` (e.g. `30s`, `5m`).
    #[serde(default)]
    pub at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum MatchQuery {
    One(String),
    Many(Vec<String>),
}

impl MatchQuery {
    #[allow(dead_code)]
    pub fn into_vec(self) -> Vec<String> {
        match self {
            MatchQuery::One(s) => vec![s],
            MatchQuery::Many(v) => v,
        }
    }

    pub fn as_slice(&self) -> &[String] {
        match self {
            MatchQuery::One(s) => std::slice::from_ref(s),
            MatchQuery::Many(v) => v.as_slice(),
        }
    }
}

/// Successful `snapshot` response — a single point-in-time aggregate of
/// host-level metrics. No per-pid data; use `top` (Phase 3) for that.
#[derive(Debug, Clone, Serialize)]
pub struct SnapshotResponse {
    pub schema: &'static str,
    pub ts: String,
    pub host: HostSummary,
}

/// Host-level summary used by `snapshot` and (later) `summary`.
///
/// All rates are bytes-per-second. Fields are `Option`/`0`-default rather
/// than missing so agents can rely on a stable shape.
#[derive(Debug, Clone, Default, Serialize)]
pub struct HostSummary {
    pub cpu_pct: f32,
    pub mem_used_bytes: u64,
    pub mem_total_bytes: u64,
    pub swap_used_bytes: u64,
    pub swap_total_bytes: u64,
    pub load_1m: Option<f32>,
    pub load_5m: Option<f32>,
    pub load_15m: Option<f32>,
    pub net_rx_bps: u64,
    pub net_tx_bps: u64,
    pub disk_r_bps: u64,
    pub disk_w_bps: u64,
    pub n_procs: u32,
}

/// Successful `top` response — ranked rows of pids or aggregated groups.
#[derive(Debug, Clone, Serialize)]
pub struct TopResponse {
    pub schema: &'static str,
    pub ts: String,
    pub by: String,
    pub group: &'static str,
    pub rows: Vec<Row>,
}

/// Uniform process-bearing row used by every grouping mode. Aggregated
/// groupings (e.g. `exec`) carry a `pids` member list; `flat` carries a
/// single-element list with the row's pid.
#[derive(Debug, Clone, Serialize)]
pub struct Row {
    /// Stable identifier within the row's `kind`. Pid for flat rows,
    /// executable name for exec rows, etc.
    pub id: String,
    /// `flat` | `exec` | `cgroup` | `tree`.
    pub kind: &'static str,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cmdline: Option<String>,
    pub pids: Vec<u32>,
    #[serde(skip_serializing_if = "skip_false")]
    pub pids_truncated: bool,
    pub cpu_pct: f32,
    pub mem_bytes: u64,
    pub net_rx_bps: u64,
    pub net_tx_bps: u64,
    pub disk_r_bps: u64,
    pub disk_w_bps: u64,
    /// Which field the `match` filter hit on the row's first member,
    /// or `None` when no filter was applied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_on: Option<&'static str>,
}

fn skip_false(b: &bool) -> bool {
    !*b
}

/// Successful `window` response — aggregate stats over a retrospective
/// window for a single host metric.
#[derive(Debug, Clone, Serialize)]
pub struct WindowResponse {
    pub schema: &'static str,
    pub ts: String,
    pub metric: String,
    pub window: String,
    pub samples: usize,
    pub avg: f64,
    pub peak: f64,
    pub p95: f64,
}

/// `responsible_for` response — the top-N pids that owned `metric` at a
/// specific point in the past (`ago_secs` from now).
#[derive(Debug, Clone, Serialize)]
pub struct ResponsibleResponse {
    pub schema: &'static str,
    pub ts: String,
    pub metric: String,
    pub at: String,
    pub ago_secs: u64,
    pub responsible: Vec<RespRef>,
}

pub fn build_responsible(
    metric: &str,
    at_raw: &str,
    ago_secs: u64,
    responsible: Vec<ProcRef>,
) -> ResponsibleResponse {
    ResponsibleResponse {
        schema: SCHEMA_VERSION,
        ts: rfc3339_now_pub(),
        metric: metric.into(),
        at: at_raw.into(),
        ago_secs,
        responsible: responsible.iter().map(RespRef::from).collect(),
    }
}

/// `summary` response. Either a host-wide `HostSummary` (the same shape
/// used by `snapshot`) or a per-scope rollup over the matched pids.
#[derive(Debug, Clone, Serialize)]
pub struct SummaryResponse {
    pub schema: &'static str,
    pub ts: String,
    pub scope: String,
    pub host: HostSummary,
    /// Number of pids included in the rollup. `0` for host scope.
    pub pid_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched: Option<String>,
}

/// `pid_inspect` response — full detail for one process. Mirrors the
/// internal `ProcessInfo` but with byte fields named like the rest of
/// the agent surface (`*_bps` not `*_per_sec`).
#[derive(Debug, Clone, Serialize)]
pub struct PidInspectResponse {
    pub schema: &'static str,
    pub ts: String,
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub name: String,
    pub cmdline: String,
    pub user: String,
    pub state: String,
    pub cpu_pct: f32,
    pub mem_rss_bytes: u64,
    pub mem_vsz_bytes: u64,
    pub threads: u32,
    pub net_rx_bps: Option<f64>,
    pub net_tx_bps: Option<f64>,
    pub disk_r_bps: Option<f64>,
    pub disk_w_bps: Option<f64>,
    pub cgroup: Option<String>,
}

/// Successful `peak` response — the peak's value, when it occurred, and
/// who was responsible at that tick.
#[derive(Debug, Clone, Serialize)]
pub struct PeakResponse {
    pub schema: &'static str,
    pub ts: String,
    pub metric: String,
    pub window: String,
    pub peak: PeakBody,
}

#[derive(Debug, Clone, Serialize)]
pub struct PeakBody {
    pub value: f64,
    pub ago_secs: u64,
    pub responsible: Vec<RespRef>,
}

/// Compact pid/name/value triple used by the `peak.responsible` field.
/// Mirrors `bobtop_core::ProcRef` but is the serialization-stable form.
#[derive(Debug, Clone, Serialize)]
pub struct RespRef {
    pub pid: u32,
    pub name: String,
    pub value: f64,
}

impl From<&ProcRef> for RespRef {
    fn from(p: &ProcRef) -> Self {
        Self {
            pid: p.pid,
            name: p.name.clone(),
            value: p.value,
        }
    }
}

/// Build a `WindowResponse` from history-side stats.
pub fn build_window(metric: &str, window: &str, stats: WindowStats) -> WindowResponse {
    WindowResponse {
        schema: SCHEMA_VERSION,
        ts: rfc3339_now_pub(),
        metric: metric.into(),
        window: window.into(),
        samples: stats.samples,
        avg: stats.avg,
        peak: stats.peak,
        p95: stats.p95,
    }
}

/// Build a `PeakResponse` from history-side peak result.
pub fn build_peak(metric: &str, window: &str, peak: PeakResult) -> PeakResponse {
    PeakResponse {
        schema: SCHEMA_VERSION,
        ts: rfc3339_now_pub(),
        metric: metric.into(),
        window: window.into(),
        peak: PeakBody {
            value: peak.value,
            ago_secs: peak.offset_secs,
            responsible: peak.responsible.iter().map(RespRef::from).collect(),
        },
    }
}

/// Error envelope. Mutually exclusive with the success shape — clients
/// detect by presence of the `error` field.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorResponse {
    pub schema: &'static str,
    pub error: ErrorBody,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorBody {
    pub code: &'static str,
    pub message: String,
}

impl ErrorResponse {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            schema: SCHEMA_VERSION,
            error: ErrorBody {
                code,
                message: message.into(),
            },
        }
    }
}

/// Build a `SnapshotResponse` from the latest `HostSample`. Cheap — a few
/// scalar reads and a small allocation for the timestamp string.
pub fn build_snapshot(snap: &HostSample) -> SnapshotResponse {
    let cpu_pct = snap
        .cpu
        .as_ref()
        .map(|c| c.aggregate_utilization * 100.0)
        .unwrap_or(0.0);
    let (mem_used, mem_total, swap_used, swap_total) = snap
        .memory
        .as_ref()
        .map(|m| (m.used_bytes, m.total_bytes, m.swap_used_bytes, m.swap_total_bytes))
        .unwrap_or((0, 0, 0, 0));
    let load = snap.cpu.as_ref().and_then(|c| c.load_average);
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
    SnapshotResponse {
        schema: SCHEMA_VERSION,
        ts: rfc3339_now(),
        host: HostSummary {
            cpu_pct,
            mem_used_bytes: mem_used,
            mem_total_bytes: mem_total,
            swap_used_bytes: swap_used,
            swap_total_bytes: swap_total,
            load_1m: load.map(|l| l.one),
            load_5m: load.map(|l| l.five),
            load_15m: load.map(|l| l.fifteen),
            net_rx_bps: net_rx,
            net_tx_bps: net_tx,
            disk_r_bps: disk_r,
            disk_w_bps: disk_w,
            n_procs,
        },
    }
}

/// Public alias used by sibling modules (e.g. `query`) so they don't each
/// re-implement the timestamp format.
pub fn rfc3339_now_pub() -> String {
    rfc3339_now()
}

/// Compact RFC3339-ish UTC timestamp — produced without pulling in `chrono`
/// since the daemon already avoids that dep elsewhere. Format:
/// `YYYY-MM-DDTHH:MM:SSZ`. Suffices for agent consumption.
fn rfc3339_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let t: libc::time_t = secs as libc::time_t;
    if unsafe { libc::gmtime_r(&t, &mut tm) }.is_null() {
        return "1970-01-01T00:00:00Z".into();
    }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_from_empty_sample_yields_zeroed_summary() {
        let snap = HostSample::default();
        let r = build_snapshot(&snap);
        assert_eq!(r.schema, SCHEMA_VERSION);
        assert_eq!(r.host.cpu_pct, 0.0);
        assert_eq!(r.host.mem_total_bytes, 0);
        assert_eq!(r.host.n_procs, 0);
        assert!(r.host.load_1m.is_none());
    }

    #[test]
    fn error_response_wraps_code_and_message() {
        let e = ErrorResponse::new("bad_query", "no q field");
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"schema\":\"gtop/v1\""));
        assert!(s.contains("\"code\":\"bad_query\""));
        assert!(s.contains("\"message\":\"no q field\""));
    }

    #[test]
    fn timestamp_is_rfc3339_shaped() {
        let ts = rfc3339_now();
        // YYYY-MM-DDTHH:MM:SSZ
        assert_eq!(ts.len(), 20);
        assert_eq!(ts.as_bytes()[10], b'T');
        assert!(ts.ends_with('Z'));
    }
}
