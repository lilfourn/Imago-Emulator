//! Where each emulated screen goes in the window, and the inverse mapping
//! from a window point back to a screen pixel for the touchscreen.
//!
//! Pure geometry in logical pixels, independent of GPUI. Screens are first
//! arranged upright in emulated pixels; then the whole group is turned,
//! scaled uniformly to fit the window and centred.

use emu_core::{ScreenSize, TouchPoint};

/// About the distance between the real DS screens across the hinge, in
/// emulated pixels.
pub const HINGE_GAP: f32 = 90.0;

/// Space between consoles shown side by side, in emulated pixels.
pub const CONSOLE_GAP: f32 = 16.0;

/// Gaps offered between screens, in emulated pixels.
pub const GAPS: [f32; 4] = [0.0, 16.0, 48.0, HINGE_GAP];

/// The gap in [`GAPS`] after `gap`, wrapping round to the first.
pub fn next_gap(gap: f32) -> f32 {
    GAPS.into_iter()
        .find(|&preset| preset > gap)
        .unwrap_or(GAPS[0])
}

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
    /// The first screen in order large on the left, with every screen small
    /// in a column on its right, as in melonDS. Swap to enlarge the other.
    Hybrid,
}

/// How far the whole arrangement is turned clockwise, for games played
/// with the console held like a book.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Rotation {
    /// Not turned.
    #[default]
    Upright,
    /// A quarter turn clockwise.
    Clockwise,
    /// A half turn.
    UpsideDown,
    /// A quarter turn anticlockwise.
    Anticlockwise,
}

impl Rotation {
    /// Every rotation, a quarter turn clockwise apart.
    pub const ALL: [Self; 4] = [
        Self::Upright,
        Self::Clockwise,
        Self::UpsideDown,
        Self::Anticlockwise,
    ];

    /// The angle clockwise, in degrees.
    pub fn degrees(self) -> u16 {
        match self {
            Self::Upright => 0,
            Self::Clockwise => 90,
            Self::UpsideDown => 180,
            Self::Anticlockwise => 270,
        }
    }

    /// The rotation `degrees` clockwise: 0, 90, 180 or 270.
    pub fn from_degrees(degrees: u16) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|rotation| rotation.degrees() == degrees)
    }

    /// This rotation and a further quarter turn clockwise.
    pub fn clockwise(self) -> Self {
        Self::from_degrees((self.degrees() + 90) % 360).unwrap_or_default()
    }

    /// This rotation and a further quarter turn anticlockwise.
    pub fn anticlockwise(self) -> Self {
        Self::from_degrees((self.degrees() + 270) % 360).unwrap_or_default()
    }

    /// Whether width and height trade places.
    fn swaps_axes(self) -> bool {
        matches!(self, Self::Clockwise | Self::Anticlockwise)
    }

    /// The size of a `size` screen once turned.
    pub fn turn_size(self, size: ScreenSize) -> ScreenSize {
        if self.swaps_axes() {
            ScreenSize::new(size.height, size.width)
        } else {
            size
        }
    }

    /// The pixel of an upright `size` screen that shows at `column` and
    /// `row` once the screen is turned.
    pub fn source_pixel(self, size: ScreenSize, column: u16, row: u16) -> (u16, u16) {
        let (width, height) = (size.width, size.height);
        match self {
            Self::Upright => (column, row),
            Self::Clockwise => (row, height - 1 - column),
            Self::UpsideDown => (width - 1 - column, height - 1 - row),
            Self::Anticlockwise => (width - 1 - row, column),
        }
    }

    /// A movement `(dx, dy)` seen on the turned screen, as a movement on
    /// the upright one.
    pub fn unturn_vector(self, (dx, dy): (f32, f32)) -> (f32, f32) {
        match self {
            Self::Upright => (dx, dy),
            Self::Clockwise => (dy, -dx),
            Self::UpsideDown => (-dx, -dy),
            Self::Anticlockwise => (-dy, dx),
        }
    }

    /// The point a fraction `(s, t)` across and down an upright screen, as a
    /// fraction across and down the turned screen.
    fn turn_fraction(self, (s, t): (f32, f32)) -> (f32, f32) {
        match self {
            Self::Upright => (s, t),
            Self::Clockwise => (1.0 - t, s),
            Self::UpsideDown => (1.0 - s, 1.0 - t),
            Self::Anticlockwise => (t, 1.0 - s),
        }
    }

    /// `rect`, inside an upright group of size `extent` with its top-left
    /// corner at the origin, once the group is turned in place.
    fn turn_rect(self, rect: Rect, extent: (f32, f32)) -> Rect {
        let (width, height) = extent;
        match self {
            Self::Upright => rect,
            Self::Clockwise => Rect {
                x: height - rect.y - rect.height,
                y: rect.x,
                width: rect.height,
                height: rect.width,
            },
            Self::UpsideDown => Rect {
                x: width - rect.x - rect.width,
                y: height - rect.y - rect.height,
                ..rect
            },
            Self::Anticlockwise => Rect {
                x: rect.y,
                y: width - rect.x - rect.width,
                width: rect.height,
                height: rect.width,
            },
        }
    }
}

