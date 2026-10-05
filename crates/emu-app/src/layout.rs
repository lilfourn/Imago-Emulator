//! Where each emulated screen goes in the window, and the inverse mapping
//! from a window point back to a screen pixel for the touchscreen.
//!
//! Pure geometry in logical pixels, independent of GPUI.

use emu_core::{ScreenSize, TouchPoint};

/// How multiple screens are arranged. Single-screen systems ignore this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Arrangement {
    /// Stacked top to bottom, like the hardware.
    #[default]
    Vertical,
    /// Side by side.
    Horizontal,
    /// Only the first screen in order; swap to see the other.
    Single,
}

/// The user's screen layout preferences.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Layout {
    /// How screens are arranged.
    pub arrangement: Arrangement,
    /// Reverses the screen order (or, with [`Arrangement::Single`], shows
    /// the other screen).
    pub swapped: bool,
    /// Space between screens, in emulated pixels.
    pub gap: f32,
}

/// An axis-aligned rectangle in logical window pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

impl Rect {
    /// Whether `point` lies inside.
    pub fn contains(&self, point: (f32, f32)) -> bool {
        (self.x..self.x + self.width).contains(&point.0)
            && (self.y..self.y + self.height).contains(&point.1)
    }
}

/// One screen's position in the window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// Index into the core's screens.
    pub screen: usize,
    /// Where it is drawn.
    pub rect: Rect,
}

impl Layout {
    /// Places `screens` in a `viewport` of logical pixels, scaled uniformly
    /// to fit and centred.
    pub fn place(&self, screens: &[ScreenSize], viewport: (f32, f32)) -> Vec<Placement> {
        let mut order: Vec<usize> = (0..screens.len()).collect();
        if self.swapped {
            order.reverse();
        }
        let arrangement = match (self.arrangement, order.len()) {
            (_, 0 | 1) | (Arrangement::Single, _) => {
                order.truncate(1);
                Arrangement::Single
            }
            (arrangement, _) => arrangement,
        };

        // Lay screens out in emulated pixels, then scale the whole group.
        let mut cursor = 0.0;
        let mut extent = (0.0f32, 0.0f32);
        let mut unscaled = Vec::with_capacity(order.len());
        for &index in &order {
            let size = screens[index];
            let (width, height) = (f32::from(size.width), f32::from(size.height));
            let (x, y) = match arrangement {
                Arrangement::Horizontal => (cursor, 0.0),
                Arrangement::Vertical | Arrangement::Single => (0.0, cursor),
            };
            cursor += match arrangement {
                Arrangement::Horizontal => width + self.gap,
                Arrangement::Vertical | Arrangement::Single => height + self.gap,
            };
            extent = (extent.0.max(x + width), extent.1.max(y + height));
            unscaled.push((index, x, y, width, height));
        }

        if extent.0 <= 0.0 || extent.1 <= 0.0 {
            return Vec::new();
        }
        let scale = (viewport.0 / extent.0).min(viewport.1 / extent.1).max(0.0);
        let origin = (
            (viewport.0 - extent.0 * scale) / 2.0,
            (viewport.1 - extent.1 * scale) / 2.0,
        );
        unscaled
            .into_iter()
            .map(|(screen, x, y, width, height)| {
                // Centre each screen across the group's other axis.
                let (x, y) = match arrangement {
                    Arrangement::Horizontal => (x, y + (extent.1 - height) / 2.0),
                    Arrangement::Vertical | Arrangement::Single => {
                        (x + (extent.0 - width) / 2.0, y)
                    }
                };
                Placement {
                    screen,
                    rect: Rect {
                        x: origin.0 + x * scale,
                        y: origin.1 + y * scale,
                        width: width * scale,
                        height: height * scale,
                    },
                }
            })
            .collect()
    }
}

/// The pixel of a `size` screen drawn at `rect` that lies under `point`, if
/// any.
pub fn pixel_at(rect: Rect, size: ScreenSize, point: (f32, f32)) -> Option<TouchPoint> {
    let u = (point.0 - rect.x) / rect.width;
    let v = (point.1 - rect.y) / rect.height;
    if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
        return None;
    }
    // `u` and `v` are in [0, 1), so the products fit in u16; `as` saturates
    // regardless.
    Some(TouchPoint {
        x: (u * f32::from(size.width)) as u16,
        y: (v * f32::from(size.height)) as u16,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DS: [ScreenSize; 2] = [ScreenSize::new(256, 192), ScreenSize::new(256, 192)];

    fn rects(layout: Layout, screens: &[ScreenSize], viewport: (f32, f32)) -> Vec<(usize, Rect)> {
        layout
            .place(screens, viewport)
            .into_iter()
            .map(|placement| (placement.screen, placement.rect))
            .collect()
    }

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn vertical_ds_fills_a_matching_window() {
        let placed = rects(Layout::default(), &DS, (512.0, 768.0));
        assert_eq!(
            placed,
            [
                (0, rect(0.0, 0.0, 512.0, 384.0)),
                (1, rect(0.0, 384.0, 512.0, 384.0))
            ]
        );
    }

    #[test]
    fn horizontal_ds_is_centred_vertically() {
        let layout = Layout {
            arrangement: Arrangement::Horizontal,
            ..Layout::default()
        };
        let placed = rects(layout, &DS, (512.0, 400.0));
        assert_eq!(
            placed,
            [
                (0, rect(0.0, 104.0, 256.0, 192.0)),
                (1, rect(256.0, 104.0, 256.0, 192.0))
            ]
        );
    }

    #[test]
    fn gap_is_scaled_with_the_screens() {
        let layout = Layout {
            gap: 16.0,
            ..Layout::default()
        };
        let placed = rects(layout, &DS, (256.0, 400.0));
        assert_eq!(placed[1].1.y, 208.0);
    }

    #[test]
    fn swapped_single_shows_the_bottom_screen() {
        let layout = Layout {
            arrangement: Arrangement::Single,
            swapped: true,
            gap: 0.0,
        };
        let placed = rects(layout, &DS, (256.0, 192.0));
        assert_eq!(placed, [(1, rect(0.0, 0.0, 256.0, 192.0))]);
    }

    #[test]
    fn one_screen_is_letterboxed() {
        let gba = [ScreenSize::new(240, 160)];
        let placed = rects(Layout::default(), &gba, (480.0, 480.0));
        assert_eq!(placed, [(0, rect(0.0, 80.0, 480.0, 320.0))]);
    }

    #[test]
    fn pixel_at_inverts_placement() {
        let bottom = rect(0.0, 384.0, 512.0, 384.0);
        let size = DS[1];
        assert_eq!(
            pixel_at(bottom, size, (0.0, 384.0)),
            Some(TouchPoint { x: 0, y: 0 })
        );
        assert_eq!(
            pixel_at(bottom, size, (511.9, 767.9)),
            Some(TouchPoint { x: 255, y: 191 })
        );
        assert_eq!(pixel_at(bottom, size, (100.0, 383.0)), None);
        assert_eq!(pixel_at(bottom, size, (512.0, 500.0)), None);
    }
}
