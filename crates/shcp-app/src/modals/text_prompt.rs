use super::{button_row, card, title};
use crate::theme::ActiveTheme as _;
use crate::widgets::button::{Button, ButtonStyle};
use crate::widgets::text_input::{TextInput, TextInputEvent};
use gpui::prelude::*;
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Subscription, Window,
    div,
};

type Callback = Box<dyn FnOnce(String, &mut Window, &mut App)>;

/// One-line text prompt (new folder name, rename).
pub struct TextPrompt {
    title: String,
    ok_label: String,
    input: Entity<TextInput>,
    on_submit: Option<Callback>,
    error: Option<String>,
    /// The text is not a file name.
    any_text: bool,
    _subscription: Subscription,
}

impl EventEmitter<DismissEvent> for TextPrompt {}

impl Focusable for TextPrompt {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl TextPrompt {
    pub fn new(
        title: &str,
        ok_label: &str,
        initial: &str,
        on_submit: impl FnOnce(String, &mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            let mut input = TextInput::new("", false, cx);
            input.set_text(initial, cx);
            input
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &TextInputEvent, window, cx| match event {
                TextInputEvent::Submit => this.submit(window, cx),
                TextInputEvent::Cancel => cx.emit(DismissEvent),
                TextInputEvent::Changed => {}
            },
        );
        Self {
            title: title.to_string(),
            ok_label: ok_label.to_string(),
            input,
            on_submit: Some(Box::new(on_submit)),
            error: None,
            any_text: false,
            _subscription: subscription,
        }
    }

    /// Accepts any text that is not empty, for what is not a file name.
    pub fn any_text(mut self) -> Self {
        self.any_text = true;
        self
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text().trim().to_string();
        if text.is_empty() {
            self.error = Some("Enter a name.".into());
            cx.notify();
            return;
        }
        if !self.any_text && (matches!(text.as_str(), "." | "..") || text.contains(['/', '\0'])) {
            self.error = Some("Enter a single file or folder name (without /).".into());
            cx.notify();
            return;
        }
        if let Some(callback) = self.on_submit.take() {
            callback(text, window, cx);
        }
        cx.emit(DismissEvent);
    }
}

impl Render for TextPrompt {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        card(&theme, 420.)
            .child(title(&theme, self.title.clone()))
            .child(div().child(self.input.clone()))
            .children(
                self.error
                    .clone()
                    .map(|e| div().text_color(theme.error).child(e)),
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
                        Button::new("ok")
                            .keyboard_focus()
                            .label(self.ok_label.clone())
                            .style(ButtonStyle::Primary)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}

impl super::ModalView for TextPrompt {}
