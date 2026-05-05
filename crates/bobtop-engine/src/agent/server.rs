//! Unix-socket JSON-RPC server.
//!
//! ## Lifecycle
//!
//! - Bind path: `$XDG_RUNTIME_DIR/bobtop.sock` if set, else `/tmp/bobtop-$UID.sock`.
//! - Single-instance: if the socket file exists, probe it; if a peer answers,
//!   we exit without binding (another bobtop owns the socket). If the probe
//!   fails (stale socket from a crashed previous run), we unlink and rebind.
//! - On `Drop` of the listener task, the socket file is removed best-effort.
//!
//! ## Wire format
//!
//! Line-delimited JSON: each request is a single line ending in `\n`,
//! each response likewise. Connections are kept open for multiple
//! request/response pairs to amortize accept/connect cost.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bobtop_core::{History, SampleStore};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use super::query::{
    find_pid, history_metric_for, match_summary, parse_window, resolve_pid_by_match, run_top,
    Group, MatchPattern, Metric, DEFAULT_N,
};
use super::schema::{
    build_peak, build_responsible, build_snapshot, build_window, ErrorResponse, HostSummary,
    PidInspectResponse, Request, SummaryResponse, SCHEMA_VERSION,
};

/// Resolve the socket path using the first available of:
/// 1. `$XDG_RUNTIME_DIR/bobtop.sock`
/// 2. `/tmp/bobtop-$UID.sock`
pub fn socket_path() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("bobtop.sock");
        }
    }
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/tmp/bobtop-{uid}.sock"))
}

/// Handle returned by [`spawn`]: the bound socket path plus an atomic
/// timestamp the listener bumps on every dispatched request. Daemon
/// mode uses the timestamp to implement an idle-exit watchdog without
/// the server itself having to know about lifecycle.
#[derive(Debug, Clone)]
pub struct AgentHandle {
    pub socket_path: PathBuf,
    pub last_activity: Arc<AtomicU64>,
}

/// Spawn the listener task. Returns a handle to the bound socket on
/// success.
///
/// Failures (path collision with a live peer, permission denied) are logged
/// and the daemon keeps running without the agent surface — the TUI is the
/// primary product, the socket is supplementary.
pub fn spawn(store: SampleStore, history: History) -> Option<AgentHandle> {
    let path = socket_path();
    let listener = match bind_with_stale_recovery(&path) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "agent socket unavailable");
            return None;
        }
    };
    tracing::info!(path = %path.display(), "agent socket listening");
    // Initialize to startup time so a freshly-launched daemon doesn't
    // idle-exit before any agent has had a chance to connect.
    let last_activity = Arc::new(AtomicU64::new(now_unix()));
    let path_for_task = path.clone();
    let activity_for_task = Arc::clone(&last_activity);
    tokio::spawn(async move {
        run_accept_loop(listener, store, history, activity_for_task).await;
        // Best-effort cleanup. Leaving a stale socket is recovered from on
        // next startup, so failure here is non-fatal.
        let _ = std::fs::remove_file(&path_for_task);
    });
    Some(AgentHandle {
        socket_path: path,
        last_activity,
    })
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Bind, recovering from a stale socket file left by a crashed predecessor.
/// Returns an error if a live peer is already listening on the path.
fn bind_with_stale_recovery(path: &Path) -> io::Result<UnixListener> {
    match UnixListener::bind(path) {
        Ok(l) => Ok(l),
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
            // Probe the existing socket synchronously. A live peer accepts;
            // a stale file refuses with ECONNREFUSED.
            match std::os::unix::net::UnixStream::connect(path) {
                Ok(_) => Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "another bobtop is listening on this socket",
                )),
                Err(_) => {
                    let _ = std::fs::remove_file(path);
                    UnixListener::bind(path)
                }
            }
        }
        Err(e) => Err(e),
    }
}

