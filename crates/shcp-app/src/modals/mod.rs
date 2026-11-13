//! Modal stack: centered cards over a dimmed backdrop. Modals emit `CancelEvent` to be cancelled
//! (the stack asks before dropping unsaved changes) and `DismissEvent` to close.
pub(crate) mod confirm;
pub(crate) mod folder_picker;
mod host_appearance;
pub(crate) mod host_commands;
pub(crate) mod host_editor;
mod host_settings;
pub(crate) mod key_setup;
pub(crate) mod login;
pub(crate) mod palette;
pub(crate) mod settings;
pub(crate) mod text_prompt;
mod theme_browser;

#[cfg(test)]
mod tests;

use crate::actions::{FocusNext, FocusPrev};
use crate::motion;
use crate::state::AppState;
use crate::theme::{ActiveTheme as _, Theme};
use crate::widgets::button::Button;
use gpui::prelude::*;
use gpui::{
    AnimationExt as _, AnyView, App, Context, DismissEvent, Div, Entity, EntityId, EventEmitter,
    FocusHandle, Focusable, Hsla, Subscription, Window, WindowControlArea, div, px,
};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Moves the focus to the next tab stop, or the previous one, inside `scope`: the tab stops
/// outside it do not count.
fn cycle_focus(scope: &FocusHandle, reverse: bool, window: &mut Window, cx: &App) {
    let start = window.focused(cx);
    for _ in 0..10_000 {
        if reverse {
            window.focus_prev();
        } else {
            window.focus_next();
        }
        if scope.contains_focused(window, cx) || window.focused(cx) == start {
            break;
        }
    }
}

/// Keeps Tab and Shift+Tab of an element inside the scope that `scope` gives, when there is one.
pub(crate) fn trap_focus<E: InteractiveElement, V: 'static>(
    element: E,
    scope: fn(&V) -> Option<FocusHandle>,
    cx: &mut Context<V>,
) -> E {
    element
        .on_action(cx.listener(move |this, _: &FocusNext, window, cx| {
            if let Some(scope) = scope(this) {
                cycle_focus(&scope, false, window, cx);
            }
        }))
        .on_action(cx.listener(move |this, _: &FocusPrev, window, cx| {
            if let Some(scope) = scope(this) {
                cycle_focus(&scope, true, window, cx);
            }
        }))
}

pub(crate) struct CancelEvent;

