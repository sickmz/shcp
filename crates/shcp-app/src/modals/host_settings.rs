//! All that belongs to one host, in one place: reached from its menu and from the settings. What
//! is a switch or a color applies at once, as in the settings. The connection and the commands
//! have editors of their own, which take its place.
use super::host_appearance::AppearancePicker;
use super::settings::{Setter, row, switch};
use super::{button_row, editor_frame, title};
use crate::state::AppState;
use crate::theme::{ActiveTheme as _, Theme};
use crate::widgets::button::{Button, ButtonStyle};
use crate::widgets::icon::IconName;
use gpui::prelude::*;
use gpui::{
    App, ClipboardItem, Context, DismissEvent, Div, Entity, EventEmitter, FocusHandle, Focusable,
    SharedString, Subscription, Window, div, px,
};
use shcp_core::{HostConfig, HostId, HostSource, address_text};
use std::rc::Rc;

/// An editor to go on to.
#[derive(Clone, Copy)]
pub(crate) enum HostPage {
    Connection,
    KeyLogin,
    Commands,
}

/// Opens an editor. Only one modal shows at a time, so this one closes first.
pub(crate) type OpenHostPage = Rc<dyn Fn(HostPage, &mut Window, &mut App)>;

pub(crate) struct HostSettingsModal {
    embedded: bool,
    state: Entity<AppState>,
    host: HostId,
    picker: Entity<AppearancePicker>,
    open: OpenHostPage,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for HostSettingsModal {}
impl EventEmitter<super::CancelEvent> for HostSettingsModal {}

impl Focusable for HostSettingsModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl HostSettingsModal {
    pub(crate) fn embedded(mut self) -> Self {
        self.embedded = true;
        self
    }

    pub(crate) fn new(
        state: Entity<AppState>,
        host: HostId,
        open: OpenHostPage,
        cx: &mut Context<Self>,
    ) -> Self {
        let appearance = state.read(cx).host_appearance(&host);
        let picker = cx.new(|_| AppearancePicker::new(&appearance));
        let subscriptions = vec![
            cx.observe(&picker, |this, picker, cx| {
                let appearance = picker.read(cx).appearance();
                let host = this.host.clone();
                this.state.update(cx, |state, cx| {
                    state.set_host_appearance(&host, appearance, cx);
                });
                cx.notify();
            }),
            // What the server says about tmux can arrive while this is open.
            cx.observe(&state, |_, _, cx| cx.notify()),
        ];
        Self {
            embedded: false,
            state,
            host,
            picker,
            open,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    fn go_to(&mut self, page: HostPage, window: &mut Window, cx: &mut Context<Self>) {
        if !self.embedded {
            cx.emit(DismissEvent);
        }
        let open = self.open.clone();
        open(page, window, cx);
    }

    /// What the switch for tmux means on this host, as far as the server has said.
    fn keep_help(&self, cx: &App) -> &'static str {
        let state = self.state.read(cx);
        match (
            state.settings.restore_tabs,
            state.keeps_terminals(&self.host),
            state.has_tmux(&self.host),
        ) {
            (false, ..) => "Needs Reopen tabs at startup, under Connections in the settings",
            (_, _, Some(false)) => "This server has no tmux 3.2 or newer",
            (_, true, Some(true)) => "tmux is keeping them running on this server",
            (_, true, None) => {
                "They keep running when shcp closes, if the server has tmux 3.2 or newer"
            }
            (_, false, _) => "They end when shcp closes",
        }
    }

    fn commands_help(&self, cx: &App) -> SharedString {
        let settings = &self.state.read(cx).settings;
        let saved = settings.snippets_for(&self.host).count();
        let startup = match settings.startup_commands.get(&self.host.0) {
            Some(command) => format!("New terminals run {command}"),
            None => "Nothing runs in new terminals".to_string(),
        };
        match saved {
            0 => startup.into(),
            1 => format!("{startup}. 1 saved command").into(),
            saved => format!("{startup}. {saved} saved commands").into(),
        }
    }
}

impl HostSettingsModal {
    /// Who connects where, with the button that copies it and the one that edits it.
    fn connection_row(
        theme: &Theme,
        alias: String,
        address: String,
        target: String,
        editable: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let connection_help: Option<SharedString> =
            (!editable).then(|| "Managed in ~/.ssh/config".into());
        div()
            .flex()
            .items_center()
            .gap(px(16.))
            .min_h(px(48.))
            .py(px(8.))
            .border_b_1()
            .border_color(theme.hover)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(6.))
                            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(alias))
                            .child(div().text_color(theme.text_muted).child("•"))
                            .child(div().min_w_0().text_color(theme.text_muted).child(address)),
                    )
                    .children(connection_help.map(|help| {
                        div()
                            .text_size(crate::theme::ui_px(12.))
                            .text_color(theme.text_muted)
                            .child(help)
                    })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(
                        Button::new("host-copy-target")
                            .keyboard_focus()
                            .icon(IconName::Copy)
                            .tooltip("Copy user@host")
                            .style(ButtonStyle::Ghost)
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(target.clone()));
                            }),
                    )
                    .when(editable, |this| {
                        this.child(
                            Button::new("host-edit-connection")
                                .keyboard_focus()
                                .label("Edit")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.go_to(HostPage::Connection, window, cx)
                                })),
                        )
                    }),
            )
    }
}

