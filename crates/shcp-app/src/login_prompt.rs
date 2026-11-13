//! What a connection needs from the user: accepting a host key, a passphrase or a password.
//! All the tabs of a host share its session, so one prompt exists per host, and it is drawn by one
//! view at a time: the one with the focus. It never takes the focus from anywhere else.
use crate::modals::trap_focus;
use crate::theme::{ActiveTheme as _, Theme, mono_font, ui_font};
use crate::widgets::button::{Button, ButtonStyle};
use crate::widgets::icon::IconName;
use crate::widgets::text_input::{TextInput, TextInputEvent};
use gpui::prelude::*;
use gpui::{
    App, Context, DismissEvent, Div, Entity, EntityId, EventEmitter, FocusHandle, Focusable,
    KeyDownEvent, Subscription, Window, div, px,
};
use shcp_core::{EngineHandle, HostKeyKind, SessionId, SessionState};
use std::{cell::Cell, cell::RefCell, path::PathBuf};

pub(crate) enum LoginAsk {
    HostKey {
        host: String,
        key_type: String,
        fingerprint: String,
        kind: HostKeyKind,
    },
    Passphrase {
        key_path: PathBuf,
    },
    Password {
        login: String,
    },
    /// A question of a keyboard-interactive login.
    Answer {
        prompt: String,
        instructions: String,
        echo: bool,
        can_remember: bool,
    },
}

impl LoginAsk {
    pub(crate) fn from_state(state: &SessionState) -> Option<Self> {
        match state {
            SessionState::AwaitingHostKey {
                host,
                key_type,
                fingerprint_sha256,
                kind,
            } => Some(Self::HostKey {
                host: host.clone(),
                key_type: key_type.clone(),
                fingerprint: fingerprint_sha256.clone(),
                kind: *kind,
            }),
            SessionState::AwaitingPassphrase { key_path } => Some(Self::Passphrase {
                key_path: key_path.clone(),
            }),
            SessionState::AwaitingPassword { login } => Some(Self::Password {
                login: login.clone(),
            }),
            SessionState::AwaitingAnswer {
                prompt,
                instructions,
                echo,
                can_remember,
            } => Some(Self::Answer {
                prompt: prompt.clone(),
                instructions: instructions.clone(),
                echo: *echo,
                can_remember: *can_remember,
            }),
            _ => None,
        }
    }
}

pub(crate) struct LoginPrompt {
    engine: EngineHandle,
    session: SessionId,
    ask: LoginAsk,
    /// The last answer to this same question was wrong.
    retry: bool,
    input: Option<Entity<TextInput>>,
    /// Keep the secret in the Credential Manager. Off unless asked for.
    remember: bool,
    answered: bool,
    focus: FocusHandle,
    /// The view drawing it.
    owner: Cell<Option<EntityId>>,
    /// Where the focus goes once answered.
    return_focus: RefCell<Option<FocusHandle>>,
    _subscription: Option<Subscription>,
}

impl EventEmitter<DismissEvent> for LoginPrompt {}

impl Focusable for LoginPrompt {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.input {
            Some(input) => input.focus_handle(cx),
            None => self.focus.clone(),
        }
    }
}

impl LoginPrompt {
    pub(crate) fn new(
        engine: EngineHandle,
        session: SessionId,
        ask: LoginAsk,
        retry: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let masked = !matches!(ask, LoginAsk::Answer { echo: true, .. });
        let input = match ask {
            LoginAsk::HostKey { .. } => None,
            _ => Some(cx.new(|cx| {
                let input = TextInput::new("", cx);
                if masked { input.masked() } else { input }
            })),
        };
        let subscription = input.as_ref().map(|input| {
            cx.subscribe_in(
                input,
                window,
                |this, _, event: &TextInputEvent, window, cx| match event {
                    TextInputEvent::Submit => this.answer(true, window, cx),
                    TextInputEvent::Cancel => this.answer(false, window, cx),
                    TextInputEvent::Changed => {}
                },
            )
        });
        Self {
            engine,
            session,
            ask,
            retry,
            input,
            remember: false,
            answered: false,
            focus: cx.focus_handle(),
            owner: Cell::new(None),
            return_focus: RefCell::new(None),
            _subscription: subscription,
        }
    }