/// The user's screen layout preferences.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Layout {
    /// How screens are arranged.
    pub arrangement: Arrangement,
    /// Reverses the screen order: with [`Arrangement::Single`] shows the
    /// other screen, with [`Arrangement::Hybrid`] enlarges it.
    pub swapped: bool,
    /// How far the whole arrangement is turned.
    pub rotation: Rotation,
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

    /// The rectangle with its edges moved to the nearest device pixel, on
    /// a display of `scale_factor` device pixels to a logical one. An
    /// image as many device pixels across is then drawn one to one, not
    /// resampled half a pixel off.
    pub fn snapped(self, scale_factor: f32) -> Self {
        if scale_factor.is_nan() || scale_factor <= 0.0 {
            return self;
        }
        let snap = |edge: f32| (edge * scale_factor).round() / scale_factor;
        let (left, top) = (snap(self.x), snap(self.y));
        Self {
            x: left,
            y: top,
            width: snap(self.x + self.width) - left,
            height: snap(self.y + self.height) - top,
        }
    }
}

/// One screen's position in the window. A screen can be placed more than
/// once, as in [`Arrangement::Hybrid`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// Index into the core's screens.
    pub screen: usize,
    /// Where it is drawn.
    pub rect: Rect,
    /// How far its picture is turned inside `rect`.
    pub rotation: Rotation,
}

impl Placement {
    /// The pixel of a `size` screen that lies under window `point`, if
    /// any.
    pub fn pixel_at(&self, size: ScreenSize, point: (f32, f32)) -> Option<TouchPoint> {
        let u = (point.0 - self.rect.x) / self.rect.width;
        let v = (point.1 - self.rect.y) / self.rect.height;
        if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
            return None;
        }
        // The pixel of the turned picture, then the upright one it shows.
        // `u` and `v` are in [0, 1), so the products fit in u16; `as`
        // saturates regardless, and `min` guards against rounding up.
        let shown = self.rotation.turn_size(size);
        let column = ((u * f32::from(shown.width)) as u16).min(shown.width - 1);
        let row = ((v * f32::from(shown.height)) as u16).min(shown.height - 1);
        let (x, y) = self.rotation.source_pixel(size, column, row);
        Some(TouchPoint { x, y })
    }

    /// The pixel of a `size` screen nearest window `point`: the one under
    /// it, or for a point outside, the one at the nearest edge. `None`
    /// only if the placement has no area.
    pub fn pixel_nearest(&self, size: ScreenSize, point: (f32, f32)) -> Option<TouchPoint> {
        let rect = self.rect;
        // Just inside the far edges, which belong to the next pixel over.
        let inside = (
            point.0.clamp(rect.x, rect.x + rect.width * 0.999),
            point.1.clamp(rect.y, rect.y + rect.height * 0.999),
        );
        self.pixel_at(size, inside)
    }

    /// Where `point`, in pixels from the top-left corner of an upright
    /// `size` screen, is drawn in the window.
    pub fn window_point(&self, size: ScreenSize, point: (f32, f32)) -> (f32, f32) {
        let fraction = (
            point.0 / f32::from(size.width),
            point.1 / f32::from(size.height),
        );
        let (u, v) = self.rotation.turn_fraction(fraction);
        (
            self.rect.x + u * self.rect.width,
            self.rect.y + v * self.rect.height,
        )
    }
}