impl Render for HostSettingsModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let (alias, target, address, editable, tabs_return, keeping, logging) = {
            let state = self.state.read(cx);
            let host = state.host(&self.host);
            (
                state.alias(&self.host),
                host.map_or(String::new(), HostConfig::target),
                host.map_or(String::new(), |host| {
                    address_text(Some(&host.user), &host.hostname, Some(host.port))
                }),
                host.is_some_and(|host| host.source == HostSource::App),
                state.settings.restore_tabs,
                state.keeps_terminals(&self.host),
                state.logs_sessions(&self.host),
            )
        };
        let toggle = |toggle: fn(&mut AppState, &HostId, &mut Context<AppState>)| -> Setter<bool> {
            let (state, host) = (self.state.clone(), self.host.clone());
            Rc::new(move |_, _, cx: &mut App| {
                state.update(cx, |state, cx| toggle(state, &host, cx));
            })
        };
        let keep_switch = toggle(AppState::toggle_keep_terminals);
        let log_switch = toggle(AppState::toggle_session_log);
        editor_frame(&theme, 520., self.embedded)
            .track_focus(&self.focus)
            .when(!self.embedded, |this| {
                this.child(title(&theme, format!("Settings of {alias}")))
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(Self::connection_row(
                        &theme, alias, address, target, editable, cx,
                    ))
                    .child(row(
                        &theme,
                        "Key login",
                        Some("Log in with a key instead of typing a password".into()),
                        Button::new("host-set-up-key")
                            .keyboard_focus()
                            .label("Set up")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.go_to(HostPage::KeyLogin, window, cx)
                            })),
                    ))
                    .child(row(
                        &theme,
                        "Commands",
                        Some(self.commands_help(cx)),
                        Button::new("host-edit-commands")
                            .keyboard_focus()
                            .label("Edit")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.go_to(HostPage::Commands, window, cx)
                            })),
                    ))
                    .child(row(
                        &theme,
                        "Keep terminals via tmux",
                        Some(self.keep_help(cx).into()),
                        // Without tabs that come back, there would be no way to find them again.
                        if tabs_return {
                            switch(&theme, "host-keep-terminals", keeping, keep_switch)
                        } else {
                            div().into_any_element()
                        },
                    ))
                    .child(row(
                        &theme,
                        "Log terminals to a file",
                        Some("Whatever new terminals show goes there, secrets too".into()),
                        switch(&theme, "host-log-terminals", logging, log_switch),
                    )),
            )
            .child(self.picker.clone())
            .when(!self.embedded, |this| {
                this.child(
                    button_row().child(
                        Button::new("host-settings-close")
                            .keyboard_focus()
                            .label("Close")
                            .style(ButtonStyle::Primary)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    ),
                )
            })
    }
}

impl super::ModalView for HostSettingsModal {}