async fn run_accept_loop(
    listener: UnixListener,
    store: SampleStore,
    history: History,
    last_activity: Arc<AtomicU64>,
) {
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let s = store.clone();
                let h = history.clone();
                let a = Arc::clone(&last_activity);
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, s, h, a).await {
                        tracing::debug!(error = %e, "agent connection ended");
                    }
                });
            }
            Err(e) => {
                tracing::warn!(error = %e, "agent accept failed");
                // Brief back-off; if the listener itself is broken we keep
                // logging until the daemon exits.
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        }
    }
}

/// Max bytes accepted in a single request line. Defends against a
/// malformed or hostile peer streaming an unbounded line and OOM'ing
/// the daemon. Real requests are <1 KB; 64 KB is generous headroom.
const MAX_REQUEST_BYTES: usize = 64 * 1024;

/// Drop connections that have been silent for this long. The typical
/// request-response cycle is sub-millisecond, so a 5-minute idle is
/// already extreme — anything beyond is almost certainly a forgotten
/// client holding a tokio task hostage.
const CONNECTION_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

async fn handle_connection(
    stream: UnixStream,
    store: SampleStore,
    history: History,
    last_activity: Arc<AtomicU64>,
) -> io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    // Cap the buffered reader's read budget per `read_line` call so a
    // peer can't trickle bytes forever without a newline.
    let mut reader = BufReader::with_capacity(8 * 1024, reader).take(MAX_REQUEST_BYTES as u64);
    let mut line = String::new();
    loop {
        line.clear();
        // Per-line read with idle timeout. EOF (n == 0) and timeout
        // both shut the connection cleanly.
        let n = match tokio::time::timeout(
            CONNECTION_IDLE_TIMEOUT,
            reader.read_line(&mut line),
        )
        .await
        {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return Err(e),
            Err(_elapsed) => return Ok(()), // idle timeout
        };
        if n == 0 {
            return Ok(()); // peer closed
        }
        if line.len() >= MAX_REQUEST_BYTES && !line.ends_with('\n') {
            // Hit the size cap mid-line; reject this request and drop
            // the connection — keeping it open after a half-read line
            // would desync subsequent requests.
            let err = serde_json::to_string(&ErrorResponse::new(
                "bad_query",
                format!("request exceeds {MAX_REQUEST_BYTES} bytes"),
            ))
            .unwrap_or_default();
            let _ = writer.write_all(err.as_bytes()).await;
            let _ = writer.write_all(b"\n").await;
            let _ = writer.flush().await;
            return Ok(());
        }
        // After each successful line, refill the take() budget so the
        // connection stays usable for the next request.
        reader.set_limit(MAX_REQUEST_BYTES as u64);
        last_activity.store(now_unix(), Ordering::Relaxed);
        let response = dispatch(line.trim(), &store, &history);
        writer.write_all(response.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
    }
}

/// Parse, dispatch, and serialize. Always returns a single JSON line —
/// `serde_json::to_string` never emits `\n`, so the line-delimited contract
/// is preserved without explicit scrubbing.
fn dispatch(raw: &str, store: &SampleStore, history: &History) -> String {
    let req: Request = match serde_json::from_str(raw) {
        Ok(r) => r,
        Err(e) => {
            return encode(&ErrorResponse::new(
                "bad_query",
                format!("invalid JSON: {e}"),
            ));
        }
    };
    match req.q.as_str() {
        "snapshot" => encode(&build_snapshot(&store.latest())),
        "top" => handle_top(req, store),
        "window" => handle_window(req, history),
        "peak" => handle_peak(req, history),
        "summary" => handle_summary(req, store),
        "pid_inspect" => handle_pid_inspect(req, store),
        "responsible_for" => handle_responsible_for(req, history),
        other => encode(&ErrorResponse::new(
            "unknown_verb",
            format!("verb '{other}' is not supported in bobtop/v1 yet"),
        )),
    }
}

