//! Main window content: a sidebar tree of saved connections → databases
//! (connections dial lazily on expand) plus a "+" to add new ones, and a
//! main panel listing the selected database's tables. With no saved
//! connection, the "new connection" dialog opens on launch.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, Icon, Sizable as _, WindowExt as _};
use gpui_kit::prelude::*;
use gpui_kit::*;

use sqldr_core::Table;

use crate::appearance::{self, Appearance};
use crate::config::{self, ConnEntry};
use crate::connection::{self, Session};
use crate::connection_form::{ConnectionCreated, ConnectionForm};
use crate::runtime;
use crate::query_panel::{QueryPanel, SchemaMaybeChanged};
use crate::table_preview::{PreviewEvent, TablePreview};

/// What the main panel shows for the selected database.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DbView {
    Tables,
    Query,
}

const TREE_ROW_HEIGHT: f32 = 30.;

/// One line of the sidebar tree.
#[derive(Clone, Copy)]
enum TreeRow {
    Conn(usize),
    Error(usize),
    NoDbs(usize),
    NoMatches(usize),
    Db(usize, usize),
}

enum ConnStatus {
    Idle,
    Connecting,
    Connected(Rc<Session>),
    Failed(SharedString),
}

enum Tables {
    Loading,
    Loaded(Rc<[Table]>),
    Failed(SharedString),
}

struct ConnNode {
    entry: ConnEntry,
    status: ConnStatus,
    expanded: bool,
    /// Tables per database name, loaded on first selection. Reset on
    /// reconnect since they belong to the previous session.
    tables: HashMap<String, Tables>,
}

impl ConnNode {
    fn new(entry: ConnEntry, status: ConnStatus, expanded: bool) -> Self {
        Self { entry, status, expanded, tables: HashMap::new() }
    }
}

pub struct Workspace {
    conns: Vec<ConnNode>,
    load_error: Option<SharedString>,
    /// (connection index, database index)
    selected: Option<(usize, usize)>,
    /// Row preview of a table in the selected database, if one is open.
    preview: Option<Entity<TablePreview>>,
    view: DbView,
    /// One editor per (connection, database), kept so switching databases
    /// doesn't lose what was typed. Dropped on reconnect: they hold the
    /// previous session's driver.
    query_panels: HashMap<(usize, String), (Entity<QueryPanel>, Subscription)>,
    /// Sidebar filter over database names, and the table list filter.
    db_filter: Entity<InputState>,
    table_filter: Entity<InputState>,
    _form_subscription: Option<Subscription>,
    _preview_subscription: Option<Subscription>,
    _filter_subscriptions: [Subscription; 2],
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (conns, load_error) = match config::load() {
            Ok(cfg) => (
                cfg.connections
                    .into_iter()
                    .map(|entry| ConnNode::new(entry, ConnStatus::Idle, false))
                    .collect(),
                None,
            ),
            Err(err) => (Vec::new(), Some(format!("{err:#}").into())),
        };

