# bobtop

bobtop is a terminal system monitor in Rust with a btop-style TUI, async
sampling, and per-process network bandwidth attribution. It is Linux-first;
macOS support is best-effort.

## Summary

bobtop gives you two ways to work with the same engine:

1. The TUI for interactive monitoring.
2. The agent query mode for structured host-state questions over a Unix socket.

Key capabilities:

- CPU, memory, disk, network, and process views.
- Per-process network attribution with fallback tiers.
- Process clustering by executable, cgroup, or tree.
- Theme support for btop `.theme` files.
- Headless daemon mode plus auto-spawning agent clients.

### Agent prompt

Use this prompt for an agent that should operate bobtop:

```text
You are operating bobtop through its agent interface.
Use `snapshot` for current host state, `top` for ranked process lists,
`summary` for host or process-family rollups, `pid_inspect` for a single
process, `window` and `peak` for history, and `responsible_for` for
point-in-time ownership.

Prefer the smallest query that answers the question.
Use `--group flat|exec|cgroup|tree` when ranking processes.
Use `--match` to narrow by process name or command line. In the raw JSON
wire format, `match` accepts a single string or an array of strings, and
array entries are OR'd together. In the `bobtop agent` CLI, pass one
pattern per `--match`.
Use `re:` for regex matching and glob wildcards like `*chrome*` for pattern
matching.
If a match is ambiguous, refine it or switch to `--pid`.
Return the answer and the exact bobtop query you used.
```

### Agent verbs

- `snapshot` - latest host-level summary.
- `top --by <metric> [--n N] [--group G] [--match PATTERN]` - ranked
  processes or groups.
- `summary [--match PAT]... [--pid N]` - host, match, or pid rollup.
- `pid_inspect (--pid N | --match PATTERN)...` - full detail for one process.
- `window --metric <m> --window <w>` - avg, peak, and p95 over history.
- `peak --metric <m> --window <w>` - peak value and who owned it.
- `responsible_for --metric <m> --at <offset>` - who owned a metric at a
  point in the past.

## Install

```bash
git clone https://github.com/bobcolner/bobtop
cd bobtop
cargo build --release -p bobtop-daemon
```

Optional features:

| feature | adds | system deps |
|---|---|---|
| `pcap` | Tier 2 per-process bandwidth attribution | `libpcap-dev` |
| `ebpf` | Tier 3 per-process bandwidth attribution | `clang`, `libbpf-dev` |
| `nvidia` | NVIDIA GPU stats via NVML | proprietary NVIDIA drivers |

Examples:

```bash
# Linux + libpcap
sudo apt install libpcap-dev
cargo build --release -p bobtop-daemon --features pcap

# Linux + eBPF
sudo apt install clang libbpf-dev
cargo build --release -p bobtop-daemon --features ebpf
```

## Run

```bash
./target/release/bobtop
```

Useful flags:

```bash
./target/release/bobtop --theme tokyo-night
./target/release/bobtop --list-themes
./target/release/bobtop --help-keys
./target/release/bobtop --layout minimal
./target/release/bobtop --tick-ms 500
./target/release/bobtop --corners square
./target/release/bobtop --tty
./target/release/bobtop --show-virtual-net
./target/release/bobtop --no-ebpf --no-pcap
RUST_LOG=info ./target/release/bobtop
```

Sticky preferences live at `~/.config/bobtop/bobtop.toml`. CLI flags override
file values. Edit live in the TUI via the `O` overlay.

Agent mode:

```bash
./target/release/bobtop agent snapshot
./target/release/bobtop agent top --by cpu --n 5
./target/release/bobtop agent top --by mem --group exec --match '*chrome*' --match 're:^pg_'
./target/release/bobtop agent summary --match 'node' --match 'redis'
./target/release/bobtop agent pid_inspect --match 'postgres' --match 're:^pg_'
./target/release/bobtop agent summary --pid 1234
```

Raw socket example:

```json
{"q":"top","by":"mem","group":"exec","match":["*chrome*","re:^pg_"]}
```

