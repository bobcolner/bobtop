//! Lightweight L2/L3/L4 dissector for the packets pcap hands us.
//!
//! Just enough to extract the 5-tuple + L3 wire length. We deliberately do
//! NOT pull in `etherparse` or `pnet` — they bring large dependency trees
//! and we only need a handful of fields.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};

use super::FlowKey;
use crate::sample::{AddrEndpoint, Protocol};

const ETHER_TYPE_IPV4: u16 = 0x0800;
const ETHER_TYPE_IPV6: u16 = 0x86DD;
const ETHER_TYPE_VLAN: u16 = 0x8100;
const IP_PROTO_TCP: u8 = 6;
const IP_PROTO_UDP: u8 = 17;

/// Parse an Ethernet frame and return `(flow_key, wire_length)` for the
/// L4 5-tuple if it's a TCP/UDP packet over IPv4 or IPv6.
pub fn parse_l4(frame: &[u8]) -> Option<(FlowKey, u32)> {
    if frame.len() < 14 {
        return None;
    }
    let mut etype = u16::from_be_bytes([frame[12], frame[13]]);
    let mut payload_offset = 14;
    // 802.1Q VLAN tag (single tag).
    if etype == ETHER_TYPE_VLAN && frame.len() >= 18 {
        etype = u16::from_be_bytes([frame[16], frame[17]]);
        payload_offset = 18;
    }
    let payload = frame.get(payload_offset..)?;
    match etype {
        ETHER_TYPE_IPV4 => parse_ipv4(payload),
        ETHER_TYPE_IPV6 => parse_ipv6(payload),
        _ => None,
    }
}

fn parse_ipv4(buf: &[u8]) -> Option<(FlowKey, u32)> {
    if buf.len() < 20 {
        return None;
    }
    let version_ihl = buf[0];
    if version_ihl >> 4 != 4 {
        return None;
    }
    let ihl = (version_ihl & 0x0F) as usize * 4;
    if buf.len() < ihl {
        return None;
    }
    let total_len = u16::from_be_bytes([buf[2], buf[3]]) as u32;
    let proto = buf[9];
    let src = Ipv4Addr::new(buf[12], buf[13], buf[14], buf[15]);
    let dst = Ipv4Addr::new(buf[16], buf[17], buf[18], buf[19]);
    let l4 = buf.get(ihl..)?;
    let (sport, dport, ip_proto) = parse_ports(proto, l4)?;
    Some((
        FlowKey {
            src: AddrEndpoint::V4 { addr: src, port: sport },
            dst: AddrEndpoint::V4 { addr: dst, port: dport },
            proto: ip_proto,
        },
        total_len,
    ))
}

fn parse_ipv6(buf: &[u8]) -> Option<(FlowKey, u32)> {
    if buf.len() < 40 {
        return None;
    }
    let version = buf[0] >> 4;
    if version != 6 {
        return None;
    }
    let payload_len = u16::from_be_bytes([buf[4], buf[5]]) as u32;
    let next_header = buf[6];
    let mut src_bytes = [0u8; 16];
    src_bytes.copy_from_slice(&buf[8..24]);
    let mut dst_bytes = [0u8; 16];
    dst_bytes.copy_from_slice(&buf[24..40]);
    let l4 = buf.get(40..)?;
    let (sport, dport, ip_proto) = parse_ports(next_header, l4)?;
    Some((
        FlowKey {
            src: AddrEndpoint::V6 {
                addr: Ipv6Addr::from(src_bytes),
                port: sport,
            },
            dst: AddrEndpoint::V6 {
                addr: Ipv6Addr::from(dst_bytes),
                port: dport,
            },
            proto: ip_proto,
        },
        payload_len + 40,
    ))
}

fn parse_ports(proto: u8, l4: &[u8]) -> Option<(u16, u16, Protocol)> {
    match proto {
        IP_PROTO_TCP if l4.len() >= 4 => Some((
            u16::from_be_bytes([l4[0], l4[1]]),
            u16::from_be_bytes([l4[2], l4[3]]),
            Protocol::Tcp,
        )),
        IP_PROTO_UDP if l4.len() >= 4 => Some((
            u16::from_be_bytes([l4[0], l4[1]]),
            u16::from_be_bytes([l4[2], l4[3]]),
            Protocol::Udp,
        )),
        _ => None,
    }
}

