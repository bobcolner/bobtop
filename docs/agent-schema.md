# bobtop agent schema (v1, draft)

bobtop exposes its sampling engine as a queryable host-state engine. Agents
ask questions; bobtop returns small, shaped JSON. No log files, no feeds,
no client-side aggregation.

- **Transport:** Unix socket at `$XDG_RUNTIME_DIR/bobtop.sock`
  (fallback `/tmp/bobtop-$UID.sock`).
- **Wire format:** line-delimited JSON. One request → one response.
- **Versioning:** every response carries `"schema": "bobtop/v1"`.
- **Lifecycle:** running TUI serves the socket. `bobtop --daemon` runs the
  same engine without a TUI. CLI clients auto-connect or auto-spawn.

## Quick start

```bash
bobtop agent snapshot                                  # current host state
bobtop agent top --by cpu --group exec --n 5           # heaviest exec groups
bobtop agent top --by mem --match '*chrome*' --n 5
bobtop agent peak --metric cpu --window 5m             # spike + responsible pid
bobtop agent summary --pid 1234
```

## Query grammar

Every query is one JSON object with a `q` verb and a few common parameters:

| Param     | Type                  | Meaning                                              |
|-----------|-----------------------|------------------------------------------------------|
| `q`       | string                | Verb (see below).                                    |
| `by`      | string                | Sort/aggregate metric: `cpu`, `mem`, `net.rx`, `net.tx`, `disk.r`, `disk.w`. |
| `n`       | int                   | Top-N rows. Default 10.                              |
| `window`  | duration              | `1s`, `30s`, `1m`, `5m`, `30m`. Default = current sample. |
| `match`   | string \| string[]    | Process filter (see "Matching"). Default = all.      |
| `group`   | string                | `flat` \| `exec` \| `cgroup` \| `tree`. Default `flat`. |
| `metric`  | string                | For `peak` / `responsible_for`. Same vocab as `by`.  |
| `at`      | duration              | "30s_ago" — relative offset for `responsible_for`.   |
| `pid`     | int                   | For `summary` / `pid_inspect`.                       |
| `case`    | bool                  | Case-sensitive match. Default `false`.               |

### Verbs

- **`snapshot`** — latest host sample. Aggregates only; no per-pid list.
- **`top`** — ranked rows. Honors `by`, `n`, `match`, `group`, `window`.
- **`summary`** — aggregate stats for a scope. `scope: host` (default) or
  `scope: pid` (requires `pid`) or `scope: match` (requires `match`).
- **`peak`** — find the highest value of `metric` within `window`. Returns
  `{value, at, responsible}` where `responsible` is the row that owned the
  spike (respects `group`).
- **`responsible_for`** — at offset `at`, who owned `metric`. Like `peak`
  but for a specific point in time.
- **`pid_inspect`** — full detail for one pid (or one match that resolves
  to a single pid).
- **`raw`** — escape hatch for the full sample. Avoid in agent prompts;
  use `snapshot` + `top` instead.

## Matching (`match`)

Names match against both `comm` and `cmdline`, case-insensitive by default.

| Form              | Meaning                                       |
|-------------------|-----------------------------------------------|
| `"node"`          | Substring match (most common).                |
| `"node*"`         | Glob: starts with `node`.                     |
| `"*chrome*"`      | Glob: contains `chrome`.                      |
| `"re:^pg_"`       | Regex (with `re:` prefix).                    |
| `["node", "rg"]`  | OR list — match any literal.                  |

`match` runs **before** `group`, so aggregates reflect only the matched
population. Every returned row carries `matched_on: "name" | "cmdline"`
so agents can verify.

## Grouping (`group`)

bobtop already tracks four views of the process list. Same query, four
levels of aggregation, same response shape:

- **`flat`** — one row per pid; `id` is the pid as a string.
- **`exec`** — one row per executable; `id` is the binary name; aggregates
  every pid running that binary.
- **`cgroup`** — one row per cgroup leaf; `id` is the cgroup path.
- **`tree`** — one row per parent-rooted subtree; `id` is the root pid.

Aggregated rows carry a `pids` field listing members (capped at 50; if
truncated, `pids_truncated: true`). Use `group: flat, match: ...` to drill
down.

## Response shape

All process-bearing responses return a uniform `rows` array:

```json
{
  "schema": "bobtop/v1",
  "ts": "2026-05-05T18:42:11Z",
  "tick_ms": 1000,
  "rows": [
    {
      "id": "node",
      "kind": "exec",
      "pids": [8847, 8851, 8852],
      "name": "node",
      "cmdline": "node next-server",
      "cpu_pct": 312.4,
      "mem_bytes": 1840000000,
      "net_rx_bps": 0,
      "net_tx_bps": 12000,
      "disk_r_bps": 0,
      "disk_w_bps": 4096,
      "matched_on": "name"
    }
  ]
}
```

`snapshot` and `summary` return a single object instead of `rows`:

```json
{
  "schema": "bobtop/v1",
  "ts": "2026-05-05T18:42:11Z",
  "host": {
    "cpu_pct": 47.2,
    "mem_used_bytes": 12_400_000_000,
    "mem_total_bytes": 32_000_000_000,
    "swap_used_bytes": 0,
    "load_1m": 1.42,
    "net_rx_bps": 81000,
    "net_tx_bps": 12000,
    "disk_r_bps": 0,
    "disk_w_bps": 4096,
    "n_procs": 412
  }
}
```

## Errors

```json
{"schema": "bobtop/v1", "error": {"code": "bad_query", "message": "..."}}
```

Codes: `bad_query`, `unknown_verb`, `unknown_metric`, `pid_not_found`,
`window_unavailable`, `internal`.

## Out of scope (v1)

- Disk persistence / log files. All queries are on-demand against an
  in-memory ring buffer.
- Multi-host aggregation.
- Subscriptions / streaming push (use `bobtop agent watch` for polling).
- User-defined groupings beyond the four built-in views.
- Mutating operations (`kill`, `renice`). Read-only by design.

## Stability

`bobtop/v1` is locked once the first verb set ships. Additive changes
(new verbs, new metrics, new optional fields) are non-breaking.
Breaking changes bump to `bobtop/v2` with a parallel socket path.
