# gtop refactor

Living plan document for the cross-cutting work that turns the current
`bobtop` workspace into the `gtop` family of three publicly-released
crates. Updated as phases land. Older sections stay accurate to the
final state — this doc IS the spec.

## Goal

Ship three crates to crates.io. Two binaries and one library, each
with a clear, intentional surface and its own user base.

| Crate | Kind | What it is | Users |
| --- | --- | --- | --- |
| `gtui` | library | Reusable TUI components — widgets, theme, layout, keymap, tree/browser primitives | Anyone building Rust TUI apps |
| `gtop` | binary | btop-flavoured system monitor: CPU / memory / network / disk / processes | Anyone running a system monitor |
| `gfb` | binary | File and database browser: filesystem + Postgres + DuckDB / DuckLake, all under one tree-and-preview UI | Anyone browsing files or DB catalogs |

These three are the *only* published crates. Internal plumbing that
today lives as separate workspace members (`bobtop-core`, `-collectors`,
`-pid-attr`, `-engine`) gets folded into `gtop` as internal modules.
`bobtop-db` dissolves into `gfb`.

## Final crate layout

```
crates/
├── gtui/                        (toolkit, public library)
│   └── src/
│       ├── widgets/             (existing — BoxedPanel, LiveTable, ...)
│       ├── tree/                (NEW — Catalog trait, TreeState, flatten)
│       ├── browser/             (NEW — BrowserShell layout helper)
│       ├── keymap/              (existing — ScopeStack)
│       ├── theme/  layout/  text/  util/  color/
│       └── lib.rs
├── gtop/                        (system monitor, public binary)
│   └── src/
│       ├── core/                (formerly bobtop-core)
│       ├── collectors/          (formerly bobtop-collectors)
│       ├── pid_attr/            (formerly bobtop-pid-attr — build.rs + .bpf.c too)
│       ├── engine/              (formerly bobtop-engine)
│       ├── ui/  app.rs  ...     (existing daemon code)
│       └── main.rs
└── gfb/                         (file + db browser, public binary)
    └── src/
        ├── sources/
        │   ├── fs.rs            (impl Catalog over the filesystem)
        │   ├── pg.rs            (feature-gated: Postgres)
        │   ├── duckdb.rs        (feature-gated: DuckDB / DuckLake)
        │   └── mock.rs
        ├── preview/
        │   ├── text.rs  image.rs  markdown.rs  (existing fs preview)
        │   └── table.rs         (NEW — DB-table preview via LiveTable)
        ├── app.rs  ui.rs  ...
        └── main.rs
```

No other crates in the workspace.

## Naming map

| Old | New |
| --- | --- |
| `bobtop-tui` | `gtui` |
| `bobtop-daemon` (binary `bobtop`) | `gtop` (binary `gtop`) |
| `bobtop-fb` (binary `bobtop-fb`) | `gfb` (binary `gfb`) |
| `bobtop-db` | dissolves into `gfb` |
| `bobtop-core` | `gtop`'s `src/core/` |
| `bobtop-collectors` | `gtop`'s `src/collectors/` |
| `bobtop-pid-attr` | `gtop`'s `src/pid_attr/` |
| `bobtop-engine` | `gtop`'s `src/engine/` |

The `bobtop` brand name retires entirely. `gtop` is the project
umbrella and the monitor binary.

## Cross-cutting commitments

These apply to every phase and every commit on the branch:

- **`gtui` is feature-rich on purpose.** Every shareable bit of logic
  pushes upward into the toolkit. The two binaries should be thin —
  domain-specific glue over toolkit primitives.
- **Public API discipline.** Every new `pub` item in `gtui` ships with
  rustdoc and an explicit pub-vs-pub(crate) call at write-time. No
  "leave it pub for now."
- **No `unsafe`** in any of the three published crates (already enforced
  via `#![forbid(unsafe_code)]` on `gtui`; extending to `gtop` and
  `gfb`).
