# bobtop

A best-in-class terminal system monitor in Rust — btop-quality visuals, async-first
runtime, and **per-process network bandwidth attribution** (which neither btop nor
btm provide).

Linux-first; macOS support is best-effort.

## Install

```bash
git clone https://github.com/bobcolner/bobtop
cd bobtop
cargo build --release -p bobtop-daemon
# binary at ./target/release/bobtop
```

Optional features:

| feature   | adds                                            | system deps                |
|-----------|-------------------------------------------------|----------------------------|
| `pcap`    | Tier 2 per-process bandwidth via libpcap        | `libpcap-dev`              |
| `ebpf`    | Tier 3 per-process bandwidth via eBPF kprobes   | `clang`, `libbpf-dev`      |
| `nvidia`  | NVIDIA GPU stats via NVML (not yet wired in UI) | proprietary NVIDIA drivers |

```bash
# Linux + libpcap
sudo apt install libpcap-dev
cargo build --release -p bobtop-daemon --features pcap

# Linux + eBPF (best per-process net data)
sudo apt install clang libbpf-dev
cargo build --release -p bobtop-daemon --features ebpf
```

## Run

```bash
./target/release/bobtop
```

Useful flags:

```bash
./target/release/bobtop --theme tokyo-night        # any of 41 bundled btop themes
./target/release/bobtop --list-themes              # print every theme name and exit
./target/release/bobtop --help-keys                # print all keybinds and exit
./target/release/bobtop --layout minimal           # cpu + processes only
./target/release/bobtop --tick-ms 500              # faster updates (default 1500ms)
./target/release/bobtop --corners square           # plain box corners (default rounded)
./target/release/bobtop --tty                      # block-char fallback for VTs without braille
./target/release/bobtop --show-virtual-net         # include lo, docker0, veth*, tun* in net aggregate
./target/release/bobtop --no-ebpf --no-pcap        # force lower-tier net attribution
RUST_LOG=info ./target/release/bobtop              # log tier selection + collector errors to stderr
```

Sticky preferences live at `~/.config/bobtop/bobtop.toml` (XDG-respected);
CLI flags override file values. Edit live in-app via the `O` overlay
(see Keyboard below).

### Keyboard

| key                 | action                                                     |
|---------------------|------------------------------------------------------------|
| `q` / Ctrl-C        | quit                                                       |
| `Esc`               | close overlay (or quit when none open)                     |
| `?`                 | help overlay                                               |
| `↑` `↓`             | move process selection                                     |
| `PgUp` `PgDn`       | jump 10 rows                                               |
| `Home` `End`        | jump to top / bottom                                       |
| `+` `-`             | tune update tick by ±100ms (live)                          |
| `←` `→`             | cycle sort column                                          |
| `r`                 | reverse sort direction                                     |
| `p` `n` `m` `c`     | sort by Pid / Name / Mem / Cpu                             |
| `1` `2` `3` `4`     | preset layouts (CPU / MEM / NET-RX / minimal)              |
| `B`                 | boxes overlay — show/hide individual panels live           |
| `O`                 | options overlay — edit config + save to disk               |
| `f`                 | filter processes by name/cmdline                           |
| `g`                 | cycle group mode: flat → exec → cgroup → tree              |
| `Space`             | expand/collapse selected group or subtree                  |
| `Enter`             | process detail (read-only) or expand on group header       |
| `k` / `K`           | send SIGTERM / SIGKILL to selected process (confirm modal) |

## Themes

bobtop reads btop's native `.theme` format directly. All 41 upstream
btop themes ship embedded in the binary. Drop your own at:

- `~/.config/bobtop/themes/<name>.theme`
- `~/.config/btop/themes/<name>.theme` (existing btop users get them for free)

Then `--theme <name>` to load it.

## Network attribution tiers

bobtop picks the most accurate available backend at startup; lower tiers are
graceful fallbacks.

| tier | backend             | per-pid bytes | privilege            |
|------|---------------------|---------------|----------------------|
| 3    | eBPF kprobes        | exact         | `CAP_BPF + CAP_PERFMON` (or root) |
| 2    | libpcap + inode map | sampled       | `CAP_NET_RAW` (or root)           |
| 1    | `/proc/net/tcp` walk| —             | none (sees own user only)         |
| 0    | unavailable         | —             | —                                 |

To unlock Tier 3 on Linux without running as root:

```bash
sudo setcap 'cap_bpf,cap_perfmon=ep' ./target/release/bobtop
./target/release/bobtop                            # process table now shows RX/s, TX/s columns
```

The CPU panel title shows the active tier (e.g. `attributor: ebpf`) and the
process table grows two extra columns (`RX/s`, `TX/s`) automatically when the
tier provides bandwidth.

## Workspace layout

```
crates/
  bobtop-core/         shared types, Collector trait, DataBus
  bobtop-collectors/   CPU, memory, network, disk, process collectors
  bobtop-net/          tiered network attribution (proc / pcap / ebpf)
  bobtop-tui/          ratatui widgets, themes, layout
  bobtop-daemon/       binary
```

## Process clustering

`g` cycles between four views of the process list:

- **flat** — one row per process (default)
- **exec** — collapse by executable name; "chrome (47)" sums to one row
- **cgroup** — collapse by `/proc/[pid]/cgroup` leaf; on systemd hosts
  this groups by service / container — `firefox.service`,
  `docker-<sha>.scope`, `user@1000.service` — and containers + k8s
  pods show up as named cgroups for free
- **tree** — parent_pid hierarchy with collapsible subtrees

Headers carry aggregated CPU / MEM / threads / RX / TX / DR / DW so the
collapsed view is informative on its own. Headers sort by the
aggregate matching the active sort key, so `m` (sort by mem) +
`g`-to-cgroup gives "which cgroup is using the most memory" — the
clustering question, answered.

Per-mode column layouts: Flat shows everything; Grouped drops Pid,
Command, User (no aggregate value at header level — Program flexes for
long group keys); Tree drops Command (Program flexes for indent +
branch glyphs).

## Status

See [ROADMAP.md](ROADMAP.md) for shipped Phase A (perf) + Phase B (UX)
items and what remains open.

## Tests

```bash
cargo test --workspace                              # 115 tests, default features
cargo test --workspace --features bobtop-net/pcap   # +pcap parser tests
cargo bench -p bobtop-daemon                        # render-loop perf benches
```

Visual smoke tests (render full frame with truecolor ANSI, no real terminal needed):

```bash
cargo run --example frame_smoke -p bobtop-daemon
cargo run --example braille_smoke -p bobtop-daemon
cargo run --example collectors_smoke -p bobtop-daemon
cargo run --example ebpf_smoke -p bobtop-daemon --features ebpf   # needs CAP_BPF
```

## License

Dual-licensed under MIT or Apache-2.0, at your option.

The vendored `.theme` files in `crates/bobtop-tui/themes/` come from
[aristocratos/btop](https://github.com/aristocratos/btop) and are
redistributed under Apache-2.0; see `crates/bobtop-tui/themes/NOTICE`.
