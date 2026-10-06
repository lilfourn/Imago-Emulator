//! Scan conversion: which pixels a polygon covers, with their interpolated
//! depth, colour and texture coordinates (GBATEK, "DS 3D Polygon
//! Definitions by Vertices", and "Polygon Size" and "Anti-Aliasing" under
//! "DS 3D Toon, Edge, Fog, Alpha-Blending, Anti-Aliasing").
//!
//! Vertices sit on whole pixels. Edges are sampled at the centre of each
//! row, and a pixel is covered when its centre lies in `[left, right)`, so
//! polygons that share an edge neither overlap nor leave a gap: the
//! hardware's "small polygon" rule, applied to all polygons. Polygons of
//! zero area (line segments, dots) and wire-frames are drawn as outlines.
//!
//! Pixels on a polygon's outline are flagged as edges, for edge marking
//! and anti-aliasing: its top and bottom rows, and on each row the pixels
//! its left and right edges run across. An edge runs across as many
//! pixels as it moves sideways in one row, at least one. With
//! anti-aliasing, [`fill_antialiased`] draws every pixel an edge touches,
//! each with the share of it the polygon covers: half, plus the distance
//! from the pixel's centre to the edge across the row, over the edge's
//! run. The hardware ramps its coverage along each edge the same way; its
//! exact rounding is not reproduced.
//!
//! Depth is linear in screen space for Z-buffering and perspective-correct
//! for W-buffering. Colour and texture coordinates are always
//! perspective-correct, weighted by each polygon's W values scaled to 16
//! bits as the hardware does.

use crate::gpu3d::polygon::{MAX_POLYGON_VERTICES, ScreenVertex};

pub(crate) const WIDTH: usize = 256;
pub(crate) const HEIGHT: usize = 192;

/// Extra fractional bits on interpolated colours and texture coordinates.
const ATTR_SHIFT: u32 = 8;

/// How a polygon covers one of its pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Coverage {
    /// The pixel is on the polygon's outline.
    pub(crate) edge: bool,
    /// The share of the pixel covered, in 32nds less one: 31 is all of it.
    /// Below 31 only on anti-aliased edges.
    pub(crate) amount: u8,
}

impl Coverage {
    const EDGE: Self = Self {
        edge: true,
        amount: 31,
    };
}

/// Receives a polygon's pixels; `index` is `y * 256 + x`.
pub(crate) trait Target {
    /// Where in the frame to draw the pixel at `index` with `depth`, or
    /// `None` to skip it. May have side effects, such as setting stencil
    /// bits.
    fn test(&mut self, index: usize, depth: u32) -> Option<usize>;
    /// Draws the pixel at `slot`, as chosen by [`Target::test`]: 6-bit
    /// colour, 12.4 texture coordinates and how the polygon covers it.
    fn shade(
        &mut self,
        slot: usize,
        depth: u32,
        color: [u8; 3],
        texcoord: [i32; 2],
        coverage: Coverage,
    );
}

/// A vertex ready for interpolation.
#[derive(Debug, Clone, Copy, Default)]
struct Point {
    x: i32,
    y: i32,
    depth: i64,
    /// W scaled into 16 bits; at least 1.
    weight: i64,
    /// Red, green, blue, S, T, with `ATTR_SHIFT` extra fractional bits.
    attrs: [i64; 5],
}

/// Where an edge crosses a row, or one end of a span.
#[derive(Debug, Clone, Copy)]
struct Crossing {
    /// Column, 16 fractional bits.
    x: i64,
    /// How far the edge moves sideways over one row, at least one pixel;
    /// 16 fractional bits.
    run: i64,
    depth: i64,
    weight: i64,
    attrs: [i64; 5],
}

