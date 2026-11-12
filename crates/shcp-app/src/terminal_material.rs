//! Smoked glass for the cards of a tab, its terminals and its files. Two things, kept apart:
//!
//! - an ambient field of faint tints that belongs to the whole area the cards share: plum from
//!   its top, amber from its bottom, copper from its sides. It is one picture, as large as the
//!   area, and a card shows the part it sits on: a split cuts one surface in pieces instead of
//!   repeating it, and dragging a divider moves the cut and not the field;
//! - a rim that belongs to the card: the lift of the one with the keyboard, its crests, their
//!   glow, its corners and the dark line that parts it from the backdrop.
//!
//! The field is a picture and not gradients because gpui does not dither: tints this faint, as
//! gradients, come out as blocks a level apart, in rows and in columns. The picture is computed
//! with the rounding spread by a noise, once per size of the area.
//!
//! The colors were measured on a reference screenshot; how they are layered is a reconstruction.
use gpui::prelude::*;
use gpui::{
    App, BorderStyle, Bounds, ContentMask, Corners, Div, Edges, Hsla, Image, ImageFormat, Pixels,
    RenderImage, Window, canvas, div, linear_color_stop, linear_gradient, point, px, quad, rgb,
    transparent_black, white,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

/// The reference is a screenshot at twice the size: its pixels, in ours.
const SCALE: f32 = 0.5;
/// No picture larger than this on a side: the cards go flat instead.
const LARGEST: u32 = 8192;

/// A tint of the field: `alpha` of `color` at the share `from` of the area, fading to nothing
/// at the share `to`. Shares go from 0, the top or the left, to 1.
#[derive(Clone, Copy)]
pub struct Tint {
    pub color: Hsla,
    pub alpha: f32,
    pub from: f32,
    pub to: f32,
}

impl Tint {
    /// How much of it there is at `share` of the area.
    fn at(&self, share: f32) -> f32 {
        self.alpha * ((self.to - share) / (self.to - self.from)).clamp(0., 1.)
    }
}

/// A glow inside an edge: `alpha` at the edge, nothing `reach` pixels in.
#[derive(Clone, Copy)]
pub struct Glow {
    pub alpha: f32,
    pub reach: f32,
}

#[derive(Clone)]
pub struct Material {
    /// The surface where no tint reaches.
    pub base: Hsla,
    /// How much of what is behind the window the surface lets through: 1 for none.
    pub alpha: f32,
    /// Plum, from the top of the area: a near ramp and a far one.
    pub top: [Tint; 2],
    /// Amber, from the bottom of the area.
    pub bottom: Tint,
    /// Copper, from the left of the area: a near ramp and a far one.
    pub left: [Tint; 2],
    /// A warmth from the right of the area.
    pub right: Tint,
    /// A veil over the field where it is the window's backdrop, around the cards: what parts
    /// them from it.
    pub shade: Hsla,
    /// White over the card that has the keyboard.
    pub focus_lift: f32,
    /// The lines of light along the edges of a card, one pixel each.
    pub top_crest: Hsla,
    pub bottom_crest: Hsla,
    pub left_crest: Hsla,
    pub right_crest: Hsla,
    /// What the crests shed inside the card, in `top_crest`'s and `bottom_crest`'s color.
    pub top_glow: [Glow; 2],
    pub bottom_glow: Glow,
    /// How much of the crests and glows a card without the keyboard keeps.
    pub idle_rim: f32,
    pub radius: f32,
    /// The line around a card, over its surface: darker than the backdrop.
    pub separation: Hsla,
}

impl Material {
    /// The material over a dark `base`. With the reference's own base, #22201e, the tints land
    /// on the colors measured there.
    pub fn smoked(base: Hsla) -> Self {
        let color = |hex| -> Hsla { rgb(hex).into() };
        let (plum, amber) = (color(0x72488c), color(0x723400));
        let (copper, warm) = (color(0xae3e14), color(0x86481e));
        let glass = color(0xf0dde8);
        let tint = |color, alpha, from, to| Tint {
            color,
            alpha,
            from,
            to,
        };
        Self {
            base,
            alpha: 1.,
            top: [tint(plum, 0.085, 0., 0.30), tint(plum, 0.035, 0., 0.55)],
            bottom: tint(amber, 0.10, 1., 0.60),
            left: [tint(copper, 0.07, 0., 0.14), tint(copper, 0.035, 0., 0.50)],
            right: tint(warm, 0.10, 1., 0.80),
            shade: Hsla {
                a: 0.10,
                ..gpui::black()
            },
            focus_lift: 0.02,
            top_crest: Hsla { a: 0.30, ..glass },
            bottom_crest: Hsla {
                a: 0.15,
                ..color(0xf0e4dc)
            },
            left_crest: Hsla {
                a: 0.08,
                ..color(0xffaa70)
            },
            right_crest: Hsla { a: 0.04, ..glass },
            top_glow: [
                Glow {
                    alpha: 0.06,
                    reach: 6. * SCALE,
                },
                Glow {
                    alpha: 0.02,
                    reach: 24. * SCALE,
                },
            ],
            bottom_glow: Glow {
                alpha: 0.03,
                reach: 13. * SCALE,
            },
            idle_rim: 0.6,
            radius: 20. * SCALE,
            separation: Hsla {
                a: 0.4,
                ..gpui::black()
            },
        }
    }

    /// The same material, letting nothing through: for what floats over the window, where what
    /// would show through is the window's own content.
    pub fn opaque(self) -> Self {
        Self { alpha: 1., ..self }
    }

    /// The same material in other colors, at the same strengths: the tints from the top, the
    /// bottom, the left and the right of the area, the light of the crests, and the veil of the
    /// backdrop.
    pub fn recolored(mut self, tints: [Hsla; 4], glass: Hsla, shade: Hsla) -> Self {
        let [top, bottom, left, right] = tints;
        for tint in &mut self.top {
            tint.color = top;
        }
        self.bottom.color = bottom;
        for tint in &mut self.left {
            tint.color = left;
        }
        self.right.color = right;
        for crest in [
            &mut self.top_crest,
            &mut self.bottom_crest,
            &mut self.left_crest,
            &mut self.right_crest,
        ] {
            *crest = Hsla {
                a: crest.a,
                ..glass
            };
        }
        self.shade = shade;
        self
    }

    /// The field at the share `down` of the area, before the tints of its sides: red, green and
    /// blue from 0 to 255.
    fn row(&self, down: f32) -> [f32; 3] {
        let mut color = channels(self.base);
        for tint in self.top.iter().chain([&self.bottom]) {
            color = over(color, tint, down);
        }
        color
    }

    /// The field at the share `across` of the area, on a `row` of it.
    fn tinted(&self, row: [f32; 3], across: f32) -> [f32; 3] {
        let mut color = row;
        for tint in self.left.iter().chain([&self.right]) {
            color = over(color, tint, across);
        }
        color
    }

    /// The field over an area of `width` by `height` device pixels, as the bytes of a BMP: what
    /// gpui reads without a dependency of ours, and nearly free to write and to read.
    fn bake(&self, width: u32, height: u32) -> Vec<u8> {
        let stride = (width as usize * 3).div_ceil(4) * 4;
        let pixels = stride * height as usize;
        let mut out = Vec::with_capacity(54 + pixels);
        // File header, then the 40 bytes that describe 24 bits a pixel, rows from the bottom.
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&(54 + pixels as u32).to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&(width as i32).to_le_bytes());
        out.extend_from_slice(&(height as i32).to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&24u16.to_le_bytes());
        out.extend_from_slice(&[0; 24]);
        for y in (0..height).rev() {
            let row = self.row((y as f32 + 0.5) / height as f32);
            let start = out.len();
            for x in 0..width {
                let color = self.tinted(row, (x as f32 + 0.5) / width as f32);
                // Blue first. Each channel rounds on its own noise, so no step lines up.
                for channel in [2usize, 1, 0] {
                    let noise = noise(x + 17 * channel as u32, y + 31 * channel as u32);
                    out.push((color[channel] + noise).floor().clamp(0., 255.) as u8);
                }
            }
            out.resize(start + stride, 0);
        }
        out
    }

    /// Paints the surface of the card at `outer`: the part of the field it covers. Opaque: the
    /// element it is painted in gives it the surface's alpha.
    fn paint_field(
        &self,
        outer: Bounds<Pixels>,
        ambient: &Ambient,
        window: &mut Window,
        cx: &mut App,
    ) {
        let clear = transparent_black();
        let Some((area, picture)) = ambient.field(self, window, cx) else {
            // No picture yet, or none this large: the plain surface.
            window.paint_quad(quad(
                outer,
                fitting(px(self.radius), outer),
                Hsla { a: 1., ..self.base },
                px(0.),
                clear,
                BorderStyle::Solid,
            ));
            return;
        };
        let (origin, size) = (outer.origin, outer.size);
        let radius = fitting(px(self.radius), outer);
        let (right, bottom) = (origin.x + size.width, origin.y + size.height);
        // gpui clips to rectangles, and the picture is as large as the area. So the corners are
        // quads, each in the one color the field has there: over a corner it moves by less than
        // a level. They go under the picture and a pixel past their square, so no seam shows.
        let reach = radius + px(1.);
        let corner = |x: Pixels, y: Pixels, corners: Corners<Pixels>| {
            let center = point(x + reach / 2., y + reach / 2.);
            let color = self.tinted(
                self.row(f32::from(center.y - area.origin.y) / f32::from(area.size.height)),
                f32::from(center.x - area.origin.x) / f32::from(area.size.width),
            );
            quad(
                Bounds::new(point(x, y), gpui::size(reach, reach)),
                corners,
                gpui::Rgba {
                    r: color[0] / 255.,
                    g: color[1] / 255.,
                    b: color[2] / 255.,
                    a: 1.,
                },
                px(0.),
                clear,
                BorderStyle::Solid,
            )
        };
        let (none, round) = (px(0.), radius);
        let rounded = |top_left, top_right, bottom_right, bottom_left| Corners {
            top_left,
            top_right,
            bottom_right,
            bottom_left,
        };
        for corner in [
            corner(origin.x, origin.y, rounded(round, none, none, none)),
            corner(right - reach, origin.y, rounded(none, round, none, none)),
            corner(
                right - reach,
                bottom - reach,
                rounded(none, none, round, none),
            ),
            corner(origin.x, bottom - reach, rounded(none, none, none, round)),
        ] {
            window.paint_quad(corner);
        }
        // The rest is three bands of the picture: between the corners at the top and at the
        // bottom, and the whole width in between.
        let between = gpui::size(size.width - radius * 2., radius);
        for band in [
            Bounds::new(point(origin.x + radius, origin.y), between),
            Bounds::new(
                point(origin.x, origin.y + radius),
                gpui::size(size.width, size.height - radius * 2.),
            ),
            Bounds::new(point(origin.x + radius, bottom - radius), between),
        ] {
            window.with_content_mask(Some(ContentMask { bounds: band }), |window| {
                let _ = window.paint_image(area, Corners::default(), picture.clone(), 0, false);
            });
        }
    }

    /// Paints what is the card's own at `outer`: the dark line, the lift, the crests and glows.
    fn paint_rim(&self, outer: Bounds<Pixels>, active: bool, lifted: bool, window: &mut Window) {
        let clear = transparent_black();
        // Inside the dark line.
        let bounds = outer.inset(px(1.));
        let height = f32::from(bounds.size.height);
        if f32::from(bounds.size.width) <= 0. || height <= 0. {
            return;
        }
        window.paint_quad(quad(
            outer,
            fitting(px(self.radius), outer),
            clear,
            px(1.),
            self.separation,
            BorderStyle::Solid,
        ));
        let radius = fitting(px((self.radius - 1.).max(0.)), bounds);
        let mut fill = |background: gpui::Background| {
            window.paint_quad(quad(
                bounds,
                radius,
                background,
                px(0.),
                clear,
                BorderStyle::Solid,
            ));
        };
        if lifted {
            fill(
                Hsla {
                    a: self.focus_lift,
                    ..white()
                }
                .into(),
            );
        }

        // The rim, in pixels: a resize moves it and never stretches it. Its ramps are steep
        // enough to need no noise.
        let rim = if active { 1. } else { self.idle_rim };
        let glow = |color: Hsla, glow: &Glow, angle: f32| {
            linear_gradient(
                angle,
                linear_color_stop(
                    Hsla {
                        a: glow.alpha * rim,
                        ..color
                    },
                    0.,
                ),
                linear_color_stop(Hsla { a: 0., ..color }, (glow.reach / height).min(0.5)),
            )
        };
        for part in &self.top_glow {
            fill(glow(self.top_crest, part, 180.));
        }
        fill(glow(self.bottom_crest, &self.bottom_glow, 0.));
        self.paint_crests(bounds, radius, rim, window);
    }

    /// The crests alone, at `bounds`, `rim` of them: a border on one side tapers to nothing
    /// around the corners, which makes it the crest of that side.
    fn paint_crests(&self, bounds: Bounds<Pixels>, radius: Pixels, rim: f32, window: &mut Window) {
        let radius = fitting(radius, bounds);
        let clear = transparent_black();
        let (none, line) = (px(0.), px(1.));
        let side = |top, right, bottom, left| Edges {
            top,
            right,
            bottom,
            left,
        };
        let sides = [
            (self.top_crest, side(line, none, none, none)),
            (self.right_crest, side(none, line, none, none)),
            (self.bottom_crest, side(none, none, line, none)),
            (self.left_crest, side(none, none, none, line)),
        ];
        for (color, edges) in sides {
            window.paint_quad(quad(
                bounds,
                radius,
                clear,
                edges,
                color.opacity(rim),
                BorderStyle::Solid,
            ));
        }
    }
}

