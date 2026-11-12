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

- **`sqldr-core` dependency** — `Cargo.toml` points at the local path
  `../sqldr/crates/sqldr-core`, so a fresh clone doesn't build unless sqldr
  sits next to it. Switch to a git dependency
  (`git = "https://github.com/dparmijog/sqldr.git"`) or document the
  side-by-side layout in a README.
- **README** — what the app is, requirements (Linux build needs
  `libxkbcommon-x11-devel`), how to build and run.
- **Release profile** — `strip = true` (68.9 MB → 57.5 MB, free); measure
  `lto = "thin"` and `codegen-units = 1` before adopting them (smaller
  binary, slower release builds).
- **Not yet exercised with real input** — Ctrl+Enter, Ctrl+Space, Cancel,
  result paging, and a real write (INSERT/UPDATE/DELETE) from the editor were
  only verified by code, smoke hooks or unit tests.
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
