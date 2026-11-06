//! The "Ports" tab of the bottom dock: the tunnels of the active host, as a grid.
use crate::state::{AppState, ForwardInfo};
use crate::theme::{ActiveTheme as _, Theme, row_px};
use crate::widgets::button::{Button, ButtonStyle};
use crate::widgets::forward_fields::ForwardFields;
use crate::widgets::icon::IconName;
use crate::widgets::text_input::{TextInput, TextInputEvent};
use crate::widgets::tooltip::tooltip;
use gpui::prelude::*;
use gpui::{AnyElement, Context, Entity, Hsla, SharedString, Subscription, Window, div, px};
use shcp_core::{ForwardKind, ForwardState, HostId, HostSource, SessionId, SessionState};

const PORT_W: f32 = 96.;
const ACTIONS_W: f32 = 64.;
const ROW_H: f32 = 36.;
const HEADING_H: f32 = 30.;
const SIDE_PADDING: f32 = 16.;
const COLUMN_GAP: f32 = 20.;

/// Dot color, state text and its full explanation.
fn describe(forward: &ForwardInfo, theme: &Theme) -> (Hsla, String, String) {
    match &forward.state {
        ForwardState::Listening if forward.active_conns > 0 => {
            let text = format!("Open · {} connections", forward.active_conns);
            (theme.success, text.clone(), text)
        }
        ForwardState::Listening => (
            theme.success,
            "Open".to_string(),
            "Tunnel open. It says nothing about the program on the server until you connect"
                .to_string(),
        ),
        ForwardState::ServerDown if forward.spec.kind == ForwardKind::Remote => (
            theme.warning,
            "Nothing on this PC".to_string(),
            "The server listens, but the last connection found nothing to reach on this PC. \
             It goes back to Open at the next connection that works"
                .to_string(),
        ),
        ForwardState::ServerDown => (
            theme.warning,
            "Server not responding".to_string(),
            "The tunnel is open, but the last connection found nothing on the server. \
             It goes back to Open at the next connection that works"
                .to_string(),
        ),
        ForwardState::InUse { owner } => {
            let owner = owner.as_deref().unwrap_or("another program");
            (
                theme.text_disabled,
                format!("Used by {owner}"),
                format!("{owner} already uses this port on this PC, so shcp is not handling it"),
            )
        }
        ForwardState::Failed(message) => (theme.warning, "Failed".to_string(), message.clone()),
        ForwardState::Stopped => (
            theme.text_disabled,
            "Not open".to_string(),
            "Press the play button to open this tunnel".to_string(),
        ),
    }
}

pub struct PortsPanel {
    state: Entity<AppState>,
    fields: Entity<ForwardFields>,
    adding: bool,
    error: Option<String>,
    _subscriptions: [Subscription; 3],
}

impl PortsPanel {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let fields = cx.new(|cx| ForwardFields::new(None, cx));
        let [bind, target] = fields.read(cx).inputs().map(Entity::clone);
        let subscriptions = [
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.subscribe_in(&bind, window, Self::on_input),
            cx.subscribe_in(&target, window, Self::on_input),
        ];
        Self {
            state,
            fields,
            adding: false,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    /// The active host and its session, when connected.
    pub fn target(&self, cx: &gpui::App) -> Option<(HostId, SessionId)> {
        let state = self.state.read(cx);
        let host = state.active_host.clone()?;
        let session = state.host_session_id(&host)?;
        state
            .session(session)
            .is_some_and(|info| matches!(info.state, SessionState::Connected))
            .then_some((host, session))
    }

    pub fn start_adding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.adding = true;
        self.error = None;
        self.fields.update(cx, |fields, cx| fields.clear(cx));
        self.fields.read(cx).focus(window, cx);
        cx.notify();
    }

