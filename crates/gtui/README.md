# gtui

`gtui` is a reusable Ratatui toolkit: widgets, themes, layout, keymap,
and tree-walk primitives. Pulled out of the `gtop` system monitor and
the `gfb` file-and-database browser; designed so a third TUI app on
the same workspace would be cheap to add.

## Install

```toml
[dependencies]
gtui = "0.3"
ratatui = "0.29"
```

## What's in here

- **`widgets/`** — `LiveTable`, `BoxedPanel`, `BrailleGraph`,
  `BrailleText`, `ConfirmDialog`, `EditableText`, `Meter`,
  `MillerColumns`, `ModalShell`, `ScrollableText`, `SelectableList`,
  `SettingsForm`, `Sparkline`, `StackedBar`, `Table`, `ToggleRow`,
  and a handful of tighter primitives. Each is a thin Ratatui
  `Widget` impl with a builder API.
- **`tree`** — `Catalog` trait + `flatten()` walker + `TreeState`
  (expanded set + cursor). Plug in a custom `Catalog` impl to render
  any hierarchical source as a flat row list with depth /
  ancestor-line metadata for `LiveTable`'s tree-glyph mode.
- **`browser`** — `BrowserShell`, a render helper for the
  tree-on-the-left + preview-on-the-right composition both `gtop fb`
  and `gfb` use.
- **`theme`** — btop's `.theme` format parser + 41 bundled themes
  (Dracula, Gruvbox, Tokyo Night, Nord, Solarized, …). Drop your own
  at `~/.config/gtop/themes/<name>.theme` or
  `~/.config/btop/themes/<name>.theme`.
- **`keymap`** — `ScopeStack` for layered keymaps (modal overlays,
  per-mode bindings).
- **`layout`** — responsive frame-splitting given a per-panel
  weight bitmap (`BoxesEnabled`).
- **`text`**, **`color`**, **`util`** — small string / color /
  scroll helpers shared across widgets.

## Examples

```bash
cargo run -p gtui --example process_table
cargo run -p gtui --example two_pane_browser
cargo run -p gtui --example themes
```

`process_table` shows a sortable `LiveTable` with grouping; the other
two demonstrate `Catalog` + `BrowserShell` and the bundled-theme
registry respectively. Each is a single self-contained `.rs` file
under `examples/`.

## Quick taste

```rust
use gtui::widgets::{Cell, ColumnDef, LiveTable, TableEntry, TableRowExt, WidthSpec};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Col { Name, Count }

struct Row { name: &'static str, count: u32 }

impl TableRowExt<Col> for Row {
    fn cell(&self, col: Col) -> Cell {
        match col {
            Col::Name  => Cell::plain(self.name),
            Col::Count => Cell::plain(self.count.to_string()),
        }
    }
}
```

Wire it to a `Frame` in your app's draw fn — see
`examples/process_table.rs` for the rest.

## Stability

The crate sits at `0.3.0` for the first public release. The widgets,
`tree`, and `browser` modules are the surfaces we expect to keep
stable. Anything in `text` / `color` / `util` is helper-tier: useful,
but shifts may happen during the `0.x` line as the consumer apps
discover new patterns.

`#![forbid(unsafe_code)]` on every module.

## License

Dual-licensed under MIT or Apache-2.0 at your option.
