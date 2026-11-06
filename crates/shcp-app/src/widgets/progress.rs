use crate::theme::ActiveTheme as _;
use gpui::prelude::*;
use gpui::{App, Path, PathBuilder, Pixels, Point, Window, canvas, div, point, px, relative};

/// Thin horizontal progress bar. `fraction` is clamped to 0..=1.
#[derive(IntoElement)]
pub struct ProgressBar {
    fraction: f32,
    failed: bool,
}

impl ProgressBar {
    pub fn new(fraction: f32) -> Self {
        Self {
            fraction: fraction.clamp(0., 1.),
            failed: false,
        }
    }
    pub fn failed(mut self, failed: bool) -> Self {
        self.failed = failed;
        self
    }
}

impl RenderOnce for ProgressBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let color = if self.failed {
            theme.error
        } else {
            theme.accent
        };
        div()
            .h(px(4.))
            .w_full()
            .rounded(px(2.))
            .bg(theme.hover)
            .child(
                div()
                    .h_full()
                    .w(relative(self.fraction))
                    .rounded(px(2.))
                    .bg(color),
            )
    }
}

const RING_STROKE: f32 = 2.;

/// A point on a circle, in degrees clockwise from its top.
fn on_circle(center: Point<Pixels>, radius: f32, degrees: f32) -> Point<Pixels> {
    let (sin, cos) = degrees.to_radians().sin_cos();
    point(center.x + px(radius * sin), center.y - px(radius * cos))
}

/// A stroke along a circle from its top, clockwise. Whole turns need more than one arc: a single
/// one would end where it starts, which draws nothing.
fn arc_from_top(center: Point<Pixels>, radius: f32, degrees: f32) -> Option<Path<Pixels>> {
    if degrees <= 0. {
        return None;
    }
    let mut builder = PathBuilder::stroke(px(RING_STROKE));
    builder.move_to(on_circle(center, radius, 0.));
    let pieces = (degrees / 90.).ceil() as u32;
    for piece in 1..=pieces {
        builder.arc_to(
            point(px(radius), px(radius)),
            px(0.),
            false,
            true,
            on_circle(center, radius, degrees * piece as f32 / pieces as f32),
        );
    }
    builder.build().ok()
}

/// Round progress, for where a bar has no room. `percent` is clamped to 0..=100.
#[derive(IntoElement)]
pub struct ProgressRing {
    percent: u8,
    size: Pixels,
}

impl ProgressRing {
    pub fn new(percent: u8, size: Pixels) -> Self {
        Self {
            percent: percent.min(100),
            size,
        }
    }
}

impl RenderOnce for ProgressRing {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (track, color) = (cx.theme().border, cx.theme().accent);
        let degrees = self.percent as f32 * 3.6;
        canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let radius = (f32::from(bounds.size.width) - RING_STROKE) / 2.;
                let center = bounds.center();
                if let Some(path) = arc_from_top(center, radius, 360.) {
                    window.paint_path(path, track);
                }
                if let Some(path) = arc_from_top(center, radius, degrees) {
                    window.paint_path(path, color);
                }
            },
        )
        .size(self.size)
        .flex_none()
    }
}