/// The largest size with the proportions of `size` that fits in `area`.
pub fn fit(size: (f32, f32), area: (f32, f32)) -> (f32, f32) {
    let scale = (area.0 / size.0).min(area.1 / size.1).max(0.0);
    (size.0 * scale, size.1 * scale)
}

impl Layout {
    /// Places `screens` in a `viewport` of logical pixels, turned, scaled
    /// uniformly to fit and centred.
    #[cfg(test)]
    pub fn place(&self, screens: &[ScreenSize], viewport: (f32, f32)) -> Vec<Placement> {
        self.place_consoles(screens, 1, viewport)
    }

    /// Places the screens of `consoles` consoles, which divide `screens`
    /// evenly in order, as [`Layout::place`] does: each console's screens
    /// arranged by this layout, and the consoles side by side, first on
    /// the left, [`CONSOLE_GAP`] apart. Two linked DSes show each DS
    /// stacked (by default) next to the other.
    pub fn place_consoles(
        &self,
        screens: &[ScreenSize],
        consoles: usize,
        viewport: (f32, f32),
    ) -> Vec<Placement> {
        let per_console = screens.len().div_ceil(consoles.max(1)).max(1);
        let mut upright = Vec::new();
        let mut left = 0.0;
        for (console, group) in screens.chunks(per_console).enumerate() {
            let arranged = self.arrange(group);
            let width = extent(&arranged).0;
            upright.extend(arranged.into_iter().map(|(screen, rect)| {
                let rect = Rect {
                    x: rect.x + left,
                    ..rect
                };
                (screen + console * per_console, rect)
            }));
            left += width + CONSOLE_GAP;
        }
        let extent = extent(&upright);
        if extent.0 <= 0.0 || extent.1 <= 0.0 {
            return Vec::new();
        }
        let turned = if self.rotation.swaps_axes() {
            (extent.1, extent.0)
        } else {
            extent
        };
        let scale = (viewport.0 / turned.0).min(viewport.1 / turned.1).max(0.0);
        let origin = (
            (viewport.0 - turned.0 * scale) / 2.0,
            (viewport.1 - turned.1 * scale) / 2.0,
        );
        upright
            .into_iter()
            .map(|(screen, rect)| {
                let rect = self.rotation.turn_rect(rect, extent);
                Placement {
                    screen,
                    rect: Rect {
                        x: origin.0 + rect.x * scale,
                        y: origin.1 + rect.y * scale,
                        width: rect.width * scale,
                        height: rect.height * scale,
                    },
                    rotation: self.rotation,
                }
            })
            .collect()
    }

    /// Each placed screen's rectangle in emulated pixels, upright and
    /// unscaled, with the group's top-left corner at the origin.
    fn arrange(&self, screens: &[ScreenSize]) -> Vec<(usize, Rect)> {
        let mut order: Vec<usize> = (0..screens.len()).collect();
        if self.swapped {
            order.reverse();
        }
        match (self.arrangement, order.as_slice()) {
            (_, []) => Vec::new(),
            (_, [only]) | (Arrangement::Single, [only, ..]) => {
                line(screens, &[*only], Direction::Down, 0.0)
            }
            (Arrangement::Vertical, _) => line(screens, &order, Direction::Down, self.gap),
            (Arrangement::Horizontal, _) => line(screens, &order, Direction::Across, self.gap),
            (Arrangement::Hybrid, [large, ..]) => hybrid(screens, *large, self.gap),
        }
    }
}

/// Which way [`line`] lays screens out.
#[derive(Debug, Clone, Copy)]
enum Direction {
    /// Top to bottom.
    Down,
    /// Left to right.
    Across,
}