- **Minimal default features on the binaries.** `gfb`'s DB sources are
  feature-gated so a default install doesn't pull in libduckdb (~30 MB).
- **CI must build and test each published crate independently** —
  `cargo check -p gtui`, `cargo test -p gtui` etc. — to catch
  cross-crate coupling that wouldn't survive `cargo publish`.

## Phases

Each phase is one or more commits on `refactor/gtop`. Estimates are
focused-work hours, not wall time. Order is dependency-ordered: each
phase assumes the prior phases have landed.

### Phase 0 · Rename to gtop family (~2-3h)

Pure rename; no behavioural change.

- `git mv crates/bobtop-tui crates/gtui`
- `git mv crates/bobtop-daemon crates/gtop` (binary already `bobtop` →
  rename to `gtop`)
- `git mv crates/bobtop-fb crates/gfb` (binary `bobtop-fb` → `gfb`)
- Update `Cargo.toml` package names, binary names, workspace members,
  workspace.dependencies entries.
- Search-replace `bobtop_tui` → `gtui`, `bobtop_daemon` → `gtop`,
  `bobtop_fb` → `gfb` across all source files.
- Update `[[bin]]` paths.
- `bobtop-db`, `bobtop-core`, `bobtop-collectors`, `bobtop-pid-attr`,
  `bobtop-engine` remain in place at this stage — they're handled in
  Phase 1.
- README, repository URL, project description text updates.

**Validation:** `cargo build --workspace`, `cargo test --workspace
--all-targets` green. Both binaries (`gtop`, `gfb`) run.

### Phase 1 · Consolidate the monitor (~4-6h)

Fold the four internal crates into `gtop`.

- Move sources from `bobtop-core/src/*` → `gtop/src/core/*`, similarly
  for `-collectors`, `-pid-attr` (with `build.rs` + `.bpf.c`),
  `-engine`.
- Update internal imports: `bobtop_core::sample::ProcessInfo` →
  `crate::core::sample::ProcessInfo`, etc.
- Delete the four crate directories from the workspace.
- Update workspace `[members]` and `[workspace.dependencies]`.
- Verify `gtop`'s `build.rs` still compiles eBPF (the build script
  moved with `pid_attr/`).

**Validation:** `cargo build -p gtop --release`, eBPF programs still
load, monitor still functions. `cargo build --workspace` shows only
three crates.

### Phase 2 · `tree` module in `gtui` (~2h)

Extract the tree-walk logic both today's `bobtop-fb` and `bobtop-db`
duplicate.

- New `gtui::tree` module:
  - `pub trait Catalog { type NodeId; type Row; fn root() -> ...; fn
    children(&Self::NodeId) -> ...; fn is_expandable(&Self::NodeId) ->
    bool; }`
  - `pub struct TreeState<NodeId> { pub expanded: HashSet<NodeId>, pub
    nav: Nav, }`
  - `pub fn flatten<C: Catalog>(catalog: &C, state: &TreeState<...>) ->
    Vec<TreeRow<C::Row>>` — emits depth, ancestor_continues,
    is_last_sibling.
- Tests against a synthetic `Catalog`.
- `gfb`'s tree-mode `flatten_tree` (in `crates/gfb/src/tree.rs`) becomes
  `impl Catalog for FsTree { ... }` driving the toolkit `flatten`.
