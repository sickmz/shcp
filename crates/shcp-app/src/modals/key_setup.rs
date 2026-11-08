//! Guided move from a password to a key: creates the key if needed and installs it on the server.
use super::{button_row, card, title};
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
use shcp_core::{Event, HostConfig, HostSource, RequestId, SessionId};
use std::path::{Path, PathBuf};

/// The keys a host without identity files logs in with by itself.
const DEFAULT_KEYS: [&str; 3] = ["id_ed25519", "id_ecdsa", "id_rsa"];

pub struct KeySetupModal {
    state: Entity<AppState>,
    host: HostConfig,
    session: SessionId,
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
        self.path.focus_handle(cx)
    }
}

impl KeySetupModal {
    pub fn new(
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
            .unwrap_or_else(|| ssh.join(DEFAULT_KEYS[0]));
        let path = cx.new(|cx| {
            let mut input = TextInput::new("Key file", false, cx);
            input.set_text(&start.display().to_string(), cx);
            input
        });
        let passphrase = cx.new(|cx| TextInput::new("Passphrase, optional", true, cx));
        let subscriptions = vec![
            cx.subscribe_in(&path, window, Self::on_input),
            cx.subscribe_in(&passphrase, window, Self::on_input),
            cx.subscribe(&state, |this, _, event: &Event, cx| {
                this.on_event(event, cx)
            }),
        ];
        Self {
            state,
            host,
            session,
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
            || (identities.is_empty() && DEFAULT_KEYS.iter().any(|name| ssh.join(name) == key));
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

pub(super) fn ssh_folder(state: &Entity<AppState>, cx: &App) -> PathBuf {
    let config = state.read(cx).engine.paths().ssh_config;
    config
        .parent()
        .map_or_else(|| PathBuf::from(".ssh"), Path::to_path_buf)
}

/// `~/` starts in the home folder, which holds `.ssh`.
pub(super) fn expand(typed: &str, ssh: &Path) -> PathBuf {
    match (typed.strip_prefix("~/"), ssh.parent()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(typed),
    }
}

impl Render for KeySetupModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let typed = self.path.read(cx).text().trim().to_string();
        let key = expand(&typed, &ssh_folder(&self.state, cx));
        let exists = !typed.is_empty() && key.exists();
        let working = self.pending.is_some();
        let finished = self.finished.clone();
        let target = format!("{}@{}", self.host.user, self.host.hostname);
        let label = |text: &'static str| {
            div()
                .w(px(90.))
                .flex_none()
                .text_color(theme.text_muted)
                .child(text)
        };
        card(&theme, 560.)
            .child(title(&theme, format!("Set up key login for {}", self.host.alias)))
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
                                .disabled(working)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.install(window, cx)),
                                ),
                        )
                    }),
            )
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
