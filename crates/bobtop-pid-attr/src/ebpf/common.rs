//! Shared helpers used by both the network and disk eBPF tiers.
//!
//! Capability probes, RLIMIT_MEMLOCK fallback, and the kprobe attach
//! shortcut all live here so `ebpf::net` and `ebpf::disk` don't duplicate
//! them. Each tier loads its own BPF object (different probes, different
//! maps) but the privilege-detection plumbing is identical.

#![cfg(all(target_os = "linux", feature = "ebpf"))]
#![allow(unsafe_code)]

use std::path::Path;

use crate::{NetError, Result};

pub(super) const CGROUP2_PATH: &str = "/sys/fs/cgroup";

/// Recognise the specific aya/kernel error that means "increase RLIMIT_MEMLOCK".
/// On 5.11+ this is rare (memcg accounting); on older kernels or when memcg
/// is disabled, BPF map allocations check the rlimit. The kernel returns
/// EPERM with a message containing "memlock" or just `EAGAIN`/`ENOMEM` when
/// the rlimit is exhausted. We check the error text since aya doesn't
/// expose the underlying errno cleanly.
pub(super) fn is_memlock_error(err: &aya::EbpfError) -> bool {
    let s = err.to_string().to_ascii_lowercase();
    s.contains("memlock")
        || s.contains("rlimit")
        || s.contains("operation not permitted")
        || s.contains("cannot allocate memory")
}

pub(super) fn bump_memlock_rlimit() -> Result<()> {
    let lim = libc::rlimit {
        rlim_cur: libc::RLIM_INFINITY,
        rlim_max: libc::RLIM_INFINITY,
    };
    let rc = unsafe { libc::setrlimit(libc::RLIMIT_MEMLOCK, &lim) };
    if rc != 0 {
        return Err(NetError::other(format!(
            "failed to raise RLIMIT_MEMLOCK: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

/// Look up a BPF program in the loaded object, load it into the kernel,
/// and attach as a kprobe to `kernel_fn`.
pub(super) fn attach_kprobe(
    ebpf: &mut aya::Ebpf,
    prog_name: &str,
    kernel_fn: &str,
) -> Result<()> {
    let prog = ebpf
        .program_mut(prog_name)
        .ok_or_else(|| NetError::other(format!("BPF program `{prog_name}` not found in object")))?;
    let kprobe: &mut aya::programs::KProbe = prog
        .try_into()
        .map_err(|e: aya::programs::ProgramError| NetError::Backend {
            backend: "aya-program",
            source: Box::new(e),
        })?;
    kprobe.load().map_err(|e| NetError::Backend {
        backend: "aya-kprobe-load",
        source: Box::new(e),
    })?;
    kprobe.attach(kernel_fn, 0).map_err(|e| NetError::Backend {
        backend: "aya-kprobe-attach",
        source: Box::new(e),
    })?;
    Ok(())
}

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

/// Standard load+retry for a BPF object. Tries to load WITHOUT raising
/// RLIMIT_MEMLOCK first (modern kernels charge to memcg). If load fails
/// with what looks like a memlock error, raises the rlimit and retries.
/// All other errors propagate immediately.
pub(super) fn load_with_memlock_fallback(bpf_object: &[u8]) -> Result<aya::Ebpf> {
    match aya::EbpfLoader::new().load(bpf_object) {
        Ok(e) => Ok(e),
        Err(e) if is_memlock_error(&e) => {
            tracing::debug!("BPF load hit memlock limit, raising RLIMIT_MEMLOCK and retrying");
            bump_memlock_rlimit()?;
            aya::EbpfLoader::new()
                .load(bpf_object)
                .map_err(|e| NetError::Backend { backend: "aya-load", source: Box::new(e) })
        }
        Err(e) => Err(NetError::Backend {
            backend: "aya-load",
            source: Box::new(e),
        }),
    }
}
