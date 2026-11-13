//! Guided move from a password to a key: creates the key if needed and installs it on the server.
use super::{button_row, editor_frame, expand, ssh_folder, title};
use crate::login_prompt::LoginPrompt;
use crate::state::AppState;
use crate::theme::{ActiveTheme as _, ui_px};
use crate::widgets::button::{Button, ButtonStyle};
use crate::widgets::icon::{Icon, IconName};
use crate::widgets::text_input::{TextInput, TextInputEvent};
use gpui::prelude::*;
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, PathPromptOptions,
    Subscription, Window, div, px,
};
use shcp_core::{Event, HostConfig, HostSource, RequestId, SessionId, USUAL_KEYS};
use std::path::PathBuf;

pub(crate) struct KeySetupModal {
    embedded: bool,
    state: Entity<AppState>,
    host: HostConfig,
    session: SessionId,
    login: Option<Entity<LoginPrompt>>,
    login_subscription: Option<Subscription>,
    path: Entity<TextInput>,
    passphrase: Entity<TextInput>,
    /// The installation under way, and the key it is for.
    pending: Option<(RequestId, PathBuf)>,
    finished: Option<String>,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for KeySetupModal {}
impl EventEmitter<super::CancelEvent> for KeySetupModal {}

impl Focusable for KeySetupModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.login.as_ref().map_or_else(
            || self.path.focus_handle(cx),
            |prompt| prompt.focus_handle(cx),
        )
    }
}

impl KeySetupModal {
    pub(crate) fn show_login(
        &mut self,
        prompt: Entity<LoginPrompt>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        prompt.read(cx).set_owner(cx.entity_id());
        self.login_subscription =
            Some(
                cx.subscribe_in(&prompt, window, |this, _, _: &DismissEvent, window, cx| {
                    this.login = None;
                    window.focus(&this.path.focus_handle(cx));
                    cx.notify();
                }),
            );
        window.focus(&prompt.focus_handle(cx));
        self.login = Some(prompt);
        cx.notify();
    }

    pub(crate) fn cancel_login(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(prompt) = self.login.take() {
            prompt.update(cx, |prompt, cx| prompt.answer(false, window, cx));
        }
        self.login_subscription = None;
    }

    pub(crate) fn embedded(mut self) -> Self {
        self.embedded = true;
        self
    }

    pub(crate) fn new(
        state: Entity<AppState>,
        host: HostConfig,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let ssh = ssh_folder(&state, cx);
        let start = host
            .identity_files
            .first()
            .cloned()
            .unwrap_or_else(|| ssh.join(USUAL_KEYS[0]));
        let path =
            cx.new(|cx| TextInput::new("Key file", cx).with_text(&start.display().to_string()));
        let passphrase = cx.new(|cx| TextInput::new("Passphrase, optional", cx).masked());
        let subscriptions = vec![
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.subscribe_in(&path, window, Self::on_input),
            cx.subscribe_in(&passphrase, window, Self::on_input),
            cx.subscribe(&state, |this, _, event: &Event, cx| {
                this.on_event(event, cx)
            }),
        ];
        Self {
            embedded: false,
            state,
            host,
            session,
            login: None,
            login_subscription: None,
            path,
            passphrase,
            pending: None,
            finished: None,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    fn on_input(
        &mut self,
        _: &Entity<TextInput>,
        event: &TextInputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TextInputEvent::Submit => self.install(window, cx),
            TextInputEvent::Cancel => cx.emit(super::CancelEvent),
            TextInputEvent::Changed => cx.notify(),
        }
    }

    fn on_event(&mut self, event: &Event, cx: &mut Context<Self>) {
        if let Event::Session { id, state, .. } = event
            && *id == self.session
            && crate::login_prompt::LoginAsk::from_state(state).is_none()
        {
            self.login = None;
            self.login_subscription = None;
            cx.notify();
        }
        let Event::OpDone { req, result } = event else {
            return;
        };
        if self.pending.as_ref().is_none_or(|(asked, _)| asked != req) {
            return;
        }
        let Some((_, key)) = self.pending.take() else {
            return;
        };
        match result {
            Ok(()) => self.finish(key, cx),
            Err(error) => self.error = Some(error.clone()),
        }
        cx.notify();
    }

    /// The server takes the key: the host has to use it too.
    fn finish(&mut self, key: PathBuf, cx: &mut Context<Self>) {
        self.passphrase
            .update(cx, |input, cx| input.set_text("", cx));
        let ssh = ssh_folder(&self.state, cx);
        let identities = &self.host.identity_files;
        let used = identities.contains(&key)
            || (identities.is_empty() && USUAL_KEYS.iter().any(|name| ssh.join(name) == key));
        let alias = self.host.alias.clone();
        self.finished = Some(if used {
            format!("Done. {alias} logs in with this key from the next connection.")
        } else if self.host.source == HostSource::App {
            let mut host = self.host.clone();
            host.identity_files.insert(0, key);
            match self.state.read(cx).engine.save_host(host) {
                Ok(()) => format!("Done. {alias} logs in with this key from the next connection."),
                Err(error) => {
                    format!("The server has the key, but saving {alias} failed: {error:#}")
                }
            }
        } else {
            format!(
                "The server has the key. {alias} comes from ~/.ssh/config: add \"IdentityFile {}\" to it there.",
                key.display()
            )
        });
    }

    fn install(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.pending.is_some() || self.finished.is_some() {
            return;
        }
        if !self.state.read(cx).session_connected(self.session) {
            return;
        }
        let typed = self.path.read(cx).text().trim().to_string();
        if typed.is_empty() {
            self.error = Some("Enter where the key file is, or goes.".to_string());
            cx.notify();
            return;
        }
        let key = expand(&typed, &ssh_folder(&self.state, cx));
        let passphrase = self.passphrase.read(cx).text().to_string();
        let passphrase = (!passphrase.is_empty()).then_some(passphrase);
        let req = self
            .state
            .read(cx)
            .engine
            .install_key(self.session, key.clone(), passphrase);
        self.pending = Some((req, key));
        self.error = None;
        cx.notify();
    }

    fn browse(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Select key file".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await
                && let Some(path) = paths.first()
            {
                this.update(cx, |this, cx| {
                    let text = path.display().to_string();
                    this.path.update(cx, |input, cx| input.set_text(&text, cx));
                })
                .ok();
            }
        })
        .detach();
    }
}

