//! The panel's colours, taken from the game on screen, so Guide looks like
//! part of whatever is being played (PRD 6.7).
//!
//! They come from the small blurred picture the ambient backdrop makes of
//! the screens. The glass is ChatGPT's dark with a little of the picture's
//! average colour in it; the accent is the picture's most prominent
//! saturated colour, lightened until it reads on the glass. Text stays
//! near-white, so it reads on any game. Dark, white or colourless pictures
//! (fades, loading screens) give no palette, and the panel keeps the one it
//! has.

use gpui::{Hsla, Rgba};

/// Red, green and blue, from 0 to 1, in sRGB.
pub type Rgb = [f32; 3];

/// ChatGPT's dark panel.
const BASE: Rgb = rgb8(0x12, 0x12, 0x12);
/// How much of the picture's average colour tints the glass.
const TINT: f32 = 0.08;
/// Near-white text, then muted and faint.
const TEXT: Rgb = rgb8(0xED, 0xED, 0xF0);
const MUTED: Rgb = rgb8(0xA1, 0xA1, 0xAA);
const FAINT: Rgb = rgb8(0x71, 0x71, 0x7A);
/// How far text leans towards the accent.
const LEAN: f32 = 0.04;
/// Imago's coral, for errors.
const CORAL: Rgb = rgb8(0xFC, 0x61, 0x46);

/// The contrast each colour keeps against the glass.
pub const TEXT_CONTRAST: f32 = 7.0;
/// For muted text and the accent.
pub const MUTED_CONTRAST: f32 = 4.5;
/// For faint text.
pub const FAINT_CONTRAST: f32 = 3.0;

const fn rgb8(red: u8, green: u8, blue: u8) -> Rgb {
    [
        red as f32 / 255.0,
        green as f32 / 255.0,
        blue as f32 / 255.0,
    ]
}

/// The panel's colours.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// The panel itself.
    pub glass: Rgb,
    /// The Screenshot button, a fresh thumbnail's edge, inline code.
    pub accent: Rgb,
    /// Body text.
    pub text: Rgb,
    /// Secondary text: the thinking line, captions.
    pub muted: Rgb,
    /// The least: fine print, placeholders.
    pub faint: Rgb,
    /// Errors.
    pub danger: Rgb,
}

impl Palette {
    /// The palette for a game that has drawn nothing yet, or never shows
    /// colour: from its system's colour, `tint`, as `0xRRGGBB`.
    pub fn from_tint(tint: u32) -> Self {
        let [_, red, green, blue] = tint.to_be_bytes();
        Self::from_parts(BASE, rgb8(red, green, blue))
    }

    /// The palette of a backdrop picture's `colors` (0 to 255 each), or
    /// `None` if the picture is too dark, too light or too grey to say:
    /// keep the palette there is.
    pub fn from_picture(colors: &[[f32; 3]]) -> Option<Self> {
        if colors.is_empty() {
            return None;
        }
        let pixels: Vec<Rgb> = colors
            .iter()
            .map(|[r, g, b]| [r / 255.0, g / 255.0, b / 255.0])
            .collect();
        let count = pixels.len() as f32;
        let average = pixels.iter().fold([0.0; 3], |sum, pixel| {
            [sum[0] + pixel[0], sum[1] + pixel[1], sum[2] + pixel[2]]
        });
        let average = average.map(|channel| channel / count);
        let lightness = to_oklab(average)[0];
        if !(0.08..=0.92).contains(&lightness) {
            return None;
        }
        let accent = prominent(&pixels)?;
        let glass = mix(BASE, average, TINT);
        Some(Self::from_parts(glass, accent))
    }

    /// The palette with `glass`, and an accent from `seed`.
    fn from_parts(glass: Rgb, seed: Rgb) -> Self {
        let accent = readable(seed, glass, MUTED_CONTRAST);
        let lean = |colour: Rgb, needs: f32| {
            let leaned = mix(colour, accent, LEAN);
            if contrast(leaned, glass) >= needs {
                leaned
            } else {
                colour
            }
        };
        Self {
            glass,
            accent,
            text: lean(TEXT, TEXT_CONTRAST),
            muted: lean(MUTED, MUTED_CONTRAST),
            faint: lean(FAINT, FAINT_CONTRAST),
            danger: danger(accent, glass),
        }
    }

    /// The way from `self` to `other`, `t` of the way along (0 to 1).
    pub fn mix(&self, other: &Self, t: f32) -> Self {
        if t <= 0.0 {
            return *self;
        }
        if t >= 1.0 {
            return *other;
        }
        Self {
            glass: mix(self.glass, other.glass, t),
            accent: mix(self.accent, other.accent, t),
            text: mix(self.text, other.text, t),
            muted: mix(self.muted, other.muted, t),
            faint: mix(self.faint, other.faint, t),
            danger: mix(self.danger, other.danger, t),
        }
    }
}

