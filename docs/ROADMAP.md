# Roadmap

Features planned for mysqlgpui, in the agreed order. Numbers match the
original list of ideas so they can be referenced in conversation.

## Up next

1. **(1) Table structure** — a "Structure" tab in the table view: columns,
   types, NULL, keys, indexes and foreign keys (already loaded with the
   table list).
2. **(?)** — to confirm: "1 y 7" was requested after 7 was already done;
   most likely (2) query history.

## Backlog

- **(2) Query history** — reuse `sqldr_core::History` (per-connection JSONL,
  shared with the sqldr TUI) to reload past queries into the editor.
- **(3) EXPLAIN** — button next to Run; `driver.explain()` in the same grid.
- **(4) Edit / delete connections** — context menu on a connection row,
  reusing the connection form.
- **(5) Copy from the grid** — cell, row as JSON / CSV / `INSERT` (sqldr's
  `clipboard.rs` formats, ideally moved into `sqldr-core`).
- **(8) Foreign-key navigation** — click an FK cell to open the referenced
  row (sqldr's `g`).
- **(9) Connection heartbeat** — periodic `SELECT 1`, flag dropped
  connections in the sidebar.
- **(10) Multiple query tabs** in the main panel.
- **(11) Edit data in the grid** — generate `UPDATE … WHERE pk = …`, only for
  tables with a primary key.
- **(12) Export results** to CSV / JSON.
- **(13) PostgreSQL** — `sqldr-core` already supports it; the connection
  form needs an engine selector.

## Technical pending

- **README** — what the app is, requirements (Linux build needs
  `libxkbcommon-x11-devel`), how to build and run.
- **Release profile** — `strip = true` (68.9 MB → 57.5 MB, free); measure
  `lto = "thin"` and `codegen-units = 1` before adopting them (smaller
  binary, slower release builds).
- **Unverified with real input (manual check)** — only exercised through
  smoke hooks, code paths or unit tests so far:
  - mouse-wheel and touchpad scrolling feel (after the optimized-deps change);
  - clicking a connection to expand it; saving a connection from the "+"
    dialog;
  - clicking the Light/Dark toggle, and that the choice persists;
  - Ctrl+Enter and Ctrl+Space on the real keyboard;
  - Cancel, ‹ › result paging;
  - a real write (INSERT/UPDATE/DELETE) from the editor.
- **`target/` growth** — cargo keeps stale artifacts after every feature or
  profile change; run `cargo clean` (or `cargo-sweep`) now and then.

## Done

- Saved connections shared with sqldr (`config.toml` + keyring), sidebar
  tree, new-connection dialog.
- Table list, paginated row preview (`DataTable`), SQL editor with
  highlighting, guards (read-only, UPDATE/DELETE without WHERE, leading
  comments), cancel, paging.
- Light/dark theme toggle, monospace font, virtualized lists.
- **(7) Search** — filter databases in the sidebar (every connected
  connection, expanded or not) and tables in the table list.
- **(6) Autocomplete** — dialect keywords (in the case being typed), table
  and column names of the open database; after `table.` only that table's
  columns. Pops up while typing; Ctrl+Space opens it on demand.
- `sqldr-core` imported into `crates/sqldr-core` with its git history
  (subtree); the repo builds on its own as a cargo workspace.