fn prepare(vertices: &[ScreenVertex], points: &mut [Point; MAX_POLYGON_VERTICES]) -> usize {
    let largest = vertices.iter().map(|v| v.w.max(1)).max().unwrap_or(1);
    let shift = (32 - largest.leading_zeros()).saturating_sub(16);
    for (point, vertex) in points.iter_mut().zip(vertices) {
        let [r, g, b] = vertex.color.map(i32::from);
        let [s, t] = vertex.texcoord;
        *point = Point {
            x: vertex.x,
            y: vertex.y,
            depth: i64::from(vertex.depth),
            weight: i64::from((vertex.w.max(1) >> shift).max(1)),
            attrs: [r, g, b, s, t].map(|a| i64::from(a) << ATTR_SHIFT),
        };
    }
    vertices.len().min(MAX_POLYGON_VERTICES)
}

/// The perspective-correct weight of `b` (16 fractional bits) at
/// `num / den` of the way from `a` to `b` on screen.
fn weight_of_b(num: i64, den: i64, a_weight: i64, b_weight: i64) -> i64 {
    if a_weight == b_weight {
        (num << 16) / den
    } else {
        ((num * a_weight) << 16) / ((den - num) * b_weight + num * a_weight)
    }
}

fn lerp(a: i64, b: i64, weight: i64) -> i64 {
    a + (((b - a) * weight) >> 16)
}

/// Depth at `num / den` of the way from `a` to `b`, whose perspective
/// weight is `weight`.
fn depth_between(a: i64, b: i64, num: i64, den: i64, weight: i64, w_buffer: bool) -> i64 {
    if w_buffer {
        lerp(a, b, weight)
    } else {
        a + (b - a) * num / den
    }
}

/// How far the edge between `a` and `b` moves sideways over one row, at
/// least one pixel; 16 fractional bits.
fn run_between(a: &Point, b: &Point) -> i64 {
    let rows = i64::from((b.y - a.y).abs()).max(1);
    ((i64::from((b.x - a.x).abs()) << 16) / rows).max(1 << 16)
}

/// Where the edge from `a` down to `b`, whose run is `run`, crosses the
/// centre of row `y`.
fn crossing(a: &Point, b: &Point, run: i64, y: i32, w_buffer: bool) -> Crossing {
    let num = i64::from(2 * (y - a.y) + 1);
    let den = i64::from(2 * (b.y - a.y));
    let weight = weight_of_b(num, den, a.weight, b.weight);
    Crossing {
        x: (i64::from(a.x) << 16) + ((i64::from(b.x - a.x) << 16) * num).div_euclid(den),
        run,
        depth: depth_between(a.depth, b.depth, num, den, weight, w_buffer),
        weight: lerp(a.weight, b.weight, weight),
        attrs: std::array::from_fn(|i| lerp(a.attrs[i], b.attrs[i], weight)),
    }
}

/// Hands one pixel to `target`, interpolating attributes only if it
/// passes the test.
fn emit<T: Target>(
    target: &mut T,
    index: usize,
    depth: i64,
    coverage: Coverage,
    attrs: impl FnOnce() -> [i64; 5],
) {
    let depth = depth.clamp(0, 0xFF_FFFF) as u32;
    let Some(slot) = target.test(index, depth) else {
        return;
    };
    let attrs = attrs();
    let channel = |i: usize| (attrs[i] >> ATTR_SHIFT).clamp(0, 63) as u8;
    target.shade(
        slot,
        depth,
        [channel(0), channel(1), channel(2)],
        [
            (attrs[3] >> ATTR_SHIFT) as i32,
            (attrs[4] >> ATTR_SHIFT) as i32,
        ],
        coverage,
    );
}

/// The share of a pixel, in 32nds, on the inside of an edge: half, plus
/// `inside`, how far the pixel's centre lies inside the edge across the
/// row (16 fractional bits), times `scale`, 32 over the edge's run (16
/// fractional bits). Rounding is the same both ways, so two polygons
/// sharing an edge share each pixel on it exactly.
fn edge_share(inside: i64, scale: i64) -> i64 {
    let scaled = inside * scale;
    let rounded = (scaled.abs() + (1 << 31)) >> 32;
    (16 + scaled.signum() * rounded).clamp(0, 32)
}

