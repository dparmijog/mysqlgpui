//! SQL editor + results for one database: runs the selection (or the whole
//! buffer) with Ctrl+Enter or the Run button.

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, InputEvent, ShowCompletions};
use gpui_kit::component::table::{DataTable, TableState};
use gpui_kit::component::{
    h_flex, v_flex, ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _,
};
use gpui_kit::prelude::*;
use gpui_kit::*;
use sqldr_core::{
    is_mutating, needs_where_confirmation, paginate, Driver, Table, DEFAULT_PAGE_SIZE,
};
use tokio_util::sync::CancellationToken;

use crate::completion::{SchemaHandle, SqlCompletions};
use crate::results::{self, RowsDelegate};
use crate::runtime;

/// gpui-component ships no key for opening completions on demand (they
/// still pop up while typing): Ctrl+Space, as in most editors.
pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("ctrl-space", ShowCompletions, Some("Input"))]);
}

/// The server's message. sqlx errors already embed their cause in their own
/// text, so the full `{:#}` chain would print it twice.
fn server_error(err: &anyhow::Error) -> SharedString {
    err.to_string().into()
}

/// Drops leading whitespace and MySQL comments (`-- …`, `# …`, `/* … */`).
/// An unterminated comment swallows the rest, leaving nothing to run.
fn strip_leading_comments(sql: &str) -> &str {
    let mut rest = sql.trim_start();
    loop {
        if rest.starts_with("--") || rest.starts_with('#') {
            rest = rest.find('\n').map_or("", |nl| &rest[nl + 1..]).trim_start();
        } else if let Some(body) = rest.strip_prefix("/*") {
            rest = body.find("*/").map_or("", |end| &body[end + 2..]).trim_start();
        } else {
            return rest;
        }
    }
}

/// A statement that changed data or schema ran; the table list of this
/// database may be stale.
pub struct SchemaMaybeChanged;

/// Paging context of the last row-returning run: `paginate` added a
/// `LIMIT/OFFSET` to `base_sql`. Absent when the user wrote their own `LIMIT`.
struct Paging {
    base_sql: String,
    page: usize,
}

enum Outcome {
    Idle,
    /// `cancel` is `None` for writes: `execute` can't be interrupted.
    Running { cancel: Option<CancellationToken> },
    Rows {
        table: Entity<TableState<RowsDelegate>>,
        count: usize,
        elapsed: Duration,
        cancelled: bool,
    },
    Affected { count: u64, elapsed: Duration },
    Failed(SharedString),
}

pub struct QueryPanel {
    driver: Arc<dyn Driver>,
    db: String,
    read_only: bool,
    editor: Entity<EditorState>,
    outcome: Outcome,
    paging: Option<Paging>,
    /// Feeds table/column names to the editor's autocomplete.
    schema: SchemaHandle,
    /// Dropping it (on a new run) abandons the old result, so a slow
    /// earlier query can never overwrite a newer one.
    _run: Option<Task<()>>,
    _subscription: Subscription,
}

impl EventEmitter<SchemaMaybeChanged> for QueryPanel {}

impl QueryPanel {
    pub fn new(
        driver: Arc<dyn Driver>,
        db: String,
        read_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| EditorState::new(window, cx).language("sql").folding(false));
        let schema = SchemaHandle::default();
        let completions = SqlCompletions::new(driver.dialect().keywords(), schema.clone());
        editor.update(cx, |editor, _| editor.lsp_mut().completion_provider = Some(Rc::new(completions)));
        // Ctrl+Enter (Cmd+Enter on macOS) arrives as a "secondary" Enter.
        let _subscription = cx.subscribe_in(&editor, window, |this, _, event, window, cx| {
            if let InputEvent::PressEnter { secondary: true, .. } = event {
                this.run(window, cx);
            }
        });
        Self {
            driver,
            db,
            read_only,
            editor,
            outcome: Outcome::Idle,
            paging: None,
            schema,
            _run: None,
            _subscription,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.editor.update(cx, |editor, cx| editor.focus(window, cx));
    }

    /// Updates the table/column names offered by autocomplete.
    pub fn set_schema(&self, tables: Rc<[Table]>) {
        *self.schema.borrow_mut() = tables;
    }

