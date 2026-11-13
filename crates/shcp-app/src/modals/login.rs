//! A login prompt for a host with no tab to show it in: connected from the sidebar, or for its
//! tunnels.
use super::{card, title};
use crate::login_prompt::LoginPrompt;
use crate::theme::{ActiveTheme as _, ui_px};
use gpui::prelude::*;
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Subscription, Window,
};

pub(crate) struct LoginModal {
    host: String,
    prompt: Entity<LoginPrompt>,
    _subscription: Subscription,
}

impl EventEmitter<DismissEvent> for LoginModal {}
impl EventEmitter<super::CancelEvent> for LoginModal {}

impl Focusable for LoginModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.prompt.focus_handle(cx)
    }
}

impl LoginModal {
    pub(crate) fn new(host: String, prompt: Entity<LoginPrompt>, cx: &mut Context<Self>) -> Self {
        prompt.read(cx).set_owner(cx.entity_id());
        let subscription = cx.subscribe(&prompt, |_, _, _: &DismissEvent, cx| {
            cx.emit(DismissEvent);
        });
        Self {
            host,
            prompt,
            _subscription: subscription,
        }
    }
}

impl Render for LoginModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        card(&theme, 520.)
            .child(title(&theme, self.host.clone()))
            .child(gpui::div().text_size(ui_px(12.)).child(self.prompt.clone()))
    }
}

impl super::ModalView for LoginModal {
    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt
            .update(cx, |prompt, cx| prompt.answer(false, window, cx));
    }
}
