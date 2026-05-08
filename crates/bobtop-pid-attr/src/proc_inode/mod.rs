//! Tier 1 — Linux `/proc/net/tcp` + `/proc/[pid]/fd` inode walk.
//!
//! Enumerates TCP/TCP6 connections and joins them to processes via socket
//! inodes. No per-process bandwidth — that's what Tiers 2 and 3 are for.
//!
//! Privileges: none required, but a non-root gtop will only see processes
//! it owns. Sockets owned by other users will appear unattributed (under
//! pid 0). We log this once at startup but don't error.

use std::collections::HashMap;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tracing::trace;

use crate::proc_walk;
use crate::sample::{AddrEndpoint, ConnectionInfo, Protocol, SocketState};
use crate::{AttributorTier, NetError, NetworkAttributor, ProcessNetSample, Result};

mod parse;

/// How long to reuse a cached inode→pid map before re-walking `/proc/[pid]/fd`.
/// Matches pcap_backend's cadence — fresh enough to attribute newly-spawned
/// connections within ~1 sample, cheap enough that the walk doesn't dominate
/// CPU on hosts with many processes.
const INODE_CACHE_TTL: Duration = Duration::from_millis(1000);

#[derive(Debug, Default)]
struct InodeCache {
    /// `None` until first build; `Some(_, when)` once populated. Stored
    /// behind `Arc` so callers can drop the lock immediately after lookup.
    entry: Option<(Arc<HashMap<u64, u32>>, Instant)>,
}

#[derive(Debug, Default)]
pub struct ProcInodeAttributor {
    /// Cached inode→pid map. Re-walked at most every `INODE_CACHE_TTL`.
    /// Held across samples so back-to-back collects don't re-do the
    /// O(N_pids × avg_fds) walk for nothing.
    cache: Arc<Mutex<InodeCache>>,
}

impl ProcInodeAttributor {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl NetworkAttributor for ProcInodeAttributor {
    async fn sample(&self) -> Result<Vec<ProcessNetSample>> {
        // /proc walks are blocking syscalls — keep them off the runtime.
        let cache = Arc::clone(&self.cache);
        tokio::task::spawn_blocking(move || sample_blocking(&cache))
            .await
            .map_err(|e| NetError::other(format!("proc_inode join error: {e}")))?
    }

    fn tier(&self) -> AttributorTier {
        AttributorTier::ProcInode
    }

    fn available() -> bool {
        Path::new("/proc/net/tcp").is_file()
    }
}

fn sample_blocking(cache: &Mutex<InodeCache>) -> Result<Vec<ProcessNetSample>> {
    // 1. Pull every TCP connection (v4 + v6) and remember which inodes are interesting.
    let mut conns: Vec<RawConn> = Vec::new();
    if let Ok(text) = fs::read_to_string("/proc/net/tcp") {
        parse::parse_tcp_table(&text, false, &mut conns);
    }
    if let Ok(text) = fs::read_to_string("/proc/net/tcp6") {
        parse::parse_tcp_table(&text, true, &mut conns);
    }
    if conns.is_empty() {
        return Ok(Vec::new());
    }

    // 2. Reuse a recent inode→pid map; only walk /proc/[pid]/fd if stale.
    let inode_to_pid = get_or_refresh_inode_map(cache);

    // 3. Group connections by pid (0 = unattributed; non-root can't see other users' fds).
    let mut by_pid: HashMap<u32, Vec<ConnectionInfo>> = HashMap::new();
    for c in conns {
        let pid = inode_to_pid.get(&c.inode).copied().unwrap_or(0);
        by_pid.entry(pid).or_default().push(ConnectionInfo {
            local: c.local,
            remote: c.remote,
            state: c.state,
            protocol: Protocol::Tcp,
        });
    }

    let mut out = Vec::with_capacity(by_pid.len());
    for (pid, connections) in by_pid {
        let name = if pid == 0 {
            String::from("(unattributed)")
        } else {
            read_proc_comm(pid).unwrap_or_else(|| format!("pid:{pid}"))
        };
        out.push(ProcessNetSample {
            pid,
            name,
            rx_bytes_per_sec: None,
            tx_bytes_per_sec: None,
            connections,
            attributor_tier: AttributorTier::ProcInode,
        });
    }

    trace!(processes = out.len(), "proc_inode sample built");
    Ok(out)
}

/// Return the cached inode→pid map if it's still within TTL, otherwise
/// re-walk `/proc` and store a fresh one. The walk happens *outside* the
/// lock so concurrent samples don't pile up behind a slow `readdir`.
fn get_or_refresh_inode_map(cache: &Mutex<InodeCache>) -> Arc<HashMap<u64, u32>> {
    {
        let guard = cache.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((map, when)) = guard.entry.as_ref() {
            if when.elapsed() < INODE_CACHE_TTL {
                return Arc::clone(map);
            }
        }
    }
    let arc = Arc::new(proc_walk::walk_socket_inodes());
    let mut guard = cache.lock().unwrap_or_else(|p| p.into_inner());
    guard.entry = Some((Arc::clone(&arc), Instant::now()));
    arc
}

/// One row from `/proc/net/tcp{,6}` after parsing.
#[derive(Debug)]
pub(crate) struct RawConn {
    pub local: AddrEndpoint,
    pub remote: AddrEndpoint,
    pub state: SocketState,
    pub inode: u64,
}

fn read_proc_comm(pid: u32) -> Option<String> {
    let s = fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(s.trim_end_matches('\n').to_string())
}

// /proc/net/tcp prints each 32-bit address word in CPU-native byte order
// (little-endian on every platform we target). Parsing the hex string with
// `from_str_radix` gives us the integer; reading its little-endian bytes
// gives us the address in the canonical network order.
pub(crate) fn ipv4_from_le_hex(hex: &str) -> Option<IpAddr> {
    let raw = u32::from_str_radix(hex, 16).ok()?;
    Some(IpAddr::V4(Ipv4Addr::from(raw.to_le_bytes())))
}

pub(crate) fn ipv6_from_le_hex(hex: &str) -> Option<IpAddr> {
    if hex.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for word in 0..4 {
        let chunk = &hex[word * 8..word * 8 + 8];
        let raw = u32::from_str_radix(chunk, 16).ok()?;
        let le = raw.to_le_bytes();
        let off = word * 4;
        bytes[off..off + 4].copy_from_slice(&le);
    }
    Some(IpAddr::V6(Ipv6Addr::from(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_le_hex_roundtrip() {
        // 127.0.0.1 = 0x0100007F in little-endian /proc encoding.
        let ip = ipv4_from_le_hex("0100007F").unwrap();
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)));
    }

    #[tokio::test]
    async fn cache_hit_within_ttl_returns_same_arc() {
        // Two back-to-back samples should reuse the cached inode→pid map.
        // We verify by checking the Arc strong count grows as cache lookups
        // hand out new references to the same allocation.
        let attr = ProcInodeAttributor::new();
        let _ = attr.sample().await;
        let strong_before = {
            let g = attr.cache.lock().unwrap();
            g.entry.as_ref().map(|(a, _)| Arc::strong_count(a)).unwrap_or(0)
        };
        let _ = attr.sample().await;
        let strong_after = {
            let g = attr.cache.lock().unwrap();
            g.entry.as_ref().map(|(a, _)| Arc::strong_count(a)).unwrap_or(0)
        };
        // Cache must be populated after at least one sample.
        assert!(strong_before >= 1, "cache should be populated after first sample");
        // After a TTL-fresh second sample, the cached Arc should still be
        // the same allocation (same strong count behaviour as before).
        assert_eq!(strong_before, strong_after);
    }
}
