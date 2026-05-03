use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttributorTier {
    /// Tier 3 — Linux + `CAP_BPF`. Exact byte counts via sock_ops / kprobes.
    EbpfKernel,
    /// Tier 2 — `CAP_NET_RAW` + libpcap. Sampled bytes, exact connections.
    PcapUserspace,
    /// Tier 1 — Linux `/proc/net/tcp` + `/proc/[pid]/fd` inode walk. Connections only.
    ProcInode,
    /// Tier 1 — macOS `proc_pidinfo(PROC_PIDLISTFDS)`. Connections only.
    ProcPidInfo,
    /// Tier 0 — no backend available. Net columns hidden in the TUI.
    Unavailable,
}

impl AttributorTier {
    pub fn name(self) -> &'static str {
        match self {
            Self::EbpfKernel => "ebpf",
            Self::PcapUserspace => "pcap",
            Self::ProcInode => "proc_inode",
            Self::ProcPidInfo => "proc_pidinfo",
            Self::Unavailable => "unavailable",
        }
    }

    /// `true` when this tier reports per-process bandwidth, `false` when it
    /// only enumerates connections. Drives column visibility in the TUI.
    pub fn has_bandwidth(self) -> bool {
        matches!(self, Self::EbpfKernel | Self::PcapUserspace)
    }
}