/// Parse `/proc/net/{tcp,tcp6,udp,udp6}` into a "local-only" flow → inode
/// map. Captured packets look up by (local_endpoint, zero_remote, proto)
/// because the proc table lists *bound* endpoints, not full 5-tuples
/// against arbitrary peers.
pub fn parse_proc_net_table(
    text: &str,
    ipv6: bool,
    proto: Protocol,
    out: &mut HashMap<FlowKey, u64>,
) {
    for line in text.lines().skip(1) {
        let mut it = line.split_ascii_whitespace();
        let _sl = it.next();
        let Some(local) = it.next().and_then(|s| parse_endpoint(s, ipv6)) else {
            continue;
        };
        let _remote = it.next();
        let _state = it.next();
        let _txrx = it.next();
        let _trwhen = it.next();
        let _retr = it.next();
        let _uid = it.next();
        let _timeout = it.next();
        let Some(inode) = it.next().and_then(|s| s.parse::<u64>().ok()) else {
            continue;
        };
        if inode == 0 {
            continue;
        }
        let key = FlowKey {
            src: local,
            dst: zero_endpoint(local),
            proto,
        };
        out.insert(key, inode);
    }
}

fn parse_endpoint(s: &str, ipv6: bool) -> Option<AddrEndpoint> {
    let (ip_hex, port_hex) = s.split_once(':')?;
    let port = u16::from_str_radix(port_hex, 16).ok()?;
    if ipv6 {
        if ip_hex.len() != 32 {
            return None;
        }
        let mut bytes = [0u8; 16];
        for word in 0..4 {
            let chunk = &ip_hex[word * 8..word * 8 + 8];
            let raw = u32::from_str_radix(chunk, 16).ok()?;
            let le = raw.to_le_bytes();
            bytes[word * 4..word * 4 + 4].copy_from_slice(&le);
        }
        Some(AddrEndpoint::V6 {
            addr: Ipv6Addr::from(bytes),
            port,
        })
    } else {
        let raw = u32::from_str_radix(ip_hex, 16).ok()?;
        Some(AddrEndpoint::V4 {
            addr: Ipv4Addr::from(raw.to_le_bytes()),
            port,
        })
    }
}

fn zero_endpoint(e: AddrEndpoint) -> AddrEndpoint {
    match e {
        AddrEndpoint::V4 { .. } => AddrEndpoint::V4 {
            addr: Ipv4Addr::UNSPECIFIED,
            port: 0,
        },
        AddrEndpoint::V6 { .. } => AddrEndpoint::V6 {
            addr: Ipv6Addr::UNSPECIFIED,
            port: 0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_v4_tcp() -> Vec<u8> {
        // Eth: dst+src+etype(0800)
        let mut frame = vec![0u8; 14];
        frame[12..14].copy_from_slice(&0x0800u16.to_be_bytes());
        // IPv4 header (20 bytes, no options)
        let mut ip = [0u8; 20];
        ip[0] = 0x45; // ver=4, IHL=5
        ip[2..4].copy_from_slice(&60u16.to_be_bytes()); // total len = 20 + 20 + 20 payload
        ip[9] = IP_PROTO_TCP;
        ip[12..16].copy_from_slice(&[10, 0, 0, 1]);
        ip[16..20].copy_from_slice(&[8, 8, 8, 8]);
        frame.extend_from_slice(&ip);
        // TCP header (20 bytes minimal): src_port=80, dst_port=12345
        let mut tcp = [0u8; 20];
        tcp[0..2].copy_from_slice(&80u16.to_be_bytes());
        tcp[2..4].copy_from_slice(&12345u16.to_be_bytes());
        frame.extend_from_slice(&tcp);
        // 20 bytes of payload to match total_len
        frame.extend_from_slice(&[0u8; 20]);
        frame
    }

    #[test]
    fn parses_v4_tcp_5tuple_and_length() {
        let f = build_v4_tcp();
        let (flow, len) = parse_l4(&f).expect("parse");
        assert_eq!(len, 60);
        assert_eq!(flow.proto, Protocol::Tcp);
        assert!(matches!(flow.src, AddrEndpoint::V4 { port: 80, .. }));
        assert!(matches!(flow.dst, AddrEndpoint::V4 { port: 12345, .. }));
    }

    #[test]
    fn ignores_unknown_etype() {
        let mut frame = vec![0u8; 18];
        frame[12..14].copy_from_slice(&0x9999u16.to_be_bytes()); // not IP
        assert!(parse_l4(&frame).is_none());
    }

    #[test]
    fn proc_net_table_yields_local_inode_keys() {
        let text = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0100007F:13F8 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 999 1 0000000000000000 100 0 0 10 0\n";
        let mut out = HashMap::new();
        parse_proc_net_table(text, false, Protocol::Tcp, &mut out);
        assert_eq!(out.len(), 1);
        let (key, inode) = out.iter().next().unwrap();
        assert_eq!(*inode, 999);
        assert!(matches!(key.src, AddrEndpoint::V4 { port: 0x13F8, .. }));
        assert!(matches!(key.dst, AddrEndpoint::V4 { port: 0, .. }));
    }
}