    /// The selection if there is one, else the whole buffer, without
    /// leading comments (the guards classify a statement by its first
    /// keyword, so `-- note\nDELETE …` must be seen as a DELETE) and
    /// without a trailing `;` (so the auto `LIMIT` lands inside it).
    fn statement(&self, cx: &App) -> String {
        let editor = self.editor.read(cx);
        let selected = editor.selected_text().to_string();
        let sql = if selected.trim().is_empty() { editor.value().to_string() } else { selected };
        strip_leading_comments(&sql).trim_end().trim_end_matches(';').trim_end().to_string()
    }

    fn run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sql = self.statement(cx);
        if sql.is_empty() || matches!(self.outcome, Outcome::Running { .. }) {
            return;
        }
        if self.read_only && is_mutating(&sql) {
            self.outcome = Outcome::Failed(
                "This connection is read-only; that statement looks like a write.".into(),
            );
            cx.notify();
            return;
        }
        if needs_where_confirmation(&sql) {
            let this = cx.entity().downgrade();
            window.open_alert_dialog(cx, move |dialog, _, _| {
                let this = this.clone();
                let sql = sql.clone();
                dialog
                    .title("Run without WHERE?")
                    .description("This UPDATE/DELETE has no WHERE clause and will affect every row of the table.")
                    .ok_text("Run anyway")
                    .ok_variant(ButtonVariant::Danger)
                    .cancel_text("Cancel")
                    .show_cancel(true)
                    .on_ok(move |_, window, cx| {
                        this.update(cx, |this, cx| this.start(sql.clone(), 0, window, cx)).ok();
                        true
                    })
            });
            return;
        }
        self.start(sql, 0, window, cx);
    }

    fn start(&mut self, sql: String, page: usize, window: &mut Window, cx: &mut Context<Self>) {
        let driver = self.driver.clone();
        let db = self.db.clone();
        let started = Instant::now();

        if is_mutating(&sql) {
            self.paging = None;
            self.outcome = Outcome::Running { cancel: None };
            cx.notify();
            self._run = Some(cx.spawn_in(window, async move |this, cx| {
                let exec = async move { driver.execute(&sql, Some(&db)).await };
                let result = runtime::run(exec).await.and_then(|r| r);
                this.update(cx, |this, cx| {
                    this.outcome = match result {
                        Ok(count) => {
                            cx.emit(SchemaMaybeChanged);
                            Outcome::Affected { count, elapsed: started.elapsed() }
                        }
                        Err(err) => Outcome::Failed(server_error(&err)),
                    };
                    cx.notify();
                })
                .ok();
            }));
            return;
        }

        let (exec_sql, paging) = match paginate(&sql, page, DEFAULT_PAGE_SIZE) {
            Some(paged) => (paged, Some(Paging { base_sql: sql, page })),
            None => (sql, None),
        };
        let cancel = CancellationToken::new();
        self.paging = paging;
        self.outcome = Outcome::Running { cancel: Some(cancel.clone()) };
        cx.notify();
        self._run = Some(cx.spawn_in(window, async move |this, cx| {
            let result = results::fetch(driver, exec_sql, Some(db), cancel.clone()).await;
            this.update_in(cx, |this, window, cx| {
                this.outcome = match result {
                    Ok(rows) => {
                        let count = rows.len();
                        let delegate = RowsDelegate::new(rows, Vec::new(), |_| None);
                        let table = cx.new(|cx| TableState::new(delegate, window, cx));
                        Outcome::Rows {
                            table,
                            count,
                            elapsed: started.elapsed(),
                            cancelled: cancel.is_cancelled(),
                        }
                    }
                    Err(err) => Outcome::Failed(server_error(&err)),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Outcome::Running { cancel: Some(cancel) } = &self.outcome {
            cancel.cancel();
        }
        cx.notify();
    }

    fn go_to_page(&mut self, page: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(paging) = &self.paging {
            let sql = paging.base_sql.clone();
            self.start(sql, page, window, cx);
        }
    }

    fn render_status(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let ms = |d: &Duration| format!("{} ms", d.as_millis());
        let (text, color): (SharedString, Hsla) = match &self.outcome {
            Outcome::Idle => ("Ctrl+Enter runs the selection, or everything.".into(), theme.muted_foreground),
            Outcome::Running { .. } => ("Running…".into(), theme.muted_foreground),
            Outcome::Affected { count, elapsed } => (
                format!("{count} row(s) affected · {}", ms(elapsed)).into(),
                theme.muted_foreground,
            ),
            Outcome::Rows { count, elapsed, cancelled: true, .. } => (
                format!("cancelled after {count} row(s) · {}", ms(elapsed)).into(),
                theme.muted_foreground,
            ),
            Outcome::Rows { count, elapsed, .. } => {
                let range = match &self.paging {
                    Some(p) if *count > 0 => {
                        let first = p.page * DEFAULT_PAGE_SIZE as usize;
                        format!("rows {}–{}", first + 1, first + count)
                    }
                    _ => format!("{count} row(s)"),
                };
                (format!("{range} · {}", ms(elapsed)).into(), theme.muted_foreground)
            }
            Outcome::Failed(_) => ("Error".into(), theme.danger),
        };
        div().text_sm().text_color(color).child(text)
    }
}

impl Render for QueryPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let running = matches!(self.outcome, Outcome::Running { .. });
        let cancellable = matches!(self.outcome, Outcome::Running { cancel: Some(_) });
        // Pager only next to rows: running keeps it (disabled) so the
        // toolbar doesn't jump while the next page loads.
        let page = self
            .paging
            .as_ref()
            .filter(|_| matches!(self.outcome, Outcome::Rows { .. } | Outcome::Running { .. }))
            .map(|p| p.page);
        let full_page =
            matches!(&self.outcome, Outcome::Rows { count, .. } if *count == DEFAULT_PAGE_SIZE as usize);

        let toolbar = h_flex()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("run")
                    .primary()
                    .small()
                    .icon(IconName::Play)
                    .label("Run")
                    .tooltip("Run (Ctrl+Enter)")
                    .loading(running)
                    .disabled(running)
                    .on_click(cx.listener(|this, _, window, cx| this.run(window, cx))),
            )
            .when(cancellable, |this| {
                this.child(
                    Button::new("cancel")
                        .ghost()
                        .small()
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                )
            })
            .child(div().flex_1())
            .child(self.render_status(cx))
            .when_some(page, |this, page| {
                this.child(
                    Button::new("prev-page")
                        .ghost()
                        .small()
                        .icon(IconName::ChevronLeft)
                        .tooltip("Previous page")
                        .disabled(running || page == 0)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.go_to_page(page - 1, window, cx)
                        })),
                )
                .child(
                    Button::new("next-page")
                        .ghost()
                        .small()
                        .icon(IconName::ChevronRight)
                        .tooltip("Next page")
                        .disabled(running || !full_page)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.go_to_page(page + 1, window, cx)
                        })),
                )
            });

        let theme = cx.theme();
        let message = |text: SharedString, color: Hsla| {
            div().p_4().text_sm().text_color(color).child(text).into_any_element()
        };
        let results = match &self.outcome {
            Outcome::Idle | Outcome::Running { .. } => div().into_any_element(),
            Outcome::Failed(err) => message(err.clone(), theme.danger),
            Outcome::Affected { count, .. } => {
                message(format!("{count} row(s) affected.").into(), theme.muted_foreground)
            }
            Outcome::Rows { count: 0, .. } => {
                message("The statement returned no rows.".into(), theme.muted_foreground)
            }
            Outcome::Rows { table, .. } => DataTable::new(table).stripe(true).into_any_element(),
        };

        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .h(px(200.))
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(Editor::new(&self.editor).h_full().appearance(false)),
            )
            .child(toolbar)
            .child(div().flex_1().min_h_0().child(results))
    }
}

#[cfg(test)]
mod tests {
    // Explicit imports: the module's `gpui_kit::*` glob would shadow `#[test]`.
    use super::strip_leading_comments;
    use sqldr_core::{is_mutating, needs_where_confirmation};

    #[test]
    fn a_leading_comment_does_not_hide_a_write_from_the_guards() {
        let sql = strip_leading_comments("-- cleanup\n# old\n/* multi\nline */  DELETE FROM t");
        assert_eq!(sql, "DELETE FROM t");
        assert!(is_mutating(sql));
        assert!(needs_where_confirmation(sql));
    }

    #[test]
    fn an_unterminated_comment_leaves_nothing_to_run() {
        assert_eq!(strip_leading_comments("/* DELETE FROM t"), "");
        assert_eq!(strip_leading_comments("-- only a note"), "");
    }
}
