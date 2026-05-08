// SPDX-License-Identifier: GPL-2.0
//
// bobtop per-process network attribution — Tier 3 BPF program.
//
// Two kprobes track every TCP byte that crosses userspace:
//   - tcp_sendmsg(struct sock *sk, struct msghdr *msg, size_t size)
//       PT_REGS_PARM3 = `size`, the *requested* send length. We accumulate
//       this as TX. (The kernel may send slightly less if the socket buffer
//       is full, but tcp_sendmsg won't return until everything's queued, so
//       size is the right number for "what userspace asked to send".)
//   - tcp_cleanup_rbuf(struct sock *sk, int copied)
//       PT_REGS_PARM2 = `copied`, the number of bytes the receive path is
//       acknowledging back to the sender. Equivalent to "bytes the user
//       actually consumed" — preferred over tcp_recvmsg because that fires
//       even on ENOTCONN/EAGAIN and double-counts retransmits.
//
// Both update a single BPF_MAP_TYPE_HASH keyed by tgid (process ID, NOT
// thread ID — userspace cares about processes, not threads).
//
// License: GPL-2.0 because we use the kprobe BPF helper which is GPL-only
// in the kernel verifier.

#include <linux/bpf.h>
#include <linux/ptrace.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>

char LICENSE[] SEC("license") = "GPL";

struct pid_bytes {
    __u64 rx;
    __u64 tx;
};

// Keyed by tgid (top-level pid). Sized to handle a busy server (4096 procs).
struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 4096);
    __type(key, __u32);
    __type(value, struct pid_bytes);
} pid_bytes_map SEC(".maps");

static __always_inline void bump(__u32 tgid, __u64 rx_delta, __u64 tx_delta)
{
    struct pid_bytes *cur = bpf_map_lookup_elem(&pid_bytes_map, &tgid);
    if (cur) {
        // Hash map values are inline; in-place add is safe & lock-free.
        __sync_fetch_and_add(&cur->rx, rx_delta);
        __sync_fetch_and_add(&cur->tx, tx_delta);
        return;
    }
    struct pid_bytes init = { .rx = rx_delta, .tx = tx_delta };
    bpf_map_update_elem(&pid_bytes_map, &tgid, &init, BPF_NOEXIST);
}

// Use __u64 instead of size_t — the BPF header set doesn't pull in <stddef.h>,
// and on the kernel ABI sock_ops invokes for x86_64/arm64, size_t and __u64
// have identical layout so reading PT_REGS_PARM3 produces the same value.
SEC("kprobe/tcp_sendmsg")
int BPF_KPROBE(probe_tcp_sendmsg, void *sk, void *msg, __u64 size)
{
    __u32 tgid = bpf_get_current_pid_tgid() >> 32;
    if (tgid == 0)
        return 0;
    bump(tgid, 0, size);
    return 0;
}

SEC("kprobe/tcp_cleanup_rbuf")
int BPF_KPROBE(probe_tcp_cleanup_rbuf, void *sk, int copied)
{
    if (copied <= 0)
        return 0;
    __u32 tgid = bpf_get_current_pid_tgid() >> 32;
    if (tgid == 0)
        return 0;
    bump(tgid, (__u64)copied, 0);
    return 0;
}