/// Row `y` of a polygon, between its `left` and `right` edges. On the
/// polygon's top and bottom rows, every pixel is an edge (`outline`).
/// Anti-aliased, the row takes every pixel the edges touch; otherwise,
/// the pixels whose centres lie between them.
fn span<const ANTIALIAS: bool, T: Target>(
    y: usize,
    left: &Crossing,
    right: &Crossing,
    outline: bool,
    w_buffer: bool,
    target: &mut T,
) {
    // The row's first and last pixels (plus one), and where the left
    // edge's run ends and the right edge's begins.
    let (start, end, left_end, right_start) = if ANTIALIAS {
        // Each run is centred on its crossing.
        (
            (left.x - left.run / 2) >> 16,
            (right.x + right.run / 2 + 0xFFFF) >> 16,
            (left.x + left.run / 2 + 0xFFFF) >> 16,
            (right.x - right.run / 2) >> 16,
        )
    } else {
        // Each run starts at the outermost pixel drawn.
        let start = (left.x + 0x7FFF) >> 16;
        let end = (right.x + 0x7FFF) >> 16;
        (
            start,
            end,
            start + ((left.run + 0xFFFF) >> 16),
            end - ((right.run + 0xFFFF) >> 16),
        )
    };
    let scales = if ANTIALIAS {
        ((32 << 32) / left.run, (32 << 32) / right.run)
    } else {
        (0, 0)
    };
    let den = (right.x - left.x).max(1);
    for x in start.max(0)..end.min(WIDTH as i64) {
        let centre = (x << 16) + 0x8000;
        let (on_left, on_right) = (x < left_end, x >= right_start);
        let coverage = if ANTIALIAS && (on_left || on_right) {
            let mut share = 32;
            if on_left {
                share = share.min(edge_share(centre - left.x, scales.0));
            }
            if on_right {
                share = share.min(edge_share(right.x - centre, scales.1));
            }
            if share == 0 {
                continue;
            }
            Coverage {
                edge: true,
                amount: (share - 1) as u8,
            }
        } else {
            Coverage {
                edge: outline || on_left || on_right,
                amount: 31,
            }
        };
        let num = if ANTIALIAS {
            // Pixels beyond a crossing take its values.
            (centre - left.x).clamp(0, den)
        } else {
            centre - left.x
        };
        let weight = || weight_of_b(num, den, left.weight, right.weight);
        let (depth, known) = if w_buffer {
            let weight = weight();
            (lerp(left.depth, right.depth, weight), Some(weight))
        } else {
            (left.depth + (right.depth - left.depth) * num / den, None)
        };
        emit(target, y * WIDTH + x as usize, depth, coverage, || {
            let weight = known.unwrap_or_else(weight);
            std::array::from_fn(|i| lerp(left.attrs[i], right.attrs[i], weight))
        });
    }
}

/// Walks one side of a polygon from its top vertex.
struct Chain {
    current: usize,
    /// 1 to go forwards around the polygon, `len - 1` to go backwards.
    step: usize,
}

impl Chain {
    /// The edge of this side that spans the centre of row `y`, as vertex
    /// indices from top to bottom.
    fn edge(&mut self, points: &[Point], y: i32) -> Option<(usize, usize)> {
        for _ in 0..points.len() {
            let next = (self.current + self.step) % points.len();
            if points[next].y > y {
                return Some((self.current, next));
            }
            self.current = next;
        }
        None
    }
}

/// Fills a convex polygon: the pixels whose centres it covers.
pub(crate) fn fill<T: Target>(vertices: &[ScreenVertex], w_buffer: bool, target: &mut T) {
    fill_rows::<false, T>(vertices, w_buffer, target);
}

