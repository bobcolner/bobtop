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
./target/release/bobtop --layout minimal           # cpu + processes only
./target/release/bobtop --tick-ms 250              # faster updates (default 500ms)
./target/release/bobtop --tty                      # block-char fallback for VTs without braille
./target/release/bobtop --show-virtual-net         # include lo, docker0, veth*, tun* in net aggregate
./target/release/bobtop --no-ebpf --no-pcap        # force lower-tier net attribution
RUST_LOG=info ./target/release/bobtop              # log tier selection + collector errors to stderr
```

### Keyboard

| key                 | action                                       |
|---------------------|----------------------------------------------|
| `q` / `Esc` / Ctrl-C| quit                                         |
| `↑` `↓` / `j` `k`   | move process selection                       |
| `PgUp` `PgDn`       | jump 10 rows                                 |
| `Home` `End`        | jump to top / bottom                         |
| `+` `-`             | tune update tick by ±100ms (live)            |
| `1`                 | full layout (CPU + Mem + Net + Procs)        |
| `m`                 | minimal layout (CPU + Procs)                 |

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

## Tests

```bash
cargo test --workspace                              # 78 tests, default features
cargo test --workspace --features bobtop-net/pcap   # +pcap parser tests
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