fn handle_top(req: Request, store: &SampleStore) -> String {
    let metric_raw = match req.by.as_deref() {
        Some(s) => s,
        None => {
            return encode(&ErrorResponse::new(
                "bad_query",
                "`top` requires `by` (cpu | mem | net.tx | net.rx | disk.r | disk.w)",
            ))
        }
    };
    let metric = match Metric::parse(metric_raw) {
        Some(m) => m,
        None => {
            return encode(&ErrorResponse::new(
                "unknown_metric",
                format!("unknown metric '{metric_raw}'"),
            ))
        }
    };
    let group = match req.group.as_deref() {
        None => Group::Flat,
        Some(s) => match Group::parse(s) {
            Some(g) => g,
            None => {
                return encode(&ErrorResponse::new(
                    "bad_query",
                    format!(
                        "group '{s}' not supported in bobtop/v1 (use `flat` or `exec`)"
                    ),
                ))
            }
        },
    };
    let n = req.n.unwrap_or(DEFAULT_N);
    let pat = req.match_.as_deref().map(MatchPattern::new);
    let resp = run_top(&store.latest(), metric, n, group, pat.as_ref());
    encode(&resp)
}

fn handle_summary(req: Request, store: &SampleStore) -> String {
    let snap = store.latest();
    // Pid-scoped summary — single-pid drilldown that returns the same
    // SummaryResponse shape so clients can parse uniformly.
    if let Some(pid) = req.pid {
        let p = match find_pid(&snap, pid) {
            Some(p) => p,
            None => {
                return encode(&ErrorResponse::new(
                    "pid_not_found",
                    format!("pid {pid} not in current snapshot"),
                ))
            }
        };
        return encode(&SummaryResponse {
            schema: SCHEMA_VERSION,
            ts: super::schema::rfc3339_now_pub(),
            scope: "pid".into(),
            host: HostSummary {
                cpu_pct: p.cpu_fraction * 100.0,
                mem_used_bytes: p.mem_rss_bytes,
                mem_total_bytes: 0,
                swap_used_bytes: 0,
                swap_total_bytes: 0,
                load_1m: None,
                load_5m: None,
                load_15m: None,
                net_rx_bps: p.net_rx_bytes_per_sec.unwrap_or(0.0) as u64,
                net_tx_bps: p.net_tx_bytes_per_sec.unwrap_or(0.0) as u64,
                disk_r_bps: p.disk_read_bytes_per_sec.unwrap_or(0.0) as u64,
                disk_w_bps: p.disk_write_bytes_per_sec.unwrap_or(0.0) as u64,
                n_procs: 1,
            },
            pid_count: 1,
            matched: Some(p.name.clone()),
        });
    }
    // Match-scoped summary — aggregate over a process family.
    if let Some(pat_raw) = req.match_.as_deref() {
        let pat = MatchPattern::new(pat_raw);
        let agg = match match_summary(&snap, Some(&pat)) {
            Some(a) => a,
            None => {
                return encode(&ErrorResponse::new(
                    "pid_not_found",
                    format!("no pids match '{pat_raw}'"),
                ))
            }
        };
        return encode(&SummaryResponse {
            schema: SCHEMA_VERSION,
            ts: super::schema::rfc3339_now_pub(),
            scope: "match".into(),
            host: HostSummary {
                cpu_pct: agg.cpu_pct,
                mem_used_bytes: agg.mem_bytes,
                mem_total_bytes: 0,
                swap_used_bytes: 0,
                swap_total_bytes: 0,
                load_1m: None,
                load_5m: None,
                load_15m: None,
                net_rx_bps: agg.net_rx_bps,
                net_tx_bps: agg.net_tx_bps,
                disk_r_bps: agg.disk_r_bps,
                disk_w_bps: agg.disk_w_bps,
                n_procs: agg.pid_count,
            },
            pid_count: agg.pid_count,
            matched: Some(pat_raw.into()),
        });
    }
    // Default: host scope. Reuse the snapshot builder's HostSummary.
    let snap_resp = build_snapshot(&snap);
    encode(&SummaryResponse {
        schema: SCHEMA_VERSION,
        ts: snap_resp.ts.clone(),
        scope: "host".into(),
        host: snap_resp.host,
        pid_count: 0,
        matched: None,
    })
}

