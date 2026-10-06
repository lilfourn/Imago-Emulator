//! Clipping polygons to the view volume (GBATEK, "DS 3D Polygon Definitions
//! by Vertices": Clipping).
//!
//! The view volume is -w ≤ x, y, z ≤ w in clip coordinates. Each of its six
//! planes cuts the polygon in turn (Sutherland–Hodgman). A new vertex
//! interpolates colour and texture coordinates linearly in clip space,
//! which keeps them perspective-correct, and always from the inside vertex
//! towards the outside one, so polygons sharing an edge get the same
//! vertex.

use serde::{Deserialize, Serialize};

/// Room for a quad cut by six planes, with margin for the "prohibited"
/// non-convex quads, which can gain more vertices per plane.
const CAPACITY: usize = 16;

/// A vertex in clip coordinates with its attributes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ClipVertex {
    /// x, y, z, w; 20.12.
    pub(crate) position: [i32; 4],
    /// Red, green, blue; 6 bits each.
    pub(crate) color: [i32; 3],
    /// S, T; 12.4.
    pub(crate) texcoord: [i32; 2],
}

/// A polygon being clipped.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ClipPolygon {
    vertices: [ClipVertex; CAPACITY],
    len: usize,
}

impl ClipPolygon {
    pub(crate) fn new(vertices: &[ClipVertex]) -> Self {
        let mut polygon = Self {
            vertices: [ClipVertex::default(); CAPACITY],
            len: 0,
        };
        for &vertex in vertices {
            polygon.push(vertex);
        }
        polygon
    }

    pub(crate) fn vertices(&self) -> &[ClipVertex] {
        &self.vertices[..self.len]
    }

    fn push(&mut self, vertex: ClipVertex) {
        if self.len < CAPACITY {
            self.vertices[self.len] = vertex;
            self.len += 1;
        }
    }
}

/// How far inside the plane `sign * position[axis] <= w` the vertex is;
/// negative outside.
fn distance(vertex: &ClipVertex, axis: usize, sign: i64) -> i64 {
    i64::from(vertex.position[3]) - sign * i64::from(vertex.position[axis])
}

/// Where the edge from `inside` (distance `d_in` ≥ 0) to `outside`
/// (distance `d_out` < 0) crosses the plane.
fn intersect(
    inside: &ClipVertex,
    outside: &ClipVertex,
    d_in: i64,
    d_out: i64,
    axis: usize,
    sign: i64,
) -> ClipVertex {
    let span = d_in - d_out;
    let lerp = |a: i32, b: i32| (i64::from(a) + (i64::from(b) - i64::from(a)) * d_in / span) as i32;
    let mut vertex = ClipVertex {
        position: std::array::from_fn(|i| lerp(inside.position[i], outside.position[i])),
        color: std::array::from_fn(|i| lerp(inside.color[i], outside.color[i])),
        texcoord: std::array::from_fn(|i| lerp(inside.texcoord[i], outside.texcoord[i])),
    };
    // Land exactly on the plane despite rounding.
    vertex.position[axis] = (sign * i64::from(vertex.position[3])) as i32;
    vertex
}

/// Cuts `polygon` by one plane. Returns whether any vertex was outside.
fn clip_plane(polygon: &mut ClipPolygon, axis: usize, sign: i64) -> bool {
    let input = *polygon;
    let vertices = input.vertices();
    if vertices.iter().all(|v| distance(v, axis, sign) >= 0) {
        return false;
    }
    polygon.len = 0;
    let mut previous = vertices[vertices.len() - 1];
    for &current in vertices {
        let d_current = distance(&current, axis, sign);
        let d_previous = distance(&previous, axis, sign);
        if d_current >= 0 {
            if d_previous < 0 {
                polygon.push(intersect(
                    &current, &previous, d_current, d_previous, axis, sign,
                ));
            }
            polygon.push(current);
        } else if d_previous >= 0 {
            polygon.push(intersect(
                &previous, &current, d_previous, d_current, axis, sign,
            ));
        }
        previous = current;
    }
    true
}

/// Clips `polygon` to the view volume: far and near, then the sides.
/// Returns whether anything was cut away; the polygon may end up empty.
pub(crate) fn clip(polygon: &mut ClipPolygon) -> bool {
    let mut cut = false;
    for (axis, sign) in [(2, 1), (2, -1), (0, 1), (0, -1), (1, 1), (1, -1)] {
        if polygon.len == 0 {
            break;
        }
        cut |= clip_plane(polygon, axis, sign);
    }
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: i32 = 1 << 12;

    fn vertex(x: i32, y: i32, color: i32) -> ClipVertex {
        ClipVertex {
            position: [x, y, 0, ONE],
            color: [color; 3],
            texcoord: [0; 2],
        }
    }

    #[test]
    fn inside_polygons_are_untouched() {
        let triangle = [
            vertex(0, 0, 0),
            vertex(ONE / 2, 0, 0),
            vertex(0, ONE / 2, 0),
        ];
        let mut polygon = ClipPolygon::new(&triangle);
        assert!(!clip(&mut polygon));
        assert_eq!(polygon.vertices(), &triangle);
    }

    #[test]
    fn crossing_the_right_plane_adds_interpolated_vertices() {
        // From x = 0 (colour 0) to x = 2.0 (colour 60): the cut at x = 1.0
        // is halfway.
        let triangle = [
            vertex(0, 0, 0),
            vertex(2 * ONE, 0, 60),
            vertex(0, ONE / 2, 0),
        ];
        let mut polygon = ClipPolygon::new(&triangle);
        assert!(clip(&mut polygon));
        let vertices = polygon.vertices();
        assert_eq!(vertices.len(), 4);
        assert!(vertices.iter().all(|v| v.position[0] <= ONE));
        let on_plane: Vec<_> = vertices.iter().filter(|v| v.position[0] == ONE).collect();
        assert_eq!(on_plane.len(), 2);
        assert_eq!(on_plane[0].color[0].min(on_plane[1].color[0]), 30);
    }

    #[test]
    fn fully_outside_polygons_vanish() {
        let triangle = [
            vertex(2 * ONE, 0, 0),
            vertex(3 * ONE, 0, 0),
            vertex(2 * ONE, ONE, 0),
        ];
        let mut polygon = ClipPolygon::new(&triangle);
        clip(&mut polygon);
        assert!(polygon.vertices().is_empty());
    }
}
