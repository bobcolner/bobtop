// SPDX-License-Identifier: GPL-2.0
//
// bobtop per-process disk-I/O attribution — Tier 2 BPF program.
//
// Kretprobes on `vfs_read` / `vfs_write`. Inspecting the return value
// (ssize_t) gives us:
//   - positive: number of bytes actually transferred
//   - 0:        EOF (skip)
//   - negative: -errno (skip)
//
// VFS-layer attribution means:
//   1. PID is the *issuing* process — `bpf_get_current_pid_tgid()` returns
//      the user-space task that called read()/write(). No writeback
//      indirection like `block_rq_issue` suffers (where buffered writes
//      get credited to kworker/jbd2 threads instead of the originator).
//   2. Both cache hits and disk-bound I/O count, matching what users mean
//      when they ask "what is process X reading/writing on disk?". Device-
//      level pressure (cache-bypassing only) is already exposed via the
//      per-disk panel which reads /proc/diskstats.
//
// Map and probe naming intentionally avoids collision with bobtop_net.bpf.c
// so both can be loaded as separate aya::Ebpf instances without sharing
// the kernel-side program/map namespace.
//
// License: GPL-2.0 — kretprobe BPF helper is GPL-only in the verifier.

#include <linux/bpf.h>
#include <linux/ptrace.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>

char LICENSE[] SEC("license") = "GPL";

struct pid_disk_bytes {
    __u64 r;
    __u64 w;
};

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 4096);
    __type(key, __u32);
    __type(value, struct pid_disk_bytes);
} pid_disk_bytes_map SEC(".maps");

static __always_inline void bump_disk(__u32 tgid, __u64 r_delta, __u64 w_delta)
{
    struct pid_disk_bytes *cur = bpf_map_lookup_elem(&pid_disk_bytes_map, &tgid);
    if (cur) {
        __sync_fetch_and_add(&cur->r, r_delta);
        __sync_fetch_and_add(&cur->w, w_delta);
        return;
    }
    struct pid_disk_bytes init = { .r = r_delta, .w = w_delta };
    bpf_map_update_elem(&pid_disk_bytes_map, &tgid, &init, BPF_NOEXIST);
}

SEC("kretprobe/vfs_read")
int BPF_KRETPROBE(probe_vfs_read_ret, long ret)
{
    if (ret <= 0)
        return 0;
    __u32 tgid = bpf_get_current_pid_tgid() >> 32;
    if (tgid == 0)
        return 0;
    bump_disk(tgid, (__u64)ret, 0);
    return 0;
}

SEC("kretprobe/vfs_write")
int BPF_KRETPROBE(probe_vfs_write_ret, long ret)
{
    if (ret <= 0)
        return 0;
    __u32 tgid = bpf_get_current_pid_tgid() >> 32;
    if (tgid == 0)
        return 0;
    bump_disk(tgid, 0, (__u64)ret);
    return 0;
}