fn handle_pid_inspect(req: Request, store: &SampleStore) -> String {
    let snap = store.latest();
    // Two ways to identify the target: explicit `pid` field or a `match`
    // pattern that resolves to exactly one pid.
    let pid: u32 = if let Some(p) = req.pid {
        p
    } else if let Some(pat_raw) = req.match_.as_deref() {
        let pat = MatchPattern::new(pat_raw);
        match resolve_pid_by_match(&snap, &pat) {
            Ok(Some(p)) => p,
            Ok(None) => {
                return encode(&ErrorResponse::new(
                    "pid_not_found",
                    format!("no pid matches '{pat_raw}'"),
                ))
            }
            Err(count) => {
                return encode(&ErrorResponse::new(
                    "bad_query",
                    format!(
                        "match '{pat_raw}' is ambiguous ({count}+ pids); \
                         use --pid <n> or a stricter pattern"
                    ),
                ))
            }
        }
    } else {
        return encode(&ErrorResponse::new(
            "bad_query",
            "`pid_inspect` requires --pid <n> or --match <pattern>",
        ));
    };
    let p = match find_pid(&snap, pid) {
        Some(p) => p,
        None => {
            return encode(&ErrorResponse::new(
                "pid_not_found",
                format!("pid {pid} not in current snapshot"),
            ))
        }
    };
    encode(&PidInspectResponse {
        schema: SCHEMA_VERSION,
        ts: super::schema::rfc3339_now_pub(),
        pid: p.pid,
        parent_pid: p.parent_pid,
        name: p.name.clone(),
        cmdline: p.cmdline.clone(),
        user: p.user.clone(),
        state: format!("{:?}", p.state),
        cpu_pct: p.cpu_fraction * 100.0,
        mem_rss_bytes: p.mem_rss_bytes,
        mem_vsz_bytes: p.mem_vsz_bytes,
        threads: p.threads,
        net_rx_bps: p.net_rx_bytes_per_sec,
        net_tx_bps: p.net_tx_bytes_per_sec,
        disk_r_bps: p.disk_read_bytes_per_sec,
        disk_w_bps: p.disk_write_bytes_per_sec,
        cgroup: p.cgroup.clone(),
    })
}

fn handle_responsible_for(req: Request, history: &History) -> String {
    let metric_raw = match req.metric.as_deref() {
        Some(s) => s.to_string(),
        None => {
            return encode(&ErrorResponse::new(
                "bad_query",
                "`responsible_for` requires `metric` (cpu | mem | net.tx | net.rx)",
            ))
        }
    };
    let metric = match Metric::parse(&metric_raw) {
        Some(m) => m,
        None => {
            return encode(&ErrorResponse::new(
                "unknown_metric",
                format!("unknown metric '{metric_raw}'"),
            ))
        }
    };
    let hist_metric = match history_metric_for(metric) {
        Ok(m) => m,
        Err(msg) => return encode(&ErrorResponse::new("unknown_metric", msg)),
    };
    let at_raw = match req.at.as_deref() {
        Some(s) => s.to_string(),
        None => {
            return encode(&ErrorResponse::new(
                "bad_query",
                "`responsible_for` requires `at` (e.g. 30s, 5m)",
            ))
        }
    };
    let at = match parse_window(&at_raw) {
        Ok(d) => d,
        Err(msg) => return encode(&ErrorResponse::new("bad_query", msg)),
    };
    match history.responsible_at(at, hist_metric) {
        Some(refs) => encode(&build_responsible(&metric_raw, &at_raw, at.as_secs(), refs)),
        None => encode(&ErrorResponse::new(
            "window_unavailable",
            format!("no history sample at offset {at_raw}"),
        )),
    }
}

fn handle_window(req: Request, history: &History) -> String {
    let (metric, metric_raw, window_raw, window) = match parse_history_args(&req) {
        Ok(t) => t,
        Err(s) => return s,
    };
    let hist_metric = match history_metric_for(metric) {
        Ok(m) => m,
        Err(msg) => return encode(&ErrorResponse::new("unknown_metric", msg)),
    };
    match history.host_stats(window, hist_metric) {
        Some(stats) => encode(&build_window(&metric_raw, &window_raw, stats)),
        None => encode(&ErrorResponse::new(
            "window_unavailable",
            "no history samples in the requested window yet",
        )),
    }
}

