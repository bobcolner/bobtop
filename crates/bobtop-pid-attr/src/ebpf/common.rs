//! Shared helpers used by both the network and disk eBPF tiers.
//!
//! Capability probes and `/proc/[pid]/comm` lookup. Loader plumbing
//! moved into the tier modules themselves now that libbpf-rs handles
//! the ELF/BTF/program-iteration details that aya forced us to do
//! by hand.

#![cfg(all(target_os = "linux", feature = "ebpf"))]

use std::path::Path;

pub(super) const CGROUP2_PATH: &str = "/sys/fs/cgroup";

pub(super) fn read_proc_comm(pid: u32) -> Option<String> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(s.trim_end_matches('\n').to_string())
}

pub(super) fn is_cgroup_v2_unified_mounted() -> bool {
    Path::new(CGROUP2_PATH).join("cgroup.controllers").exists()
}

pub(super) fn has_kernel_min_version(major: u32, minor: u32) -> bool {
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").ok();
    let Some(release) = release else { return false };
    let mut parts = release.split('.');
    let Some(maj) = parts.next().and_then(|s| s.parse::<u32>().ok()) else {
        return false;
    };
    let Some(min) = parts.next().and_then(|s| s.parse::<u32>().ok()) else {
        return false;
    };
    (maj, min) >= (major, minor)
}

pub(super) fn has_bpf_capability() -> bool {
    if uid_is_zero() {
        return true;
    }
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return false;
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("CapEff:") {
            if let Ok(mask) = u64::from_str_radix(rest.trim(), 16) {
                return mask & (1u64 << 39) != 0;
            }
        }
    }
    false
}

fn uid_is_zero() -> bool {
    let Ok(s) = std::fs::read_to_string("/proc/self/status") else {
        return false;
    };
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            return rest.split_ascii_whitespace().next() == Some("0");
        }
    }
    false
}