/// The screens in `order` one after another with `gap` between them, each
/// centred across the line.
fn line(
    screens: &[ScreenSize],
    order: &[usize],
    direction: Direction,
    gap: f32,
) -> Vec<(usize, Rect)> {
    let size = |index: usize| {
        let size = screens[index];
        (f32::from(size.width), f32::from(size.height))
    };
    let thickness = order
        .iter()
        .map(|&index| match direction {
            Direction::Down => size(index).0,
            Direction::Across => size(index).1,
        })
        .fold(0.0, f32::max);
    let mut cursor = 0.0;
    order
        .iter()
        .map(|&index| {
            let (width, height) = size(index);
            let rect = match direction {
                Direction::Down => Rect {
                    x: (thickness - width) / 2.0,
                    y: cursor,
                    width,
                    height,
                },
                Direction::Across => Rect {
                    x: cursor,
                    y: (thickness - height) / 2.0,
                    width,
                    height,
                },
            };
            cursor += gap
                + match direction {
                    Direction::Down => height,
                    Direction::Across => width,
                };
            (index, rect)
        })
        .collect()
}

/// Screen `large`, enlarged to the height of a column of every screen at
/// its own size, with that column on its right.
fn hybrid(screens: &[ScreenSize], large: usize, gap: f32) -> Vec<(usize, Rect)> {
    let all: Vec<usize> = (0..screens.len()).collect();
    let column = line(screens, &all, Direction::Down, gap);
    let height = extent(&column).1;
    let size = screens[large];
    let width = f32::from(size.width) * height / f32::from(size.height);
    let large_rect = Rect {
        x: 0.0,
        y: 0.0,
        width,
        height,
    };
    let column = column.into_iter().map(|(index, rect)| {
        let rect = Rect {
            x: rect.x + width + gap,
            ..rect
        };
        (index, rect)
    });
    std::iter::once((large, large_rect)).chain(column).collect()
}