fn handle_peak(req: Request, history: &History) -> String {
    let (metric, metric_raw, window_raw, window) = match parse_history_args(&req) {
        Ok(t) => t,
        Err(s) => return s,
    };
    let hist_metric = match history_metric_for(metric) {
        Ok(m) => m,
        Err(msg) => return encode(&ErrorResponse::new("unknown_metric", msg)),
    };
    match history.peak(window, hist_metric) {
        Some(p) => encode(&build_peak(&metric_raw, &window_raw, p)),
        None => encode(&ErrorResponse::new(
            "window_unavailable",
            "no history samples in the requested window yet",
        )),
    }
}

/// Shared parameter parsing for `window` and `peak` — both take the same
/// `metric` + `window` pair. Returns the parsed metric, the raw strings
/// (for echoing back in the response), and the parsed Duration. Errors
/// are pre-encoded so callers can `return` them directly.
fn parse_history_args(
    req: &Request,
) -> Result<(Metric, String, String, std::time::Duration), String> {
    let metric_raw = req
        .metric
        .as_deref()
        .ok_or_else(|| {
            encode(&ErrorResponse::new(
                "bad_query",
                "`metric` is required (cpu | mem | net.tx | net.rx)",
            ))
        })?
        .to_string();
    let metric = Metric::parse(&metric_raw).ok_or_else(|| {
        encode(&ErrorResponse::new(
            "unknown_metric",
            format!("unknown metric '{metric_raw}'"),
        ))
    })?;
    let window_raw = req
        .window
        .as_deref()
        .ok_or_else(|| {
            encode(&ErrorResponse::new(
                "bad_query",
                "`window` is required (e.g. 30s, 1m, 5m, 30m)",
            ))
        })?
        .to_string();
    let window = parse_window(&window_raw)
        .map_err(|msg| encode(&ErrorResponse::new("bad_query", msg)))?;
    Ok((metric, metric_raw, window_raw, window))
}