- `bobtop-db`'s `CatalogTree::rebuild` becomes `impl Catalog for
  ConnectionGroup { ... }`. (`bobtop-db` still exists at this stage —
  its merge into `gfb` is Phase 4.)
- ~150 LOC of duplication retired across the two apps.

**Validation:** existing tree tests in both apps still pass. Tree mode
in `gfb` and tree pane in `bobtop-db` byte-identical.

### Phase 3 · `browser` module in `gtui` (~2h)

A composition helper for the two-pane tree+preview shape both apps
share.

- `gtui::browser::BrowserShell` — layout helper (not a Widget):
  - Splits a `Rect` into tree + preview rects (configurable ratio).
  - Renders the tree pane via `LiveTable` driven by a `Catalog` +
    `TreeState`.
  - Returns the preview `Rect` for the caller to fill (preview
    rendering stays domain-specific — file content vs DB rows).
  - Handles tree-mode key dispatch (j/k/h/l/Enter/Space) — mutates
    the passed `&mut TreeState`.
  - Sticky-by-NodeId selection wired through.
- `gfb`'s `draw_tree_view` becomes a thin wrapper over `BrowserShell`.
- `bobtop-db`'s 2-pane render becomes a thin wrapper over
  `BrowserShell`.

**Validation:** existing UX in both apps byte-identical for the
tree+preview path.

### Phase 4 · Move DB sources into `gfb` (feature-gated) (~1.5h)

- `gfb`'s `Cargo.toml`:
  ```toml
  [features]
  default = []
  postgres = ["dep:tokio-postgres"]
  duckdb = ["dep:duckdb"]
  all-sources = ["postgres", "duckdb"]
  ```
- `Connection` trait + `Database/Schema/Table/Row/ColumnSpec` types and
  `MockConnection` always-on under `gfb/src/sources/db/`.
- `PgConnection` (was `bobtop-db`'s `pg.rs`) → `gfb/src/sources/pg.rs`,
  feature `postgres`.
- `DuckConnection` (was `bobtop-db`'s `duck.rs`) →
  `gfb/src/sources/duckdb.rs`, feature `duckdb`.

**Validation:** `cargo build -p gfb` produces a 7M binary (no DB
sources). `cargo build -p gfb --features all-sources` produces ~38M
with everything wired. `bobtop-db` still runs because its conn module
now imports from `gfb` via path/dep — transitional.

### Phase 5 · Multi-root tree in `gfb` (~3-4h)

Where `gfb` actually becomes the unified browser.

- `gfb::App` holds `roots: Vec<Box<dyn Catalog>>` — first entry is the
  filesystem source, subsequent entries are connections from CLI flags.
- CLI: `--cwd PATH`, repeatable `--connect URL`, `--ducklake-catalog
  URL` / `--ducklake-path PATH` / `--ducklake-name NAME`.
- Tree pane's `flatten` walks every root in order — filesystem and
  every DB connection appear as siblings at depth 0.
- Preview pane dispatches by row-type-of-selection:
  - File node → existing fs preview (text / image / markdown)
  - DB table node → `LiveTable` showing first 100 rows
- File ops (rename / trash / hard-delete / touch) — guarded: only fire
  when selection is a filesystem entry. Otherwise show "not applicable
  in DB tree" status. (Tree-mode file ops were already deferred from
  the `bobtop-fb` tree-mode work; covered cleanly here.)
- Miller-mode UX preserved exactly for filesystem-only browsing.

**Validation:** smoke test
```
gfb \
  --connect postgresql://root@localhost/ml_momo \
  --connect duckdb:///root/repos/ml_momo/lake_data/ml_momo.duckdb \
  --ducklake-catalog postgresql://root@localhost/ml_momo \
  --ducklake-path /root/repos/ml_momo/lake_data