/// Fills a convex polygon with anti-aliased edges: every pixel it covers
/// at all, with the share it covers.
pub(crate) fn fill_antialiased<T: Target>(
    vertices: &[ScreenVertex],
    w_buffer: bool,
    target: &mut T,
) {
    fill_rows::<true, T>(vertices, w_buffer, target);
}

fn fill_rows<const ANTIALIAS: bool, T: Target>(
    vertices: &[ScreenVertex],
    w_buffer: bool,
    target: &mut T,
) {
    let mut storage = [Point::default(); MAX_POLYGON_VERTICES];
    let len = prepare(vertices, &mut storage);
    let points = &storage[..len];
    if len < 3 {
        return;
    }
    let top = (0..len).min_by_key(|&i| points[i].y).unwrap_or(0);
    let bottom = points.iter().map(|p| p.y).max().unwrap_or(0);
    let mut forward = Chain {
        current: top,
        step: 1,
    };
    let mut backward = Chain {
        current: top,
        step: len - 1,
    };
    // The run of the edge from point `i` to the next, whichever way a
    // chain walks it.
    let mut runs = [0; MAX_POLYGON_VERTICES];
    for (i, run) in runs.iter_mut().enumerate().take(len) {
        *run = run_between(&points[i], &points[(i + 1) % len]);
    }
    let run_of = |a: usize, b: usize| runs[if b == (a + 1) % len { a } else { b }];
    let first = points[top].y;
    for y in first.max(0)..bottom.min(HEIGHT as i32) {
        let (Some((a, b)), Some((c, d))) = (forward.edge(points, y), backward.edge(points, y))
        else {
            continue;
        };
        let one = crossing(&points[a], &points[b], run_of(a, b), y, w_buffer);
        let other = crossing(&points[c], &points[d], run_of(c, d), y, w_buffer);
        let (left, right) = if one.x <= other.x {
            (one, other)
        } else {
            (other, one)
        };
        let outline = y == first || y == bottom - 1;
        span::<ANTIALIAS, T>(y as usize, &left, &right, outline, w_buffer, target);
    }
}

/// One point of the line from `a` to `b`, `step` of `steps` along.
fn line_point<T: Target>(
    a: &Point,
    b: &Point,
    step: i32,
    steps: i32,
    w_buffer: bool,
    target: &mut T,
) {
    let along = |from: i32, to: i32| from + (2 * (to - from) * step + steps).div_euclid(2 * steps);
    let (x, y) = (along(a.x, b.x), along(a.y, b.y));
    if !(0..WIDTH as i32).contains(&x) || !(0..HEIGHT as i32).contains(&y) {
        return;
    }
    let (num, den) = (i64::from(step), i64::from(steps));
    let weight = weight_of_b(num, den, a.weight, b.weight);
    let depth = depth_between(a.depth, b.depth, num, den, weight, w_buffer);
    emit(
        target,
        y as usize * WIDTH + x as usize,
        depth,
        Coverage::EDGE,
        || std::array::from_fn(|i| lerp(a.attrs[i], b.attrs[i], weight)),
    );
}