/// `colour` with `alpha`, for GPUI.
pub fn hsla(colour: Rgb, alpha: f32) -> Hsla {
    Rgba {
        r: colour[0],
        g: colour[1],
        b: colour[2],
        a: alpha,
    }
    .into()
}

/// Errors: Imago's coral, unless the accent is itself red; then the
/// coral's lightness and chroma at the accent's hue turned 30°, so an
/// error still stands out in a red game.
fn danger(accent: Rgb, glass: Rgb) -> Rgb {
    let coral = to_oklch(CORAL);
    let accent_hue = to_oklch(accent)[2];
    let near = hue_distance(coral[2], accent_hue) < 30.0 && to_oklch(accent)[1] > 0.05;
    let colour = if near {
        from_oklch([coral[0], coral[1], accent_hue + 30.0])
    } else {
        CORAL
    };
    readable(colour, glass, MUTED_CONTRAST)
}

/// The picture's most prominent saturated colour: pixels are grouped by
/// hue, each counting by its chroma, and the heaviest group's average is
/// taken. `None` if nothing has colour enough.
fn prominent(pixels: &[Rgb]) -> Option<Rgb> {
    const BINS: usize = 12;
    let mut weights = [0.0f32; BINS];
    let mut sums = [[0.0f32; 3]; BINS];
    for &pixel in pixels {
        let lab = to_oklab(pixel);
        let [lightness, chroma, hue] = to_oklch(pixel);
        if chroma < 0.04 || !(0.15..=0.95).contains(&lightness) {
            continue;
        }
        let bin = ((hue.rem_euclid(360.0) / 360.0 * BINS as f32) as usize).min(BINS - 1);
        weights[bin] += chroma;
        for (sum, value) in sums[bin].iter_mut().zip(lab) {
            *sum += value * chroma;
        }
    }
    let (bin, weight) = weights
        .iter()
        .copied()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    // Enough colour to matter: as if a twentieth of the picture were
    // moderately saturated.
    if weight < pixels.len() as f32 * 0.05 * 0.08 {
        return None;
    }
    let lab = sums[bin].map(|sum| sum / weight);
    Some(from_oklab(lab))
}

/// `colour` made light enough to keep `needs` contrast against `glass`,
/// keeping its hue, and its chroma as far as sRGB allows.
fn readable(colour: Rgb, glass: Rgb, needs: f32) -> Rgb {
    let [mut lightness, chroma, hue] = to_oklch(colour);
    lightness = lightness.max(0.55);
    let mut candidate = in_gamut([lightness, chroma, hue]);
    while contrast(candidate, glass) < needs && lightness < 1.0 {
        lightness = (lightness + 0.02).min(1.0);
        candidate = in_gamut([lightness, chroma, hue]);
    }
    candidate
}

/// The sRGB colour of `lch`, its chroma lowered until it fits.
fn in_gamut(lch: [f32; 3]) -> Rgb {
    let [lightness, mut chroma, hue] = lch;
    loop {
        let colour = from_oklch_unclamped([lightness, chroma, hue]);
        if colour.iter().all(|c| (-0.001..=1.001).contains(c)) || chroma <= 0.0 {
            return colour.map(|c| c.clamp(0.0, 1.0));
        }
        chroma -= 0.005;
    }
}

fn hue_distance(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}

/// Mixes `b` into `a`, `t` of the way.
pub fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// WCAG's contrast ratio between two colours.
pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (light, dark) = if la > lb { (la, lb) } else { (lb, la) };
    (light + 0.05) / (dark + 0.05)
}