/// `radius`, or the most `bounds` can be rounded with: a capsule asks for more than it has, and
/// a quad painted directly is not brought back to its size as an element is.
fn fitting(radius: Pixels, bounds: Bounds<Pixels>) -> Pixels {
    radius
        .min(bounds.size.width / 2.)
        .min(bounds.size.height / 2.)
}

/// Red, green and blue from 0 to 255.
fn channels(color: Hsla) -> [f32; 3] {
    let color = color.to_rgb();
    [color.r * 255., color.g * 255., color.b * 255.]
}

/// `tint`, as much as there is at `share`, over `under`.
fn over(under: [f32; 3], tint: &Tint, share: f32) -> [f32; 3] {
    let (amount, top) = (tint.at(share), channels(tint.color));
    std::array::from_fn(|i| under[i] + amount * (top[i] - under[i]))
}

/// Interleaved gradient noise: from 0 to 1, even over any few pixels, the same for a pixel
/// every time.
fn noise(x: u32, y: u32) -> f32 {
    (52.982_92 * (0.067_110_56 * x as f32 + 0.005_837_15 * y as f32).fract()).fract()
}

/// The field of the window: where the cards are, all of them together, and its picture.
#[derive(Default)]
pub struct Ambient {
    area: Cell<Option<Bounds<Pixels>>>,
    baked: RefCell<Option<Baked>>,
}