pub(crate) trait ModalView:
    Render + Focusable + EventEmitter<DismissEvent> + EventEmitter<CancelEvent>
{
    /// Whether it fades in. Not what is opened all day from the keyboard.
    const ENTRANCE: bool = true;

    /// A page that owns a connection flow can display its sign-in prompt inline.
    fn inline_login(
        &mut self,
        _host: &shcp_core::HostId,
        _prompt: Entity<crate::login_prompt::LoginPrompt>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> bool {
        false
    }

    /// Editable values before saving. UI-only state does not count as a change.
    fn draft(&self, _cx: &gpui::App) -> Vec<String> {
        Vec::new()
    }

    fn has_unsaved_changes(&self, initial: &[String], cx: &gpui::App) -> bool {
        self.draft(cx) != initial
    }

    fn cancel(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }
}

type Cancel = Rc<dyn Fn(&mut Window, &mut gpui::App)>;
/// Whether a modal has changes that closing it would lose.
pub(crate) type Dirty = Box<dyn Fn(&gpui::App) -> bool>;
type CurrentFocus = Box<dyn Fn(&gpui::App) -> Option<FocusHandle>>;
type InlineLogin = Box<
    dyn Fn(
        &shcp_core::HostId,
        Entity<crate::login_prompt::LoginPrompt>,
        &mut Window,
        &mut gpui::App,
    ) -> bool,
>;

struct Active {
    id: EntityId,
    session: Option<shcp_core::SessionId>,
    view: AnyView,
    entrance: bool,
    focus: FocusHandle,
    current_focus: CurrentFocus,
    cancel: Cancel,
    dirty: Dirty,
    inline_login: InlineLogin,
    _cancel_subscription: Subscription,
    _subscription: Subscription,
}

#[derive(Default)]
pub(crate) struct ModalLayer {
    stack: Vec<Active>,
    /// Focus to restore once the last modal closes.
    restore: Option<FocusHandle>,
    scope: Option<FocusHandle>,
    skip_next_entrance: bool,
}

impl ModalLayer {
    pub(crate) fn inline_login(
        &self,
        host: &shcp_core::HostId,
        prompt: &Entity<crate::login_prompt::LoginPrompt>,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> bool {
        self.stack
            .iter()
            .rev()
            .any(|active| (active.inline_login)(host, prompt.clone(), window, cx))
    }

    pub(crate) fn is_open(&self) -> bool {
        !self.stack.is_empty()
    }

    /// The next modal was asked for with a shortcut: it appears at once.
    pub(crate) fn skip_next_entrance(&mut self) {
        self.skip_next_entrance = true;
    }

    /// Shows a modal. If one is already open the new one waits behind it.
    pub(crate) fn show<M: ModalView>(
        &mut self,
        modal: Entity<M>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_for_session(modal, None, window, cx);
    }

    fn show_for_session<M: ModalView>(
        &mut self,
        modal: Entity<M>,
        session: Option<shcp_core::SessionId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.stack.is_empty() {
            self.restore = window.focused(cx);
        }
        let id = modal.entity_id();
        let subscription = cx.subscribe_in(
            &modal,
            window,
            move |this, _, _: &DismissEvent, window, cx| {
                this.dismiss(id, window, cx);
            },
        );
        let focus = modal.focus_handle(cx);
        let weak = modal.downgrade();
        let current_focus =
            Box::new(move |cx: &gpui::App| weak.upgrade().map(|modal| modal.focus_handle(cx)));
        let weak = modal.downgrade();
        let cancel = Rc::new(move |window: &mut Window, cx: &mut gpui::App| {
            weak.update(cx, |modal, cx| modal.cancel(window, cx)).ok();
        });
        let dirty = dirty_since_now(&modal, cx);
        let cancel_subscription = cx.subscribe_in(
            &modal,
            window,
            move |this, _, _: &CancelEvent, window, cx| {
                if this.stack.first().is_some_and(|active| active.id == id) {
                    this.request_cancel(window, cx);
                }
            },
        );
        let weak = modal.downgrade();
        let inline_login = Box::new(
            move |host: &shcp_core::HostId, prompt, window: &mut Window, cx: &mut gpui::App| {
                weak.update(cx, |modal, cx| modal.inline_login(host, prompt, window, cx))
                    .unwrap_or(false)
            },
        );
        self.stack.push(Active {
            id,
            session,
            view: modal.into(),
            entrance: M::ENTRANCE && !std::mem::take(&mut self.skip_next_entrance),
            focus,
            current_focus,
            cancel,
            dirty,
            inline_login,
            _cancel_subscription: cancel_subscription,
            _subscription: subscription,
        });
        if self.stack.len() == 1 {
            window.focus(&self.stack[0].focus);
        }
        cx.notify();
    }

    pub(crate) fn dismiss_session(
        &mut self,
        session: shcp_core::SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ids: Vec<_> = self
            .stack
            .iter()
            .filter(|m| m.session == Some(session))
            .map(|m| m.id)
            .collect();
        for id in ids {
            self.dismiss(id, window, cx);
        }
    }

    /// Interrupt the current page with a confirmation or an authentication prompt.
    pub(crate) fn show_front<M: ModalView>(
        &mut self,
        modal: Entity<M>,
        session: Option<shcp_core::SessionId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_for_session(modal, session, window, cx);
        let front = self.stack.pop().unwrap();
        window.focus(&front.focus);
        self.stack.insert(0, front);
    }

    fn request_cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(active) = self.stack.first() else {
            return;
        };
        let cancel = active.cancel.clone();
        let session = active.session;
        if !(active.dirty)(cx) {
            cancel(window, cx);
            return;
        }
        let confirm = discard_confirm(move |window, cx| cancel(window, cx), cx);
        self.show_front(confirm, session, window, cx);
    }

    fn dismiss(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let was_front = self.stack.first().is_some_and(|a| a.id == id);
        self.stack.retain(|a| a.id != id);
        if was_front {
            match self.stack.first() {
                Some(next) => {
                    window.focus(&(next.current_focus)(cx).unwrap_or_else(|| next.focus.clone()))
                }
                None => {
                    if let Some(restore) = self.restore.take() {
                        window.focus(&restore);
                    }
                }
            }
        }
        cx.notify();
    }
}

impl Render for ModalLayer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(active) = self.stack.first() else {
            return gpui::Empty.into_any_element();
        };
        let scope = self.scope.get_or_insert_with(|| cx.focus_handle()).clone();
        let scrim = cx.theme().scrim;
        // The backdrop and the card arrive together, as one thing. The scrim's animation stays
        // mounted while modals follow each other, so it never fades in a second time.
        let scrim_animation = if active.entrance {
            motion::MODAL.animation()
        } else {
            motion::settled()
        };
        let front = if active.entrance {
            // Lays the modal out as the layer does, for those that fill it.
            motion::enter(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(active.view.clone()),
                ("modal-card", active.id),
                motion::MODAL,
            )
            .into_any_element()
        } else {
            active.view.clone().into_any_element()
        };
        div()
            .id("modal-layer")
            .track_focus(&scope)
            .key_context("Modal")
            .absolute()
            .inset_0()
            // The modal stays below the strip that moves the window.
            .pt(px(crate::title_bar::HEIGHT))
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.request_cancel(window, cx);
                    cx.stop_propagation();
                }),
            )
            // Skip tab stops behind the scrim; only the front modal participates.
            .map(|this| trap_focus(this, |layer: &Self| layer.scope.clone(), cx))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.request_cancel(window, cx);
                    cx.stop_propagation();
                }
            }))
            // The window can still be moved; the caption buttons are drawn above this layer.
            .child(
                div()
                    .id("modal-drag-area")
                    .occlude()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(crate::title_bar::HEIGHT))
                    .window_control_area(WindowControlArea::Drag),
            )
            .child(front)
            .with_animation("modal-scrim", scrim_animation, move |layer, delta| {
                layer.bg(Hsla {
                    a: scrim.a * delta,
                    ..scrim
                })
            })
            .into_any_element()
    }
}