fn luminance(colour: Rgb) -> f32 {
    let [r, g, b] = colour.map(linear);
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

fn linear(channel: f32) -> f32 {
    if channel <= 0.040_45 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

fn gamma(channel: f32) -> f32 {
    if channel <= 0.003_130_8 {
        channel * 12.92
    } else {
        1.055 * channel.powf(1.0 / 2.4) - 0.055
    }
}

fn to_oklab(colour: Rgb) -> [f32; 3] {
    let [r, g, b] = colour.map(linear);
    let l = (0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

fn from_oklab_unclamped(lab: [f32; 3]) -> Rgb {
    let [lightness, a, b] = lab;
    let l = (lightness + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let m = (lightness - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
    let s = (lightness - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
    let r = 4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s;
    let g = -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s;
    let b = -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s;
    // Out-of-gamut channels stay out of 0 to 1, for `in_gamut` to see.
    [r, g, b].map(|c| if c < 0.0 { c } else { gamma(c) })
}

fn from_oklab(lab: [f32; 3]) -> Rgb {
    from_oklab_unclamped(lab).map(|c| c.clamp(0.0, 1.0))
}

fn to_oklch(colour: Rgb) -> [f32; 3] {
    let [lightness, a, b] = to_oklab(colour);
    [
        lightness,
        a.hypot(b),
        b.atan2(a).to_degrees().rem_euclid(360.0),
    ]
}

fn from_oklch_unclamped(lch: [f32; 3]) -> Rgb {
    let [lightness, chroma, hue] = lch;
    let radians = hue.to_radians();
    from_oklab_unclamped([lightness, chroma * radians.cos(), chroma * radians.sin()])
}

fn from_oklch(lch: [f32; 3]) -> Rgb {
    in_gamut(lch)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture of `size` pixels, each `colour` (0 to 255).
    fn flat(colour: [f32; 3], size: usize) -> Vec<[f32; 3]> {
        vec![colour; size]
    }

    fn assert_readable(palette: &Palette) {
        let glass = palette.glass;
        assert!(
            contrast(palette.text, glass) >= TEXT_CONTRAST,
            "{palette:?}"
        );
        assert!(
            contrast(palette.muted, glass) >= MUTED_CONTRAST,
            "{palette:?}"
        );
        assert!(
            contrast(palette.faint, glass) >= FAINT_CONTRAST,
            "{palette:?}"
        );
        assert!(
            contrast(palette.accent, glass) >= MUTED_CONTRAST,
            "{palette:?}"
        );
        assert!(
            contrast(palette.danger, glass) >= MUTED_CONTRAST,
            "{palette:?}"
        );
    }

    #[test]
    fn colour_round_trips_through_oklab() {
        for colour in [
            [1.0, 0.0, 0.0],
            [0.2, 0.6, 0.3],
            [0.5, 0.5, 0.5],
            [0.0, 0.0, 1.0],
        ] {
            let back = from_oklab(to_oklab(colour));
            for (a, b) in colour.iter().zip(back) {
                assert!((a - b).abs() < 0.01, "{colour:?} came back {back:?}");
            }
        }
    }

    #[test]
    fn a_green_game_gets_a_green_accent_that_reads() {
        // Omega Ruby's grass: mostly green with some sky.
        let mut picture = flat([70.0, 150.0, 60.0], 300);
        picture.extend(flat([120.0, 170.0, 220.0], 100));
        let palette = Palette::from_picture(&picture).expect("colourful");
        let [_, chroma, hue] = to_oklch(palette.accent);
        assert!((100.0..=160.0).contains(&hue), "green, not {hue}");
        assert!(chroma > 0.05);
        assert_readable(&palette);
        // The glass is tinted, barely.
        assert!(palette.glass[1] > palette.glass[0]);
        assert!(contrast(palette.glass, BASE) < 1.15);
    }

    #[test]
    fn extremes_keep_every_pairing_readable() {
        for colour in [
            [255.0, 0.0, 0.0],
            [0.0, 0.0, 255.0],
            // Pure yellow is as light as a white flash, so it keeps the
            // last palette; a game's yellow is a little deeper.
            [230.0, 190.0, 20.0],
            [0.0, 255.0, 255.0],
            [200.0, 30.0, 160.0],
        ] {
            let palette = Palette::from_picture(&flat(colour, 64)).expect("colourful");
            assert_readable(&palette);
        }
        for platform_tint in [
            0x004F_AE4C,
            0x00E0_8506,
            0x001F_9FE8,
            0x00EC_4A2E,
            0x00C8_3314,
        ] {
            assert_readable(&Palette::from_tint(platform_tint));
        }
    }

    #[test]
    fn a_red_game_gets_errors_in_another_hue() {
        let palette = Palette::from_picture(&flat([220.0, 30.0, 30.0], 64)).expect("red");
        let accent = to_oklch(palette.accent)[2];
        let danger = to_oklch(palette.danger)[2];
        assert!(hue_distance(accent, danger) >= 20.0, "{accent} vs {danger}");
        let green = Palette::from_picture(&flat([40.0, 160.0, 60.0], 64)).expect("green");
        assert_eq!(green.danger, readable(CORAL, green.glass, MUTED_CONTRAST));
    }

    #[test]
    fn fades_white_frames_and_grey_pictures_keep_the_palette_there_is() {
        assert_eq!(Palette::from_picture(&flat([0.0, 0.0, 0.0], 64)), None);
        assert_eq!(
            Palette::from_picture(&flat([255.0, 255.0, 255.0], 64)),
            None
        );
        assert_eq!(
            Palette::from_picture(&flat([128.0, 128.0, 128.0], 64)),
            None
        );
        assert_eq!(Palette::from_picture(&[]), None);
        // A Game Boy's two greys.
        let mut game_boy = flat([60.0, 60.0, 60.0], 32);
        game_boy.extend(flat([200.0, 200.0, 200.0], 32));
        assert_eq!(Palette::from_picture(&game_boy), None);
    }

    #[test]
    fn palettes_ease_from_one_to_the_next() {
        let a = Palette::from_tint(0x004F_AE4C);
        let b = Palette::from_tint(0x001F_9FE8);
        assert_eq!(a.mix(&b, 0.0), a);
        assert_eq!(a.mix(&b, 1.0), b);
        let half = a.mix(&b, 0.5);
        assert!((half.accent[2] - (a.accent[2] + b.accent[2]) / 2.0).abs() < 1e-6);
    }
}
