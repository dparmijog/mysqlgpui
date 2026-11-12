//! Result-set grid shared by the table preview and the query editor:
//! a `TableDelegate` over a page of rows for gpui-component's virtualized
//! `DataTable`.

use std::sync::Arc;

use futures::TryStreamExt as _;
use gpui_kit::component::table::{Column, TableDelegate, TableState};
use gpui_kit::component::{h_flex, ActiveTheme as _};
use gpui_kit::*;
use sqldr_core::{Driver, Row, Value};
use tokio_util::sync::CancellationToken;

use crate::runtime;

/// Runs a row-returning statement on the tokio runtime and collects it.
/// Cancelling `cancel` ends the stream early with the rows read so far.
pub async fn fetch(
    driver: Arc<dyn Driver>,
    sql: String,
    db: Option<String>,
    cancel: CancellationToken,
) -> anyhow::Result<Vec<Row>> {
    let fetch = async move { driver.query(&sql, db.as_deref(), cancel).try_collect().await };
    runtime::run(fetch).await?
}

/// Longest text drawn in a cell; the full value stays available to copy.
const CELL_PREVIEW_CHARS: usize = 200;

pub struct RowsDelegate {
    columns: Vec<Column>,
    types: Vec<SharedString>,
    rows: Vec<Row>,
    /// Display text per cell, built once per page rather than per frame.
    cells: Vec<Vec<Option<SharedString>>>,
}

impl RowsDelegate {
    /// `fallback_names` labels the columns when the page has no rows (the
    /// result itself carries the names otherwise); `type_of` supplies an
    /// optional type label per column name.
    pub fn new(
        rows: Vec<Row>,
        fallback_names: Vec<String>,
        type_of: impl Fn(&str) -> Option<String>,
    ) -> Self {
        let names = match rows.first() {
            Some(row) => row.cols.clone(),
            None => fallback_names,
        };
        let types: Vec<SharedString> =
            names.iter().map(|n| type_of(n).map(Into::into).unwrap_or_default()).collect();
        let cells: Vec<Vec<Option<SharedString>>> = rows
            .iter()
            .map(|row| row.values.iter().map(cell_preview).collect())
            .collect();
        let columns = names
            .iter()
            .enumerate()
            .map(|(ix, name)| {
                // Width from the header and a sample of values, clamped so
                // one long text column doesn't swallow the view.
                let widest = cells
                    .iter()
                    .take(50)
                    .map(|r| r.get(ix).and_then(|c| c.as_ref()).map_or(4, |c| c.chars().count()))
                    .chain([name.chars().count() + types[ix].chars().count() + 2])
                    .max()
                    .unwrap_or(0);
                Column::new(SharedString::from(name.clone()), SharedString::from(name.clone()))
                    .width(px((widest as f32 * 8.5 + 28.).clamp(70., 360.)))
            })
            .collect();
        Self { columns, types, rows, cells }
    }
}

/// Single-line, length-capped text for a cell; `None` for SQL `NULL`.
/// Long text/JSON would otherwise break row height, and a BLOB's full hex
/// would cost twice its size in memory per cell.
fn cell_preview(value: &Value) -> Option<SharedString> {
    let text = match value {
        Value::Null => return None,
        Value::Bytes(b) if b.len() > 32 => format!("<{} bytes>", b.len()),
        other => other.to_string(),
    };
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("");
    let multiline = lines.next().is_some();
    let mut out: String = first.chars().take(CELL_PREVIEW_CHARS).collect();
    if multiline || first.chars().count() > CELL_PREVIEW_CHARS {
        out.push('…');
    }
    Some(out.into())
}

impl TableDelegate for RowsDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        self.columns[col_ix].clone()
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        // Same line as the name: the header row has a fixed height.
        h_flex()
            .w_full()
            .gap_1p5()
            .overflow_hidden()
            .child(div().flex_shrink_0().child(self.columns[col_ix].name.clone()))
            .child(
                div()
                    .text_xs()
                    .truncate()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.types[col_ix].clone()),
            )
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        match self.cells[row_ix].get(col_ix) {
            Some(Some(text)) => div().truncate().child(text.clone()),
            _ => div().text_color(cx.theme().muted_foreground).child("NULL"),
        }
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        self.rows[row_ix].values.get(col_ix).map_or_else(String::new, ToString::to_string)
    }
}

#[cfg(test)]
mod tests {
    // Explicit imports: the module's `gpui_kit::*` glob would shadow `#[test]`.
    use super::{cell_preview, CELL_PREVIEW_CHARS};
    use sqldr_core::Value;

    #[test]
    fn cell_preview_keeps_one_capped_line() {
        assert_eq!(cell_preview(&Value::Null), None);
        assert_eq!(cell_preview(&Value::Text("a\nb".into())).unwrap(), "a…");
        let long = "x".repeat(CELL_PREVIEW_CHARS + 5);
        let shown = cell_preview(&Value::Text(long)).unwrap();
        assert_eq!(shown.chars().count(), CELL_PREVIEW_CHARS + 1);
        assert_eq!(cell_preview(&Value::Bytes(vec![0; 1000])).unwrap(), "<1000 bytes>");
        assert_eq!(cell_preview(&Value::Bytes(vec![0xab])).unwrap(), "0xab");
    }
}