struct Baked {
    /// The size in device pixels and the colors it was computed for: the base and the tints.
    key: (u32, u32, [[u8; 3]; 5]),
    picture: Arc<RenderImage>,
}

thread_local! {
    static AMBIENT: Rc<Ambient> = Rc::default();
}

/// The one field of the window. Every tab shows its cards in the same place, so they share it.
pub fn ambient() -> Rc<Ambient> {
    AMBIENT.with(Rc::clone)
}

impl Ambient {
    /// Where the cards are. To call while laying out: the cards read it while painting.
    pub fn set_area(&self, area: Bounds<Pixels>) {
        self.area.set(Some(area));
    }

    /// The area and its picture in `material`, computed again only when the area changes size
    /// or the theme its base.
    fn field(
        &self,
        material: &Material,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<(Bounds<Pixels>, Arc<RenderImage>)> {
        let area = self.area.get()?;
        let scale = window.scale_factor();
        let width = (f32::from(area.size.width) * scale).ceil() as u32;
        let height = (f32::from(area.size.height) * scale).ceil() as u32;
        if width == 0 || height == 0 || width > LARGEST || height > LARGEST {
            return None;
        }
        let bytes = |color: Hsla| channels(color).map(|v| v.round() as u8);
        let colors = [
            material.base,
            material.top[0].color,
            material.bottom.color,
            material.left[0].color,
            material.right.color,
        ];
        let key = (width, height, colors.map(bytes));
        let mut baked = self.baked.borrow_mut();
        if baked.as_ref().is_none_or(|baked| baked.key != key) {
            if let Some(old) = baked.take() {
                let _ = window.drop_image(old.picture);
            }
            let picture = Image::from_bytes(ImageFormat::Bmp, material.bake(width, height))
                .to_image_data(cx.svg_renderer())
                .ok()?;
            *baked = Some(Baked { key, picture });
        }
        baked.as_ref().map(|baked| (area, baked.picture.clone()))
    }
}

/// What the window shows around its cards and under its sidebar: the same field, a shade
/// darker, so the cards and the rest of the window are one surface and not two colors side by
/// side. It fills what it is put in, which is from then on the area of the field; `alpha` is how
/// much of it there is, under 1 for a window that lets the desktop through.
pub fn backdrop(material: Material, alpha: f32) -> Div {
    let (ambient, field) = (ambient(), ambient());
    div().absolute().inset_0().opacity(alpha).child(
        canvas(
            move |bounds, _, _| ambient.set_area(bounds),
            move |bounds, _, window, cx| {
                let flat = |color: Hsla| {
                    quad(
                        bounds,
                        px(0.),
                        color,
                        px(0.),
                        transparent_black(),
                        BorderStyle::Solid,
                    )
                };
                match field.field(&material, window, cx) {
                    Some((area, picture)) => {
                        let _ = window.paint_image(area, Corners::default(), picture, 0, false);
                    }
                    None => window.paint_quad(flat(Hsla {
                        a: 1.,
                        ..material.base
                    })),
                }
                window.paint_quad(flat(material.shade));
            },
        )
        .size_full(),
    )
}

/// A card in `material`, at its place in the window's field. What it holds is painted over the
/// material and keeps its own opacity; the material takes no pointer. `active`: it has the
/// keyboard, and its rim shows in full. `lifted`: it is lighter for it too, which says something
/// only among several cards.
pub fn frame(material: Material, active: bool, lifted: bool) -> Div {
    div()
        .relative()
        .flex()
        .flex_col()
        .min_w_0()
        .rounded(px(material.radius))
        // The room of the dark line, which the material paints itself.
        .p(px(1.))
        .child(layers(material, active, lifted))
}

/// The material alone, for what is not laid out as a card of the main area: a modal. To add
/// first to something `relative` with no fill and no border of its own, which it then fills.
pub fn layers(material: Material, active: bool, lifted: bool) -> Div {
    let ambient = ambient();
    let field = material.clone();
    div()
        .absolute()
        .inset_0()
        // The surface alone lets the window's backdrop through, when the window does.
        .child(
            div().absolute().inset_0().opacity(material.alpha).child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, cx| field.paint_field(bounds, &ambient, window, cx),
                )
                .size_full(),
            ),
        )
        .child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| material.paint_rim(bounds, active, lifted, window),
            )
            .absolute()
            .inset_0(),
        )
}

/// The crests alone, for what keeps a fill of its own and is `relative`: a field, a capsule.
/// `radius` is what it is rounded with.
pub fn crests(material: Material, radius: f32) -> Div {
    div().absolute().inset_0().child(
        canvas(
            |_, _, _| {},
            move |bounds, _, window, _| material.paint_crests(bounds, px(radius), 1., window),
        )
        .size_full(),
    )
}