fn encode<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|e| {
        // `serde_json::to_string` only fails on cycle/non-string-key
        // structures — none of our schema types contain those, so this
        // branch is effectively unreachable, but stays as a safe fallback.
        format!(
            r#"{{"schema":"bobtop/v1","error":{{"code":"internal","message":"serialize: {}"}}}}"#,
            e
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bobtop_core::DataBus;
    use std::time::{Duration, Instant};

    fn store_with_dummy_data() -> (DataBus, SampleStore, History) {
        let bus = DataBus::default();
        let store = SampleStore::spawn(&bus);
        let history = History::spawn(store.clone());
        bus.publish(bobtop_core::sample::MemorySample {
            timestamp: Instant::now(),
            total_bytes: 16_000_000_000,
            used_bytes: 4_000_000_000,
            available_bytes: 12_000_000_000,
            swap_total_bytes: 0,
            swap_used_bytes: 0,
            huge_pages: None,
            cached_bytes: 0,
            buffers_bytes: 0,
            free_bytes: 12_000_000_000,
            pressure: None,
        });
        (bus, store, history)
    }

    #[tokio::test]
    async fn snapshot_returns_v1_envelope() {
        let (_bus, store, history) = store_with_dummy_data();
        // Wait briefly for the store updater task to fold the publish.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let resp = dispatch(r#"{"q":"snapshot"}"#, &store, &history);
        assert!(resp.contains("\"schema\":\"bobtop/v1\""));
        assert!(resp.contains("\"mem_used_bytes\":4000000000"));
        assert!(resp.contains("\"mem_total_bytes\":16000000000"));
    }

    #[tokio::test]
    async fn unknown_verb_returns_error() {
        let bus = DataBus::default();
        let store = SampleStore::spawn(&bus);
        let history = History::spawn(store.clone());
        let resp = dispatch(r#"{"q":"definitely_not_a_verb"}"#, &store, &history);
        assert!(resp.contains("\"code\":\"unknown_verb\""));
    }

    #[tokio::test]
    async fn malformed_json_returns_bad_query() {
        let bus = DataBus::default();
        let store = SampleStore::spawn(&bus);
        let history = History::spawn(store.clone());
        let resp = dispatch("not json at all", &store, &history);
        assert!(resp.contains("\"code\":\"bad_query\""));
    }

    #[tokio::test]
    async fn window_requires_metric_and_window() {
        let bus = DataBus::default();
        let store = SampleStore::spawn(&bus);
        let history = History::spawn(store.clone());
        let r = dispatch(r#"{"q":"window"}"#, &store, &history);
        assert!(r.contains("\"code\":\"bad_query\""));
        let r = dispatch(r#"{"q":"window","metric":"cpu"}"#, &store, &history);
        assert!(r.contains("\"code\":\"bad_query\""));
    }

    #[tokio::test]
    async fn peak_rejects_unknown_metric() {
        let bus = DataBus::default();
        let store = SampleStore::spawn(&bus);
        let history = History::spawn(store.clone());
        let r = dispatch(
            r#"{"q":"peak","metric":"nonsense","window":"1m"}"#,
            &store,
            &history,
        );
        assert!(r.contains("\"code\":\"unknown_metric\""));
    }

    /// Test helper: store seeded with a small process list.
    fn store_with_processes() -> (DataBus, SampleStore, History) {
        use bobtop_core::sample::{ProcessInfo, ProcessSample, ProcessState};
        let bus = DataBus::default();
        let store = SampleStore::spawn(&bus);
        let history = History::spawn(store.clone());
        let mk = |pid, name: &str, cpu: f32, mem: u64| ProcessInfo {
            pid,
            parent_pid: None,
            name: name.into(),
            cmdline: format!("{name} arg"),
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
        };
        bus.publish(ProcessSample {
            timestamp: Instant::now(),
            processes: vec![
                mk(101, "node", 0.5, 1024),
                mk(102, "node", 0.2, 512),
                mk(103, "redis", 0.1, 256),
            ],
        });
        (bus, store, history)
    }

    async fn wait_for_processes(store: &SampleStore) {
        // Bus → SampleStore is async; poll briefly until the publish lands.
        for _ in 0..20 {
            if store.latest().processes.is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("ProcessSample never folded into SampleStore");
    }

    #[tokio::test]
    async fn top_cgroup_dispatches_with_no_cgroup_bucket() {
        let (_bus, store, history) = store_with_processes();
        wait_for_processes(&store).await;
        let r = dispatch(
            r#"{"q":"top","by":"cpu","group":"cgroup"}"#,
            &store,
            &history,
        );
        assert!(r.contains("\"group\":\"cgroup\""));
        // None of the processes had a cgroup; they all land in (no cgroup).
        assert!(r.contains("(no cgroup)"));
    }

    #[tokio::test]
    async fn top_tree_dispatches() {
        let (_bus, store, history) = store_with_processes();
        wait_for_processes(&store).await;
        let r = dispatch(
            r#"{"q":"top","by":"cpu","group":"tree"}"#,
            &store,
            &history,
        );
        assert!(r.contains("\"group\":\"tree\""));
        assert!(r.contains("\"kind\":\"tree\""));
    }

    #[tokio::test]
    async fn summary_match_returns_aggregate() {
        let (_bus, store, history) = store_with_processes();
        wait_for_processes(&store).await;
        let r = dispatch(r#"{"q":"summary","match":"node"}"#, &store, &history);
        assert!(r.contains("\"scope\":\"match\""));
        // Two `node` pids → 1024 + 512 = 1536 bytes RSS.
        assert!(r.contains("\"mem_used_bytes\":1536"));
        assert!(r.contains("\"pid_count\":2"));
    }

    #[tokio::test]
    async fn summary_pid_returns_drilldown() {
        let (_bus, store, history) = store_with_processes();
        wait_for_processes(&store).await;
        let r = dispatch(r#"{"q":"summary","pid":101}"#, &store, &history);
        assert!(r.contains("\"scope\":\"pid\""));
        assert!(r.contains("\"matched\":\"node\""));
    }

    #[tokio::test]
    async fn summary_pid_not_found_errors() {
        let (_bus, store, history) = store_with_processes();
        wait_for_processes(&store).await;
        let r = dispatch(r#"{"q":"summary","pid":999999}"#, &store, &history);
        assert!(r.contains("\"code\":\"pid_not_found\""));
    }

    #[tokio::test]
    async fn pid_inspect_by_pid_returns_full_detail() {
        let (_bus, store, history) = store_with_processes();
        wait_for_processes(&store).await;
        let r = dispatch(r#"{"q":"pid_inspect","pid":103}"#, &store, &history);
        assert!(r.contains("\"pid\":103"));
        assert!(r.contains("\"name\":\"redis\""));
        assert!(r.contains("\"mem_rss_bytes\":256"));
    }

    #[tokio::test]
    async fn pid_inspect_match_resolves_uniquely() {
        let (_bus, store, history) = store_with_processes();
        wait_for_processes(&store).await;
        let r = dispatch(
            r#"{"q":"pid_inspect","match":"redis"}"#,
            &store,
            &history,
        );
        assert!(r.contains("\"pid\":103"));
    }

    #[tokio::test]
    async fn pid_inspect_match_ambiguous_errors() {
        let (_bus, store, history) = store_with_processes();
        wait_for_processes(&store).await;
        // "node" matches two pids → bad_query with count.
        let r = dispatch(
            r#"{"q":"pid_inspect","match":"node"}"#,
            &store,
            &history,
        );
        assert!(r.contains("\"code\":\"bad_query\""));
        assert!(r.contains("ambiguous"));
    }

    #[tokio::test]
    async fn responsible_for_requires_metric_and_at() {
        let bus = DataBus::default();
        let store = SampleStore::spawn(&bus);
        let history = History::spawn(store.clone());
        let r = dispatch(r#"{"q":"responsible_for"}"#, &store, &history);
        assert!(r.contains("\"code\":\"bad_query\""));
        let r = dispatch(
            r#"{"q":"responsible_for","metric":"cpu"}"#,
            &store,
            &history,
        );
        assert!(r.contains("\"code\":\"bad_query\""));
    }

    #[tokio::test]
    async fn oversized_request_is_rejected() {
        // End-to-end via a Unix socket pair so we exercise the real
        // `handle_connection` path (size cap + connection drop).
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let (a, b) = tokio::net::UnixStream::pair().expect("pair");
        let bus = DataBus::default();
        let store = SampleStore::spawn(&bus);
        let history = History::spawn(store.clone());
        let activity = Arc::new(AtomicU64::new(now_unix()));
        let server_task = tokio::spawn(handle_connection(b, store, history, activity));
        // Stream more than MAX_REQUEST_BYTES without a newline.
        let big = vec![b'x'; MAX_REQUEST_BYTES + 1024];
        let mut a = a;
        let _ = a.write_all(&big).await;
        // Server should respond with a bad_query error then close.
        let mut buf = Vec::new();
        let _ = a.read_to_end(&mut buf).await;
        let resp = String::from_utf8_lossy(&buf);
        assert!(
            resp.contains("\"code\":\"bad_query\""),
            "expected bad_query, got: {resp}"
        );
        let _ = server_task.await;
    }

    #[tokio::test]
    async fn responsible_for_returns_window_unavailable_when_empty() {
        let bus = DataBus::default();
        let store = SampleStore::spawn(&bus);
        let history = History::spawn(store.clone());
        let r = dispatch(
            r#"{"q":"responsible_for","metric":"cpu","at":"5s"}"#,
            &store,
            &history,
        );
        // No history samples yet → window_unavailable.
        assert!(r.contains("\"code\":\"window_unavailable\""));
    }
}
