//! sRGB to OKLab and OKLCH and back (Björn Ottosson's matrices). The app's own themes are
//! built in OKLCH, where one step of lightness looks the same in every hue.

/// A color in OKLCH: lightness from 0 to 1, chroma, hue in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Oklch {
    pub l: f32,
    pub c: f32,
    pub h: f32,
}

fn to_linear(v: f32) -> f32 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn to_gamma(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1. / 2.4) - 0.055
    }
}

/// Gamma-encoded sRGB, 0 to 1, into OKLab `[L, a, b]`.
pub(crate) fn srgb_to_oklab(rgb: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = rgb.map(to_linear);
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

/// OKLab into gamma-encoded sRGB. Outside the sRGB gamut a channel leaves 0 to 1.
pub(crate) fn oklab_to_srgb([l, a, b]: [f32; 3]) -> [f32; 3] {
    let cube = |v: f32| v * v * v;
    let lms_l = cube(l + 0.396_337_78 * a + 0.215_803_76 * b);
    let lms_m = cube(l - 0.105_561_346 * a - 0.063_854_17 * b);
    let lms_s = cube(l - 0.089_484_18 * a - 1.291_485_5 * b);
    [
        4.076_741_7 * lms_l - 3.307_711_6 * lms_m + 0.230_969_94 * lms_s,
        -1.268_438 * lms_l + 2.609_757_4 * lms_m - 0.341_319_38 * lms_s,
        -0.004_196_086_3 * lms_l - 0.703_418_6 * lms_m + 1.707_614_7 * lms_s,
    ]
    .map(|v| v.signum() * to_gamma(v.abs()))
}

impl Oklch {
    pub(crate) fn new(l: f32, c: f32, h: f32) -> Self {
        Self { l, c, h }
    }

    pub(crate) fn from_srgb(rgb: [f32; 3]) -> Self {
        let [l, a, b] = srgb_to_oklab(rgb);
        Self {
            l,
            c: a.hypot(b),
            h: b.atan2(a).to_degrees().rem_euclid(360.),
        }
    }

    /// `0xRRGGBB`.
    pub(crate) fn from_hex(hex: u32) -> Self {
        Self::from_srgb([16, 8, 0].map(|shift| ((hex >> shift) & 0xff) as f32 / 255.))
    }

    /// Gamma-encoded sRGB, unclipped: outside the gamut a channel leaves 0 to 1.
    pub(crate) fn to_srgb(self) -> [f32; 3] {
        let radians = self.h.to_radians();
        oklab_to_srgb([self.l, self.c * radians.cos(), self.c * radians.sin()])
    }

    pub(crate) fn in_gamut(self) -> bool {
        const SLACK: f32 = 1e-4;
        self.to_srgb()
            .iter()
            .all(|v| (-SLACK..=1. + SLACK).contains(v))
    }

    /// Gamma-encoded sRGB. Outside the gamut the color keeps its lightness and hue and gives up
    /// chroma until it fits; a lightness outside 0 to 1 ends up black or white.
    pub(crate) fn to_srgb_in_gamut(self) -> [f32; 3] {
        let mut color = Self {
            l: self.l.clamp(0., 1.),
            ..self
        };
        if !color.in_gamut() {
            let (mut low, mut high) = (0., color.c);
            for _ in 0..24 {
                let mid = (low + high) / 2.;
                if (Self { c: mid, ..color }).in_gamut() {
                    low = mid;
                } else {
                    high = mid;
                }
            }
            color.c = low;
        }
        color.to_srgb().map(|v| v.clamp(0., 1.))
    }

    /// The same color, `dl` lighter: chroma and hue stay.
    pub(crate) fn lighter(self, dl: f32) -> Self {
        Self {
            l: self.l + dl,
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_srgb() {
        let steps = [0., 0.02, 0.05, 0.1, 0.25, 0.5, 0.75, 0.9, 1.];
        for r in steps {
            for g in steps {
                for b in steps {
                    let back = Oklch::from_srgb([r, g, b]).to_srgb();
                    for (x, y) in [r, g, b].into_iter().zip(back) {
                        assert!((x - y).abs() < 2e-4, "{r} {g} {b} -> {back:?}");
                    }
                }
            }
        }
        // Every 8-bit gray comes back as itself.
        for v in 0..=255u32 {
            let back = Oklch::from_hex(v * 0x010101).to_srgb_in_gamut();
            assert!(back.iter().all(|c| (c * 255.).round() as u32 == v));
        }
    }

    #[test]
    fn matches_known_colors() {
        let close = |a: f32, b: f32, tolerance: f32| (a - b).abs() < tolerance;
        let white = Oklch::from_srgb([1., 1., 1.]);
        assert!(close(white.l, 1., 1e-4) && close(white.c, 0., 1e-4));
        let black = Oklch::from_srgb([0., 0., 0.]);
        assert!(close(black.l, 0., 1e-6) && close(black.c, 0., 1e-6));
        // Reference values of sRGB red, green and blue in OKLCH.
        for (rgb, l, c, h) in [
            ([1., 0., 0.], 0.627_955, 0.257_683, 29.234),
            ([0., 1., 0.], 0.866_440, 0.294_827, 142.495),
            ([0., 0., 1.], 0.452_014, 0.313_214, 264.052),
        ] {
            let color = Oklch::from_srgb(rgb);
            assert!(close(color.l, l, 1e-3), "{color:?}");
            assert!(close(color.c, c, 1e-3), "{color:?}");
            assert!(close(color.h, h, 0.1), "{color:?}");
        }
    }

    #[test]
    fn what_falls_outside_keeps_lightness_and_hue() {
        let wild = Oklch::new(0.7, 0.4, 145.);
        assert!(!wild.in_gamut());
        let fitted = Oklch::from_srgb(wild.to_srgb_in_gamut());
        assert!((fitted.l - 0.7).abs() < 2e-3, "{fitted:?}");
        assert!((fitted.h - 145.).abs() < 1., "{fitted:?}");
        assert!(fitted.c < 0.4);
        let white = Oklch::new(1.2, 0., 0.).to_srgb_in_gamut();
        assert!(white.iter().all(|v| (v - 1.).abs() < 1e-4), "{white:?}");
    }
}