        if conns.is_empty() && load_error.is_none() {
            // The window Root (which hosts dialogs) is mounted only after
            // this view is built, so the dialog must wait for the next tick.
            cx.defer_in(window, |this, window, cx| this.open_form(window, cx));
        }
        let db_filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter databases…"));
        let table_filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter tables…"));
        let rerender = |_: &mut Self, _: Entity<InputState>, e: &InputEvent, cx: &mut Context<Self>| {
            if let InputEvent::Change = e {
                cx.notify();
            }
        };
        let _filter_subscriptions =
            [cx.subscribe(&db_filter, rerender), cx.subscribe(&table_filter, rerender)];
        Self {
            conns,
            load_error,
            selected: None,
            preview: None,
            _form_subscription: None,
            _preview_subscription: None,
            view: DbView::Tables,
            query_panels: HashMap::new(),
            db_filter,
            table_filter,
            _filter_subscriptions,
        }
    }

    fn open_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let form = cx.new(|cx| ConnectionForm::new(window, cx));
        self._form_subscription = Some(cx.subscribe_in(
            &form,
            window,
            |this, _, event: &ConnectionCreated, window, cx| {
                this.conns.push(ConnNode::new(
                    event.entry.clone(),
                    ConnStatus::Connected(event.session.clone()),
                    true,
                ));
                this._form_subscription = None;
                window.close_dialog(cx);
                cx.notify();
            },
        ));

        // With nothing saved yet the dialog is the only way forward, so it
        // can't be dismissed; afterwards it's an ordinary closable dialog.
        let closable = !self.conns.is_empty();
        let dialog_form = form.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("New MySQL connection")
                .w(px(440.))
                .close_button(closable)
                .keyboard(closable)
                .overlay_closable(closable)
                .child(dialog_form.clone())
        });
        form.update(cx, |form, cx| form.focus(window, cx));
    }

    fn toggle(&mut self, ix: usize, cx: &mut Context<Self>) {
        if matches!(self.conns[ix].status, ConnStatus::Idle | ConnStatus::Failed(_)) {
            self.query_panels.retain(|(c, _), _| *c != ix);
        }
        let node = &mut self.conns[ix];
        match node.status {
            ConnStatus::Connecting => {}
            ConnStatus::Connected(_) => node.expanded = !node.expanded,
            ConnStatus::Idle | ConnStatus::Failed(_) => {
                node.expanded = true;
                node.status = ConnStatus::Connecting;
                node.tables.clear();
                let entry = node.entry.clone();
                cx.spawn(async move |this, cx| {
                    let result = runtime::run(connection::open_saved(entry)).await.and_then(|r| r);
                    this.update(cx, |this, cx| {
                        this.conns[ix].status = match result {
                            Ok(session) => ConnStatus::Connected(Rc::new(session)),
                            Err(err) => ConnStatus::Failed(format!("{err:#}").into()),
                        };
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
        }
        cx.notify();
    }

    fn select_db(&mut self, cix: usize, dix: usize, cx: &mut Context<Self>) {
        self.selected = Some((cix, dix));
        self.preview = None;
        self._preview_subscription = None;
        cx.notify();
        let ConnStatus::Connected(session) = &self.conns[cix].status else { return };
        let db = session.databases[dix].clone();
        // Cached or in flight: nothing to do. A failed load retries.
        if matches!(self.conns[cix].tables.get(&db), Some(Tables::Loading | Tables::Loaded(_))) {
            return;
        }
        self.load_tables(cix, db, cx);
    }

    fn load_tables(&mut self, cix: usize, db: String, cx: &mut Context<Self>) {
        let node = &mut self.conns[cix];
        let ConnStatus::Connected(session) = &node.status else { return };
        node.tables.insert(db.clone(), Tables::Loading);
        cx.notify();

        let driver = session.driver.clone();
        let session = session.clone();
        cx.spawn(async move |this, cx| {
            let load = {
                let db = db.clone();
                async move { driver.tables(&db).await }
            };
            let result = runtime::run(load).await.and_then(|r| r);
            this.update(cx, |this, cx| {
                // Dropped if the connection was re-dialed meanwhile: these
                // tables belong to a session that no longer exists.
                let node = &mut this.conns[cix];
                if !matches!(&node.status, ConnStatus::Connected(s) if Rc::ptr_eq(s, &session)) {
                    return;
                }
                let state = match result {
                    Ok(tables) => {
                        let tables: Rc<[Table]> = tables.into();
                        if let Some((panel, _)) = this.query_panels.get(&(cix, db.clone())) {
                            panel.read(cx).set_schema(tables.clone());
                        }
                        Tables::Loaded(tables)
                    }
                    Err(err) => Tables::Failed(format!("{err:#}").into()),
                };
                this.conns[cix].tables.insert(db, state);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn open_table(
        &mut self,
        cix: usize,
        db: String,
        table: Table,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ConnStatus::Connected(session) = &self.conns[cix].status else { return };
        let driver = session.driver.clone();
        let preview = cx.new(|cx| TablePreview::new(driver, db, table, window, cx));
        self._preview_subscription =
            Some(cx.subscribe(&preview, |this, _, event: &PreviewEvent, cx| match event {
                PreviewEvent::Close => {
                    this.preview = None;
                    this._preview_subscription = None;
                    cx.notify();
                }
            }));
        self.preview = Some(preview);
        cx.notify();
    }

    fn show_view(&mut self, view: DbView, window: &mut Window, cx: &mut Context<Self>) {
        self.view = view;
        cx.notify();
        if view != DbView::Query {
            return;
        }
        let Some((cix, dix)) = self.selected else { return };
        let node = &self.conns[cix];
        let ConnStatus::Connected(session) = &node.status else { return };
        let db = session.databases[dix].clone();
        let panel = match self.query_panels.get(&(cix, db.clone())) {
            Some((panel, _)) => panel.clone(),
            None => {
                let (driver, read_only) = (session.driver.clone(), node.entry.read_only);
                let panel =
                    cx.new(|cx| QueryPanel::new(driver, db.clone(), read_only, window, cx));
                if let Some(Tables::Loaded(tables)) = node.tables.get(&db) {
                    panel.read(cx).set_schema(tables.clone());
                }
                // DDL/DML may have added or dropped tables: refresh the list.
                let refresh_db = db.clone();
                let sub = cx.subscribe(&panel, move |this, _, _: &SchemaMaybeChanged, cx| {
                    this.load_tables(cix, refresh_db.clone(), cx)
                });
                self.query_panels.insert((cix, db), (panel.clone(), sub));
                panel
            }
        };
        panel.update(cx, |panel, cx| panel.focus(window, cx));
    }

    /// Flat view of the tree as currently expanded: what the virtualized
    /// sidebar list indexes into. While the filter has text, connected
    /// connections show only matching databases, expanded or not; the
    /// others can't be searched until they connect.
    fn tree_rows(&self, cx: &App) -> Vec<TreeRow> {
        let filter = self.db_filter.read(cx).value().trim().to_lowercase();
        let mut rows = Vec::new();
        for (cix, node) in self.conns.iter().enumerate() {
            rows.push(TreeRow::Conn(cix));
            match &node.status {
                ConnStatus::Connected(s) if !filter.is_empty() => {
                    let hits = s.databases.iter().enumerate().filter(|(_, db)| matches(db, &filter));
                    let before = rows.len();
                    rows.extend(hits.map(|(dix, _)| TreeRow::Db(cix, dix)));
                    if rows.len() == before {
                        rows.push(TreeRow::NoMatches(cix));
                    }
                }
                _ if !node.expanded => {}
                ConnStatus::Failed(_) => rows.push(TreeRow::Error(cix)),
                ConnStatus::Connected(s) if s.databases.is_empty() => {
                    rows.push(TreeRow::NoDbs(cix))
                }
                ConnStatus::Connected(s) => {
                    rows.extend((0..s.databases.len()).map(|dix| TreeRow::Db(cix, dix)))
                }
                ConnStatus::Idle | ConnStatus::Connecting => {}
            }
        }
        rows
    }

    fn render_tree_row(&self, row: TreeRow, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        // Every row has the same height: `uniform_list` measures the first
        // and assumes the rest match.
        let base = |id: ElementId| {
            // `w_full`: list items otherwise shrink to their content.
            h_flex().id(id).w_full().h(px(TREE_ROW_HEIGHT)).gap_1p5().pr_2().rounded_md().text_sm()
        };
        match row {
            TreeRow::Conn(cix) => {
                let node = &self.conns[cix];
                let chevron =
                    if node.expanded { IconName::ChevronDown } else { IconName::ChevronRight };
                let status: Option<SharedString> = match &node.status {
                    ConnStatus::Connecting => Some("connecting…".into()),
                    ConnStatus::Connected(s) => Some(s.databases.len().to_string().into()),
                    _ => None,
                };
                base(ElementId::NamedInteger("conn".into(), cix as u64))
                    .pl_2()
                    .cursor_pointer()
                    .hover(|this| this.bg(theme.accent))
                    .child(Icon::new(chevron).small().text_color(theme.muted_foreground))
                    .child(Icon::new(IconName::Server).small())
                    .child(div().flex_1().truncate().child(node.entry.name.clone()))
                    .when_some(status, |this, status| {
                        this.child(div().text_xs().text_color(theme.muted_foreground).child(status))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(cix, cx)))
                    .into_any_element()
            }
            TreeRow::Error(cix) => {
                let ConnStatus::Failed(err) = &self.conns[cix].status else {
                    return div().into_any_element();
                };
                // One line to keep the row height; the full text on hover.
                let full = err.clone();
                base(ElementId::NamedInteger("conn-error".into(), cix as u64))
                    .pl(px(34.))
                    .text_xs()
                    .text_color(theme.danger)
                    .child(div().truncate().child(err.clone()))
                    .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
                    .into_any_element()
            }
            TreeRow::NoDbs(cix) | TreeRow::NoMatches(cix) => {
                let (key, text) = match row {
                    TreeRow::NoDbs(_) => ("no-dbs", "No databases visible to this user."),
                    _ => ("no-matches", "No matching databases."),
                };
                base(ElementId::NamedInteger(key.into(), cix as u64))
                    .pl(px(34.))
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(text)
                    .into_any_element()
            }
            TreeRow::Db(cix, dix) => {
                let ConnStatus::Connected(session) = &self.conns[cix].status else {
                    return div().into_any_element();
                };
                let selected = self.selected == Some((cix, dix));
                base(ElementId::NamedInteger(format!("db-{cix}").into(), dix as u64))
                    .pl(px(30.))
                    .cursor_pointer()
                    .when(selected, |this| this.bg(theme.list_active))
                    .when(!selected, |this| this.hover(|this| this.bg(theme.accent)))
                    .child(Icon::new(IconName::Database).small().text_color(theme.muted_foreground))
                    .child(div().truncate().child(session.databases[dix].clone()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_db(cix, dix, cx);
                        // Stay on the same tab for the new database.
                        let view = this.view;
                        this.show_view(view, window, cx);
                    }))
                    .into_any_element()
            }
        }
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // Computed once per render and shared with the list's row builder.
        let rows: Rc<[TreeRow]> = self.tree_rows(cx).into();
        // Only the rows in view are built and laid out each frame.
        let tree = uniform_list(
            "tree",
            rows.len(),
            cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                rows[range].iter().map(|row| this.render_tree_row(*row, cx)).collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .min_h_0()
        .p_2();
        let theme = cx.theme();
        let empty_hint = match &self.load_error {
            Some(err) => Some((err.clone(), theme.danger)),
            None if self.conns.is_empty() => {
                Some(("No connections yet — add one with +.".into(), theme.muted_foreground))
            }
            None => None,
        };

        v_flex()
            .w(px(280.))
            .h_full()
            .border_r_1()
            .border_color(theme.border)
            .bg(theme.sidebar)
            .child(
                h_flex()
                    .justify_between()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Connections"))
                    .child(
                        Button::new("add-connection")
                            .ghost()
                            .small()
                            .icon(IconName::Plus)
                            .tooltip("New connection")
                            .on_click(cx.listener(|this, _, window, cx| this.open_form(window, cx))),
                    ),
            )
            .when(!self.conns.is_empty(), |this| {
                this.child(
                    div().px_2().pt_2().child(
                        Input::new(&self.db_filter).small().cleanable(true).prefix(
                            Icon::new(IconName::Search).small().text_color(theme.muted_foreground),
                        ),
                    ),
                )
            })
            .when_some(empty_hint, |this, (text, color)| {
                this.child(div().p_4().text_sm().text_color(color).child(text))
            })
            .child(tree)
            .child(self.render_theme_switch(cx))
    }

    /// Fixed footer of the sidebar: it sits outside the scrolling tree, so
    /// it stays put however long the connection list gets.
    fn render_theme_switch(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = appearance::current(cx);
        let theme = cx.theme();
        let option = |id: &'static str, icon: IconName, label: &'static str, value: Appearance| {
            Button::new(id)
                .small()
                .flex_1()
                .icon(icon)
                .label(label)
                .map(|b| if current == value { b.primary() } else { b.ghost() })
                .on_click(cx.listener(move |_, _, window, cx| {
                    if appearance::current(cx) == value {
                        return;
                    }
                    if let Err(err) = appearance::set(value, cx) {
                        window.push_notification(
                            SharedString::from(format!("Theme changed but not saved: {err:#}")),
                            cx,
                        );
                    }
                }))
        };
        h_flex()
            .gap_1()
            .p_2()
            .border_t_1()
            .border_color(theme.border)
            .child(option("theme-light", IconName::Sun, "Light", Appearance::Light))
            .child(option("theme-dark", IconName::Moon, "Dark", Appearance::Dark))
    }

    fn render_main(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(preview) = &self.preview {
            return preview.clone().into_any_element();
        }
        let theme = cx.theme();
        let selection = self.selected.and_then(|(cix, dix)| {
            let node = &self.conns[cix];
            let ConnStatus::Connected(session) = &node.status else { return None };
            let db = session.databases.get(dix)?;
            Some((node, db))
        });
        let Some((node, db)) = selection else {
            return div()
                .flex_1()
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child("Select a database from the sidebar.")
                .into_any_element();
        };

        let state = node.tables.get(db);
        let count = match state {
            Some(Tables::Loaded(tables)) => Some(format!("{} tables", tables.len())),
            _ => None,
        };
        let tab = |id: &'static str, label: &'static str, view: DbView| {
            Button::new(id)
                .small()
                .label(label)
                .map(|b| if self.view == view { b.primary() } else { b.ghost() })
                .on_click(cx.listener(move |this, _, window, cx| this.show_view(view, window, cx)))
        };
        let header = h_flex()
            .gap_2()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(Icon::new(IconName::Database).text_color(theme.muted_foreground))
            .child(div().font_weight(FontWeight::SEMIBOLD).child(db.clone()))
            .child(div().text_sm().text_color(theme.muted_foreground).child(node.entry.name.clone()))
            .when_some(count, |this, count| {
                this.child(div().text_sm().text_color(theme.muted_foreground).child(count))
            })
            .child(div().flex_1())
            .child(tab("view-tables", "Tables", DbView::Tables))
            .child(tab("view-query", "Query", DbView::Query));

        let panel = self
            .selected
            .and_then(|(cix, _)| self.query_panels.get(&(cix, db.clone())))
            .map(|(panel, _)| panel.clone());
        if let (DbView::Query, Some(panel)) = (self.view, panel) {
            return v_flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(header)
                .child(panel)
                .into_any_element();
        }

        let message = |text: SharedString, color: Hsla| {
            div().p_4().text_sm().text_color(color).child(text).into_any_element()
        };
        let body = match state {
            None | Some(Tables::Loading) => {
                message("Loading tables…".into(), theme.muted_foreground)
            }
            Some(Tables::Failed(err)) => message(err.clone(), theme.danger),
            Some(Tables::Loaded(tables)) if tables.is_empty() => {
                message("This database has no tables.".into(), theme.muted_foreground)
            }
            Some(Tables::Loaded(tables)) => {
                let row = || h_flex().gap_3().px_4().py_1p5().text_sm();
                let head = row()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground)
                    .child(div().flex_1().child("TABLE"))
                    .child(div().w(px(90.)).text_right().child("COLUMNS"))
                    .child(div().w(px(110.)).text_right().child("FOREIGN KEYS"));
                let filter = self.table_filter.read(cx).value().trim().to_lowercase();
                let visible: Rc<[usize]> = (0..tables.len())
                    .filter(|&tix| filter.is_empty() || matches(&tables[tix].name, &filter))
                    .collect();
                let no_matches = visible.is_empty();
                let (tables, db) = (tables.clone(), db.clone());
                let cix = self.selected.map_or(0, |(cix, _)| cix);
                let this = cx.entity().downgrade();
                // Virtualized: a database can have thousands of tables.
                let list = uniform_list("tables", visible.len(), move |range, _, cx| {
                    let theme = cx.theme();
                    visible[range]
                        .iter()
                        .map(|&tix| {
                            let table = &tables[tix];
                            let (this, tables, db) = (this.clone(), tables.clone(), db.clone());
                            h_flex()
                                .id(("table", tix))
                                .w_full()
                                .h(px(34.))
                                .gap_3()
                                .px_4()
                                .text_sm()
                                .cursor_pointer()
                                .hover(|this| this.bg(theme.accent))
                                .border_b_1()
                                .border_color(theme.border)
                                .child(
                                    h_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .gap_2()
                                        .child(
                                            Icon::new(IconName::Table)
                                                .small()
                                                .text_color(theme.muted_foreground),
                                        )
                                        .child(div().truncate().child(table.name.clone())),
                                )
                                .child(
                                    div()
                                        .w(px(90.))
                                        .text_right()
                                        .child(table.columns.len().to_string()),
                                )
                                .child(
                                    div()
                                        .w(px(110.))
                                        .text_right()
                                        .text_color(theme.muted_foreground)
                                        .child(match table.foreign_keys.len() {
                                            0 => "—".to_string(),
                                            n => n.to_string(),
                                        }),
                                )
                                .on_click(move |_, window, cx| {
                                    let table = tables[tix].clone();
                                    this.update(cx, |this, cx| {
                                        this.open_table(cix, db.clone(), table, window, cx)
                                    })
                                    .ok();
                                })
                        })
                        .collect::<Vec<_>>()
                })
                .flex_1()
                .min_h_0();
                // `min_h_0` lets this column shrink below its content so the
                // list scrolls instead of overflowing the window.
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div().px_4().pt_2().pb_1().child(
                            Input::new(&self.table_filter).small().cleanable(true).prefix(
                                Icon::new(IconName::Search)
                                    .small()
                                    .text_color(theme.muted_foreground),
                            ),
                        ),
                    )
                    .child(head)
                    .when(no_matches, |this| {
                        this.child(message("No matching tables.".into(), theme.muted_foreground))
                    })
                    .child(list)
                    .into_any_element()
            }
        };

        v_flex().flex_1().min_w_0().h_full().child(header).child(body).into_any_element()
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sidebar = self.render_sidebar(cx);
        let main = self.render_main(cx);
        let theme = cx.theme();
        h_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(sidebar)
            .child(main)
    }
}

/// Case-insensitive substring match; `filter` must already be lowercase.
fn matches(name: &str, filter: &str) -> bool {
    name.to_lowercase().contains(filter)
}
