use super::{button_row, card, title};
use crate::theme::ActiveTheme as _;
use crate::widgets::button::{Button, ButtonStyle};
use gpui::prelude::*;
use gpui::{
    App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, KeyDownEvent, Window, div,
};

type Callback = Box<dyn FnOnce(&mut Window, &mut App)>;

/// "Are you sure?" dialog; runs `on_confirm` only when confirmed.
pub struct ConfirmModal {
    title: String,
    message: String,
    confirm_label: String,
    on_confirm: Option<Callback>,
    focus: FocusHandle,
}

impl EventEmitter<DismissEvent> for ConfirmModal {}
impl EventEmitter<super::CancelEvent> for ConfirmModal {}

impl Focusable for ConfirmModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ConfirmModal {
    pub fn new(
        title: &str,
        message: String,
        confirm_label: &str,
        on_confirm: impl FnOnce(&mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            title: title.to_string(),
            message,
            confirm_label: confirm_label.to_string(),
            on_confirm: Some(Box::new(on_confirm)),
            focus: cx.focus_handle(),
        }
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(callback) = self.on_confirm.take() {
            callback(window, cx);
        }
        cx.emit(DismissEvent);
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => cx.emit(DismissEvent),
            "enter" => self.confirm(window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl Render for ConfirmModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        card(&theme, 420.)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .child(title(&theme, self.title.clone()))
            .child(
                div()
                    .text_color(theme.text_muted)
                    .child(self.message.clone()),
            )
            .child(
                button_row()
                    .child(
                        Button::new("cancel")
                            .keyboard_focus()
                            .label("Cancel")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    )
                    .child(
                        Button::new("confirm")
                            .keyboard_focus()
                            .label(self.confirm_label.clone())
                            .style(ButtonStyle::Danger)
                            .on_click(cx.listener(|this, _, window, cx| this.confirm(window, cx))),
                    ),
            )
    }
}

impl super::ModalView for ConfirmModal {}
