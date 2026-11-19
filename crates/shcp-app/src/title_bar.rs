//! The top strip of the window: its height, the app's logo and the minimize/maximize/close
//! buttons (gpui hit-tests them through `WindowControlArea`).
use crate::theme::{
    ActiveTheme as _, PANEL_INNER_RADIUS, RADIUS_CARD, RADIUS_CONTROL, Theme, row_px,
};
use crate::widgets::icon::{Icon, IconName};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Hsla, PathBuilder, Pixels, Point, Window, WindowControlArea, canvas, div,
    hsla, point, px,
};
use std::sync::atomic::{AtomicU32, Ordering};

pub(crate) const HEIGHT: f32 = 38.;
/// Between the cards and the edges of the window around them, and between the sidebar and them.
pub(crate) const CONTENT_INSET: f32 = 4.;
/// The line around a card (`CARD_BORDER` in `theme`, which keeps it to itself).
pub(crate) const CARD_BORDER: f32 = 1.;

/// A caption button in a strip, as Windows has them.
const FLUSH_WIDTH: f32 = 44.;
/// How wide a caption button in the corner of a card is.
const FUSED_WIDTH: f32 = 36.;
/// Between the caption buttons and what is left of them.
const CAPTION_GAP: f32 = 10.;

// Every `WindowControlArea::Drag` must also `occlude()`. Otherwise a press reaches the focusable
// views behind it, which take the focus and mark the event as handled; on Windows gpui then skips
// the default window procedure, and the window never starts moving.

/// What a strip across the top of the window leaves free at its right for the caption buttons.
pub(crate) fn strip_room() -> f32 {
    FLUSH_WIDTH * 3. + CAPTION_GAP
}

/// What the header of the card in the top right corner of the window leaves free at its right,
/// as its right padding, for the caption buttons in its corner. Zero while the cards start under
/// a strip of their own. Set by the workspace each frame, before the cards draw.
static CAPTION_ROOM: AtomicU32 = AtomicU32::new(0);

pub(crate) fn set_caption_room(over_cards: bool) {
    let room = if over_cards {
        FUSED_WIDTH * 3. + CARD_BORDER + CAPTION_GAP
    } else {
        0.
    };
    CAPTION_ROOM.store(room.to_bits(), Ordering::Relaxed);
}

pub(crate) fn caption_room() -> Pixels {
    px(f32::from_bits(CAPTION_ROOM.load(Ordering::Relaxed)))
}

pub(crate) fn logo(size: Pixels, color: Hsla) -> impl IntoElement {
    Icon::new(IconName::Logo).size(size).color(color)
}

const BUTTONS: [(&str, WindowControlArea); 3] = [
    ("caption-min", WindowControlArea::Min),
    ("caption-max", WindowControlArea::Max),
    ("caption-close", WindowControlArea::Close),
];

/// Close turns red under the pointer; the others take the theme's hover.
fn caption_button(
    id: &'static str,
    area: WindowControlArea,
    maximized: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let (hover_bg, hover_fg) = if matches!(area, WindowControlArea::Close) {
        (hsla(0., 0.75, 0.47, 1.), hsla(0., 0., 1., 1.))
    } else {
        (theme.hover, theme.text)
    };
    let icon = match area {
        WindowControlArea::Min => IconName::Minus,
        WindowControlArea::Max if maximized => IconName::Restore,
        WindowControlArea::Max => IconName::Square,
        _ => IconName::Close,
    };
    div()
        .id(id)
        .occlude()
        .window_control_area(area)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .hover(move |s| s.bg(hover_bg).text_color(hover_fg))
        .child(Icon::new(icon).size(px(12.)))
}

/// The minimize, maximize and close buttons. Drawn last, so they stay usable while a modal is
/// open.
#[derive(IntoElement)]
pub(crate) struct WindowControls {
    /// In the top right corner of the card there, one with it; otherwise flush with the corner
    /// of the window, as tall as `height`.
    fused: bool,
    height: f32,
}

impl WindowControls {
    /// Along a strip across the top of the window, `height` tall.
    pub(crate) fn flush(height: f32) -> Self {
        Self {
            fused: false,
            height,
        }
    }

    /// In the corner of the card at the top right of the window.
    pub(crate) fn fused() -> Self {
        Self {
            fused: true,
            height: 0.,
        }
    }
}

impl RenderOnce for WindowControls {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let maximized = window.is_maximized();
        if self.fused {
            return fused(window, theme, maximized);
        }
        div()
            .absolute()
            .top_0()
            .right_0()
            .h(px(self.height))
            .flex()
            .text_color(theme.text_muted)
            .children(BUTTONS.map(|(id, area)| {
                caption_button(id, area, maximized, theme)
                    .w(px(FLUSH_WIDTH))
                    .h_full()
            }))
            .into_any_element()
    }
}

