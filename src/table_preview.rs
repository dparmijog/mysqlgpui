//! Paginated row preview of one table (`SELECT *`, 500 rows per page),
//! rendered with gpui-component's virtualized `DataTable`.

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::table::{DataTable, TableState};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::*;
use sqldr_core::{paginate, Driver, Table, DEFAULT_PAGE_SIZE};
use tokio_util::sync::CancellationToken;

use crate::results::{self, RowsDelegate};

pub enum PreviewEvent {
    Close,
}

enum State {
    Loading,
    Loaded { table: Entity<TableState<RowsDelegate>>, rows: usize },
    Failed(SharedString),
}

pub struct TablePreview {
    driver: Arc<dyn Driver>,
    db: String,
    meta: Table,
    page: usize,
    state: State,
    /// The in-flight page load. Replacing it drops (cancels) the previous
    /// one, so a slow earlier page can never overwrite a newer one.
    _load: Option<Task<()>>,
}

impl EventEmitter<PreviewEvent> for TablePreview {}

impl TablePreview {
    pub fn new(
        driver: Arc<dyn Driver>,
        db: String,
        meta: Table,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self { driver, db, meta, page: 0, state: State::Loading, _load: None };
        this.load(0, window, cx);
        this
    }

    fn load(&mut self, page: usize, window: &mut Window, cx: &mut Context<Self>) {
        let base = format!(
            "SELECT * FROM {}",
            self.driver.dialect().qualify_table(&self.db, &self.meta.name)
        );
        let sql = paginate(&base, page, DEFAULT_PAGE_SIZE).expect("a bare SELECT is always paginable");
        let driver = self.driver.clone();
        let db = self.db.clone();

        self.page = page;
        self.state = State::Loading;
        cx.notify();

        self._load = Some(cx.spawn_in(window, async move |this, cx| {
            let result = results::fetch(driver, sql, Some(db), CancellationToken::new()).await;
            this.update_in(cx, |this, window, cx| {
                this.state = match result {
                    Ok(rows) => {
                        let count = rows.len();
                        let meta = &this.meta;
                        let delegate = RowsDelegate::new(
                            rows,
                            meta.columns.iter().map(|c| c.name.clone()).collect(),
                            |name| meta.columns.iter().find(|c| c.name == name).map(|c| c.ty.clone()),
                        );
                        let table = cx.new(|cx| TableState::new(delegate, window, cx));
                        State::Loaded { table, rows: count }
                    }
                    Err(err) => State::Failed(format!("{err:#}").into()),
                };
                cx.notify();
            })
            .ok();
        }));
    }
}

impl Render for TablePreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let loading = matches!(self.state, State::Loading);
        let rows = match self.state {
            State::Loaded { rows, .. } => rows,
            _ => 0,
        };
        let first = self.page * DEFAULT_PAGE_SIZE as usize;
        let status: SharedString = match &self.state {
            State::Loading => "loading…".into(),
            State::Failed(_) => "error".into(),
            State::Loaded { rows: 0, .. } => "no rows".into(),
            State::Loaded { rows, .. } => format!("rows {}–{}", first + 1, first + rows).into(),
        };
        let has_next = !loading && rows == DEFAULT_PAGE_SIZE as usize;

        let header = h_flex()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new("back")
                    .ghost()
                    .small()
                    .icon(IconName::ArrowLeft)
                    .tooltip("Back to tables")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(PreviewEvent::Close))),
            )
            .child(div().text_color(theme.muted_foreground).child(format!("{} /", self.db)))
            .child(div().font_weight(FontWeight::SEMIBOLD).child(self.meta.name.clone()))
            .child(div().flex_1())
            .child(div().text_sm().text_color(theme.muted_foreground).child(status))
            .child(
                Button::new("prev-page")
                    .ghost()
                    .small()
                    .icon(IconName::ChevronLeft)
                    .tooltip("Previous page")
                    .disabled(loading || self.page == 0)
                    .on_click(cx.listener(|this, _, window, cx| this.load(this.page - 1, window, cx))),
            )
            .child(
                Button::new("next-page")
                    .ghost()
                    .small()
                    .icon(IconName::ChevronRight)
                    .tooltip("Next page")
                    .disabled(!has_next)
                    .on_click(cx.listener(|this, _, window, cx| this.load(this.page + 1, window, cx))),
            );

        let message = |text: SharedString, color: Hsla| {
            div().p_4().text_sm().text_color(color).child(text).into_any_element()
        };
        let body = match &self.state {
            State::Loading => message("Loading rows…".into(), theme.muted_foreground),
            State::Failed(err) => message(err.clone(), theme.danger),
            State::Loaded { table, .. } => DataTable::new(table).stripe(true).into_any_element(),
        };

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
    }
}
