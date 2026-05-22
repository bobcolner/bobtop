# gfb

`gfb` is a TUI file browser — and, with the right feature flags on,
a Postgres / DuckDB / DuckLake catalog browser too. Both sources
share one tree pane: the cwd and every `--connect`'d endpoint stack
as siblings at depth 0.

## Install

```bash
# Just the file browser (default — no DB deps).
cargo install gfb

# File browser + Postgres + DuckDB + DuckLake.
cargo install gfb --features all-sources
```

`all-sources` pulls `tokio-postgres` and the bundled `duckdb` build
(~30 MB of compile time, ~37 MB of binary on top of the default 7 MB
shell). Pick `--features postgres` or `--features duckdb` if you only
need one.

## Run

### File browsing

```bash
gfb                       # start in $PWD
gfb /etc                  # start in /etc
gfb --theme tokyo-night
```

Two layouts: miller (default — three columns) and tree
(toggle with `T`). Built-in nano-style editor over a syntect
highlighter (open with `e`). Image preview detects kitty / iTerm /
sixel and falls back to sextant blocks.

### File + database browsing

```bash
gfb /repos \
  --connect postgresql://root@localhost/ml_momo \
  --connect duckdb:///root/repos/ml_momo/lake_data/ml_momo.duckdb \
  --ducklake-catalog postgresql://root@localhost/ml_momo \
  --ducklake-path /root/repos/ml_momo/lake_data
```

The tree pane shows `/repos` filesystem entries followed by each DB
endpoint as siblings. Drill in to a table → the preview pane
switches from file content to the first 100 rows of the table.

`--connect` schemes:

| scheme | needs | comments |
|---|---|---|
| `mock` | always-on | built-in demo data, useful for smoke / CI |
| `postgres://...` | `--features postgres` | catalog discovery via `information_schema` |
| `duckdb:///path` | `--features duckdb` | path or `:memory:` |

`--ducklake-catalog` + `--ducklake-path` apply to every
`--connect duckdb://...` in the same invocation.

## Keys (file browsing)

| key | action |
|---|---|
| `q` / Ctrl-C | quit |
| `Esc` | close overlay or quit |
| `?` | help overlay |
| `↑` `↓` / `j` `k` | move selection |
| `Enter` / `l` | enter directory |
| `←` / `h` | parent directory |
| `T` | toggle tree / miller layout |
| `e` | open in editor |
| `r` | rename |
| `n` | new file |
| `d` | move to trash |
| `Shift-D` | hard delete (with confirm) |
| `f` | filter |
| `/` | find |
| `H` | toggle hidden |

DB-tree keys are the same — `Enter` expands an endpoint /
database / schema; on a table it pins the row preview. File ops
(rename / trash / delete / touch / editor) decline to act on DB rows
with a status-line message.

## Persistence

`~/.local/state/gfb/state.toml` remembers the last cwd so successive
launches resume there.

## License

Dual-licensed under MIT or Apache-2.0 at your option.