### Keyboard

| key | action |
|---|---|
| `q` / Ctrl-C | quit |
| `Esc` | close overlay, or quit when none is open |
| `?` | help overlay |
| `↑` `↓` | move process selection |
| `PgUp` `PgDn` | jump 10 rows |
| `Home` `End` | jump to top or bottom |
| `+` `-` | change update tick by 100 ms |
| `←` `→` | cycle sort column |
| `r` | reverse sort direction |
| `p` `n` `m` `c` | sort by pid, name, memory, or cpu |
| `1` `2` `3` `4` | preset layouts |
| `B` | show or hide panels |
| `O` | edit config and save to disk |
| `f` | filter processes by name or cmdline |
| `g` | cycle group mode |
| `Space` | expand or collapse the selected group |
| `Enter` | process detail or expand a group header |
| `k` / `K` | send SIGTERM or SIGKILL to the selected process |

## Architecture

bobtop is organized around a shared engine and thin front-ends.

### Runtime design

- Collectors gather system data.
- The engine publishes samples into a latest-value store and a history ring.
- The TUI reads the same engine state for display.
- The agent server exposes that state over a Unix socket as line-delimited JSON.
- The CLI client can auto-connect or auto-spawn `bobtop --daemon` when the
  socket is missing.

### Workspace layout

| crate | role |
|---|---|
| `bobtop-core` | shared types, history, sample store, bus, and common helpers |
| `bobtop-collectors` | CPU, memory, network, disk, and process collectors |
| `bobtop-pid-attr` | per-process network and disk attribution helpers |
| `bobtop-engine` | sampling engine plus agent query surface |
| `bobtop-tui` | ratatui widgets, themes, and layout code |
| `bobtop-daemon` | binary, CLI, and TUI wiring |

### Agent surface

The agent socket lives at `$XDG_RUNTIME_DIR/bobtop.sock`, with a fallback to
`/tmp/bobtop-$UID.sock`. Responses are schema-versioned as `bobtop/v1`.

The current query surface is read-only and includes:

- `snapshot`
- `top`
- `window`
- `peak`
- `summary`
- `pid_inspect`
- `responsible_for`

Process grouping supports `flat`, `exec`, `cgroup`, and `tree`.
`match` accepts a single string or an array of strings in the raw wire
format. Each entry can use substring, glob, or `re:` regex matching, and
array entries are OR'd.

### Attribution tiers

bobtop chooses the best available bandwidth attribution backend at startup.

| tier | backend | per-pid bytes | privilege |
|---|---|---|---|
| 3 | eBPF kprobes | exact | `CAP_BPF + CAP_PERFMON` or root |
| 2 | libpcap + inode map | sampled | `CAP_NET_RAW` or root |
| 1 | `/proc/net/tcp` walk | no per-pid attribution | none |
| 0 | unavailable | none | none |

To unlock tier 3 on Linux without root:

```bash
sudo setcap 'cap_bpf,cap_perfmon=ep' ./target/release/bobtop
./target/release/bobtop
```

## Themes

bobtop reads btop's native `.theme` format directly. All bundled themes ship
in the binary. Drop your own at:

- `~/.config/bobtop/themes/<name>.theme`
- `~/.config/btop/themes/<name>.theme`

Then run `--theme <name>`.

## Tests

```bash
cargo test --workspace
cargo test -p bobtop-daemon --features pcap
cargo bench -p bobtop-daemon
```

Visual smoke tests:

```bash
cargo run --example frame_smoke -p bobtop-daemon
cargo run --example braille_smoke -p bobtop-daemon
cargo run --example collectors_smoke -p bobtop-daemon
cargo run --example ebpf_smoke -p bobtop-daemon --features ebpf
```

## License

Dual-licensed under MIT or Apache-2.0, at your option.

The vendored `.theme` files in `crates/bobtop-tui/themes/` come from
[aristocratos/btop](https://github.com/aristocratos/btop) and are
redistributed under Apache-2.0; see `crates/bobtop-tui/themes/NOTICE`.
