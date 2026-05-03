//! Line-oriented parser for `/proc/net/tcp{,6}`.
//!
//! Format reminder:
//!
//! ```text
//!   sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid timeout inode
//!    0: 0100007F:13F8 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0       0 234023 1 ffff... 100 0 0 10 0
//! ```
//!
//! Field 1 = local_address, 2 = remote_address, 3 = state (hex byte), 9 = inode.
//! Address format: `HEX_IP:HEX_PORT` (little-endian for IPv4/v6 words).

use crate::sample::{AddrEndpoint, SocketState};

use super::{ipv4_from_le_hex, ipv6_from_le_hex, RawConn};

pub(super) fn parse_tcp_table(text: &str, ipv6: bool, out: &mut Vec<RawConn>) {
    // Skip the header line.
    for line in text.lines().skip(1) {
        if let Some(c) = parse_line(line, ipv6) {
            out.push(c);
        }
    }
}

fn parse_line(line: &str, ipv6: bool) -> Option<RawConn> {
    let mut it = line.split_ascii_whitespace();
    let _sl = it.next()?;
    let local = it.next()?;
    let remote = it.next()?;
    let state = it.next()?;
    let _tx_rx = it.next()?;
    let _tr_when = it.next()?;
    let _retrnsmt = it.next()?;
    let _uid = it.next()?;
    let _timeout = it.next()?;
    let inode = it.next()?;

    let local = parse_endpoint(local, ipv6)?;
    let remote = parse_endpoint(remote, ipv6)?;
    let state_byte = u8::from_str_radix(state, 16).ok()?;
    let state = SocketState::from_proc_hex(state_byte)?;
    let inode: u64 = inode.parse().ok()?;
    if inode == 0 {
        // Unbound or just-closed sockets — not actionable, skip.
        return None;
    }

    Some(RawConn {
        local,
        remote,
        state,
        inode,
    })
}

fn parse_endpoint(s: &str, ipv6: bool) -> Option<AddrEndpoint> {
    let (ip_hex, port_hex) = s.split_once(':')?;
    let port = u16::from_str_radix(port_hex, 16).ok()?;
    if ipv6 {
        match ipv6_from_le_hex(ip_hex)? {
            std::net::IpAddr::V6(addr) => Some(AddrEndpoint::V6 { addr, port }),
            std::net::IpAddr::V4(_) => None,
        }
    } else {
        match ipv4_from_le_hex(ip_hex)? {
            std::net::IpAddr::V4(addr) => Some(AddrEndpoint::V4 { addr, port }),
            std::net::IpAddr::V6(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    const SAMPLE_V4: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0100007F:13F8 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 234023 1 0000000000000000 100 0 0 10 0\n   1: 0100007F:A52A 0100007F:13F8 01 00000000:00000000 00:00000000 00000000  1000        0 234100 1 0000000000000000 20 4 30 10 -1\n";

    #[test]
    fn parses_two_v4_rows_skipping_header() {
        let mut out = Vec::new();
        parse_tcp_table(SAMPLE_V4, false, &mut out);
        assert_eq!(out.len(), 2);

        let listen = &out[0];
        assert_eq!(listen.state, SocketState::Listen);
        assert_eq!(listen.inode, 234023);
        assert_eq!(
            listen.local,
            AddrEndpoint::V4 {
                addr: Ipv4Addr::new(127, 0, 0, 1),
                port: 0x13F8,
            }
        );

        let established = &out[1];
        assert_eq!(established.state, SocketState::Established);
        assert_eq!(established.inode, 234100);
        assert_eq!(established.remote.port(), 0x13F8);
    }

    #[test]
    fn skips_inode_zero() {
        // TIME_WAIT and similar transient sockets sometimes appear with inode 0
        // and aren't process-attributable.
        let line = "   2: 0100007F:1234 0100007F:5678 06 00000000:00000000 00:00000000 00000000     0        0 0 0 0000000000000000\n";
        let header = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n";
        let mut out = Vec::new();
        parse_tcp_table(&format!("{header}{line}"), false, &mut out);
        assert!(out.is_empty(), "inode=0 rows must be filtered");
    }

    #[test]
    fn parses_ipv6_loopback() {
        let header = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n";
        // ::1 in /proc encoding = 00000000000000000000000001000000
        let row = "   0: 00000000000000000000000001000000:13F8 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 555 1 0000000000000000 100 0 0 10 0\n";
        let mut out = Vec::new();
        parse_tcp_table(&format!("{header}{row}"), true, &mut out);
        assert_eq!(out.len(), 1);
        let c = &out[0];
        assert!(matches!(c.local, AddrEndpoint::V6 { port: 0x13F8, .. }));
        assert_eq!(c.state, SocketState::Listen);
        assert_eq!(c.inode, 555);
    }
}
