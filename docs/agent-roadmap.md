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

## After Phase 3-R

These ride on top of the refactored engine and are mostly additive.

- **`summary` verb** — host or per-pid aggregate. `{q:"summary"}` for host;
  `{q:"summary","pid":N}` or `{q:"summary","match":"node*"}` for a scoped
  rollup. Returns a single object, not a row list.
- **`pid_inspect` verb** — full detail for one pid (or one match resolving
  to a single pid): name, cmdline, user, state, parent_pid, cgroup,
  threads, all metrics, started_ago. The "drill down" verb.
- **`cgroup` grouping** — `top --group cgroup` aggregates rows by the
  trailing `/proc/<pid>/cgroup` segment (`firefox.service`,
  `docker-<sha>.scope`, etc.). The `bobtop-daemon::group` module already
  does this for the TUI; lift its core logic into the engine and reuse.
- **`tree` grouping** — `top --group tree` returns subtree-rooted rows
  with descendant lists. Same source-of-truth move.
- **`responsible_for` verb** — point-in-time variant of `peak`.
- **Idle-exit timer** — daemon-mode polish; auto-shutdown after N min of
  no socket activity.
- **Auto-spawn from CLI client** — when the socket is missing, fork
  `bobtop --daemon` transparently before the request.
- **MCP shim** — once the verb set is stable.

## Out of scope (v1)

- Disk persistence / log files.
- Multi-host aggregation.
- Subscriptions / streaming push.
- User-defined process groupings beyond the four built-in views.
- Mutating operations (`kill`, `renice`).
