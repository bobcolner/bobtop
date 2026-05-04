# bobtop roadmap

Two-track polish effort comparing bobtop against btop. Phase A is
performance / resource parity; Phase B is UX/UI parity. Both started
2026-05-03 and shipped through eight commits on `main` over the
following session block.

## Status snapshot

- **115 tests** across the workspace, zero failures.
- **Idle CPU at default settings**: ~0.02% of one core (was ~1.75%
  pre-Phase-A).
- **Per-process network attribution** at Tier 3 (eBPF) — feature btm
  has never shipped and btop only added recently.
- **Process clustering**: flat / by-executable / by-cgroup / tree —
  cgroup grouping is the standout, with full systemd service +
  container visibility for free.

## Phase A — Performance (resource parity with btop)

Goal: match btop's idle resource profile on a stock 180×50 layout.

| ID | Item | Status | Commit |
|----|------|--------|--------|
| A1 | Render-on-change loop (event-driven instead of 60 Hz) | ✅ shipped | `c9d9e66` |
| A2 | Default `tick_ms = 1500` (matches btop's calm default) | ✅ shipped | `c9d9e66` |
| A3 | Hidden boxes skip collectors (`BoxesEnabled` bitfield) | ✅ shipped | `c9d9e66` |
| A4 | BrailleGraph thread-local scratch buffers (zero-alloc render) | ✅ shipped | `c9d9e66` |
| A5 | Replace sysinfo process scan with direct `/proc` walk | ⏸ deferred | — |
| A6 | Single tick driver fans out to all collectors | ✅ shipped | `c9d9e66` |
| A7 | Cache MiniMeter strings on 64-core hosts | ⏸ deferred | — |
| A8 | Criterion bench harness (regression gate) | ✅ shipped | `c9d9e66` |

**Bench baselines** (`cargo bench -p bobtop-daemon`):
- `apply_event/cpu`: ~21 ns
- `ui_draw/full_180x50`: ~283 µs
- `ui_draw/full_120x40`: ~159 µs

**Net effect on idle CPU**: pre-A1 was 60 frames/s × 291 µs ≈ 1.75% of
one core. Post-A1 at default 1.5 s tick: ~0.7 frames/s × 283 µs ≈
0.02% — a ~85× drop. A3 compounds when users hide panels.

### Phase A — open items (low priority, lower yield)

- **A5** direct `/proc` walk for processes. sysinfo's `refresh_processes`
  is the dominant per-tick cost; replacing it would shave a few
  hundred µs/sec but requires a significant rewrite.
- **A7** MiniMeter string caching. Tiny win on 64-core hosts; not
  load-bearing.

## Phase B — UX / UI (btop visual + interaction parity)

Goal: indistinguishable polish vs. btop, plus features btop lacks
(per-process network, intelligent process clustering).

| ID | Item | Status | Commit |
|----|------|--------|--------|
| B1 | Header status bar (version · tier · theme · uptime) | ✅ shipped | `b63470a` |
| B2 | `?` help overlay | ✅ shipped | `b63470a` |
| B3a | Direct sort shortcuts `p`/`n`/`m`/`c` | ✅ shipped | `be736d3` |
| B3b | Process filter `f` | ✅ shipped | `be736d3` |
| B3c | Kill confirm dialog `k`/`K` | ✅ shipped | `be736d3` |
| B3d | Process detail modal `Enter` | ✅ shipped | `be736d3` |
| B3e | Tree view (`t` standalone) | ✅ shipped via grouping `g` | `28d0758` |
| B4 | Smooth meter tweens | ⏸ open | — |
| B5 | `B` boxes overlay (show/hide panels live) | ✅ shipped | `9b63ba2` |
| B6 | Preset slots `1`–`4` | ✅ shipped | `9b63ba2` |
| B7 | Per-box graph symbol mode (`braille \| block \| tty`) | ⏸ partial | global `--tty` exists; per-box deferred |
| B8 | Darkening fade down process list | ✅ shipped | `b63470a` |
| B9 | Auto-scale Y label on net graph | ✅ shipped | `b63470a` |
| B10 | `~/.config/bobtop/bobtop.toml` | ✅ shipped | `9b63ba2` |
| B11 | `O` options overlay + write-back | ✅ shipped | `be736d3` |
| B12 | Rounded vs square corners config | ✅ shipped | `9d72c34` |

### Phase B extensions (shipped beyond the original list)

| Item | Commit |
|------|--------|
| `--help-keys` flag (single-source-of-truth keybind table) | `632f16e` |
| CPU avg frequency in panel title | `632f16e` |
| Process grouping engine (`g` cycles flat/exec/cgroup/tree) | `28d0758` |
| Per-mode column layouts (Flat/Grouped/Tree) | `1b3f749` |
| Per-mode column inclusion (drop pid/cmd/user where useless) | `48b5132` |
| Live theme preview while cycling in `O` overlay | `73ea7b1` |
| `/proc/[pid]/cmdline` for full argv (sysinfo `cmd()` was truncated) | `48b5132` |
| UID → username resolution in collector | `48b5132` |
| 60% width for proc panel + btop column ordering | `8621e22` |
| Detail-modal artifact fix (sanitize `/proc` tab characters) | `73ea7b1` |
| Grouped-header sort follows active sort key | `73ea7b1` |
| Threads total in group headers | `73ea7b1` |

### Phase B — open items

**B4 — Smooth meter tweens** (medium):
Lerp meter fills from previous→target over 200 ms with easing. Needs a
persistent `HashMap<MeterId, Tween>` on App; `mark_dirty` while a tween
is in flight so the render loop wakes during animation. Visible "btop
signature" feel — meters currently snap.

**B7-full — Per-box graph symbol mode** (medium):
Today a single `tty_graphs: bool` toggles all graphs together. btop
lets you pick `braille | block | tty` per panel. Plumbing is in place
(`GraphStyle` enum already three-valued); just needs a per-box config
field + `O` overlay editing.

**Grouping follow-ups** (low):
- Per-mode default expand state in Config.
- Cgroup full-path display option (today shows leaf only).

## Open polish (smaller items, not in Phase A/B)

Discovered during Phase B work but not part of the original scope.

### Quick wins (~30 min each)
- **CPU per-core temperature column** — `MiniMeter` already has the
  trailing-text slot; just needs a `/sys/class/hwmon/hwmon*/temp*_input`
  walker in the CPU collector. Currently shows `—` on real hardware.
- **Net auto-scale baseline** — startup-only `/sys/class/net/<iface>/speed`
  read so gigabit hosts don't show "1.2K/s scale" until traffic arrives.
- **Kernel-thread filter** `--kthreads` opt-in flag.
- **First sample after launch shows 0%** — render "sampling…" until the
  second sample lands so the initial click doesn't look like a glitch.

### Medium (1–2 h each)
- **Memory panel split** — disks currently piggyback. Give them a
  dedicated panel with Cached/Free meters.
- **Network panel polish** — per-interface selector (`b`/`n`),
  packet-rate row, real-vs-virtual filter pill.
- **Mouse support** — click-to-select rows, click `+`/`-` to tune tick.
- **Hold-key acceleration for ↑↓** (currently single-step).
- **Resize-aware panel hiding** (current full layout forces ≥4 panels
  even at 80×24).

### Bigger (half-day each)
- **CPU panel sparklines per-core** — replace big aggregate trace with
  N tiny sparklines for high-core-count hosts.
- **Process column visibility config** — per-column on/off flags in
  `bobtop.toml`, edited via `O` overlay.
- **First-run startup splash** — like btop's BTOP++ banner.

## Architecture notes for future contributors

- **Modal stack ordering** matters: detail/help/options/boxes all
  intercept keys in `App::handle_key` before the main key match runs.
  When adding a new modal, add the early-return guard at the top.
- **`DisplayRow` vs `ProcessInfo`**: the table widget consumes
  `&[DisplayRow]` (Header | Process). The grouping module
  (`bobtop-daemon::group`) builds the rows; the renderer doesn't see
  raw processes anymore.
- **Column ordering invariants**: `build_cols(layout)`,
  `build_row_cells(p, layout)`, and the header-row cells builder all
  follow the same `includes_*` predicates. If you change one, change
  all three — comments tag the invariant.
- **Theme resolution**: always go through `bobtop_tui::load_theme(name)`,
  not `find_source` + `Theme::from_source` directly. The latter is a
  footgun — `find_source` returns `(source, origin_label)` not
  `(name, source)`.
- **Render-on-change**: `App::dirty` is set by `apply_event` /
  `apply_net` / `handle_input`. If you add state that the renderer
  reads, mark it dirty when you mutate.