    pub(crate) fn session(&self) -> SessionId {
        self.session
    }

    pub(crate) fn answered(&self) -> bool {
        self.answered
    }

    /// What the connection is waiting for, as its current step.
    pub(crate) fn waiting_text(&self) -> &'static str {
        match self.ask {
            LoginAsk::HostKey { .. } => "Check the host key",
            LoginAsk::Passphrase { .. } => "Waiting for your passphrase",
            LoginAsk::Password { .. } => "Waiting for your password",
            LoginAsk::Answer { .. } => "Waiting for your answer",
        }
    }

    /// For a modal, which is the only place that can draw it.
    pub(crate) fn set_owner(&self, owner: EntityId) {
        self.owner.set(Some(owner));
    }

    pub(crate) fn answer(&mut self, accept: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.answered {
            return;
        }
        self.answered = true;
        let secret = self
            .input
            .as_ref()
            .filter(|_| accept)
            .map(|input| input.read(cx).text().to_string());
        if let Some(input) = &self.input {
            input.update(cx, |input, cx| input.set_text("", cx));
        }
        match self.ask {
            LoginAsk::HostKey { .. } => self.engine.answer_host_key(self.session, accept),
            LoginAsk::Passphrase { .. } => {
                self.engine
                    .answer_passphrase(self.session, secret, self.remember)
            }
            LoginAsk::Password { .. } | LoginAsk::Answer { .. } => {
                self.engine
                    .answer_password(self.session, secret, self.remember)
            }
        }
        if let Some(focus) = self.return_focus.borrow_mut().take() {
            window.focus(&focus);
        }
        cx.emit(DismissEvent);
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let LoginAsk::HostKey { kind, .. } = &self.ask else {
            return;
        };
        match event.keystroke.key.as_str() {
            "escape" => self.answer(false, window, cx),
            // A changed key must be accepted with an explicit click.
            "enter" if *kind == HostKeyKind::Unknown => self.answer(true, window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    fn host_key(
        &self,
        host: &str,
        key_type: &str,
        fingerprint: &str,
        kind: HostKeyKind,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let changed = kind == HostKeyKind::Changed;
        let cancel = Button::new("login-cancel")
            .keyboard_focus()
            .label("Cancel")
            .on_click(cx.listener(|this, _, window, cx| this.answer(false, window, cx)));
        let accept = Button::new("login-accept")
            .keyboard_focus()
            .label(if changed {
                "Replace key and connect"
            } else {
                "Accept"
            })
            .style(if changed {
                ButtonStyle::Danger
            } else {
                ButtonStyle::Primary
            })
            .on_click(cx.listener(|this, _, window, cx| this.answer(true, window, cx)));
        // The safe choice comes first when the key may be an attack.
        let buttons = if changed {
            div().child(cancel).child(accept)
        } else {
            div().child(accept).child(cancel)
        };
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .font_family(ui_font())
            .when(changed, |this| {
                this.child(div().text_color(theme.error).child(format!(
                    "The key of {host} has changed. Someone may be intercepting this connection."
                )))
                .child(
                    div()
                        .text_color(theme.text_muted)
                        .child("Accepting replaces the key saved in known_hosts."),
                )
            })
            .when(!changed, |this| {
                this.child(div().text_color(theme.warning).child(format!(
                    "Unknown host {host}. Check the fingerprint before accepting."
                )))
            })
            .child(
                div()
                    .text_color(theme.text_muted)
                    .child(key_type.to_string()),
            )
            .child(
                div()
                    .font_family(mono_font())
                    .text_color(theme.text)
                    .child(fingerprint.to_string()),
            )
            .when(!changed, |this| {
                this.child(
                    div()
                        .text_color(theme.text_muted)
                        .child("It will be saved in known_hosts"),
                )
            })
            .child(buttons.flex().justify_end().gap(px(8.)).pt(px(4.)))
    }

    fn secret(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let can_remember = !matches!(
            self.ask,
            LoginAsk::Answer {
                can_remember: false,
                ..
            }
        );
        let instructions = match &self.ask {
            LoginAsk::Answer { instructions, .. } => instructions.trim().to_string(),
            _ => String::new(),
        };
        let (label, accept, cancel, wrong) = match &self.ask {
            LoginAsk::Passphrase { key_path } => (
                format!("Passphrase for {}", key_path.display()),
                "Continue",
                "Use another key",
                "Could not decrypt the key. Try again.",
            ),
            LoginAsk::Password { login } => (
                format!("Password for {login}"),
                "Connect",
                "Cancel",
                "Incorrect password. Try again.",
            ),
            LoginAsk::Answer { prompt, .. } => (
                prompt.trim().trim_end_matches(':').to_string(),
                "Continue",
                "Cancel",
                "The server did not accept that. Try again.",
            ),
            LoginAsk::HostKey { .. } => unreachable!("a host key is not a secret"),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .font_family(ui_font())
            .when(self.retry, |this| {
                this.child(div().text_color(theme.error).child(wrong))
            })
            .when(!instructions.is_empty(), |this| {
                this.child(div().text_color(theme.text_muted).child(instructions))
            })
            .child(div().text_color(theme.text_muted).child(label))
            .children(self.input.clone())
            .when(can_remember, |this| {
                this.child(
                    // Without its padding, the box lines up with the text above.
                    div().flex().ml(px(-10.)).child(
                        Button::new("login-remember")
                            .keyboard_focus()
                            .icon(if self.remember {
                                IconName::Check
                            } else {
                                IconName::Square
                            })
                            .label("Remember in Windows Credential Manager")
                            .tooltip("Other programs running as your Windows user could read it")
                            .style(ButtonStyle::Ghost)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.remember = !this.remember;
                                cx.notify();
                            })),
                    ),
                )
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        Button::new("login-cancel")
                            .keyboard_focus()
                            .label(cancel)
                            .style(ButtonStyle::Ghost)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.answer(false, window, cx)),
                            ),
                    )
                    .child(
                        Button::new("login-accept")
                            .keyboard_focus()
                            .label(accept)
                            .style(ButtonStyle::Primary)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.answer(true, window, cx)),
                            ),
                    ),
            )
    }
}

