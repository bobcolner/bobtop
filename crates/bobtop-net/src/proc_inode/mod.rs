//! Tier 1 — Linux `/proc/net/tcp` + `/proc/[pid]/fd` inode walk.
//!
//! Enumerates TCP/TCP6 connections and joins them to processes via socket
//! inodes. No per-process bandwidth — that's what Tiers 2 and 3 are for.
//!
//! Privileges: none required, but a non-root bobtop will only see processes
//! it owns. Sockets owned by other users will appear unattributed (under
//! pid 0). We log this once at startup but don't error.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use async_trait::async_trait;
use tracing::trace;

use crate::sample::{AddrEndpoint, ConnectionInfo, Protocol, SocketState};
use crate::{AttributorTier, NetError, NetworkAttributor, ProcessNetSample, Result};

mod parse;

#[derive(Debug, Default)]
pub struct ProcInodeAttributor;

impl ProcInodeAttributor {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl NetworkAttributor for ProcInodeAttributor {
    async fn sample(&self) -> Result<Vec<ProcessNetSample>> {
        // /proc walks are blocking syscalls — keep them off the runtime.
        tokio::task::spawn_blocking(sample_blocking)
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

fn sample_blocking() -> Result<Vec<ProcessNetSample>> {
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

    // 2. Build inode → pid map by walking /proc/[pid]/fd/.
    let inode_to_pid = build_inode_pid_map();

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

/// One row from `/proc/net/tcp{,6}` after parsing.
#[derive(Debug)]
pub(crate) struct RawConn {
    pub local: AddrEndpoint,
    pub remote: AddrEndpoint,
    pub state: SocketState,
    pub inode: u64,
}

fn build_inode_pid_map() -> HashMap<u64, u32> {
    let mut map = HashMap::new();
    let proc_dir = match fs::read_dir("/proc") {
        Ok(d) => d,
        Err(_) => return map,
    };
    for entry in proc_dir.flatten() {
        let name = entry.file_name();
        let Some(pid) = parse_pid(&name) else { continue };
        let fd_dir = entry.path().join("fd");
        let Ok(fds) = fs::read_dir(&fd_dir) else {
            // EACCES on other users' fds when running unprivileged — expected.
            continue;
        };
        for fd in fds.flatten() {
            let Ok(target) = fs::read_link(fd.path()) else {
                continue;
            };
            let Some(inode) = parse_socket_inode(target.as_os_str()) else {
                continue;
            };
            map.insert(inode, pid);
        }
    }
    map
}

fn parse_pid(name: &OsStr) -> Option<u32> {
    name.to_str()?.parse().ok()
}

/// Match `socket:[NNN]` symlink targets — the `NNN` is the inode.
fn parse_socket_inode(target: &OsStr) -> Option<u64> {
    let bytes = target.as_bytes();
    let prefix = b"socket:[";
    if !bytes.starts_with(prefix) || !bytes.ends_with(b"]") {
        return None;
    }
    let inner = &bytes[prefix.len()..bytes.len() - 1];
    std::str::from_utf8(inner).ok()?.parse().ok()
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

    #[test]
    fn parses_socket_inode_symlink() {
        let target = OsStr::new("socket:[123456]");
        assert_eq!(parse_socket_inode(target), Some(123456));
        assert_eq!(parse_socket_inode(OsStr::new("/dev/null")), None);
        assert_eq!(parse_socket_inode(OsStr::new("socket:[]")), None);
    }
}
