//! The pill behind the current item of a list: it glides there when the mouse picks another
//! one, and is simply there otherwise. Painted by the list, under its items.
use crate::motion::PillMotion;
use crate::theme::Theme;
use gpui::prelude::*;
use gpui::{
    App, BorderStyle, Bounds, Edges, Hsla, Pixels, Window, canvas, point, px, quad,
    transparent_black,
};
use shcp_core::Settings;
use std::{cell::RefCell, rc::Rc, time::Instant};

/// Keep one per list, for the life of the list: it remembers where the pill is.
#[derive(Clone, Default)]
pub struct SlidingPill(Rc<RefCell<PillMotion>>);

impl SlidingPill {
    /// The next change of item came from the mouse, so the pill may show the way, if the user
    /// wants that.
    pub fn arm(&self, settings: &Settings) {
        if settings.animated_selection {
            self.0.borrow_mut().arm();
        }
    }

    /// For `on_children_prepainted` of the list. The first child must be the pill itself, and
    /// `item` is the index among the children of the current item.
    pub fn measure(
        &self,
        item: Option<usize>,
    ) -> impl Fn(Vec<Bounds<Pixels>>, &mut Window, &mut App) + 'static {
        let motion = self.0.clone();
        move |children, _, _| {
            let target = children.first().zip(item.and_then(|i| children.get(i)));
            // From the pill's own origin: whatever scrolls the list moves both alike.
            let target = target.map(|(origin, bounds)| {
                let (left, top) = (bounds.left() - origin.left(), bounds.top() - origin.top());
                let (right, bottom) = (left + bounds.size.width, top + bounds.size.height);
                [left, top, right, bottom].map(f32::from)
            });
            motion.borrow_mut().aim(target);
        }
    }

    /// Without the accent: the neutral fill of the current item, with a faint edge. In a theme
    /// with a material it has the crests of a card too: light along the top, less at the bottom.
    pub fn neutral(&self, theme: &Theme, radius: f32) -> impl IntoElement + use<> {
        self.painted(
            theme.active,
            Some(Hsla {
                a: 0.05,
                ..theme.text
            }),
            theme
                .material()
                .map(|material| (material.top_crest, material.bottom_crest)),
            radius,
        )
    }

    fn painted(
        &self,
        fill: Hsla,
        border: Option<Hsla>,
        crests: Option<(Hsla, Hsla)>,
        radius: f32,
    ) -> impl IntoElement + use<> {
        let motion = self.0.clone();
        canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let (edges, moving) = motion.borrow_mut().frame(Instant::now());
                let Some([left, top, right, bottom]) = edges else {
                    return;
                };
                let pill = Bounds::from_corners(
                    bounds.origin + point(px(left), px(top)),
                    bounds.origin + point(px(right), px(bottom)),
                );
                window.paint_quad(quad(
                    pill,
                    px(radius),
                    fill,
                    px(0.),
                    transparent_black(),
                    BorderStyle::Solid,
                ));
                if let Some(border) = border {
                    window.paint_quad(quad(
                        pill,
                        px(radius),
                        Hsla { a: 0., ..border },
                        px(1.),
                        border,
                        BorderStyle::Solid,
                    ));
                }
                // A border on one side tapers to nothing around the pill's ends.
                if let Some((top, bottom)) = crests {
                    let (none, line) = (px(0.), px(1.));
                    for (color, top, bottom) in [(top, line, none), (bottom, none, line)] {
                        window.paint_quad(quad(
                            pill,
                            px(radius),
                            transparent_black(),
                            Edges {
                                top,
                                bottom,
                                left: none,
                                right: none,
                            },
                            color,
                            BorderStyle::Solid,
                        ));
                    }
                }
                if moving {
                    window.request_animation_frame();
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }
}