impl Render for KeySetupModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        if let Some(prompt) = &self.login {
            return editor_frame(&theme, 560., self.embedded)
                .when(!self.embedded, |this| {
                    this.child(title(&theme, format!("Sign in to {}", self.host.alias)))
                })
                .child(prompt.clone())
                .into_any_element();
        }
        let typed = self.path.read(cx).text().trim().to_string();
        let key = expand(&typed, &ssh_folder(&self.state, cx));
        let exists = !typed.is_empty() && key.exists();
        let working = self.pending.is_some();
        let connected = self.state.read(cx).session_connected(self.session);
        let finished = self.finished.clone();
        let target = self.host.target();
        let label = |text: &'static str| {
            div()
                .w(px(90.))
                .flex_none()
                .text_color(theme.text_muted)
                .child(text)
        };
        editor_frame(&theme, 560., self.embedded)
            .when(self.embedded && !connected, |this| this.child(div().text_color(theme.text_muted).child("Connect to the server to install a key. Complete any sign-in prompt to continue.")))
            .when(!self.embedded, |this| {
                this.child(title(&theme, format!("Set up key login for {}", self.host.alias)))
            })
            .child(div().text_color(theme.text_muted).child(format!(
                "A key replaces the password. Its public half goes in ~/.ssh/authorized_keys of {target}, \
                 through this connection, and the private one stays on this PC."
            )))
            .when(finished.is_none(), |this| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(label("Key file"))
                        .child(div().flex_1().min_w_0().child(self.path.clone()))
                        .child(
                            Button::new("key-browse")
                                .keyboard_focus()
                                .label("Browse…")
                                .on_click(cx.listener(|this, _, _, cx| this.browse(cx))),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(label("Passphrase"))
                        .child(div().flex_1().min_w_0().child(self.passphrase.clone())),
                )
                .child(
                    div()
                        .text_size(ui_px(12.))
                        .text_color(theme.text_muted)
                        .child(if exists {
                            "This key exists and stays as it is: only its public half is sent."
                        } else {
                            "A new Ed25519 key is created in this file. With a passphrase it asks for it when used."
                        }),
                )
            })
            .children(self.error.clone().map(|error| {
                div()
                    .id("key-error")
                    .max_h(px(120.))
                    .overflow_y_scroll()
                    .text_color(theme.error)
                    .child(error)
            }))
            .children(finished.clone().map(|message| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(Icon::new(IconName::Check).color(theme.success))
                    .child(div().flex_1().min_w_0().child(message))
            }))
            .child(
                button_row()
                    .child(
                        Button::new("key-close")
                            .keyboard_focus()
                            .label(if finished.is_some() { "Close" } else { "Cancel" })
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(super::CancelEvent))),
                    )
                    .when(finished.is_none(), |this| {
                        this.child(
                            Button::new("key-install")
                                .keyboard_focus()
                                .label(if working {
                                    "Installing…"
                                } else if exists {
                                    "Install on server"
                                } else {
                                    "Create and install"
                                })
                                .style(ButtonStyle::Primary)
                                .disabled(working || !connected)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.install(window, cx)),
                                ),
                        )
                    }),
            )
            .into_any_element()
    }
}

impl super::ModalView for KeySetupModal {
    fn draft(&self, cx: &App) -> Vec<String> {
        vec![self.path.read(cx).text().to_string()]
    }

    fn has_unsaved_changes(&self, initial: &[String], cx: &App) -> bool {
        self.finished.is_none()
            && (self.draft(cx) != initial || !self.passphrase.read(cx).text().is_empty())
    }
}