/// Draws a polygon's edges one pixel wide: line segments, dots and
/// wire-frames. Every pixel is an edge, wholly covered.
pub(crate) fn outline<T: Target>(vertices: &[ScreenVertex], w_buffer: bool, target: &mut T) {
    let mut storage = [Point::default(); MAX_POLYGON_VERTICES];
    let len = prepare(vertices, &mut storage);
    let points = &storage[..len];
    let mut drawn = false;
    for (i, a) in points.iter().enumerate() {
        let b = &points[(i + 1) % len];
        let steps = (b.x - a.x).abs().max((b.y - a.y).abs());
        for step in 0..steps {
            line_point(a, b, step, steps, w_buffer, target);
        }
        drawn |= steps > 0;
    }
    if let (false, Some(dot)) = (drawn, points.first()) {
        line_point(dot, dot, 0, 1, w_buffer, target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Counts how often each pixel was drawn.
    struct Hits(Vec<u8>);

    impl Hits {
        fn new() -> Self {
            Self(vec![0; WIDTH * HEIGHT])
        }

        fn pixels(&self) -> usize {
            self.0.iter().filter(|&&times| times > 0).count()
        }

        fn drawn(&self, x: usize, y: usize) -> bool {
            self.0[y * WIDTH + x] > 0
        }
    }

    impl Target for Hits {
        fn test(&mut self, index: usize, _: u32) -> Option<usize> {
            Some(index)
        }

        fn shade(&mut self, index: usize, _: u32, _: [u8; 3], _: [i32; 2], _: Coverage) {
            self.0[index] += 1;
        }
    }

    fn vertex(x: i32, y: i32) -> ScreenVertex {
        ScreenVertex {
            x,
            y,
            w: 1 << 12,
            ..ScreenVertex::default()
        }
    }

    #[test]
    fn a_right_triangle_covers_pixel_centres_inside_it() {
        let mut coverage = Hits::new();
        fill(
            &[vertex(0, 0), vertex(0, 8), vertex(8, 0)],
            false,
            &mut coverage,
        );
        // Row y holds x < 7 - y: 7 + 6 + ... + 1.
        assert_eq!(coverage.pixels(), 28);
        assert!(coverage.drawn(0, 0) && coverage.drawn(6, 0) && !coverage.drawn(7, 0));
    }

    #[test]
    fn shared_edges_neither_overlap_nor_gap() {
        let mut halves = Hits::new();
        let quad = [vertex(3, 2), vertex(1, 40), vertex(70, 33), vertex(60, 5)];
        fill(&[quad[0], quad[1], quad[2]], false, &mut halves);
        fill(&[quad[0], quad[2], quad[3]], false, &mut halves);
        let mut whole = Hits::new();
        fill(&quad, false, &mut whole);
        assert_eq!(halves.0, whole.0);
        assert!(halves.0.iter().all(|&times| times <= 1));
    }

    #[test]
    fn the_full_viewport_covers_the_screen() {
        let mut coverage = Hits::new();
        let corners = [
            vertex(0, 0),
            vertex(0, 192),
            vertex(256, 192),
            vertex(256, 0),
        ];
        fill(&corners, false, &mut coverage);
        assert_eq!(coverage.pixels(), WIDTH * HEIGHT);
    }

    #[test]
    fn outlines_draw_lines_and_dots() {
        let mut line = Hits::new();
        outline(
            &[vertex(2, 2), vertex(2, 2), vertex(6, 4)],
            false,
            &mut line,
        );
        assert_eq!(line.pixels(), 5);
        assert!(line.drawn(2, 2) && line.drawn(6, 4));
        let mut dot = Hits::new();
        outline(&[vertex(9, 9); 3], false, &mut dot);
        assert!(dot.drawn(9, 9) && dot.pixels() == 1);
    }

    /// Records the interpolated colour of each pixel.
    struct Colors(Vec<[u8; 3]>);

    impl Target for Colors {
        fn test(&mut self, index: usize, _: u32) -> Option<usize> {
            Some(index)
        }

        fn shade(&mut self, index: usize, _: u32, color: [u8; 3], _: [i32; 2], _: Coverage) {
            self.0[index] = color;
        }
    }

    #[test]
    fn attributes_are_perspective_correct() {
        // A span from a near vertex (w = 1) to a far one (w = 3): the
        // screen midpoint is only a quarter of the way in attribute space.
        let near = ScreenVertex {
            color: [0; 3],
            ..vertex(0, 0)
        };
        let far = ScreenVertex {
            w: 3 << 12,
            color: [63; 3],
            ..vertex(64, 0)
        };
        let mut colors = Colors(vec![[0; 3]; WIDTH * HEIGHT]);
        fill(
            &[
                near,
                ScreenVertex { y: 4, ..near },
                ScreenVertex { y: 4, ..far },
                far,
            ],
            false,
            &mut colors,
        );
        let middle = colors.0[WIDTH + 32][0];
        assert!((15..=17).contains(&middle), "got {middle}");
    }

    /// Records how each pixel was covered.
    struct Coverages(Vec<Option<Coverage>>);

    impl Coverages {
        fn new() -> Self {
            Self(vec![None; WIDTH * HEIGHT])
        }

        fn at(&self, x: usize, y: usize) -> Option<Coverage> {
            self.0[y * WIDTH + x]
        }
    }

    impl Target for Coverages {
        fn test(&mut self, index: usize, _: u32) -> Option<usize> {
            Some(index)
        }

        fn shade(&mut self, index: usize, _: u32, _: [u8; 3], _: [i32; 2], coverage: Coverage) {
            self.0[index] = Some(coverage);
        }
    }

    #[test]
    fn edges_flag_the_whole_outline() {
        // A shallow top-right edge runs nine pixels per row.
        let mut drawn = Coverages::new();
        fill(
            &[vertex(10, 10), vertex(20, 40), vertex(100, 20)],
            false,
            &mut drawn,
        );
        let inside = |x: usize, y: usize| drawn.at(x, y).is_some();
        for y in 1..HEIGHT - 1 {
            for x in 1..WIDTH - 1 {
                let Some(coverage) = drawn.at(x, y) else {
                    continue;
                };
                let outline =
                    !(inside(x - 1, y) && inside(x + 1, y) && inside(x, y - 1) && inside(x, y + 1));
                if outline {
                    assert!(coverage.edge, "({x}, {y}) is on the outline");
                }
                assert_eq!(coverage.amount, 31);
            }
        }
        assert_eq!(drawn.at(40, 25).map(|c| c.edge), Some(false), "inside");
    }

    #[test]
    fn antialiased_edges_cover_part_of_their_pixels() {
        // The right edge moves half a pixel left per row.
        let corners = [vertex(0, 0), vertex(0, 64), vertex(32, 0)];
        let mut sharp = Coverages::new();
        fill(&corners, false, &mut sharp);
        let mut smooth = Coverages::new();
        fill_antialiased(&corners, false, &mut smooth);
        let partial = |x, y| smooth.at(x, y).map(|c| (c.edge, c.amount));
        // Row 0 crosses at 31.75: pixel 31 is three quarters covered.
        assert_eq!(partial(31, 0), Some((true, 23)));
        assert_eq!(partial(32, 0), None);
        // Row 1 crosses at 31.25: pixel 31 joins with a quarter.
        assert_eq!(sharp.at(31, 1), None);
        assert_eq!(partial(31, 1), Some((true, 7)));
        assert_eq!(partial(30, 1), Some((true, 31)));
        // The vertical left edge and the inside are whole.
        assert_eq!(partial(0, 10), Some((true, 31)));
        assert_eq!(partial(5, 10), Some((false, 31)));
        for (index, coverage) in sharp.0.iter().enumerate() {
            if coverage.is_some() {
                assert!(smooth.0[index].is_some(), "pixel {index} drawn sharp");
            }
        }
    }

    #[test]
    fn neighbours_share_their_common_edge_exactly() {
        let quad = [vertex(3, 2), vertex(1, 40), vertex(70, 33), vertex(60, 5)];
        let mut first = Coverages::new();
        fill_antialiased(&[quad[0], quad[1], quad[2]], false, &mut first);
        let mut second = Coverages::new();
        fill_antialiased(&[quad[0], quad[2], quad[3]], false, &mut second);
        let share = |c: Option<Coverage>| c.map_or(0, |c| u32::from(c.amount) + 1);
        // Away from the outer edges, every pixel is covered exactly once,
        // whether by one triangle or split along the diagonal.
        for y in 8..30 {
            for x in 10..55 {
                let shares = share(first.at(x, y)) + share(second.at(x, y));
                assert_eq!(shares, 32, "({x}, {y})");
            }
        }
    }
}
