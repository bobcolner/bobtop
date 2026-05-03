//! Tier 2 — userspace packet capture via libpcap.
//!
//! ## Linux design
//!
//! 1. **Capture loop (one OS thread per interface).** Open every UP, non-loopback
//!    interface in promiscuous mode. For each captured packet, extract the
//!    L4 5-tuple (src_ip, src_port, dst_ip, dst_port, protocol) and the wire
//!    length, then push onto a shared `(tuple, dir, len, ts)` queue.
//! 2. **Inode cache.** A timer task refreshes `/proc/net/{tcp,tcp6,udp,udp6}`
//!    every 500 ms into a map `tuple → inode`. Captured packets look up
//!    their inode here.
//! 3. **Inode → pid cache.** A separate timer walks `/proc/[pid]/fd/` every
//!    1 s into a map `inode → pid`. Refresh is rate-limited because the
//!    walk is the hot path; new processes will appear in the next refresh.
//! 4. **Per-pid byte accumulator.** A `HashMap<pid, (rx_bytes, tx_bytes)>`
//!    is bumped on each packet. `sample()` swaps the current map with an
//!    empty one and divides by the elapsed wall-clock interval to compute
//!    bytes/second per pid.
//!
//! ## Privileges
//!
//! libpcap on Linux requires either `CAP_NET_RAW` (recommended via
//! `setcap cap_net_raw=ep ./bobtop`) or root. `available()` probes for the
//! capability before claiming the tier.
//!
//! ## macOS
//!
//! macOS has the `pktap` pseudo-interface (`pktap,any`) which embeds the
//! originating PID directly in per-packet metadata, sidestepping the inode
//! mapping problem. This implementation focuses on Linux first; macOS support
//! lands as a follow-up.
//!
//! ## Status
//!
//! Available behind `feature = "pcap"`. The implementation depends on the
//! `pcap` crate's optional libpcap headers; on systems without the headers,
//! `cargo build --features pcap` will fail with a linker error.

#![cfg(feature = "pcap")]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::sample::{AddrEndpoint, ConnectionInfo, Protocol, SocketState};
use crate::{AttributorTier, NetError, NetworkAttributor, ProcessNetSample, Result};

mod parser;

/// Cap on the per-pid byte accumulator before we force a flush. Prevents
/// runaway memory growth if `sample()` is never called.
const MAX_PIDS_TRACKED: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct FlowKey {
    src: AddrEndpoint,
    dst: AddrEndpoint,
    proto: Protocol,
}

#[derive(Debug, Default)]
struct ByteCounters {
    rx: u64,
    tx: u64,
}

#[derive(Debug)]
struct Shared {
    /// Per-pid accumulated bytes since last `sample()`.
    bytes: Mutex<HashMap<u32, ByteCounters>>,
    /// Inode → pid (refreshed every ~1 s by a background task).
    inode_to_pid: Mutex<HashMap<u64, u32>>,
    /// Flow tuple → inode (refreshed every ~500 ms).
    flow_to_inode: Mutex<HashMap<FlowKey, u64>>,
    /// Wall-clock instant of the previous `sample()` call. Used to convert
    /// accumulated bytes into bytes/second.
    last_sample_at: Mutex<Instant>,
}

pub struct PcapAttributor {
    shared: Arc<Shared>,
    /// Capture / cache-refresh threads. Held to keep them alive; drop = exit.
    _threads: Vec<thread::JoinHandle<()>>,
}

impl std::fmt::Debug for PcapAttributor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PcapAttributor")
            .field("threads", &self._threads.len())
            .finish()
    }
}

impl PcapAttributor {
    pub fn new() -> Result<Self> {
        let shared = Arc::new(Shared {
            bytes: Mutex::new(HashMap::new()),
            inode_to_pid: Mutex::new(HashMap::new()),
            flow_to_inode: Mutex::new(HashMap::new()),
            last_sample_at: Mutex::new(Instant::now()),
        });

        let mut threads = Vec::new();
        // Capture threads — one per UP, non-loopback device.
        for dev in pickable_devices()? {
            let s = Arc::clone(&shared);
            let name = dev.name.clone();
            let handle = thread::Builder::new()
                .name(format!("bobtop-pcap-{}", name))
                .spawn(move || capture_loop(name, s))
                .map_err(|e| NetError::other(format!("spawn capture thread: {e}")))?;
            threads.push(handle);
        }

        // Cache refresh — flow→inode every 500 ms, inode→pid every 1 s.
        {
            let s = Arc::clone(&shared);
            let h = thread::Builder::new()
                .name("bobtop-pcap-cache".into())
                .spawn(move || cache_refresh_loop(s))
                .map_err(|e| NetError::other(format!("spawn cache thread: {e}")))?;
            threads.push(h);
        }

        Ok(Self {
            shared,
            _threads: threads,
        })
    }
}