/// The buttons in the corner of the card, sharing its top and right lines and its round corner.
/// Their own lines, at the left and at the bottom, meet those of the card in soft inner curves.
fn fused(window: &Window, theme: &Theme, maximized: bool) -> AnyElement {
    let width = f32::from(window.viewport_size().width);
    let (top, right) = (CONTENT_INSET, width - CONTENT_INSET);
    // The buttons are as tall as the card's header, from just under the card's top line, so their
    // glyphs sit level with the tools of the header. The group's own bottom line is under them.
    let bottom = top + CARD_BORDER + f32::from(row_px(crate::panel::HEADER_HEIGHT)) + CARD_BORDER;
    // The outer side of the group's own left line.
    let left = right - CARD_BORDER - FUSED_WIDTH * 3. - CARD_BORDER;
    let (fill, line) = (theme.elevated, theme.border);
    let first = left + CARD_BORDER;
    let buttons = BUTTONS.into_iter().enumerate().map(|(index, (id, area))| {
        caption_button(id, area, maximized, theme)
            .absolute()
            .left(px(first + FUSED_WIDTH * index as f32))
            .top(px(top + CARD_BORDER))
            .w(px(FUSED_WIDTH))
            .h(px(bottom - top - CARD_BORDER * 2.))
            // Under the pointer, the ends follow the round corners of the group.
            .when(index == 0, |this| {
                this.rounded_bl(px(RADIUS_CONTROL - CARD_BORDER))
            })
            .when(index == 2, |this| this.rounded_tr(px(PANEL_INNER_RADIUS)))
    });
    // The very corner of the window closes it too, as on any window: the gap above and right of
    // the card, without drawing there.
    let close_from = right - CARD_BORDER - FUSED_WIDTH;
    let corner = [
        ("caption-close-top", close_from, 0., width - close_from, top),
        ("caption-close-side", right, 0., CONTENT_INSET, bottom),
    ]
    .map(|(id, x, y, w, h)| {
        div()
            .id(id)
            .occlude()
            .window_control_area(WindowControlArea::Close)
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(w))
            .h(px(h))
    });
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .text_color(theme.text)
        .child(
            canvas(
                |_, _, _| {},
                move |_, _, window, _| paint_fused(left, top, right, bottom, fill, line, window),
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
        .children(buttons)
        .children(corner)
        .into_any_element()
}

/// The ground of the group and its own lines, in window coordinates: `left` and `bottom` are the
/// outer sides of its lines, `top` and `right` those of the card's. Lines are centred half a pixel
/// in, where the card draws its own; the ground stays inside the card's lines, so they keep the
/// card's color.
fn paint_fused(
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    fill: Hsla,
    line: Hsla,
    window: &mut Window,
) {
    let (soft, half) = (RADIUS_CONTROL, CARD_BORDER / 2.);
    let at = |x: f32, y: f32| -> Point<Pixels> { point(px(x), px(y)) };
    let arc = |path: &mut PathBuilder, radius: f32, clockwise: bool, to: Point<Pixels>| {
        path.arc_to(at(radius, radius), px(0.), false, clockwise, to);
    };
    // From the card's top line down into the group's left line, round its bottom left corner,
    // and from its bottom line down into the card's right line.
    let outline = |path: &mut PathBuilder| {
        path.move_to(at(left + half - soft, top + half));
        arc(path, soft, true, at(left + half, top + half + soft));
        path.line_to(at(left + half, bottom - half - soft));
        arc(path, soft, false, at(left + half + soft, bottom - half));
        path.line_to(at(right - half - soft, bottom - half));
        arc(path, soft, true, at(right - half, bottom - half + soft));
    };
    let mut ground = PathBuilder::fill();
    outline(&mut ground);
    ground.line_to(at(right - CARD_BORDER, bottom - half + soft));
    ground.line_to(at(right - CARD_BORDER, top + RADIUS_CARD));
    arc(
        &mut ground,
        PANEL_INNER_RADIUS,
        false,
        at(right - RADIUS_CARD, top + CARD_BORDER),
    );
    ground.line_to(at(left + half - soft, top + CARD_BORDER));
    ground.close();
    if let Ok(path) = ground.build() {
        window.paint_path(path, fill);
    }
    let mut edge = PathBuilder::stroke(px(CARD_BORDER));
    outline(&mut edge);
    if let Ok(path) = edge.build() {
        window.paint_path(path, line);
    }
}
