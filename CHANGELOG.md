# Changelog

All notable changes to the gtop family land here. The format roughly
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
the project adheres to [Semantic Versioning](https://semver.org/) on
the per-crate level.

## 0.3.0 — 2026-05-08

First release under the `gtop` family banner. Three crates ship:
`gtui` (library), `gtop` (system monitor), `gfb` (file + DB browser).

### Renamed

- The workspace formerly known as `bobtop` ships under `gtop`. The
  `bobtop` brand retires; existing config / agent-socket / theme-dir
  paths move to `~/.config/gtop/…`, `gtop.sock`, schema `gtop/v1`.
- Crate moves: `bobtop-tui` → `gtui`, `bobtop-daemon` → `gtop`,
  `bobtop-fb` → `gfb`. Binary `bobtop` → `gtop`, binary `bobtop-fb`
  → `gfb`.

### Added — `gtui` (new public library)

- `tree::Catalog` trait + `flatten()` walker + `TreeState` for any
  hierarchical source.
- `browser::BrowserShell` — render helper for the
  tree-on-the-left + preview-on-the-right composition.
- 41 btop themes bundled, parser handles `theme[key]="#hex"` syntax
  and gradient triples.
- Examples directory: `process_table`, `two_pane_browser`, `themes`.

### Added — `gtop`

- Internal `bobtop-core` / `bobtop-collectors` / `bobtop-pid-attr`
  / `bobtop-engine` plumbing folded in as `crate::core` /
  `crate::collectors` / `crate::pid_attr` / `crate::engine`. No
  user-facing change; consolidation drops the workspace from 8
  crates to 3.
- eBPF tier was previously silently broken because the `.bpf.c`
  build paths drifted; rename loop fixed and the
  `gtop_bpf_built` cfg now fires on default Linux installs.

### Added — `gfb`

- Optional DB sources (`postgres`, `duckdb`, `all-sources`
  features) — Postgres + DuckDB + DuckLake catalog browsing in the
  same tree pane as the filesystem.
- `--connect URL` flag (repeatable). Multi-root tree: cwd's fs
  children and each connection endpoint stack as siblings at depth
  0.
- `--ducklake-catalog` / `--ducklake-path` / `--ducklake-name` for
  attaching a lake to every `--connect duckdb://...`.
- Tree-mode preview pane: file → existing fs preview pipeline; DB
  table → first 100 rows in a `LiveTable`.
- File ops gated to fs selections — DB rows decline editor open
  with a status-line message.

### Removed

- `bobtop-db` crate dissolved into `gfb`. Its tree state / catalog
  abstraction lives in `gfb::sources::multi`; its DB backends in
  `gfb::sources::{db,pg,duckdb}`.
- `gtop`'s `fb` feature (the `gtop fb …` subcommand + the `b`
  keybind that re-execs the binary as a file browser). The two-app
  end state means `gfb` is the file browser; `gtop` no longer
  bundles it. Drops gtop's release binary from ~47 MB to **5.5 MB**.

### Internal

- Workspace shrunk 8 → 3 crates. Test count: 348 → 360. Net LOC
  reduction ~480 (after counting new toolkit modules).
- `gtui::tree` retired the duplicated tree-walk loops the file and
  DB browsers each shipped (~150 LOC).
- `gtui::browser::BrowserShell` retired ~40 LOC of two-pane
  layout boilerplate from each app.
