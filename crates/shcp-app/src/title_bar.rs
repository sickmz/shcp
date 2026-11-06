//! The top strip of the window: its height, the app's logo and, on Windows, the
//! minimize/maximize/close buttons (gpui hit-tests them through `WindowControlArea`).
use crate::theme::ActiveTheme as _;
use crate::widgets::icon::{Icon, IconName};
use gpui::prelude::*;
use gpui::{App, Hsla, Pixels, Window, WindowControlArea, div, hsla, px};

pub const HEIGHT: f32 = 38.;
const CAPTION_WIDTH: f32 = 44.;

// Every `WindowControlArea::Drag` must also `occlude()`. Otherwise a press reaches the focusable
// views behind it, which take the focus and mark the event as handled; on Windows gpui then skips
// the default window procedure, and the window never starts moving.

/// Room the top strip leaves free for the caption buttons drawn over it.
pub fn captions_width() -> f32 {
    if cfg!(windows) {
        CAPTION_WIDTH * 3.
    } else {
        0.
    }
}

pub fn logo(size: Pixels, color: Hsla) -> impl IntoElement {
    Icon::new(IconName::Logo).size(size).color(color)
}

fn caption_button(
    id: &'static str,
    icon: IconName,
    area: WindowControlArea,
    hover_bg: Hsla,
    hover_fg: Hsla,
    color: Hsla,
) -> impl IntoElement {
    div()
        .id(id)
        .occlude()
        .window_control_area(area)
        .w(px(CAPTION_WIDTH))
        .h_full()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .text_color(color)
        .hover(move |s| s.bg(hover_bg).text_color(hover_fg))
        .child(Icon::new(icon).size(px(14.)))
}

/// Drawn last, in the window's corner, so they stay usable while a modal is open.
#[derive(IntoElement)]
pub struct WindowControls;

impl RenderOnce for WindowControls {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let maximized = window.is_maximized();
        div()
            .absolute()
            .top_0()
            .right_0()
            .h(px(HEIGHT))
            .flex()
            .when(cfg!(windows), |this| {
                this.child(caption_button(
                    "caption-min",
                    IconName::Minus,
                    WindowControlArea::Min,
                    theme.hover,
                    theme.text,
                    theme.text_muted,
                ))
                .child(caption_button(
                    "caption-max",
                    if maximized {
                        IconName::Restore
                    } else {
                        IconName::Square
                    },
                    WindowControlArea::Max,
                    theme.hover,
                    theme.text,
                    theme.text_muted,
                ))
                .child(caption_button(
                    "caption-close",
                    IconName::Close,
                    WindowControlArea::Close,
                    hsla(0., 0.75, 0.47, 1.),
                    hsla(0., 0., 1., 1.),
                    theme.text_muted,
                ))
            })
    }
}