/// Whether `modal` has moved away from the draft it holds now.
pub(crate) fn dirty_since_now<M: ModalView>(modal: &Entity<M>, cx: &gpui::App) -> Dirty {
    let initial = modal.read(cx).draft(cx);
    let weak = modal.downgrade();
    Box::new(move |cx: &gpui::App| {
        weak.upgrade()
            .is_some_and(|modal| modal.read(cx).has_unsaved_changes(&initial, cx))
    })
}

/// Asks before unsaved changes are lost; `discard` runs only when the user agrees.
pub(crate) fn discard_confirm(
    discard: impl FnOnce(&mut Window, &mut gpui::App) + 'static,
    cx: &mut gpui::App,
) -> Entity<confirm::ConfirmModal> {
    cx.new(|cx| {
        confirm::ConfirmModal::new(
            "Discard changes?",
            "Your unsaved changes will be lost.".into(),
            "Discard",
            discard,
            cx,
        )
    })
}

/// Common card frame for modals.
pub(crate) fn card(theme: &Theme, width: f32) -> Div {
    card_frame(theme, theme.border)
        .w(px(width))
        .max_h(px(620.))
        .p(px(20.))
        .flex()
        .flex_col()
        .gap(px(12.))
}

/// What every dialog looks like, whatever its layout.
pub(crate) fn card_frame(theme: &Theme, border: Hsla) -> Div {
    div()
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .rounded(px(crate::theme::RADIUS_CARD))
        .border_1()
        .border_color(border)
        .bg(theme.elevated)
        .shadow(crate::theme::float_shadow())
        .text_size(crate::theme::ui_px(13.))
        .text_color(theme.text)
}

/// An editor can also occupy the content pane of Settings, without a second card.
pub(crate) fn editor_frame(theme: &Theme, width: f32, embedded: bool) -> Div {
    if embedded {
        div().w_full().min_w_0().flex().flex_col().gap(px(12.))
    } else {
        card(theme, width)
    }
}

pub(crate) fn title(theme: &Theme, text: impl Into<gpui::SharedString>) -> Div {
    div()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_size(crate::theme::ui_px(15.))
        .text_color(theme.text)
        .child(text.into())
}

pub(crate) fn button_row() -> Div {
    div().flex().justify_end().gap(px(8.)).pt(px(4.))
}

/// The end of a dialog: Cancel, then `primary`, the button the dialog is for. Both take the focus
/// with Tab.
pub(crate) fn cancel_and<V: 'static>(
    cancel_id: &'static str,
    on_cancel: fn(&mut V, &mut Context<V>),
    primary: Button,
    cx: &mut Context<V>,
) -> Div {
    button_row()
        .child(
            Button::new(cancel_id)
                .keyboard_focus()
                .label("Cancel")
                .on_click(cx.listener(move |this, _, _, cx| on_cancel(this, cx))),
        )
        .child(primary.keyboard_focus())
}

/// The folder that holds the SSH config, and the keys next to it.
fn ssh_folder(state: &Entity<AppState>, cx: &App) -> PathBuf {
    let config = state.read(cx).engine.paths().ssh_config;
    config
        .parent()
        .map_or_else(|| PathBuf::from(".ssh"), Path::to_path_buf)
}

/// `~/` starts in the home folder, which holds `.ssh`.
fn expand(typed: &str, ssh: &Path) -> PathBuf {
    match (typed.strip_prefix("~/"), ssh.parent()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(typed),
    }
}
