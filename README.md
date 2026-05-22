# gtop

`gtop` is a terminal system monitor in Rust, with async sampling and
per-process network bandwidth attribution. It is Linux-first; macOS
support is best-effort.

`gtop` is one of three crates in this workspace:

| Crate | Kind | What it is |
| --- | --- | --- |
| [`gtui`](crates/gtui)  | library | Reusable Ratatui toolkit — widgets, themes, layout, keymap |
| [`gtop`](crates/gtop)  | binary  | TUI system monitor (this README) |
| [`gfb`](crates/gfb)    | binary  | TUI file (and database) browser, built on `gtui` |

## Summary

`gtop` gives you two ways to work with the same engine:

1. The TUI for interactive monitoring.
2. The agent query mode for structured host-state questions over a Unix socket.

Key capabilities:

- CPU, memory, disk, network, and process views.
- Per-process network attribution with fallback tiers (eBPF / libpcap / `/proc`).
- Process clustering by executable, cgroup, container, parent, or flat.
- Container-aware process grouping (Docker / Podman / containerd / LXC) with
  names resolved from runtime metadata — no daemon socket required.
- Live per-flow network panel (toggle with `N`): pid · proc · remote · state ·
  ↓/s · ↑/s sorted by busiest, with proc_inode connection enumeration that
  works on top of any byte-attribution tier.
- Companion `gfb` binary: TUI file browser with a built-in nano-style
  editor and (with `--features all-sources`) Postgres / DuckDB /
  DuckLake catalog browsing. Ships separately — see
  [`crates/gfb/README.md`](crates/gfb/README.md).
- Theme support for btop `.theme` files (42 bundled).
- Headless daemon mode plus auto-spawning agent clients.

## Code quality

Public-release hygiene — what you can verify with the commands in
`Tests` below:

- `cargo build --release`: **zero warnings** across the workspace.
- `cargo test --workspace`: hundreds of tests passing, no `#[ignore]`s, no
  fixed-port `bind`s in tests.
- **Zero `TODO` / `FIXME` / `XXX` / `HACK` markers** anywhere in `crates/`.
- `#![forbid(unsafe_code)]` on `gtui` and `gfb`; `gtop`'s `pid_attr`
  module uses `#![deny(unsafe_code)]` with a single documented
  `unsafe fn` for plain-bytes round-tripping a libbpf-rs map value
  (and an unavoidable need to talk to the kernel).
- No `panic!` / `unwrap` / `expect` in non-test code paths.
- `tracing` for diagnostics; `println!` / `eprintln!` only appear in
  user-facing CLI surfaces (help text, `--list-themes`).
- Public API surface trimmed to what callers actually consume — internal
  modules are `pub(crate)`, not `pub`.
- `rustfmt.toml` + `rust-toolchain.toml` committed; CI runs
  `cargo fmt --check` + `cargo test --workspace` on every push.

### Agent prompt

Use this prompt for an agent that should operate `gtop`:

```text
You are operating gtop through its agent interface.
Use `snapshot` for current host state, `top` for ranked process lists,
`summary` for host or process-family rollups, `pid_inspect` for a single
process, `window` and `peak` for history, and `responsible_for` for
point-in-time ownership.

Prefer the smallest query that answers the question.
Use `--group flat|exec|cgroup|tree` when ranking processes.
Use `--match` to narrow by process name or command line. In the raw JSON
wire format, `match` accepts a single string or an array of strings, and
array entries are OR'd together. In the `gtop agent` CLI, pass one
pattern per `--match`.
Use `re:` for regex matching and glob wildcards like `*chrome*` for pattern
matching.
If a match is ambiguous, refine it or switch to `--pid`.
Return the answer and the exact gtop query you used.
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
git clone https://github.com/bobcolner/gverse
cd gverse
cargo build --release -p gtop
```

Optional features:

| feature | adds | system deps |
|---|---|---|
| `ebpf` (default) | Tier 3 per-process bandwidth attribution | `clang`, `libbpf-dev` |
| `pcap` | Tier 2 per-process bandwidth attribution | `libpcap-dev` |

Examples:

```bash
# Linux + libpcap
sudo apt install libpcap-dev
cargo build --release -p gtop --features pcap

# Linux + eBPF
sudo apt install clang libbpf-dev
cargo build --release -p gtop --features ebpf
```

## Run

```bash
./target/release/gtop
```

Useful flags:

```bash
./target/release/gtop --theme tokyo-night
./target/release/gtop --list-themes
./target/release/gtop --help-keys
./target/release/gtop --layout minimal
./target/release/gtop --tick-ms 500
./target/release/gtop --corners square
./target/release/gtop --tty
./target/release/gtop --show-virtual-net
./target/release/gtop --no-ebpf --no-pcap
RUST_LOG=info ./target/release/gtop
```

Sticky preferences live at `~/.config/gtop/gtop.toml`. CLI flags override
file values. Edit live in the TUI via the `O` overlay.

Agent mode:

```bash
./target/release/gtop agent snapshot
./target/release/gtop agent top --by cpu --n 5
./target/release/gtop agent top --by mem --group exec --match '*chrome*' --match 're:^pg_'
./target/release/gtop agent summary --match 'node' --match 'redis'
./target/release/gtop agent pid_inspect --match 'postgres' --match 're:^pg_'
./target/release/gtop agent summary --pid 1234
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

`gtop` is organized around a shared engine and thin front-ends.

### Runtime design

- Collectors gather system data.
- The engine publishes samples into a latest-value store and a history ring.
- The TUI reads the same engine state for display.
- The agent server exposes that state over a Unix socket as line-delimited JSON.
- The CLI client can auto-connect or auto-spawn `gtop --daemon` when the
  socket is missing.

### Workspace layout

| crate | role |
|---|---|
| `gtui` | reusable Ratatui toolkit — widgets, themes, layout, keymap, tree |
| `gtop` | the system monitor (this README). Internal `core/`, `collectors/`, `engine/`, `pid_attr/` modules. |
| `gfb` | TUI file (and optional DB) browser + embedded nano-style editor |

### Agent surface

The agent socket lives at `$XDG_RUNTIME_DIR/gtop.sock`, with a fallback to
`/tmp/gtop-$UID.sock`. Responses are schema-versioned as `gtop/v1`.

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

`gtop` chooses the best available bandwidth attribution backend at startup.

| tier | backend | per-pid bytes | privilege |
|---|---|---|---|
| 3 | eBPF kprobes | exact | `CAP_BPF + CAP_PERFMON` or root |
| 2 | libpcap + inode map | sampled | `CAP_NET_RAW` or root |
| 1 | `/proc/net/tcp` walk | no per-pid attribution | none |
| 0 | unavailable | none | none |

To unlock tier 3 on Linux without root:

```bash
sudo setcap 'cap_bpf,cap_perfmon=ep' ./target/release/gtop
./target/release/gtop
```

## Themes

`gtop` reads btop's native `.theme` format directly. All bundled themes ship
in the binary. Drop your own at:

- `~/.config/gtop/themes/<name>.theme`
- `~/.config/btop/themes/<name>.theme`

Then run `--theme <name>`.

## Tests

```bash
cargo test --workspace
cargo test -p gtop --features pcap
cargo bench -p gtop
```

## License

Dual-licensed under either of:

- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <https://opensource.org/licenses/MIT>)
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>)

at your option.

The vendored `.theme` files in `crates/gtui/themes/` come from
[aristocratos/btop](https://github.com/aristocratos/btop) and are
redistributed under Apache-2.0; see `crates/gtui/themes/NOTICE`.