#[async_trait]
impl NetworkAttributor for PcapAttributor {
    async fn sample(&self) -> Result<Vec<ProcessNetSample>> {
        let s = Arc::clone(&self.shared);
        tokio::task::spawn_blocking(move || sample_blocking(&s))
            .await
            .map_err(|e| NetError::other(format!("pcap join: {e}")))?
    }

    fn tier(&self) -> AttributorTier {
        AttributorTier::PcapUserspace
    }

    fn available() -> bool {
        // Probe: try to enumerate devices. `pcap::Device::list` requires
        // CAP_NET_RAW (or root) on Linux to actually open them, but listing
        // is permitted unprivileged. We additionally check that we can open
        // at least one device — that's the real capability test.
        match pcap::Device::list() {
            Ok(devs) => devs
                .into_iter()
                .filter(|d| !is_loopback(d) && is_up(d))
                .any(|d| pcap::Capture::from_device(d).and_then(|c| c.timeout(50).open()).is_ok()),
            Err(_) => false,
        }
    }
}

fn sample_blocking(shared: &Shared) -> Result<Vec<ProcessNetSample>> {
    let now = Instant::now();
    let dt = {
        let mut last = shared
            .last_sample_at
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dt = now.duration_since(*last).as_secs_f64().max(0.001);
        *last = now;
        dt
    };
    // Swap accumulator out so the capture threads don't block on us.
    let bytes = {
        let mut b = shared.bytes.lock().unwrap_or_else(|p| p.into_inner());
        std::mem::take(&mut *b)
    };

    let mut out: Vec<ProcessNetSample> = Vec::with_capacity(bytes.len());
    for (pid, counters) in bytes {
        let name = read_proc_comm(pid).unwrap_or_else(|| format!("pid:{pid}"));
        out.push(ProcessNetSample {
            pid,
            name,
            rx_bytes_per_sec: Some(counters.rx as f64 / dt),
            tx_bytes_per_sec: Some(counters.tx as f64 / dt),
            connections: Vec::new(),
            attributor_tier: AttributorTier::PcapUserspace,
        });
    }
    Ok(out)
}

fn read_proc_comm(pid: u32) -> Option<String> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(s.trim_end_matches('\n').to_string())
}

fn pickable_devices() -> Result<Vec<pcap::Device>> {
    pcap::Device::list()
        .map(|all| {
            all.into_iter()
                .filter(|d| !is_loopback(d) && is_up(d))
                .collect()
        })
        .map_err(|e| NetError::Backend {
            backend: "pcap",
            source: Box::new(e),
        })
}

fn is_loopback(d: &pcap::Device) -> bool {
    d.flags.is_loopback() || d.name == "lo" || d.name == "lo0"
}

fn is_up(d: &pcap::Device) -> bool {
    d.flags.is_up() && d.flags.is_running()
}

fn capture_loop(name: String, shared: Arc<Shared>) {
    let dev = match pcap::Device::list()
        .ok()
        .and_then(|list| list.into_iter().find(|d| d.name == name))
    {
        Some(d) => d,
        None => {
            tracing::warn!(iface = %name, "pcap device disappeared");
            return;
        }
    };
    let cap_result = pcap::Capture::from_device(dev)
        .and_then(|c| c.snaplen(96).timeout(100).immediate_mode(true).open());
    let mut cap = match cap_result {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(iface = %name, error = %e, "pcap open failed (need CAP_NET_RAW?)");
            return;
        }
    };
    loop {
        match cap.next_packet() {
            Ok(packet) => {
                if let Some((flow, len)) = parser::parse_l4(packet.data) {
                    attribute_packet(&shared, flow, len);
                }
            }
            Err(pcap::Error::TimeoutExpired) => continue,
            Err(_) => return,
        }
    }
}

