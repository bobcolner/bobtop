# bobtop agent surface — roadmap

This doc captures the multi-phase plan that turns the existing system
monitor into a queryable host-state engine for AI agents. Companion to
`docs/agent-schema.md` (the wire format).

## Phases shipped

### Phase 0 — Sampler aggregator (`SampleStore`)
Latest-value snapshot of every subsystem, fed by the existing `DataBus`.
Cheap clone, watch-channel updates, lock-free reads.

### Phase 1 — Bounded retrospective ring (`History`)
Three-tier `VecDeque` (1s/5s/30s) of host metrics + per-pid top-N. ~200 KB
worst-case memory, sub-millisecond per-tick CPU. Powers retrospective
queries.

### Phase 2 — Unix-socket JSON-RPC server
`$XDG_RUNTIME_DIR/bobtop.sock` with stale-socket recovery. Line-delimited
JSON. Schema-versioned (`bobtop/v1`). First verb: `snapshot`.

### Phase 3 — `top` verb + CLI client
`bobtop agent top --by <metric> [--n N] [--group flat|exec] [--match pat]`.
Glob/substring matching against `name ∪ cmdline`. Bounded min-heap top-N.

### Phase 4 — `peak` and `window` verbs
Retrospective queries over the ring buffer. `peak` returns value + ago_secs
+ responsible pids at that tick. `window` returns avg/peak/p95.

### Phase 5 — Daemon mode (`--daemon`)
Headless engine without TUI. SIGINT/SIGTERM-aware, socket cleanup on exit.

## Phase 3-R — Collection refactor (the missing structural work)

Adding verbs has been outpacing the structural work that should've come
first. `App` still owns runtime state that belongs in the engine — most
visibly, per-pid network/disk attribution flows through `App`'s mutex
instead of through the bus, so agent queries see `net_tx_bps: 0` for every
pid even when the TUI shows real numbers in the same panel.

This phase fixes that. It is purely refactoring + filling gaps — no new
verbs land here.

### Step 1 — Move attribution into the engine

**Goal:** `MetricEvent::Process` carries authoritative per-pid net/disk
rates by the time it hits the bus. `SampleStore`, `History`, and the agent
socket get the data for free.

- New `AttributionStore` (Arc-cloneable, RwLock-backed) owned by the
  engine. Holds `HashMap<pid, NetAttribution>` and `HashMap<pid, DiskAttribution>`
  + the active tier markers.
- `ProcessCollector::with_attribution(store)` — collector reads the
  attribution map inside `collect()` and fills `ProcessInfo.net_*` /
  `disk_*` before publishing.
- `spawn_attributor_loop` / `spawn_disk_attributor_loop` move out of
  `main.rs` and write into `AttributionStore`. They no longer take
  `Arc<Mutex<App>>`.
- `App::apply_net` / `App::apply_disk` and the `rebuild_sorted` join
  delete — `App` consumes the already-attributed `ProcessSample` like
  every other subsystem.

**Impact:**
- `peak net.tx` `responsible: []` → real pids.
- `top --by net.tx` showing zeros → real values.
- One source of truth for per-pid metrics.

### Step 2 — Carve out an `Engine` struct

A single owner that bundles bus + sample_store + history + attribution +
collectors + tick driver. Daemon mode and TUI mode both build an `Engine`
and hand it to their respective frontend; `main.rs` doesn't wire individual
collectors anymore.

`App` loses `apply_event`, `latest_*` fields, attribution write surfaces.
It subscribes to `engine.store` and rebuilds presentation state per tick.

### Step 3 — Move `Engine` out of the daemon crate

`Engine` becomes its own module (or crate) so it can be embedded by
benchmarks, library bindings, or a future MCP server. Optional but pays
off the next time we want to reuse the engine.

## After Phase 3-R (shipped)

- ✅ **`summary` verb** — host / match / pid scopes, uniform envelope.
- ✅ **`pid_inspect` verb** — single-pid drilldown; `--match` must
  resolve uniquely or returns `bad_query` with the count.
- ✅ **`cgroup` grouping** — buckets by `/proc/<pid>/cgroup` last segment
  (`docker-<sha>.scope`, `bars_signalgen_consumer.service`, etc.).
- ✅ **`tree` grouping** — parent-rooted subtree aggregation; memoizes
  via a `root_of` map so deep chains don't re-traverse.
- ✅ **`responsible_for` verb** — point-in-time variant of `peak`. Picks
  the smallest tier covering the offset for max fidelity.
- ✅ **Idle-exit timer** — daemon-mode auto-shutdown after 30 min of no
  socket activity. Suppressed when the socket failed to bind.
- ✅ **Auto-spawn from CLI client** — when `bobtop agent` finds the socket
  missing it forks `bobtop --daemon` (detached via `setsid`), polls for
  the socket up to 3s, then connects. Honors `BOBTOP_NO_AUTOSPAWN=1`.

## Still on the wishlist

- **MCP shim** — once the verb set is locked, expose verbs as MCP tools.
- **Phase 3-R Step 3** — move `Engine` to its own crate so embedders
  (benchmarks, MCP, future remote sinks) can pull it in without
  dragging the daemon binary's deps along.

## Out of scope (v1)

- Disk persistence / log files.
- Multi-host aggregation.
- Subscriptions / streaming push.
- User-defined process groupings beyond the four built-in views.
- Mutating operations (`kill`, `renice`).
