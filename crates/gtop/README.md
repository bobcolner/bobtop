# gtop

`gtop` is a terminal system monitor in Rust with a btop-style TUI,
async sampling, and per-process network bandwidth attribution. Linux
first; macOS support is best-effort.

## Install

```bash
cargo install gtop
```

Optional features:

| feature | adds | system deps |
|---|---|---|
| `ebpf` (default) | Tier 3 per-process bandwidth attribution | `clang`, `libbpf-dev` |
| `pcap` | Tier 2 per-process bandwidth attribution | `libpcap-dev` |

```bash
# Linux + libpcap
sudo apt install libpcap-dev
cargo install gtop --features pcap

# Linux + eBPF
sudo apt install clang libbpf-dev
cargo install gtop --features ebpf
```

## Run

```bash
gtop
gtop --theme tokyo-night
gtop --list-themes
gtop --help-keys
gtop --layout minimal
gtop --tick-ms 500
RUST_LOG=info gtop
```

Sticky preferences live at `~/.config/gtop/gtop.toml`. Edit live in
the TUI via the `O` overlay.

## What you get

- CPU / memory / disk / network / process panels in a tight,
  themable Ratatui layout.
- Per-process network attribution with fallback tiers (eBPF →
  libpcap → `/proc`); pick one with capabilities, or run as root.
- Process clustering by executable, cgroup, container, parent, or
  flat. Container-aware grouping for Docker / Podman / containerd /
  LXC pulls names from runtime metadata — no daemon socket
  required.
- Live per-flow network panel (toggle `N`): pid · proc · remote ·
  state · ↓/s · ↑/s sorted by busiest.
- 41 btop `.theme` files bundled. Drop your own at
  `~/.config/gtop/themes/<name>.theme` or
  `~/.config/btop/themes/<name>.theme`.
- Headless daemon mode + agent socket for structured queries
  (`gtop agent snapshot`, `gtop agent top --by cpu`).

## Agent mode

```bash
gtop agent snapshot
gtop agent top --by cpu --n 5
gtop agent top --by mem --group exec --match '*chrome*'
gtop agent summary --match 'postgres'
gtop agent pid_inspect --pid 1234
```

Socket lives at `$XDG_RUNTIME_DIR/gtop.sock` (fallback
`/tmp/gtop-$UID.sock`); responses are line-delimited JSON with
schema `gtop/v1`. See the workspace README at
<https://github.com/bobcolner/bobtop> for the full agent verb
reference.

## Attribution tiers

| tier | backend | per-pid bytes | privilege |
|---|---|---|---|
| 3 | eBPF kprobes | exact | `CAP_BPF + CAP_PERFMON` or root |
| 2 | libpcap + inode map | sampled | `CAP_NET_RAW` or root |
| 1 | `/proc/net/tcp` walk | none | none |

Unlock tier 3 without root:

```bash
sudo setcap 'cap_bpf,cap_perfmon=ep' ~/.cargo/bin/gtop
```

## License

Dual-licensed under MIT or Apache-2.0 at your option. Bundled
`.theme` files come from
[aristocratos/btop](https://github.com/aristocratos/btop) and are
redistributed under Apache-2.0.