fn attribute_packet(shared: &Shared, flow: FlowKey, wire_len: u32) {
    // Look up the local-endpoint inode. We try the flow as-is (outbound) and
    // its reverse (inbound) — the proc table is keyed by *local* (ip, port).
    let inode = {
        let cache = shared.flow_to_inode.lock().unwrap_or_else(|p| p.into_inner());
        cache
            .get(&local_view(&flow, true))
            .or_else(|| cache.get(&local_view(&flow, false)))
            .copied()
    };
    let Some(inode) = inode else {
        return;
    };
    let pid = {
        let cache = shared.inode_to_pid.lock().unwrap_or_else(|p| p.into_inner());
        cache.get(&inode).copied()
    };
    let Some(pid) = pid else {
        return;
    };
    let mut bytes = shared.bytes.lock().unwrap_or_else(|p| p.into_inner());
    if bytes.len() >= MAX_PIDS_TRACKED && !bytes.contains_key(&pid) {
        return;
    }
    let entry = bytes.entry(pid).or_default();
    // Heuristic direction: if our cache has the flow keyed by the source
    // tuple, this packet is outbound (TX). Otherwise inbound (RX).
    let cache = shared.flow_to_inode.lock().unwrap_or_else(|p| p.into_inner());
    if cache.contains_key(&local_view(&flow, true)) {
        entry.tx += wire_len as u64;
    } else {
        entry.rx += wire_len as u64;
    }
}

/// Reduce a 5-tuple flow to a "local-endpoint" form for inode lookups.
/// `as_source = true` returns the flow keyed by its src; `false` by its dst.
fn local_view(flow: &FlowKey, as_source: bool) -> FlowKey {
    if as_source {
        FlowKey {
            src: flow.src,
            dst: zero_endpoint(flow.dst),
            proto: flow.proto,
        }
    } else {
        FlowKey {
            src: flow.dst,
            dst: zero_endpoint(flow.src),
            proto: flow.proto,
        }
    }
}

fn zero_endpoint(e: AddrEndpoint) -> AddrEndpoint {
    match e {
        AddrEndpoint::V4 { .. } => AddrEndpoint::V4 {
            addr: std::net::Ipv4Addr::UNSPECIFIED,
            port: 0,
        },
        AddrEndpoint::V6 { .. } => AddrEndpoint::V6 {
            addr: std::net::Ipv6Addr::UNSPECIFIED,
            port: 0,
        },
    }
}

fn cache_refresh_loop(shared: Arc<Shared>) {
    let mut last_pid_refresh = Instant::now() - Duration::from_secs(10);
    loop {
        // Flow → inode: refresh every 500 ms.
        if let Ok(map) = read_flow_to_inode_cache() {
            let mut g = shared.flow_to_inode.lock().unwrap_or_else(|p| p.into_inner());
            *g = map;
        }
        // Inode → pid: refresh every 1 s.
        if last_pid_refresh.elapsed() >= Duration::from_millis(1000) {
            if let Ok(map) = read_inode_to_pid() {
                let mut g = shared.inode_to_pid.lock().unwrap_or_else(|p| p.into_inner());
                *g = map;
            }
            last_pid_refresh = Instant::now();
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn read_flow_to_inode_cache() -> std::io::Result<HashMap<FlowKey, u64>> {
    let mut out = HashMap::new();
    for (path, ipv6, proto) in [
        ("/proc/net/tcp", false, Protocol::Tcp),
        ("/proc/net/tcp6", true, Protocol::Tcp),
        ("/proc/net/udp", false, Protocol::Udp),
        ("/proc/net/udp6", true, Protocol::Udp),
    ] {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        parser::parse_proc_net_table(&text, ipv6, proto, &mut out);
    }
    Ok(out)
}

fn read_inode_to_pid() -> std::io::Result<HashMap<u64, u32>> {
    use std::os::unix::ffi::OsStrExt;

    let mut out = HashMap::new();
    let dir = std::fs::read_dir("/proc")?;
    for entry in dir.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let fd_dir = entry.path().join("fd");
        let Ok(fds) = std::fs::read_dir(&fd_dir) else {
            continue;
        };
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else {
                continue;
            };
            let bytes = target.as_os_str().as_bytes();
            let prefix = b"socket:[";
            if !bytes.starts_with(prefix) || !bytes.ends_with(b"]") {
                continue;
            }
            let inner = &bytes[prefix.len()..bytes.len() - 1];
            let Some(inode) = std::str::from_utf8(inner).ok().and_then(|s| s.parse().ok()) else {
                continue;
            };
            out.insert(inode, pid);
        }
    }
    Ok(out)
}

// Re-export connection construction for tests / future use.
#[allow(dead_code)]
fn _connection(local: AddrEndpoint, remote: AddrEndpoint, proto: Protocol) -> ConnectionInfo {
    ConnectionInfo {
        local,
        remote,
        state: SocketState::Established,
        protocol: proto,
    }
}
