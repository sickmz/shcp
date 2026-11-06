//! A small label in the corner of the panel with the latest notice, which opens the list of all
//! the current ones. It floats over the panel, so that a notice never resizes the terminal, and
//! stays inside it, so that it never covers the dock.
use crate::motion;
use crate::state::Notice;
use crate::theme::{ActiveTheme as _, Theme, ui_px};
use crate::widgets::button::{Button, ButtonStyle};
use crate::widgets::icon::{Icon, IconName};
use crate::widgets::tooltip::tooltip;
use crate::workspace::Workspace;
use gpui::prelude::*;
use gpui::{AnyElement, ClipboardItem, Context, Hsla, MouseDownEvent, div, px};
use shcp_core::NoticeLevel;

const LABEL_HEIGHT: f32 = 30.;
/// Distance from the sides of the panel.
const MARGIN: f32 = 8.;

/// Color only for what may need the user: information stays quiet.
fn look(level: NoticeLevel, theme: &Theme) -> (IconName, Hsla, Hsla) {
    match level {
        NoticeLevel::Info => (IconName::Info, theme.text_muted, theme.text_muted),
        NoticeLevel::Warning => (IconName::Warning, theme.warning, theme.text),
        NoticeLevel::Error => (IconName::Alert, theme.error, theme.text),
    }
}

/// `bottom`: how far above the panel's lower edge it goes, to leave what is there in sight.
pub fn label(
    workspace: &Workspace,
    bottom: f32,
    cx: &mut Context<Workspace>,
) -> Option<AnyElement> {
    let theme = cx.theme().clone();
    let notices = &workspace.state.read(cx).notices;
    let notice = notices.last()?;
    let (icon, icon_color, text_color) = look(notice.level, &theme);
    let error = notice.level == NoticeLevel::Error;
    // One waiting for a choice stays until it is made or closed, as an error does.
    let closable = error || notice.action.is_some();
    let others = notices
        .iter()
        .filter(|n| !n.leaving)
        .count()
        .saturating_sub(1);
    let action = if workspace.notices_open {
        Some("Hide".to_string())
    } else if others > 0 {
        Some(format!("{others} more"))
    } else {
        (error && notice.action.is_none()).then(|| "Details".to_string())
    };
    let id = notice.id;
    let state = workspace.state.clone();
    let notice_action = notice.action.clone().map(|action| {
        let state = state.clone();
        Button::new(("notice-action", id as usize))
            .label(action.label)
            .compact()
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                state.update(cx, |s, cx| s.dismiss_notice(id, cx));
                (action.run)(window, cx);
            })
    });
    let (close_hover, close_press) = (theme.hover, theme.active);
    let label = div()
        .id(("notice", id))
        // A click here must not reach what the label covers.
        .occlude()
        .min_w_0()
        .max_w(px(420.))
        .h(px(LABEL_HEIGHT))
        .flex()
        .items_center()
        .gap(px(8.))
        .pl(px(10.))
        .pr(px(if closable { 6. } else { 10. }))
        .rounded(px(7.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.elevated)
        .cursor_pointer()
        .text_size(ui_px(12.))
        .text_color(text_color)
        // The list closes on a click outside: this one must not close it just to open it again.
        .on_hover(cx.listener(|this, hovered: &bool, _, _| this.notice_hovered = *hovered))
        // A long message is cut here: the list shows all of it.
        .on_click(cx.listener(|this, _, _, cx| {
            this.notices_open = !this.notices_open;
            cx.notify();
        }))
        .child(Icon::new(icon).color(icon_color))
        .child(
            div()
                .min_w_0()
                .truncate()
                .child(notice.message.replace('\n', " ")),
        )
        .children(action.map(|text| div().flex_none().text_color(theme.accent).child(text)))
        .children(notice_action)
        // The others go by themselves.
        .when(closable, |this| {
            this.child(
                div()
                    .id(("notice-close", id))
                    .size(px(18.))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded(px(5.))
                    .text_color(theme.text_muted)
                    .hover(move |s| s.bg(close_hover))
                    .active(move |s| s.bg(close_press))
                    .tooltip(tooltip("Dismiss"))
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        state.update(cx, |s, cx| s.dismiss_notice(id, cx));
                    })
                    .child(Icon::new(IconName::Close).size(px(12.))),
            )
        });
    let label = if notice.leaving {
        motion::exit(label, ("notice-out", id), motion::NOTICE_OUT).into_any_element()
    } else {
        motion::enter(label, ("notice-in", id), motion::NOTICE_IN).into_any_element()
    };
    Some(corner(bottom).child(label).into_any_element())
}

