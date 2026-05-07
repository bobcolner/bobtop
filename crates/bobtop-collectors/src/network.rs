//! Per-interface network counter collector (Step 8).
//!
//! - **Linux:** parses `/proc/net/dev`. Holds the previous raw counter
//!   snapshot keyed by interface name and computes rates as `(new - old) /
//!   wall_clock_delta`. Counter resets (driver reload) clamp to 0.
//! - **macOS:** placeholder — returns [`bobtop_core::CoreError::Unsupported`].
//!   Real impl will use `getifaddrs` + the `IFDATA_*` sysctls.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bobtop_core::sample::{InterfaceSample, NetworkSample};
use bobtop_core::{Collector, Result};
#[cfg(not(target_os = "linux"))]
use bobtop_core::CoreError;

const DEFAULT_INTERVAL_MS: u64 = 1000;

#[derive(Debug, Clone, Copy, Default)]
struct RawCounters {
    rx_bytes: u64,
    rx_packets: u64,
    rx_errors: u64,
    tx_bytes: u64,
    tx_packets: u64,
    tx_errors: u64,
}

#[derive(Debug)]
pub struct NetworkGlobalCollector {
    interval: Duration,
    last: Mutex<HashMap<String, (RawCounters, Instant)>>,
}

impl NetworkGlobalCollector {
    pub fn new() -> Self {
        Self::with_interval(Duration::from_millis(DEFAULT_INTERVAL_MS))
    }

