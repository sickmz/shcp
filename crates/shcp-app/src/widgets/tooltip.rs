//! Plain text tooltip for `.tooltip(tooltip("…"))`. A shortcut ending the text, as in
//! "Refresh (F5)", is drawn as keys.
use crate::theme::ActiveTheme as _;
use gpui::prelude::*;
use gpui::{AnyView, App, Context, SharedString, Window, div, px};

struct Tooltip {
    text: SharedString,
    keys: Vec<SharedString>,
}

/// The text before the shortcut, and the keys of the shortcut.
fn split_keys(text: &str) -> (&str, Vec<SharedString>) {
    let found = text
        .strip_suffix(')')
        .and_then(|rest| rest.rsplit_once(" ("))
        .filter(|(_, keys)| is_shortcut(keys));
    match found {
        Some((label, keys)) => (
            label,
            keys.split('+')
                .map(|key| SharedString::from(key.to_string()))
                .collect(),
        ),
        None => (text, Vec::new()),
    }
}

fn is_shortcut(text: &str) -> bool {
    let named = matches!(text, "Enter" | "Esc" | "Del" | "Backspace" | "Tab");
    let function = text.len() <= 3
        && text
            .strip_prefix('F')
            .is_some_and(|n| n.parse::<u8>().is_ok());
    let chord = text.contains('+')
        && !text.contains([' ', ','])
        && text.split('+').all(|key| !key.is_empty());
    named || function || chord
}

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, surface, muted) = (theme.border, theme.surface, theme.text_muted);
        div()
            .max_w(px(420.))
            .px(px(8.))
            .py(px(4.))
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(px(crate::theme::RADIUS_CONTROL))
            .map(|this| crate::panel::floating(this, theme, crate::theme::RADIUS_CONTROL, border))
            .shadow(crate::theme::float_shadow())
            .text_size(crate::theme::ui_px(12.))
            .text_color(theme.text)
            .font_family(crate::theme::ui_font())
            .child(div().min_w_0().child(self.text.clone()))
            .when(!self.keys.is_empty(), |this| {
                this.child(
                    div()
                        .flex()
                        .flex_none()
                        .gap(px(3.))
                        .children(self.keys.iter().map(|key| {
                            div()
                                .h(px(18.))
                                .min_w(px(18.))
                                .px(px(4.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(crate::theme::RADIUS_SMALL))
                                .border_1()
                                .border_color(border)
                                .bg(surface)
                                .text_size(crate::theme::ui_px(11.))
                                .text_color(muted)
                                .child(key.clone())
                        })),
                )
            })
    }
}

pub(crate) fn tooltip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView {
    let text = text.into();
    move |_, cx| {
        let shown = crate::actions::with_current_keys(&text, cx);
        let (text, keys) = split_keys(&shown);
        let text = SharedString::from(text.to_string());
        cx.new(|_| Tooltip { text, keys }).into()
    }
}
