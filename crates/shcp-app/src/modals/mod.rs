//! Modal stack: centered cards over a dimmed backdrop. Modals emit `DismissEvent` to close.
pub mod confirm;
pub mod folder_picker;
pub mod host_appearance;
pub mod host_commands;
pub mod host_editor;
pub mod host_settings;
pub mod key_setup;
pub mod login;
pub mod palette;
pub mod settings;
pub mod text_prompt;
pub mod theme_browser;

#[cfg(test)]
mod tests;

use crate::actions::{FocusNext, FocusPrev};
use crate::motion;
use crate::theme::{ActiveTheme as _, Theme};
use gpui::prelude::*;
use gpui::{
    AnimationExt as _, AnyView, Context, DismissEvent, Div, Entity, EntityId, EventEmitter,
    FocusHandle, Focusable, Hsla, Subscription, Window, WindowControlArea, div, px,
};
use std::rc::Rc;

pub struct CancelEvent;

pub trait ModalView:
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
    dirty: Box<dyn Fn(&gpui::App) -> bool>,
    inline_login: InlineLogin,
    _cancel_subscription: Subscription,
    _subscription: Subscription,
}

#[derive(Default)]
pub struct ModalLayer {
    stack: Vec<Active>,
    /// Focus to restore once the last modal closes.
    restore: Option<FocusHandle>,
    scope: Option<FocusHandle>,
    skip_next_entrance: bool,
}

impl ModalLayer {
    pub fn inline_login(
        &self,
        host: &shcp_core::HostId,
        prompt: Entity<crate::login_prompt::LoginPrompt>,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> bool {
        self.stack
            .iter()
            .rev()
            .any(|active| (active.inline_login)(host, prompt.clone(), window, cx))
    }

    pub fn is_open(&self) -> bool {
        !self.stack.is_empty()
    }

    /// The next modal was asked for with a shortcut: it appears at once.
    pub fn skip_next_entrance(&mut self) {
        self.skip_next_entrance = true;
    }

    /// Shows a modal. If one is already open the new one waits behind it.
    pub fn show<M: ModalView>(
        &mut self,
        modal: Entity<M>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_for_session(modal, None, window, cx);
    }

    pub fn show_for_session<M: ModalView>(
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
        let initial = modal.read(cx).draft(cx);
        let weak = modal.downgrade();
        let dirty = Box::new(move |cx: &gpui::App| {
            weak.upgrade()
                .is_some_and(|modal| modal.read(cx).has_unsaved_changes(&initial, cx))
        });
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

    pub fn dismiss_session(
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
    pub fn show_front<M: ModalView>(
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
        let confirm = cx.new(|cx| {
            confirm::ConfirmModal::new(
                "Discard changes?",
                "Your unsaved changes will be lost.".into(),
                "Discard",
                move |window, cx| cancel(window, cx),
                cx,
            )
        });
        self.show_front(confirm, session, window, cx);
    }

    fn step_focus(&self, reverse: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(scope) = &self.scope else {
            return;
        };
        let start = window.focused(cx);
        // Skip tab stops behind the scrim; only the front modal participates.
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
            .on_action(
                cx.listener(|this, _: &FocusNext, window, cx| this.step_focus(false, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &FocusPrev, window, cx| this.step_focus(true, window, cx)),
            )
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

/// Common card frame for modals.
pub fn card(theme: &Theme, width: f32) -> Div {
    div()
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .w(px(width))
        .max_h(px(620.))
        .p(px(20.))
        .flex()
        .flex_col()
        .gap(px(12.))
        .rounded(px(crate::theme::RADIUS_CARD))
        // In a theme with a material a dialog is a card of it, lit.
        .map(|this| match theme.material() {
            Some(material) => this.relative().child(crate::terminal_material::layers(
                material.opaque(),
                true,
                true,
            )),
            None => this
                .border_1()
                .border_color(theme.border)
                .bg(theme.elevated),
        })
        .shadow(crate::theme::float_shadow())
        .text_size(crate::theme::ui_px(13.))
        .text_color(theme.text)
}

/// An editor can also occupy the content pane of Settings, without a second card.
pub fn editor_frame(theme: &Theme, width: f32, embedded: bool) -> Div {
    if embedded {
        div().w_full().min_w_0().flex().flex_col().gap(px(12.))
    } else {
        card(theme, width)
    }
}

pub fn title(theme: &Theme, text: impl Into<gpui::SharedString>) -> Div {
    div()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_size(crate::theme::ui_px(15.))
        .text_color(theme.text)
        .child(text.into())
}

pub fn button_row() -> Div {
    div().flex().justify_end().gap(px(8.)).pt(px(4.))
}