impl Render for LoginPrompt {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let content = match &self.ask {
            LoginAsk::HostKey {
                host,
                key_type,
                fingerprint,
                kind,
            } => self.host_key(host, key_type, fingerprint, *kind, &theme, cx),
            _ => self.secret(&theme, cx),
        };
        content
            .id("login-prompt")
            .track_focus(&self.focus)
            // Tab stays inside the prompt, as in a modal.
            .key_context("Modal")
            .map(|this| trap_focus(this, |prompt: &Self| Some(prompt.focus.clone()), cx))
            .on_key_down(cx.listener(Self::on_key_down))
    }
}

/// Called while rendering by a view that could draw the prompt. With the focus, the view takes the
/// prompt over and passes the focus to it. Returns whether the view should draw it.
pub(crate) fn present(
    prompt: &Entity<LoginPrompt>,
    owner: EntityId,
    focus: &FocusHandle,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let this = prompt.read(cx);
    let focused = focus.is_focused(window);
    if focused || this.owner.get().is_none() {
        this.owner.set(Some(owner));
        this.return_focus.replace(Some(focus.clone()));
    }
    let own = this.owner.get() == Some(owner);
    if focused {
        let target = this.focus_handle(cx);
        window.defer(cx, move |window, _| window.focus(&target));
    }
    own
}