```
shows filesystem + 3 DB catalogs as siblings; preview panel updates
correctly for each.

### Phase 6 · Retire `bobtop-db` (~30min)

- Delete `crates/bobtop-db/`.
- Remove from workspace `[members]`.
- Update memory notes / READMEs / any references.
- Drop `bobtop-db = { ... }` from `[workspace.dependencies]`.

**Validation:** `cargo test --workspace --all-targets` green; the
workspace contains exactly three crates.

### Phase 7 · Release readiness (~3-4h)

Everything that turns "code that works" into "crate that's ready to
publish."

- **Public API audit on `gtui`.** Walk every `pub` item; downgrade
  implementation details to `pub(crate)`. Final API ≈ what we'd commit
  to keeping stable.
- **Rustdoc pass on `gtui`.** Every public item documented: what it
  does, when to use it, invariants, tiny example for the marquee
  types (LiveTable, Catalog, BrowserShell, Theme, ScopeStack).
- **Per-crate `README.md`.** `gtui`'s reads as a library README
  (install / usage / example). `gtop`'s and `gfb`'s read as
  end-user docs (what is it, why use it, screenshots).
- **`gtui` examples directory.** 3-4 minimal example binaries:
  - `examples/process_table.rs` — a sortable LiveTable demo
  - `examples/two_pane_browser.rs` — Catalog + BrowserShell demo
  - `examples/themes.rs` — load and preview the bundled themes
- **Version alignment.** Workspace at `0.2.0` today; bump to `0.3.0`
  for the first public release. Or `0.4.0` if we want a clean break
  from anything that ever shipped under the bobtop name.
- **`CHANGELOG.md`** at workspace root summarising the rebrand and
  every phase's user-visible change.
- **CI matrix.** GitHub Actions (or whatever): `cargo check -p X` and
  `cargo test -p X` for each published crate, plus the
  `--features all-sources` matrix on `gfb`.
- **License headers / `LICENSE-MIT` / `LICENSE-APACHE`** verified
  present at workspace root and copied / referenced in each crate
  Cargo.toml.

**Validation:** `cargo publish --dry-run -p gtui` succeeds. Same for
`-p gtop`, `-p gfb`.

## Estimates summary

| Phase | Estimate |
| --- | --- |
| 0 — Rename to gtop family | 2-3h |
| 1 — Consolidate the monitor | 4-6h |
| 2 — `tree` in `gtui` | 2h |
| 3 — `browser` in `gtui` | 2h |
| 4 — Move DB sources into `gfb` | 1.5h |
| 5 — Multi-root tree in `gfb` | 3-4h |
| 6 — Retire `bobtop-db` | 0.5h |
| 7 — Release readiness | 3-4h |
| **Total** | **~18-23h** |

## Branch & commit cadence

- Branch: `refactor/gtop` cut from `main`.
- Each phase: one commit (or a small handful of tightly-scoped commits
  if the diff stays cleaner that way).
- Commit messages: subject line `phase N: <what>`, body explains the
  change and links back to this doc by section.
- This doc itself updates *with* phases as we learn — assumptions
  that turned out wrong get corrected, decisions that came up get
  added under "Decision log" below.
- Merge strategy at the end: **likely a merge commit, not a squash** —
  the phase commits are individually meaningful and worth preserving
  in `main`'s history.

## Decision log

Decisions made or revisited during the work, with brief rationale.
Append-only.

- **2026-05-08 · Rebrand from bobtop to gtop family.** Three crates:
  `gtui`, `gtop`, `gfb`. Internal plumbing crates fold into `gtop`.
  `bobtop-db` dissolves into `gfb`.
- **2026-05-08 · `gfb` keeps its short suffix.** Despite the merge
  growing scope beyond "file browser," `gfb` is opaque enough to
  carry forward without baggage. (vs renaming to `gbrowse` etc.)

## Open questions

Tracked here so they don't get lost across phases.

- **Phase 5 keybinds for switching between tree roots.** Default is
  "the tree is one big list, j/k naturally crosses root boundaries."
  If users want explicit "jump to filesystem root" / "jump to next
  connection" keybinds, that's a follow-up.
- **`bobtop-fb`-style miller mode survival in `gfb`.** Currently
  miller layout is preserved alongside tree mode (toggle with `T`).
  After Phase 5's multi-root tree, miller mode only makes sense for
  the filesystem source. Keep miller as filesystem-only? Drop it?
  Decision deferred — easiest call to make once Phase 5 lands.
- **`bobtop-pid-attr`'s build.rs and bpf-rs deps.** When folded into
  `gtop`'s `src/pid_attr/`, the build script needs to be the gtop
  crate's `build.rs`. If we already have a `build.rs` for some other
  reason, they merge. Verify in Phase 1.