    fn on_input(
        &mut self,
        _input: &Entity<TextInput>,
        event: &TextInputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TextInputEvent::Submit => {
                let fields = self.fields.read(cx);
                if fields.is_blank(cx) {
                    return;
                }
                match (fields.spec(cx), self.target(cx)) {
                    (Ok(spec), Some((host, session))) => {
                        self.state
                            .update(cx, |s, cx| s.add_forward(&host, session, spec, cx));
                        self.adding = false;
                    }
                    (Ok(_), None) => self.adding = false,
                    (Err(error), _) => self.error = Some(format!("{error:#}")),
                }
                cx.notify();
            }
            TextInputEvent::Cancel => {
                self.adding = false;
                cx.notify();
            }
            TextInputEvent::Changed => {
                if self.error.take().is_some() {
                    cx.notify();
                }
            }
        }
    }

    fn row(
        &self,
        index: usize,
        forward: &ForwardInfo,
        host: &HostId,
        session: SessionId,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let spec = forward.spec.clone();
        let state = self.state.read(cx);
        let remembered = state.is_remembered(host, &spec);
        let from_config = state.host(host).map(|h| h.source) == Some(HostSource::SshConfig);
        let open = matches!(
            forward.state,
            ForwardState::Listening | ForwardState::ServerDown
        );
        let (color, text, detail) = describe(forward, theme);
        let text = if remembered {
            format!("{text} · added")
        } else {
            text
        };
        let local_host = match spec.bind_addr.as_str() {
            "127.0.0.1" | "localhost" | "0.0.0.0" | "::1" => "localhost",
            other => other,
        };
        let address = format!("{local_host}:{}", spec.bind_port);
        let url = format!("http://{address}");
        // A proxy is no web page, and a port on the server is not reached from here.
        let browsable = open && spec.kind == ForwardKind::Local;
        let address = match spec.kind {
            ForwardKind::Remote => format!("server: {address}"),
            _ => address,
        };
        let primary = if browsable {
            let url = url.clone();
            Some(
                Button::new(("port-browser", index))
                    .icon(IconName::ExternalLink)
                    .tooltip("Open in browser")
                    .style(ButtonStyle::Ghost)
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            )
        } else if open {
            None
        } else {
            let (entity, spec) = (self.state.clone(), spec.clone());
            Some(
                Button::new(("port-open", index))
                    .icon(IconName::Play)
                    .tooltip("Open tunnel")
                    .style(ButtonStyle::Ghost)
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |s, cx| s.open_forward(session, &spec, cx))
                    }),
            )
        };
        let destination = match spec.kind {
            ForwardKind::Local => format!("{}:{}", spec.remote_host, spec.remote_port),
            ForwardKind::Remote => format!("this PC: {}:{}", spec.remote_host, spec.remote_port),
            ForwardKind::Dynamic => "SOCKS proxy".to_string(),
        };
        let origin = if from_config {
            "Defined in ~/.ssh/config"
        } else {
            "Defined in the host settings"
        };
        let close = Button::new(("port-close", index))
            .icon(IconName::Close)
            .style(ButtonStyle::Ghost);
        let close = if remembered {
            let (entity, host, spec) = (self.state.clone(), host.clone(), spec.clone());
            close
                .tooltip("Close and remove this port")
                .on_click(move |_, _, cx| {
                    entity.update(cx, |s, cx| s.forget_forward(&host, session, &spec, cx))
                })
        } else if open {
            let (entity, spec) = (self.state.clone(), spec.clone());
            close
                .tooltip(format!("Close tunnel. {origin}, so it stays in the list"))
                .on_click(move |_, _, cx| {
                    entity.read(cx).engine.stop_forward(session, spec.clone())
                })
        } else {
            close.tooltip(origin).disabled(true)
        };
        div()
            .h(row_px(ROW_H))
            .px(px(SIDE_PADDING))
            .flex()
            .items_center()
            .gap(px(COLUMN_GAP))
            .child(
                div()
                    .w(px(PORT_W))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .font_family(crate::theme::mono_font())
                    .child(div().size(px(8.)).flex_none().rounded_full().bg(color))
                    .child(spec.bind_port.to_string()),
            )
            .child(
                div()
                    .id(("port-address", index))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(crate::theme::mono_font())
                    .text_color(if browsable {
                        theme.accent
                    } else {
                        theme.text_muted
                    })
                    .when(browsable, |this| {
                        this.cursor_pointer()
                            .tooltip(tooltip("Open in browser"))
                            .on_click(move |_, _, cx| cx.open_url(&url))
                    })
                    .child(address),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(crate::theme::mono_font())
                    .text_color(theme.text_muted)
                    .child(destination),
            )
            .child(
                div()
                    .id(("port-state", index))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.text_muted)
                    .tooltip(tooltip(SharedString::from(detail)))
                    .child(text),
            )
            .child(
                div()
                    .w(px(ACTIONS_W))
                    .flex_none()
                    .flex()
                    .justify_end()
                    .gap(px(2.))
                    .children(primary)
                    .child(close),
            )
            .into_any_element()
    }
}

impl Render for PortsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let state = self.state.read(cx);
        let alias = state
            .active_host
            .as_ref()
            .and_then(|host| state.host(host))
            .map(|host| host.alias.clone());
        let target = self.target(cx);
        let forwards: Vec<ForwardInfo> = target
            .as_ref()
            .and_then(|(_, session)| state.session(*session))
            .map_or(Vec::new(), |info| info.forwards.clone());
        let message = match (&alias, &target) {
            (None, _) => Some("Select a host to see its ports.".to_string()),
            (Some(alias), None) => Some(format!("Connect to {alias} to manage its ports.")),
            (Some(_), Some(_)) if forwards.is_empty() => {
                Some("No ports yet. Use Add port to forward one.".to_string())
            }
            _ => None,
        };
        let heading = |label: &'static str| div().flex_1().min_w_0().truncate().child(label);
        div()
            .w_full()
            .min_h_0()
            .flex()
            .flex_col()
            .when(self.adding, |this| {
                this.child(
                    div()
                        .flex_none()
                        .px(px(SIDE_PADDING))
                        .py(px(6.))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .border_b_1()
                        .border_color(theme.border)
                        .child(self.fields.clone())
                        .child(
                            div().flex_none().max_w(px(320.)).truncate().child(
                                match self.error.clone() {
                                    Some(error) => div().text_color(theme.error).child(error),
                                    None => div()
                                        .text_color(theme.text_muted)
                                        .child("Enter to add, Esc to cancel"),
                                },
                            ),
                        ),
                )
            })
            .child(
                div()
                    .flex_none()
                    .h(row_px(HEADING_H))
                    .px(px(SIDE_PADDING))
                    .flex()
                    .items_center()
                    .gap(px(COLUMN_GAP))
                    .text_size(crate::theme::ui_px(13.))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text_disabled)
                    .child(div().w(px(PORT_W)).flex_none().child("Port"))
                    .child(heading("Listens on"))
                    .child(heading("Goes to"))
                    .child(heading("State"))
                    .child(div().w(px(ACTIONS_W)).flex_none()),
            )
            .child(
                div()
                    .id("ports-list")
                    .min_h_0()
                    .pb(px(4.))
                    .overflow_y_scroll()
                    .children(target.as_ref().map(|(host, session)| {
                        div().children(forwards.iter().enumerate().map(|(index, forward)| {
                            self.row(index, forward, host, *session, &theme, cx)
                        }))
                    }))
                    .children(message.map(|message| {
                        div().p(px(10.)).text_color(theme.text_muted).child(message)
                    })),
            )
    }
}
