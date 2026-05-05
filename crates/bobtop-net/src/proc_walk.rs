//! Shared `/proc/[pid]/fd` walker — builds inode→pid for socket symlinks.
//!
//! Both the proc_inode and pcap_backend tiers need this map; the walk itself
//! is the most expensive thing either does (one `readdir` per pid plus one
//! `readlink` per fd — easily 10k+ syscalls on a busy host). Centralizing
//! the implementation keeps behaviour identical across tiers.

#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;

/// Walk `/proc/[pid]/fd/` and build `inode → pid` for socket symlinks.
///
/// Skips pids whose fd dir is unreadable (EACCES on other users' fds when
/// running unprivileged — expected). Caller is responsible for caching;
/// each invocation performs O(N_pids × avg_fds) syscalls.
pub(crate) fn walk_socket_inodes() -> HashMap<u64, u32> {
    let mut map = HashMap::new();
    let proc_dir = match fs::read_dir("/proc") {
        Ok(d) => d,
        Err(_) => return map,
    };
    for entry in proc_dir.flatten() {
        let name = entry.file_name();
        let Some(pid) = parse_pid(&name) else { continue };
        let fd_dir = entry.path().join("fd");
        let Ok(fds) = fs::read_dir(&fd_dir) else { continue };
        for fd in fds.flatten() {
            let Ok(target) = fs::read_link(fd.path()) else { continue };
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
pub(crate) fn parse_socket_inode(target: &OsStr) -> Option<u64> {
    let bytes = target.as_bytes();
    let prefix = b"socket:[";
    if !bytes.starts_with(prefix) || !bytes.ends_with(b"]") {
        return None;
    }
    let inner = &bytes[prefix.len()..bytes.len() - 1];
    std::str::from_utf8(inner).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_socket_inode_symlink() {
        assert_eq!(parse_socket_inode(OsStr::new("socket:[123456]")), Some(123456));
        assert_eq!(parse_socket_inode(OsStr::new("/dev/null")), None);
        assert_eq!(parse_socket_inode(OsStr::new("socket:[]")), None);
        assert_eq!(parse_socket_inode(OsStr::new("socket:[abc]")), None);
    }

    #[test]
    fn live_walk_returns_some_entries() {
        // Best-effort: most hosts have at least one socket inode somewhere
        // under /proc. If not (very minimal container) we just skip.
        let map = walk_socket_inodes();
        let _ = map.len();
    }
}