    pub fn with_interval(interval: Duration) -> Self {
        Self {
            interval,
            last: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for NetworkGlobalCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Collector for NetworkGlobalCollector {
    type Sample = NetworkSample;

    async fn collect(&self) -> Result<NetworkSample> {
        let now = Instant::now();
        let raw = read_raw()?;
        let mut last = self
            .last
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        let mut interfaces = Vec::with_capacity(raw.len());
        for (name, cur) in raw.iter() {
            let sample = match last.get(name) {
                Some((prev, prev_t)) => {
                    let dt = now.duration_since(*prev_t).as_secs_f64().max(0.001);
                    InterfaceSample {
                        name: name.clone(),
                        rx_bytes_per_sec: rate(cur.rx_bytes, prev.rx_bytes, dt),
                        tx_bytes_per_sec: rate(cur.tx_bytes, prev.tx_bytes, dt),
                        rx_packets_per_sec: rate(cur.rx_packets, prev.rx_packets, dt),
                        tx_packets_per_sec: rate(cur.tx_packets, prev.tx_packets, dt),
                        rx_errors: cur.rx_errors,
                        tx_errors: cur.tx_errors,
                    }
                }
                None => InterfaceSample {
                    name: name.clone(),
                    rx_bytes_per_sec: 0.0,
                    tx_bytes_per_sec: 0.0,
                    rx_packets_per_sec: 0.0,
                    tx_packets_per_sec: 0.0,
                    rx_errors: cur.rx_errors,
                    tx_errors: cur.tx_errors,
                },
            };
            interfaces.push(sample);
        }

        // Replace cache wholesale (drops disappeared interfaces).
        last.clear();
        for (name, cur) in raw {
            last.insert(name, (cur, now));
        }

        // Stable order: alphabetical by interface name.
        interfaces.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(NetworkSample {
            timestamp: now,
            interfaces,
        })
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn name(&self) -> &'static str {
        "network"
    }
}

fn rate(cur: u64, prev: u64, dt_seconds: f64) -> f64 {
    if cur < prev {
        0.0 // counter reset
    } else {
        (cur - prev) as f64 / dt_seconds
    }
}

#[cfg(target_os = "linux")]
fn read_raw() -> Result<HashMap<String, RawCounters>> {
    let text = std::fs::read_to_string("/proc/net/dev")?;
    Ok(parse_proc_net_dev(&text))
}

#[cfg(not(target_os = "linux"))]
fn read_raw() -> Result<HashMap<String, RawCounters>> {
    Err(CoreError::Unsupported)
}

#[cfg(target_os = "linux")]
fn parse_proc_net_dev(text: &str) -> HashMap<String, RawCounters> {
    let mut out = HashMap::new();
    // First two lines are headers.
    for line in text.lines().skip(2) {
        let Some((name_part, counters_part)) = line.split_once(':') else {
            continue;
        };
        let name = name_part.trim().to_string();
        if name.is_empty() {
            continue;
        }
        // Field layout (16 columns):
        //   rx: bytes(0), packets(1), errs(2), drop, fifo, frame, compressed, multicast
        //   tx: bytes(8), packets(9), errs(10), drop, fifo, colls, carrier, compressed
        // Walk the iterator once into a fixed-size array — avoids the per-line
        // Vec<u64> alloc the previous version did.
        let mut fields = [0u64; 16];
        let mut count = 0usize;
        let mut malformed = false;
        for (i, tok) in counters_part.split_ascii_whitespace().take(16).enumerate() {
            match tok.parse() {
                Ok(v) => {
                    fields[i] = v;
                    count = i + 1;
                }
                Err(_) => {
                    malformed = true;
                    break;
                }
            }
        }
        if malformed || count < 16 {
            continue;
        }
        out.insert(
            name,
            RawCounters {
                rx_bytes: fields[0],
                rx_packets: fields[1],
                rx_errors: fields[2],
                tx_bytes: fields[8],
                tx_packets: fields[9],
                tx_errors: fields[10],
            },
        );
    }
    out
}

/// What kind of network interface this is — drives whether traffic on
/// it counts as "leaving the host" (External / Tunnel) or "staying
/// inside the host" (Container / Loopback) in the UI breakdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetInterfaceKind {
    /// Physical-or-equivalent NIC: ethernet, wifi, cellular, or any
    /// interface name that doesn't match a known virtual prefix. The
    /// kernel doesn't expose "is physical" cleanly, so this is the
    /// fallthrough — anything we don't recognize is treated as real.
    External,
    /// VPN / point-to-point tunnel (tun, tap, wg, ppp). Carries
    /// outbound traffic just like an NIC, so "external" totals
    /// include this kind.
    Tunnel,
    /// Container bridge or virtual ethernet (docker0, br-*, veth*,
    /// virbr*). Traffic stays on the host.
    Container,
    /// Loopback. Always stays on the host.
    Loopback,
}

impl NetInterfaceKind {
    /// Whether traffic on interfaces of this kind leaves the host.
    /// Used by the UI to split the "external" vs "internal" totals.
    pub fn is_external(self) -> bool {
        matches!(self, NetInterfaceKind::External | NetInterfaceKind::Tunnel)
    }
}

/// Classify an interface by its kernel name. Heuristic based on
/// well-known prefixes; on Linux the canonical mappings are stable
/// (kernel net device naming guidelines + container runtime
/// conventions).
pub fn classify_interface(name: &str) -> NetInterfaceKind {
    if matches!(name, "lo" | "lo0") {
        return NetInterfaceKind::Loopback;
    }
    if name.starts_with("docker")
        || name.starts_with("br-")
        || name.starts_with("veth")
        || name.starts_with("virbr")
        || name.starts_with("cni")
    {
        return NetInterfaceKind::Container;
    }
    if name.starts_with("tun")
        || name.starts_with("tap")
        || name.starts_with("wg")
        || name.starts_with("ppp")
    {
        return NetInterfaceKind::Tunnel;
    }
    NetInterfaceKind::External
}

/// Heuristic — interfaces that should be excluded from "real internet"
/// totals (loopback, docker, vlan). Kept as a thin wrapper over
/// [`classify_interface`] for callers that only need the binary
/// view; new code should use [`classify_interface`] directly.
pub fn is_virtual_interface(name: &str) -> bool {
    !classify_interface(name).is_external()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    const SAMPLE: &str = "Inter-|   Receive                                                |  Transmit\n face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n    lo: 1234567    2000    0    0    0     0          0         0  1234567    2000    0    0    0     0       0          0\n  eth0: 9000000  10000    1    0    0     0          0         0   500000    5000    0    0    0     0       0          0\n";

    #[test]
    fn parses_two_interfaces_with_correct_columns() {
        let map = parse_proc_net_dev(SAMPLE);
        assert_eq!(map.len(), 2);
        let lo = map.get("lo").unwrap();
        assert_eq!(lo.rx_bytes, 1_234_567);
        assert_eq!(lo.rx_packets, 2000);
        let eth = map.get("eth0").unwrap();
        assert_eq!(eth.rx_bytes, 9_000_000);
        assert_eq!(eth.tx_bytes, 500_000);
        assert_eq!(eth.rx_errors, 1);
    }

    #[tokio::test]
    async fn first_call_returns_zero_rates_subsequent_call_works() {
        let c = NetworkGlobalCollector::new();
        let s1 = c.collect().await.expect("first");
        for i in &s1.interfaces {
            assert_eq!(i.rx_bytes_per_sec, 0.0);
            assert_eq!(i.tx_bytes_per_sec, 0.0);
        }
        // Allow a moment to pass for nonzero-but-tiny rates.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let s2 = c.collect().await.expect("second");
        assert_eq!(s2.interfaces.len(), s1.interfaces.len());
    }

    #[test]
    fn counter_reset_clamps_to_zero() {
        assert_eq!(rate(100, 200, 1.0), 0.0);
        assert_eq!(rate(200, 100, 1.0), 100.0);
    }

    #[test]
    fn virtual_interfaces_classified() {
        assert!(is_virtual_interface("lo"));
        assert!(is_virtual_interface("docker0"));
        assert!(is_virtual_interface("veth123"));
        assert!(!is_virtual_interface("eth0"));
        assert!(!is_virtual_interface("wlp3s0"));
    }

    #[test]
    fn classify_distinguishes_tunnel_from_container() {
        assert_eq!(classify_interface("eth0"), NetInterfaceKind::External);
        assert_eq!(classify_interface("wlp3s0"), NetInterfaceKind::External);
        assert_eq!(classify_interface("lo"), NetInterfaceKind::Loopback);
        assert_eq!(classify_interface("docker0"), NetInterfaceKind::Container);
        assert_eq!(classify_interface("veth5a4b"), NetInterfaceKind::Container);
        assert_eq!(classify_interface("br-abcd"), NetInterfaceKind::Container);
        assert_eq!(classify_interface("virbr0"), NetInterfaceKind::Container);
        assert_eq!(classify_interface("tun0"), NetInterfaceKind::Tunnel);
        assert_eq!(classify_interface("wg0"), NetInterfaceKind::Tunnel);
        assert_eq!(classify_interface("ppp0"), NetInterfaceKind::Tunnel);
    }

    #[test]
    fn tunnels_count_as_external_for_outbound_totals() {
        assert!(NetInterfaceKind::External.is_external());
        assert!(NetInterfaceKind::Tunnel.is_external());
        assert!(!NetInterfaceKind::Container.is_external());
        assert!(!NetInterfaceKind::Loopback.is_external());
    }
}
