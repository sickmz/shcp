//! A thin scrollbar over the right edge of whatever scrolls: the thumb shows where the view is and
//! follows the mouse. What it reads and moves is up to the caller.
use gpui::prelude::*;
use gpui::{
    App, Bounds, Hitbox, HitboxBehavior, Hsla, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Window, canvas, div, fill, point, px, size,
};
use std::{cell::Cell, rc::Rc};

const WIDTH: f32 = 10.;
const THUMB_WIDTH: f32 = 6.;
const MIN_THUMB: f32 = 24.;
/// Room at both ends of the track, so the thumb stays clear of the rounded corners of a panel.
const INSET: f32 = 6.;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollMetrics {
    /// From 0 at the top to 1 at the bottom.
    pub position: f32,
    /// The part of the content on screen, from 0 to 1.
    pub visible: f32,
}

/// Top and height of the thumb on a track of this height.
fn thumb(track: f32, metrics: ScrollMetrics) -> (f32, f32) {
    let height = (metrics.visible * track).max(MIN_THUMB).min(track);
    (metrics.position.clamp(0., 1.) * (track - height), height)
}

/// The position that puts the thumb, held `grab` below its top, at `y` on the track.
fn position_at(y: f32, grab: f32, track: f32, height: f32) -> f32 {
    let travel = track - height;
    if travel <= 0. {
        return 0.;
    }
    ((y - grab) / travel).clamp(0., 1.)
}

/// Where the thumb is being held, kept by the view between frames.
pub type ScrollDrag = Rc<Cell<Option<f32>>>;

type Metrics = Rc<dyn Fn(&App) -> Option<ScrollMetrics>>;
type Seek = Rc<dyn Fn(f32, &mut Window, &mut App)>;

/// Goes last among the children of a relatively positioned box. `metrics` runs while painting, so
/// it sees what was laid out in the same frame; with nothing to scroll, nothing is drawn.
pub fn scrollbar(
    drag: ScrollDrag,
    color: Hsla,
    metrics: impl Fn(&App) -> Option<ScrollMetrics> + 'static,
    seek: impl Fn(f32, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let metrics: Metrics = Rc::new(metrics);
    let seek: Seek = Rc::new(seek);
    div()
        .absolute()
        .top(px(INSET))
        .bottom(px(INSET))
        .right_0()
        .w(px(WIDTH))
        .child(
            canvas(
                move |bounds, window, cx| {
                    let metrics = metrics(cx).filter(|metrics| metrics.visible < 1.)?;
                    let hitbox: Hitbox = window.insert_hitbox(bounds, HitboxBehavior::BlockMouse);
                    Some((metrics, hitbox))
                },
                move |bounds, prepared, window, _| {
                    if let Some((metrics, hitbox)) = prepared {
                        paint(bounds, metrics, hitbox, drag, color, seek, window);
                    }
                },
            )
            .size_full(),
        )
}

fn paint(
    bounds: Bounds<Pixels>,
    metrics: ScrollMetrics,
    hitbox: Hitbox,
    drag: ScrollDrag,
    color: Hsla,
    seek: Seek,
    window: &mut Window,
) {
    let track = f32::from(bounds.size.height);
    let (top, height) = thumb(track, metrics);
    let held = drag.get().is_some();
    let hovered = hitbox.is_hovered(window);
    let thumb_bounds = Bounds::new(
        point(
            bounds.origin.x + px((WIDTH - THUMB_WIDTH) / 2.),
            bounds.origin.y + px(top),
        ),
        size(px(THUMB_WIDTH), px(height)),
    );
    let alpha = if held {
        0.7
    } else if hovered {
        0.5
    } else {
        0.3
    };
    window.paint_quad(
        fill(thumb_bounds, Hsla { a: alpha, ..color }).corner_radii(px(THUMB_WIDTH / 2.)),
    );
    let origin = f32::from(bounds.origin.y);
    window.on_mouse_event({
        let (drag, seek) = (drag.clone(), seek.clone());
        move |event: &MouseDownEvent, phase, window, cx| {
            if !phase.bubble() || event.button != MouseButton::Left || !hitbox.is_hovered(window) {
                return;
            }
            let y = f32::from(event.position.y) - origin;
            // On the thumb it stays under the mouse; on the track it jumps there.
            let grab = if (top..top + height).contains(&y) {
                y - top
            } else {
                height / 2.
            };
            drag.set(Some(grab));
            seek(position_at(y, grab, track, height), window, cx);
            cx.stop_propagation();
        }
    });
    window.on_mouse_event({
        let drag = drag.clone();
        move |event: &MouseMoveEvent, phase, window, cx| {
            if !phase.bubble() {
                return;
            }
            if let Some(grab) = drag.get() {
                let y = f32::from(event.position.y) - origin;
                seek(position_at(y, grab, track, height), window, cx);
            }
        }
    });
    window.on_mouse_event(move |_: &MouseUpEvent, phase, window, _| {
        if phase.bubble() && drag.take().is_some() {
            window.refresh();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_thumb_shows_the_part_on_screen_and_where_it_is() {
        let metrics = |position, visible| ScrollMetrics { position, visible };
        assert_eq!(thumb(100., metrics(0., 0.5)), (0., 50.));
        assert_eq!(thumb(100., metrics(1., 0.5)), (50., 50.));
        assert_eq!(thumb(100., metrics(0.5, 0.5)), (25., 50.));
    }

    #[test]
    fn a_long_list_still_has_a_thumb_to_hold() {
        let (top, height) = thumb(
            100.,
            ScrollMetrics {
                position: 1.,
                visible: 0.001,
            },
        );
        assert_eq!(height, MIN_THUMB);
        assert_eq!(top, 100. - MIN_THUMB);
    }

    #[test]
    fn dragging_goes_from_top_to_bottom_and_no_further() {
        assert_eq!(position_at(10., 10., 100., 50.), 0.);
        assert_eq!(position_at(60., 10., 100., 50.), 1.);
        assert_eq!(position_at(35., 10., 100., 50.), 0.5);
        assert_eq!(position_at(-50., 10., 100., 50.), 0.);
        assert_eq!(position_at(500., 10., 100., 50.), 1.);
        assert_eq!(position_at(50., 10., 100., 100.), 0.);
    }
}
