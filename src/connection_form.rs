//! The "new connection" form shown inside a dialog: on startup when no
//! connection is saved yet, and from the sidebar's "+" button.

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, Disableable as _};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::config::ConnEntry;
use crate::connection::{self, ConnectionParams, Session};
use crate::runtime;

pub struct ConnectionCreated {
    pub entry: ConnEntry,
    pub session: Rc<Session>,
}

pub struct ConnectionForm {
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    error: Option<SharedString>,
    connecting: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ConnectionCreated> for ConnectionForm {}

impl ConnectionForm {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("user@host (default)"));
        let host = cx.new(|cx| InputState::new(window, cx).default_value("127.0.0.1"));
        let port = cx.new(|cx| InputState::new(window, cx).default_value("3306"));
        let user = cx.new(|cx| InputState::new(window, cx).default_value("root"));
        let password = cx.new(|cx| InputState::new(window, cx).masked(true));

        // Enter in any field submits, like a native login form.
        let _subscriptions = [&name, &host, &port, &user, &password]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.submit(window, cx);
                    }
                })
            })
            .collect();

        Self { name, host, port, user, password, error: None, connecting: false, _subscriptions }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.name.update(cx, |input, cx| input.focus(window, cx));
    }

    fn params(&self, cx: &App) -> Result<ConnectionParams, SharedString> {
        let port = self.port.read(cx).value();
        let port = port
            .trim()
            .parse::<u16>()
            .map_err(|_| SharedString::from(format!("invalid port '{port}'")))?;
        let host = self.host.read(cx).value().trim().to_string();
        let user = self.user.read(cx).value().trim().to_string();
        let name = match self.name.read(cx).value().trim() {
            "" => format!("{user}@{host}"),
            name => name.to_string(),
        };
        Ok(ConnectionParams {
            name,
            host,
            port,
            user,
            password: self.password.read(cx).value().to_string(),
        })
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.connecting {
            return;
        }
        let params = match self.params(cx) {
            Ok(params) => params,
            Err(err) => {
                self.error = Some(err);
                cx.notify();
                return;
            }
        };

        self.connecting = true;
        self.error = None;
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let result = runtime::run(connection::create(params)).await.and_then(|r| r);
            this.update(cx, |this, cx| {
                this.connecting = false;
                match result {
                    Ok((entry, session)) => {
                        cx.emit(ConnectionCreated { entry, session: Rc::new(session) })
                    }
                    Err(err) => this.error = Some(format!("{err:#}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

fn field(label: &'static str, input: impl IntoElement) -> Div {
    v_flex().gap_1().child(div().text_sm().child(label)).child(input)
}

impl Render for ConnectionForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.connecting;
        v_flex()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Credentials are tested against the server before saving."),
            )
            .child(field("Name", Input::new(&self.name).disabled(busy)))
            .child(
                h_flex()
                    .gap_3()
                    .child(field("Host", Input::new(&self.host).disabled(busy)).flex_1())
                    .child(field("Port", Input::new(&self.port).disabled(busy)).w(px(96.))),
            )
            .child(field("User", Input::new(&self.user).disabled(busy)))
            .child(field("Password", Input::new(&self.password).mask_toggle().disabled(busy)))
            .when_some(self.error.clone(), |this, err| {
                this.child(
                    div()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .border_1()
                        .border_color(cx.theme().danger)
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(err),
                )
            })
            .child(
                h_flex().justify_end().child(
                    Button::new("connect")
                        .primary()
                        .label(if busy { "Connecting…" } else { "Save & connect" })
                        .loading(busy)
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                ),
            )
    }
}