/// Gives what it holds a width to shorten to: alone, a truncated text would shrink to nothing.
fn corner(bottom: f32) -> gpui::Div {
    div()
        .absolute()
        .left(px(MARGIN))
        .right(px(MARGIN))
        .bottom(px(bottom))
        .flex()
        .justify_end()
}

fn row(notice: &Notice, workspace: &Workspace, theme: &Theme) -> impl IntoElement {
    let (icon, icon_color, _) = look(notice.level, theme);
    let id = notice.id;
    let message = notice.message.clone();
    let state = workspace.state.clone();
    div()
        .flex()
        .items_start()
        .gap(px(8.))
        .px(px(10.))
        .py(px(6.))
        .child(
            div()
                .flex_none()
                .pt(px(4.))
                .child(Icon::new(icon).color(icon_color)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .py(px(3.))
                .text_color(theme.text)
                .child(message.clone()),
        )
        .children(notice.action.clone().map(|action| {
            let state = state.clone();
            Button::new(("notice-row-action", id as usize))
                .label(action.label)
                .compact()
                .on_click(move |_, window, cx| {
                    state.update(cx, |s, cx| s.dismiss_notice(id, cx));
                    (action.run)(window, cx);
                })
        }))
        .child(
            Button::new(("notice-copy", id as usize))
                .icon(IconName::Copy)
                .tooltip("Copy")
                .style(ButtonStyle::Ghost)
                .compact()
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(message.clone()));
                }),
        )
        .child(
            Button::new(("notice-dismiss", id as usize))
                .icon(IconName::Close)
                .tooltip("Dismiss")
                .style(ButtonStyle::Ghost)
                .compact()
                .on_click(move |_, _, cx| {
                    state.update(cx, |s, cx| s.dismiss_notice(id, cx));
                }),
        )
}

/// The full text of every current notice, newest first, right above the label.
pub fn list(workspace: &Workspace, bottom: f32, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    if !workspace.notices_open {
        return None;
    }
    let theme = cx.theme().clone();
    let notices: Vec<_> = workspace
        .state
        .read(cx)
        .notices
        .iter()
        .filter(|n| !n.leaving)
        .rev()
        .map(|notice| row(notice, workspace, &theme))
        .collect();
    let several = notices.len() > 1;
    let state = workspace.state.clone();
    let list = motion::enter(
        div()
            .id("notice-list")
            .occlude()
            .min_w_0()
            .w(px(520.))
            .max_h(px(280.))
            .flex()
            .flex_col()
            .rounded(px(9.))
            .border_1()
            .border_color(theme.border)
            .bg(theme.elevated)
            .text_size(ui_px(12.))
            .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _, cx| {
                if !this.notice_hovered {
                    this.notices_open = false;
                    cx.notify();
                }
            }))
            .when(several, |this| {
                this.child(
                    div()
                        .flex()
                        .flex_none()
                        .justify_end()
                        .px(px(10.))
                        .pt(px(6.))
                        .child(
                            Button::new("notice-clear")
                                .label("Clear all")
                                .style(ButtonStyle::Ghost)
                                .compact()
                                .on_click(move |_, _, cx| {
                                    state.update(cx, |s, cx| s.clear_notices(cx));
                                }),
                        ),
                )
            })
            .child(
                div()
                    .id("notice-rows")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(notices),
            ),
        "notice-list-enter",
        motion::MENU,
    );
    Some(
        corner(bottom + LABEL_HEIGHT + 6.)
            .child(list)
            .into_any_element(),
    )
}