/// The size of the smallest box at the origin holding every rectangle.
fn extent(rects: &[(usize, Rect)]) -> (f32, f32) {
    rects
        .iter()
        .fold((0.0f32, 0.0f32), |(width, height), (_, rect)| {
            (
                width.max(rect.x + rect.width),
                height.max(rect.y + rect.height),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DS: [ScreenSize; 2] = [ScreenSize::new(256, 192), ScreenSize::new(256, 192)];

    const ARRANGEMENTS: [Arrangement; 4] = [
        Arrangement::Vertical,
        Arrangement::Horizontal,
        Arrangement::Single,
        Arrangement::Hybrid,
    ];

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

    fn placement(rect: Rect, rotation: Rotation) -> Placement {
        Placement {
            screen: 1,
            rect,
            rotation,
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
            ..Layout::default()
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
    fn hybrid_shows_the_top_screen_large_beside_both() {
        let layout = Layout {
            arrangement: Arrangement::Hybrid,
            ..Layout::default()
        };
        let placed = rects(layout, &DS, (768.0, 384.0));
        assert_eq!(
            placed,
            [
                (0, rect(0.0, 0.0, 512.0, 384.0)),
                (0, rect(512.0, 0.0, 256.0, 192.0)),
                (1, rect(512.0, 192.0, 256.0, 192.0))
            ]
        );
    }

    #[test]
    fn swapped_hybrid_enlarges_the_bottom_screen() {
        let layout = Layout {
            arrangement: Arrangement::Hybrid,
            swapped: true,
            ..Layout::default()
        };
        let placed = rects(layout, &DS, (768.0, 384.0));
        assert_eq!(placed[0], (1, rect(0.0, 0.0, 512.0, 384.0)));
        // The small column keeps the console's order.
        assert_eq!(placed[1].0, 0);
        assert_eq!(placed[2].0, 1);
    }

    #[test]
    fn hybrid_large_screen_matches_the_column_with_its_gap() {
        let layout = Layout {
            arrangement: Arrangement::Hybrid,
            gap: 96.0,
            ..Layout::default()
        };
        // Column: 192 + 96 + 192 = 480 tall, so the large screen is 2.5x.
        let placed = rects(layout, &DS, (640.0 + 96.0 + 256.0, 480.0));
        assert_eq!(
            placed,
            [
                (0, rect(0.0, 0.0, 640.0, 480.0)),
                (0, rect(736.0, 0.0, 256.0, 192.0)),
                (1, rect(736.0, 288.0, 256.0, 192.0))
            ]
        );
    }

    #[test]
    fn linked_consoles_sit_side_by_side_each_stacked() {
        let screens = [DS[0]; 4];
        let width = 2.0 * 256.0 + CONSOLE_GAP;
        let placed: Vec<(usize, Rect)> = Layout::default()
            .place_consoles(&screens, 2, (width, 384.0))
            .into_iter()
            .map(|placement| (placement.screen, placement.rect))
            .collect();
        let right = 256.0 + CONSOLE_GAP;
        assert_eq!(
            placed,
            [
                (0, rect(0.0, 0.0, 256.0, 192.0)),
                (1, rect(0.0, 192.0, 256.0, 192.0)),
                (2, rect(right, 0.0, 256.0, 192.0)),
                (3, rect(right, 192.0, 256.0, 192.0)),
            ]
        );
        // One console's worth is the plain layout.
        let one = Layout::default().place_consoles(&DS, 1, (256.0, 384.0));
        assert_eq!(one, Layout::default().place(&DS, (256.0, 384.0)));
    }

    #[test]
    fn single_screen_systems_ignore_hybrid() {
        let gba = [ScreenSize::new(240, 160)];
        let layout = Layout {
            arrangement: Arrangement::Hybrid,
            ..Layout::default()
        };
        let placed = rects(layout, &gba, (240.0, 160.0));
        assert_eq!(placed, [(0, rect(0.0, 0.0, 240.0, 160.0))]);
    }

    #[test]
    fn turning_a_stack_moves_the_top_screen() {
        let turned = |rotation| {
            let layout = Layout {
                rotation,
                ..Layout::default()
            };
            let viewport = if rotation == Rotation::UpsideDown {
                (256.0, 384.0)
            } else {
                (384.0, 256.0)
            };
            rects(layout, &DS, viewport)
        };
        // A quarter turn clockwise takes the top screen to the right.
        assert_eq!(
            turned(Rotation::Clockwise),
            [
                (0, rect(192.0, 0.0, 192.0, 256.0)),
                (1, rect(0.0, 0.0, 192.0, 256.0))
            ]
        );
        assert_eq!(
            turned(Rotation::UpsideDown),
            [
                (0, rect(0.0, 192.0, 256.0, 192.0)),
                (1, rect(0.0, 0.0, 256.0, 192.0))
            ]
        );
        assert_eq!(
            turned(Rotation::Anticlockwise),
            [
                (0, rect(0.0, 0.0, 192.0, 256.0)),
                (1, rect(192.0, 0.0, 192.0, 256.0))
            ]
        );
    }

    #[test]
    fn turned_layouts_fill_a_turned_window() {
        let layout = Layout {
            arrangement: Arrangement::Hybrid,
            rotation: Rotation::Anticlockwise,
            ..Layout::default()
        };
        let placed = rects(layout, &DS, (384.0, 768.0));
        let (width, height) = extent(&placed);
        assert_eq!((width, height), (384.0, 768.0));
    }

    #[test]
    fn default_stacks_the_screens_without_a_gap() {
        let layout = Layout::default();
        assert_eq!(layout.arrangement, Arrangement::Vertical);
        assert_eq!(layout.gap, 0.0);
    }

    #[test]
    fn fit_keeps_proportions() {
        assert_eq!(fit((256.0, 384.0), (1000.0, 900.0)), (600.0, 900.0));
        assert_eq!(fit((256.0, 384.0), (128.0, 900.0)), (128.0, 192.0));
    }

    #[test]
    fn rotations_compose() {
        for rotation in Rotation::ALL {
            assert_eq!(rotation.clockwise().anticlockwise(), rotation);
            assert_eq!(Rotation::from_degrees(rotation.degrees()), Some(rotation));
        }
        assert_eq!(Rotation::Anticlockwise.clockwise(), Rotation::Upright);
        assert_eq!(Rotation::Upright.anticlockwise(), Rotation::Anticlockwise);
        assert_eq!(Rotation::from_degrees(45), None);
    }

    #[test]
    fn pixel_at_inverts_placement() {
        let bottom = placement(rect(0.0, 384.0, 512.0, 384.0), Rotation::Upright);
        let size = DS[1];
        assert_eq!(
            bottom.pixel_at(size, (0.0, 384.0)),
            Some(TouchPoint { x: 0, y: 0 })
        );
        assert_eq!(
            bottom.pixel_at(size, (511.9, 767.9)),
            Some(TouchPoint { x: 255, y: 191 })
        );
        assert_eq!(bottom.pixel_at(size, (100.0, 383.0)), None);
        assert_eq!(bottom.pixel_at(size, (512.0, 500.0)), None);
    }

    #[test]
    fn a_3ds_touch_lands_on_its_narrower_bottom_screen() {
        // Stacked, the 320-wide bottom screen is centred under the 400-wide
        // top one, with a margin each side that is not the touch screen.
        let placed = Layout::default().place(&emu_3ds::SCREENS, (540.0, 648.0));
        assert_eq!(placed[0].rect, rect(0.0, 0.0, 540.0, 324.0));
        assert_eq!(placed[1].rect, rect(54.0, 324.0, 432.0, 324.0));
        let (bottom, size) = (placed[1], emu_3ds::SCREENS[1]);
        assert_eq!(
            bottom.pixel_at(size, (54.0, 324.0)),
            Some(TouchPoint { x: 0, y: 0 })
        );
        assert_eq!(
            bottom.pixel_at(size, (270.0, 486.0)),
            Some(TouchPoint { x: 160, y: 120 })
        );
        assert_eq!(
            bottom.pixel_at(size, (485.9, 647.9)),
            Some(TouchPoint { x: 319, y: 239 })
        );
        assert_eq!(bottom.pixel_at(size, (40.0, 500.0)), None, "in the margin");
        assert_eq!(
            bottom.pixel_nearest(size, (40.0, 500.0)),
            Some(TouchPoint { x: 0, y: 130 }),
            "a drag into the margin holds at the edge"
        );
        // Every pixel maps back to itself through the window.
        for (x, y) in [(0, 0), (88, 206), (319, 239), (160, 1)] {
            let point = bottom.window_point(size, (f32::from(x) + 0.5, f32::from(y) + 0.5));
            assert_eq!(bottom.pixel_at(size, point), Some(TouchPoint { x, y }));
        }
    }

    #[test]
    fn pixel_nearest_holds_points_outside_at_the_edge() {
        let bottom = placement(rect(0.0, 384.0, 512.0, 384.0), Rotation::Upright);
        let size = DS[1];
        assert_eq!(
            bottom.pixel_nearest(size, (100.0, 500.0)),
            bottom.pixel_at(size, (100.0, 500.0))
        );
        assert_eq!(
            bottom.pixel_nearest(size, (-40.0, 300.0)),
            Some(TouchPoint { x: 0, y: 0 })
        );
        assert_eq!(
            bottom.pixel_nearest(size, (900.0, 2000.0)),
            Some(TouchPoint { x: 255, y: 191 })
        );
    }

    #[test]
    fn pixel_at_finds_the_top_left_pixel_where_each_rotation_draws_it() {
        let size = DS[1];
        let corners = [
            (Rotation::Upright, (0.1, 0.1)),
            (Rotation::Clockwise, (191.9, 0.1)),
            (Rotation::UpsideDown, (255.9, 191.9)),
            (Rotation::Anticlockwise, (0.1, 255.9)),
        ];
        for (rotation, corner) in corners {
            let shown = rotation.turn_size(size);
            let rect = rect(0.0, 0.0, f32::from(shown.width), f32::from(shown.height));
            assert_eq!(
                placement(rect, rotation).pixel_at(size, corner),
                Some(TouchPoint { x: 0, y: 0 }),
                "{rotation:?}"
            );
        }
    }

    /// Every combination of layout settings, with and without the hinge
    /// gap.
    fn every_layout() -> Vec<Layout> {
        let mut layouts = Vec::new();
        for arrangement in ARRANGEMENTS {
            for rotation in Rotation::ALL {
                for swapped in [false, true] {
                    for gap in [0.0, HINGE_GAP] {
                        layouts.push(Layout {
                            arrangement,
                            swapped,
                            rotation,
                            gap,
                        });
                    }
                }
            }
        }
        layouts
    }

    /// Where `placement` draws the middle of pixel `(x, y)` of an upright
    /// `size` screen, worked out independently of [`Placement::pixel_at`].
    fn drawn_at(placement: &Placement, size: ScreenSize, (x, y): (u16, u16)) -> (f32, f32) {
        let (width, height) = (size.width, size.height);
        let (column, row) = match placement.rotation {
            Rotation::Upright => (x, y),
            Rotation::Clockwise => (height - 1 - y, x),
            Rotation::UpsideDown => (width - 1 - x, height - 1 - y),
            Rotation::Anticlockwise => (y, width - 1 - x),
        };
        let shown = placement.rotation.turn_size(size);
        let rect = placement.rect;
        (
            rect.x + (f32::from(column) + 0.5) * rect.width / f32::from(shown.width),
            rect.y + (f32::from(row) + 0.5) * rect.height / f32::from(shown.height),
        )
    }

    #[test]
    fn every_layout_touches_the_pixel_it_draws() {
        let size = DS[1];
        let samples = |length: u16| (0..length).step_by(5).chain([length - 1]);
        for layout in every_layout() {
            let placed = layout.place(&DS, (997.0, 613.0));
            for placement in placed.iter().filter(|placement| placement.screen == 1) {
                for x in samples(size.width) {
                    for y in samples(size.height) {
                        let point = drawn_at(placement, size, (x, y));
                        assert_eq!(
                            placement.pixel_at(size, point),
                            Some(TouchPoint { x, y }),
                            "{layout:?} {placement:?}"
                        );
                        // The gamepad cursor is drawn there too.
                        let centre = (f32::from(x) + 0.5, f32::from(y) + 0.5);
                        let mark = placement.window_point(size, centre);
                        let error = (mark.0 - point.0).abs() + (mark.1 - point.1).abs();
                        assert!(error < 1e-3, "{layout:?} {placement:?} {mark:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn unturned_vectors_move_the_same_way_on_screen() {
        let size = DS[1];
        let start = (100.0, 80.0);
        for rotation in Rotation::ALL {
            let shown = rotation.turn_size(size);
            let rect = rect(0.0, 0.0, f32::from(shown.width), f32::from(shown.height));
            let placed = placement(rect, rotation);
            for direction in [(1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), (0.0, -1.0)] {
                let (dx, dy) = rotation.unturn_vector(direction);
                let before = placed.window_point(size, start);
                let after = placed.window_point(size, (start.0 + dx, start.1 + dy));
                let moved = (after.0 - before.0, after.1 - before.1);
                let error = (moved.0 - direction.0).abs() + (moved.1 - direction.1).abs();
                assert!(error < 1e-3, "{rotation:?} {direction:?} moved {moved:?}");
            }
        }
    }

    #[test]
    fn gaps_cycle_through_the_presets() {
        assert_eq!(next_gap(0.0), 16.0);
        assert_eq!(next_gap(48.0), 90.0);
        assert_eq!(next_gap(90.0), 0.0);
        // A gap set by hand moves to the next preset up.
        assert_eq!(next_gap(30.0), 48.0);
    }

    #[test]
    fn snapped_rects_lie_on_device_pixels() {
        let rect = Rect {
            x: 10.3,
            y: 0.74,
            width: 654.6,
            height: 490.9,
        };
        // On a 1× display edges move to whole points; on Retina to halves.
        for (scale, expected) in [
            (1.0, (10.0, 1.0, 655.0, 491.0)),
            (2.0, (10.5, 0.5, 654.5, 491.0)),
        ] {
            let snapped = rect.snapped(scale);
            let got = (snapped.x, snapped.y, snapped.width, snapped.height);
            assert_eq!(got, expected, "at {scale}×");
            for edge in [snapped.x, snapped.y, snapped.width, snapped.height] {
                assert_eq!((edge * scale).fract(), 0.0, "{edge} at {scale}×");
            }
        }
        // Screens side by side stay edge to edge.
        let left = Rect {
            x: 0.0,
            y: 0.0,
            width: 100.3,
            height: 10.0,
        };
        let right = Rect { x: 100.3, ..left };
        let (left, right) = (left.snapped(2.0), right.snapped(2.0));
        assert_eq!(left.x + left.width, right.x);
        assert_eq!(rect.snapped(0.0), rect);
    }
}
