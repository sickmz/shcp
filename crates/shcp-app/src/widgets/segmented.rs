//! A row of choices in a groove, with a pill behind the current one: the views of a tab, panels.
use super::icon::{Icon, IconName};
use crate::theme::{ActiveTheme as _, Theme, ui_px};
use gpui::prelude::*;
use gpui::{App, Div, ElementId, Stateful, div, px};

/// The groove. Its children are a `SlidingPill`, first, then those made with `segment`.
pub fn groove(theme: &Theme) -> Div {
    div()
        .relative()
        .flex()
        .flex_none()
        .p(px(2.))
        .gap(px(1.))
        .rounded(px(9.))
        .bg(theme.track)
}

/// One choice. The caller gives it what it does with `on_click`.
pub fn segment(
    id: impl Into<ElementId>,
    icon: Option<IconName>,
    label: Option<&'static str>,
    current: bool,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    let (text, muted, accent) = (theme.text, theme.text_muted, theme.accent);
    div()
        .id(id)
        .h(px(26.))
        .px(px(9.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .rounded(px(7.))
        .text_size(ui_px(13.))
        .cursor_pointer()
        .when(current, |this| this.text_color(text))
        .when(!current, |this| {
            this.text_color(muted).hover(move |s| s.text_color(text))
        })
        .children(icon.map(|name| {
            Icon::new(name)
                .size(px(13.))
                .color(if current { accent } else { muted })
        }))
        .children(label)
}
