use std::net::{Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};

use crate::AttributorTier;

/// Per-process network attribution snapshot.
///
/// Bandwidth fields are `Option<f64>` because Tier 1 backends (inode walks)
/// can enumerate connections but can't measure bytes. `None` means "we don't
/// know," which is *different* from `Some(0.0)`.
#[derive(Debug, Clone)]
pub struct ProcessNetSample {
    pub pid: u32,
    pub name: String,
    pub rx_bytes_per_sec: Option<f64>,
    pub tx_bytes_per_sec: Option<f64>,
    pub connections: Vec<ConnectionInfo>,
    pub attributor_tier: AttributorTier,
}

#[derive(Debug, Clone, Copy)]
pub struct ConnectionInfo {
    pub local: AddrEndpoint,
    pub remote: AddrEndpoint,
    pub state: SocketState,
    pub protocol: Protocol,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AddrEndpoint {
    V4 { addr: Ipv4Addr, port: u16 },
    V6 { addr: Ipv6Addr, port: u16 },
}

impl AddrEndpoint {
    pub fn port(&self) -> u16 {
        match self {
            Self::V4 { port, .. } | Self::V6 { port, .. } => *port,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Protocol {
    Tcp,
    Udp,
}

/// TCP socket state. Matches the byte values used in `/proc/net/tcp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SocketState {
    Established = 0x01,
    SynSent = 0x02,
    SynRecv = 0x03,
    FinWait1 = 0x04,
    FinWait2 = 0x05,
    TimeWait = 0x06,
    Close = 0x07,
    CloseWait = 0x08,
    LastAck = 0x09,
    Listen = 0x0A,
    Closing = 0x0B,
    NewSynRecv = 0x0C,
}

impl SocketState {
    pub fn from_proc_hex(byte: u8) -> Option<Self> {
        Some(match byte {
            0x01 => Self::Established,
            0x02 => Self::SynSent,
            0x03 => Self::SynRecv,
            0x04 => Self::FinWait1,
            0x05 => Self::FinWait2,
            0x06 => Self::TimeWait,
            0x07 => Self::Close,
            0x08 => Self::CloseWait,
            0x09 => Self::LastAck,
            0x0A => Self::Listen,
            0x0B => Self::Closing,
            0x0C => Self::NewSynRecv,
            _ => return None,
        })
    }
}
