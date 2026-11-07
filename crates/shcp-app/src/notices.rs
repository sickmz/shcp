//! A small label in the corner of the panel with the latest notice. A click expands it, right
//! there, to the full text of all the current ones. It floats over the panel, so that a notice
//! never resizes the terminal, and stays inside it, so that it never covers the dock.
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
/// The widest a notice gets, the same expanded or not: only its height moves.
const WIDTH: f32 = 460.;
/// Past this, an expanded notice scrolls.
const MAX_HEIGHT: f32 = 280.;
/// About as many characters as fit in the label: a longer message has more to show when expanded.
const FITS_IN_LABEL: usize = 52;
/// What a row of the expanded notice takes besides its text.
const ROW_CHROME: f32 = 96.;
const ROW_PADDING: f32 = 18.;
/// The height of a row with nothing but its buttons.
const ROW_MIN_HEIGHT: f32 = 36.;
const CHAR_WIDTH: f32 = 6.5;
const LINE_HEIGHT: f32 = 19.;
const CLEAR_ALL_HEIGHT: f32 = 30.;

/// About how big the expanded notice is, to glide there: text is only measured once painted. It
/// fits short messages, and being off by a little only ends the glide with a small step.
fn expanded_size(notices: &[&Notice]) -> (f32, f32) {
    let longest = notices
        .iter()
        .map(|notice| notice.message.chars().count())
        .max()
        .unwrap_or(0) as f32;
    let width = if notices.iter().any(|notice| notice.action.is_some()) {
        WIDTH
    } else {
        (longest * CHAR_WIDTH + ROW_CHROME).clamp(240., WIDTH)
    };
    let per_line = ((width - ROW_CHROME) / CHAR_WIDTH).max(10.);
    let rows: f32 = notices
        .iter()
        .map(|notice| {
            let lines = (notice.message.chars().count() as f32 / per_line)
                .ceil()
                .max(1.);
            (lines * LINE_HEIGHT + ROW_PADDING).max(ROW_MIN_HEIGHT)
        })
        .sum();
    let clear = if notices.len() > 1 {
        CLEAR_ALL_HEIGHT
    } else {
        0.
    };
    (width, (rows + clear).min(MAX_HEIGHT))
}

/// Color only for what may need the user: information stays quiet.
fn look(level: NoticeLevel, theme: &Theme) -> (IconName, Hsla, Hsla) {
    match level {
        NoticeLevel::Info => (IconName::Info, theme.text_muted, theme.text_muted),
        NoticeLevel::Warning | NoticeLevel::Important => {
            (IconName::Warning, theme.warning, theme.text)
        }
        NoticeLevel::Error => (IconName::Alert, theme.error, theme.text),
    }
}

/// `bottom`: how far above the panel's lower edge it goes, to leave what is there in sight.
pub fn label(
    workspace: &Workspace,
    bottom: f32,
    cx: &mut Context<Workspace>,
) -> Option<AnyElement> {
    // Expanded, `list` is in its place, until it has folded back.
    if workspace.notices_open || workspace.notices_reveal.moving() {
        return None;
    }
    let theme = cx.theme().clone();
    let notices = &workspace.state.read(cx).notices;
    let notice = notices.last()?;
    let (icon, icon_color, text_color) = look(notice.level, &theme);
    let others = notices
        .iter()
        .filter(|n| !n.leaving)
        .count()
        .saturating_sub(1);
    let action = if others > 0 {
        Some(format!("{others} more"))
    } else {
        (notice.action.is_none() && notice.message.chars().count() > FITS_IN_LABEL)
            .then(|| "Details".to_string())
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
        .max_w(px(WIDTH))
        .h(px(LABEL_HEIGHT))
        .flex()
        .items_center()
        .gap(px(8.))
        .pl(px(10.))
        .pr(px(6.))
        .rounded(px(7.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.elevated)
        .cursor_pointer()
        .text_size(ui_px(12.))
        .text_color(text_color)
        // While the pointer is here the notice stays, for it to be read.
        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
            this.notice_hovered = *hovered;
            cx.notify();
        }))
        // A long message is cut here: expanded, it shows all of it.
        .on_click(cx.listener(|this, _, _, cx| this.show_notices(true, cx)))
        .child(Icon::new(icon).color(icon_color))
        .child(
            div()
                .min_w_0()
                .truncate()
                .child(notice.message.replace('\n', " ")),
        )
        .children(action.map(|text| div().flex_none().text_color(theme.accent).child(text)))
        .children(notice_action)
        .child(
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
        );
    let label = if notice.leaving {
        motion::exit(label, ("notice-out", id), motion::NOTICE_OUT).into_any_element()
    } else if workspace.notice_folded == Some(id) {
        // Back from being expanded: it does not come in a second time.
        label.into_any_element()
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
                    cx.stop_propagation();
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
                    cx.stop_propagation();
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
                    cx.stop_propagation();
                    state.update(cx, |s, cx| s.dismiss_notice(id, cx));
                }),
        )
}

/// The label expanded: the full text of every current notice, newest first.
pub fn list(workspace: &Workspace, bottom: f32, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let moving = workspace.notices_reveal.moving();
    if !workspace.notices_open && !moving {
        return None;
    }
    let theme = cx.theme().clone();
    let current: Vec<&Notice> = workspace
        .state
        .read(cx)
        .notices
        .iter()
        .filter(|n| !n.leaving)
        .rev()
        .collect();
    let (width, height) = expanded_size(&current);
    let several = current.len() > 1;
    let notices: Vec<_> = current
        .iter()
        .map(|notice| row(notice, workspace, &theme))
        .collect();
    // It grows out of the label: the top of the text shows first.
    let max_height = if moving {
        let amount = workspace.notices_reveal.value();
        LABEL_HEIGHT + (height - LABEL_HEIGHT) * amount
    } else {
        MAX_HEIGHT
    };
    let state = workspace.state.clone();
    let list = div()
        .id("notice-list")
        .occlude()
        .min_w_0()
        .w(px(width))
        .max_h(px(max_height))
        .overflow_hidden()
        .flex()
        .flex_col()
        .rounded(px(7.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.elevated)
        .cursor_pointer()
        .text_size(ui_px(12.))
        .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _, cx| {
            this.show_notices(false, cx);
        }))
        .on_click(cx.listener(|this, _, _, cx| {
            let open = !this.notices_open;
            this.show_notices(open, cx);
        }))
        .child(
            div()
                .id("notice-rows")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(notices),
        )
        .when(several, |this| {
            this.child(
                div()
                    .flex()
                    .flex_none()
                    .justify_end()
                    .px(px(10.))
                    .pb(px(6.))
                    .child(
                        Button::new("notice-clear")
                            .label("Clear all")
                            .style(ButtonStyle::Ghost)
                            .compact()
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                state.update(cx, |s, cx| s.clear_notices(cx));
                            }),
                    ),
            )
        });
    Some(corner(bottom).child(list).into_any_element())
}
