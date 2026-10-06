//! The geometry engine: executes geometry commands, transforms and lights
//! vertices, and assembles, culls, clips and stores polygons in Polygon RAM
//! (GBATEK, "DS 3D Geometry Commands", "DS 3D Polygon Definitions by
//! Vertices", "DS 3D Texture Coordinates" and "DS 3D Tests").

mod clip;
mod lighting;
mod matrix;

use serde::{Deserialize, Serialize};

use self::clip::{ClipPolygon, ClipVertex};
use self::lighting::Lighting;
use self::matrix::{Matrices, Matrix};
use super::color;
use super::polygon::{
    MAX_POLYGON_VERTICES, MAX_POLYGONS, MAX_VERTEX_RAM, Polygon, PolygonAttr, PolygonList,
    ScreenVertex, TextureParam,
};

/// Sign-extends the three 10-bit fields of NORMAL, LIGHT_VECTOR, VEC_TEST,
/// VTX_10 and VTX_DIFF parameters.
fn unpack_10(param: u32) -> [i32; 3] {
    std::array::from_fn(|i| ((param >> (10 * i)) as i32) << 22 >> 22)
}

/// `(input, 1.0) * m` over the first two columns of the texture matrix,
/// shifted right by `shift` and added to `base`, wrapped to 16 bits.
fn transform_texcoord(m: &Matrix, input: [i32; 3], shift: u32, base: [i32; 2]) -> [i32; 2] {
    std::array::from_fn(|column| {
        let sum: i64 = input
            .iter()
            .zip(&m.0)
            .map(|(&value, row)| i64::from(value) * i64::from(row[column]))
            .sum();
        ((sum >> shift) as i32).wrapping_add(base[column]) as i16 as i32
    })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
enum Primitive {
    #[default]
    Triangles,
    Quads,
    TriangleStrip,
    QuadStrip,
}

/// VIEWPORT, in hardware coordinates (Y counts up from the bottom).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Viewport {
    x1: i64,
    y1: i64,
    x2: i64,
    y2: i64,
}

impl Default for Viewport {
    fn default() -> Self {
        Self::from_param(0xBFFF_0000)
    }
}

impl Viewport {
    fn from_param(param: u32) -> Self {
        let field = |shift: u32| i64::from((param >> shift) & 0xFF);
        Self {
            x1: field(0),
            y1: field(8),
            x2: field(16),
            y2: field(24),
        }
    }
}

/// The sign of a polygon's facing in clip space: positive for the front
/// (anticlockwise on screen), negative for the back, zero for a line.
fn facing(vertices: &[ClipVertex]) -> i32 {
    let determinant = |a: &ClipVertex, b: &ClipVertex, c: &ClipVertex| {
        let [ax, ay, _, aw] = a.position.map(i128::from);
        let [bx, by, _, bw] = b.position.map(i128::from);
        let [cx, cy, _, cw] = c.position.map(i128::from);
        ax * (by * cw - cy * bw) - ay * (bx * cw - cx * bw) + aw * (bx * cy - cx * by)
    };
    let mut value = determinant(&vertices[0], &vertices[1], &vertices[2]);
    if value == 0 && vertices.len() == 4 {
        value = determinant(&vertices[0], &vertices[2], &vertices[3]);
    }
    value.signum() as i32
}

/// Twice the signed area of a screen-space polygon.
fn screen_area(vertices: &[ScreenVertex]) -> i64 {
    let mut previous = vertices[vertices.len() - 1];
    let mut sum = 0;
    for &vertex in vertices {
        sum += i64::from(previous.x) * i64::from(vertex.y)
            - i64::from(vertex.x) * i64::from(previous.y);
        previous = vertex;
    }
    sum
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Geometry {
    matrices: Matrices,
    lighting: Lighting,
    /// The colour the next vertex gets, BGR555.
    color: u32,
    /// TEXCOORD as written.
    texcoord_raw: [i32; 2],
    /// The texture coordinates the next vertex gets.
    texcoord: [i32; 2],
    /// The last vertex position, for VTX_XY/XZ/YZ/DIFF.
    last: [i16; 3],
    /// POLYGON_ATTR as written, and as latched by BEGIN_VTXS.
    pending_attr: PolygonAttr,
    attr: PolygonAttr,
    texture: TextureParam,
    palette: u32,
    primitive: Primitive,
    /// A BEGIN_VTXS has been seen; END_VTXS changes nothing.
    in_list: bool,
    /// Vertices waiting to complete a polygon.
    pending: [ClipVertex; 4],
    pending_len: usize,
    /// Every second triangle of a strip is wound the other way.
    strip_odd: bool,
    /// The previous polygon of this strip was stored unclipped, so the
    /// next one shares two of its vertices in Vertex RAM.
    strip_shared: bool,
    viewport: Viewport,
    /// DISP_1DOT_DEPTH; written directly, not through the FIFO.
    pub(crate) one_dot_depth: u32,
    box_result: bool,
    pos_result: [i32; 4],
    vec_result: [u16; 3],
    list: PolygonList,
    /// Polygon or Vertex RAM overflowed (DISP3DCNT bit 13).
    pub(crate) overflow: bool,
}

impl Default for Geometry {
    fn default() -> Self {
        Self {
            matrices: Matrices::default(),
            lighting: Lighting::default(),
            color: 0x7FFF,
            texcoord_raw: [0; 2],
            texcoord: [0; 2],
            last: [0; 3],
            pending_attr: PolygonAttr::default(),
            attr: PolygonAttr::default(),
            texture: TextureParam::default(),
            palette: 0,
            primitive: Primitive::default(),
            in_list: false,
            pending: [ClipVertex::default(); 4],
            pending_len: 0,
            strip_odd: false,
            strip_shared: false,
            viewport: Viewport::default(),
            one_dot_depth: 0,
            box_result: false,
            pos_result: [0; 4],
            vec_result: [0; 3],
            list: PolygonList::default(),
            overflow: false,
        }
    }
}

impl Geometry {
    /// Runs one complete command.
    pub(crate) fn execute(&mut self, command: u8, params: &[u32]) {
        let param = params.first().copied().unwrap_or(0);
        match command {
            0x10 => self.matrices.set_mode(param),
            0x11 => self.matrices.push(),
            0x12 => self.matrices.pop(param),
            0x13 => self.matrices.store(param),
            0x14 => self.matrices.restore(param),
            0x15 => self.matrices.load(Matrix::IDENTITY),
            0x16 => self.matrices.load(Matrix::from_4x4(params)),
            0x17 => self.matrices.load(Matrix::from_4x3(params)),
            0x18 => self.matrices.multiply(&Matrix::from_4x4(params)),
            0x19 => self.matrices.multiply(&Matrix::from_4x3(params)),
            0x1A => self.matrices.multiply(&Matrix::from_3x3(params)),
            0x1B => self.matrices.scale(&Matrix::scale(params)),
            0x1C => self.matrices.multiply(&Matrix::translation(params)),
            0x20 => self.color = param & 0x7FFF,
            0x21 => self.normal(param),
            0x22 => self.set_texcoord(param),
            0x23 => {
                let z = params.get(1).copied().unwrap_or(0);
                self.vertex([param as i16, (param >> 16) as i16, z as i16]);
            }
            0x24 => self.vertex(unpack_10(param).map(|c| (c << 6) as i16)),
            0x25 => self.vertex([param as i16, (param >> 16) as i16, self.last[2]]),
            0x26 => self.vertex([param as i16, self.last[1], (param >> 16) as i16]),
            0x27 => self.vertex([self.last[0], param as i16, (param >> 16) as i16]),
            0x28 => {
                let delta = unpack_10(param);
                let last = self.last;
                self.vertex(std::array::from_fn(|i| {
                    last[i].wrapping_add(delta[i] as i16)
                }));
            }
            0x29 => self.pending_attr = PolygonAttr(param),
            0x2A => self.texture = TextureParam(param),
            0x2B => self.palette = param & 0x1FFF,
            0x30 => {
                if let Some(color) = self.lighting.set_diffuse_ambient(param) {
                    self.color = color;
                }
            }
            0x31 => self.lighting.set_specular_emission(param),
            0x32 => {
                let vector = unpack_10(param).map(|c| c << 3);
                let direction = self.matrices.vector.transform_direction(vector);
                self.lighting
                    .set_direction((param >> 30) as usize, direction);
            }
            0x33 => self.lighting.set_color((param >> 30) as usize, param),
            0x34 => self.lighting.set_shininess(params),
            0x40 => self.begin(param),
            0x60 => self.viewport = Viewport::from_param(param),
            0x70 => self.box_test(params),
            0x71 => self.position_test(params),
            0x72 => self.vector_test(param),
            // END_VTXS has no effect; the rest are not commands.
            _ => {}
        }
    }

    /// Polygon RAM being filled.
    pub(crate) fn list_mut(&mut self) -> &mut PolygonList {
        &mut self.list
    }

    /// RAM_COUNT.
    pub(crate) fn ram_count(&self) -> u32 {
        self.list.polygons.len() as u32 | (self.list.vertex_ram as u32) << 16
    }

    /// GXSTAT bits 1 and 8–15: box test result, stack levels and error.
    pub(crate) fn status(&self) -> u32 {
        u32::from(self.box_result) << 1
            | self.matrices.stack_levels() << 8
            | u32::from(self.matrices.stack_error) << 15
    }

    pub(crate) fn acknowledge_stack_error(&mut self) {
        self.matrices.acknowledge_error();
    }

    /// POS_RESULT word `index` (x, y, z, w).
    pub(crate) fn pos_result(&self, index: usize) -> u32 {
        self.pos_result[index & 3] as u32
    }

    /// VEC_RESULT halfword `index` (x, y, z).
    pub(crate) fn vec_result(&self, index: usize) -> u16 {
        self.vec_result.get(index).copied().unwrap_or(0)
    }

    /// CLIPMTX_RESULT word `index`, row by row.
    pub(crate) fn clip_matrix_word(&self, index: usize) -> u32 {
        self.matrices.clip_now().0[(index / 4) & 3][index % 4] as u32
    }

    /// VECMTX_RESULT word `index`: the 3×3 part, row by row.
    pub(crate) fn vector_matrix_word(&self, index: usize) -> u32 {
        self.matrices.vector.0[(index / 3).min(2)][index % 3] as u32
    }

    fn begin(&mut self, param: u32) {
        self.attr = self.pending_attr;
        self.primitive = match param & 3 {
            0 => Primitive::Triangles,
            1 => Primitive::Quads,
            2 => Primitive::TriangleStrip,
            _ => Primitive::QuadStrip,
        };
        self.in_list = true;
        self.pending_len = 0;
        self.strip_odd = false;
        self.strip_shared = false;
    }

    fn set_texcoord(&mut self, param: u32) {
        let raw = [i32::from(param as i16), i32::from((param >> 16) as i16)];
        self.texcoord_raw = raw;
        self.texcoord = if self.texture.transform() == 1 {
            // (S, T, 1/16, 1/16) * M: S and T carry four fractional bits,
            // so the two 1/16 rows enter at full weight.
            let m = &self.matrices.texture.0;
            std::array::from_fn(|c| {
                let sum = i64::from(raw[0]) * i64::from(m[0][c])
                    + i64::from(raw[1]) * i64::from(m[1][c])
                    + i64::from(m[2][c])
                    + i64::from(m[3][c]);
                (sum >> 12) as i32 as i16 as i32
            })
        } else {
            raw
        };
    }

    fn normal(&mut self, param: u32) {
        let normal = unpack_10(param);
        if self.texture.transform() == 2 {
            self.texcoord =
                transform_texcoord(&self.matrices.texture, normal, 21, self.texcoord_raw);
        }
        let normal = self
            .matrices
            .vector
            .transform_direction(normal.map(|c| c << 3));
        self.color = self.lighting.color(normal, self.attr.lights());
    }

    fn vertex(&mut self, position: [i16; 3]) {
        self.last = position;
        if !self.in_list {
            return;
        }
        let point = position.map(i32::from);
        if self.texture.transform() == 3 {
            self.texcoord =
                transform_texcoord(&self.matrices.texture, point, 24, self.texcoord_raw);
        }
        self.pending[self.pending_len] = ClipVertex {
            position: self.matrices.clip().transform_point(point),
            color: color::rgb6(self.color).map(i32::from),
            texcoord: self.texcoord,
        };
        self.pending_len += 1;
        self.assemble();
    }

    /// Emits a polygon once enough vertices are pending.
    fn assemble(&mut self) {
        let v = self.pending;
        match (self.primitive, self.pending_len) {
            (Primitive::Triangles, 3) => {
                self.pending_len = 0;
                self.emit(&v[..3], 0);
            }
            (Primitive::Quads, 4) => {
                self.pending_len = 0;
                self.emit(&v, 0);
            }
            (Primitive::TriangleStrip, 3) => {
                let triangle = if self.strip_odd {
                    [v[1], v[0], v[2]]
                } else {
                    [v[0], v[1], v[2]]
                };
                self.strip_odd = !self.strip_odd;
                self.pending[..2].copy_from_slice(&v[1..3]);
                self.pending_len = 2;
                self.emit(&triangle, 2);
            }
            (Primitive::QuadStrip, 4) => {
                self.pending[..2].copy_from_slice(&v[2..4]);
                self.pending_len = 2;
                self.emit(&[v[0], v[1], v[3], v[2]], 2);
            }
            _ => {}
        }
    }

    /// Culls, clips and stores one polygon. `shared` vertices are reused
    /// from the previous polygon of a strip if that one was stored whole.
    fn emit(&mut self, vertices: &[ClipVertex], shared: usize) {
        let can_share = std::mem::take(&mut self.strip_shared);
        let attr = self.attr;
        match facing(vertices) {
            1 if !attr.render_front() => return,
            -1 if !attr.render_back() => return,
            _ => {}
        }
        if !attr.render_far() && vertices.iter().any(|v| v.position[2] > v.position[3]) {
            return;
        }
        let mut polygon = ClipPolygon::new(vertices);
        let cut = clip::clip(&mut polygon);
        let clipped = polygon.vertices();
        if clipped.is_empty() {
            return;
        }
        let mut screen = [ScreenVertex::default(); MAX_POLYGON_VERTICES];
        let len = clipped.len().min(MAX_POLYGON_VERTICES);
        for (out, vertex) in screen.iter_mut().zip(clipped) {
            *out = self.to_screen(vertex);
        }
        let screen = &screen[..len];
        if !attr.render_one_dot() && self.beyond_one_dot_depth(screen) {
            return;
        }
        let shared = if can_share && !cut { shared } else { 0 };
        let new_vertices = len.saturating_sub(shared);
        if self.list.polygons.len() >= MAX_POLYGONS
            || self.list.vertex_ram + new_vertices > MAX_VERTEX_RAM
        {
            self.overflow = true;
            return;
        }
        let alpha = attr.alpha();
        let format = self.texture.format();
        self.list.polygons.push(Polygon {
            first: self.list.vertices.len() as u32,
            len: len as u8,
            attr,
            texture: self.texture,
            palette: self.palette,
            translucent: (1..31).contains(&alpha) || format == 1 || format == 6,
            line: screen_area(screen) == 0,
            top: screen.iter().map(|v| v.y).min().unwrap_or(0),
            bottom: screen.iter().map(|v| v.y).max().unwrap_or(0),
        });
        self.list.vertices.extend_from_slice(screen);
        self.list.vertex_ram += new_vertices;
        self.strip_shared = !cut;
    }

    /// A polygon shrunk to a single dot, all of whose vertices lie beyond
    /// DISP_1DOT_DEPTH (a 12.3 W value).
    fn beyond_one_dot_depth(&self, vertices: &[ScreenVertex]) -> bool {
        let first = vertices[0];
        vertices.iter().all(|v| {
            (v.x, v.y) == (first.x, first.y) && i64::from(v.w) > i64::from(self.one_dot_depth) << 9
        })
    }

    /// The viewport transform: pixel position, depth and attributes.
    fn to_screen(&self, vertex: &ClipVertex) -> ScreenVertex {
        let [x, y, z, w] = vertex.position.map(i64::from);
        let viewport = self.viewport;
        let width = viewport.x2 - viewport.x1 + 1;
        let height = viewport.y2 - viewport.y1 + 1;
        let (screen_x, screen_y, depth) = if w > 0 {
            let screen_x = (x + w) * width / (2 * w) + viewport.x1;
            let screen_y = (y + w) * height / (2 * w) + viewport.y1;
            let depth = if self.list.w_buffer {
                w
            } else {
                (((z << 14) / w) + 0x3FFF) << 9
            };
            (screen_x, screen_y, depth)
        } else {
            (viewport.x1, viewport.y1, 0)
        };
        ScreenVertex {
            x: screen_x.clamp(-0x400, 0x400) as i32,
            // Hardware Y counts up from the bottom; rows count down.
            y: (192 - screen_y).clamp(-0x400, 0x400) as i32,
            depth: depth.clamp(0, 0xFF_FFFF) as u32,
            w: vertex.position[3],
            color: vertex.color.map(|c| c.clamp(0, 63) as u8),
            texcoord: vertex.texcoord,
        }
    }

    /// BOX_TEST: whether any face of the box reaches into the view volume.
    fn box_test(&mut self, params: &[u32]) {
        let word = |i: usize| params.get(i).copied().unwrap_or(0);
        let low = |i: usize| i32::from(word(i) as i16);
        let high = |i: usize| i32::from((word(i) >> 16) as i16);
        let origin = [low(0), high(0), low(1)];
        let size = [high(1), low(2), high(2)];
        let clip = *self.matrices.clip();
        let corners: [ClipVertex; 8] = std::array::from_fn(|corner| {
            let point = std::array::from_fn(|axis| {
                origin[axis]
                    + if corner & (1 << axis) != 0 {
                        size[axis]
                    } else {
                        0
                    }
            });
            ClipVertex {
                position: clip.transform_point(point),
                ..ClipVertex::default()
            }
        });
        const FACES: [[usize; 4]; 6] = [
            [0, 1, 3, 2],
            [4, 5, 7, 6],
            [0, 1, 5, 4],
            [2, 3, 7, 6],
            [0, 2, 6, 4],
            [1, 3, 7, 5],
        ];
        self.box_result = FACES.iter().any(|face| {
            let mut polygon = ClipPolygon::new(&face.map(|i| corners[i]));
            clip::clip(&mut polygon);
            !polygon.vertices().is_empty()
        });
    }

    /// POS_TEST: also becomes the last vertex for relative VTX commands.
    fn position_test(&mut self, params: &[u32]) {
        let first = params.first().copied().unwrap_or(0);
        let second = params.get(1).copied().unwrap_or(0);
        self.last = [first as i16, (first >> 16) as i16, second as i16];
        self.pos_result = self
            .matrices
            .clip()
            .transform_point(self.last.map(i32::from));
    }

    /// VEC_TEST: results have 12 fractional bits and a sign spread over
    /// bits 12–15.
    fn vector_test(&mut self, param: u32) {
        let vector = unpack_10(param).map(|c| c << 3);
        let result = self.matrices.vector.transform_direction(vector);
        self.vec_result = result.map(|c| ((c << 19) >> 19) as u16);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix::ONE;

    fn fixed(value: f32) -> u32 {
        // Test inputs only; the engine itself never sees floats.
        ((value * ONE as f32) as i32) as u32
    }

    fn vtx16(geometry: &mut Geometry, x: f32, y: f32, z: f32) {
        let xy = fixed(x) & 0xFFFF | fixed(y) << 16;
        geometry.execute(0x23, &[xy, fixed(z) & 0xFFFF]);
    }

    fn solid(geometry: &mut Geometry) {
        geometry.execute(0x29, &[0x001F_00C0]);
        geometry.execute(0x40, &[0]);
    }

    #[test]
    fn a_visible_triangle_lands_in_screen_space() {
        let mut geometry = Geometry::default();
        solid(&mut geometry);
        vtx16(&mut geometry, -1.0, 1.0, 0.0);
        vtx16(&mut geometry, -1.0, -1.0, 0.0);
        vtx16(&mut geometry, 1.0, -1.0, 0.0);
        let list = geometry.list_mut().clone();
        assert_eq!(list.polygons.len(), 1);
        let corners: Vec<_> = list.vertices.iter().map(|v| (v.x, v.y)).collect();
        assert_eq!(corners, [(0, 0), (0, 192), (256, 192)]);
        assert_eq!(geometry.ram_count(), 1 | 3 << 16);
    }

    #[test]
    fn back_faces_are_culled_unless_enabled() {
        let mut geometry = Geometry::default();
        geometry.execute(0x29, &[0x001F_0080]); // Front only.
        geometry.execute(0x40, &[0]);
        // Clockwise on screen: the back.
        vtx16(&mut geometry, -1.0, 1.0, 0.0);
        vtx16(&mut geometry, 1.0, -1.0, 0.0);
        vtx16(&mut geometry, -1.0, -1.0, 0.0);
        assert_eq!(geometry.ram_count(), 0);
    }

    #[test]
    fn strips_share_vertices_and_alternate_winding() {
        let mut geometry = Geometry::default();
        geometry.execute(0x29, &[0x001F_0080]); // Front only.
        geometry.execute(0x40, &[2]);
        vtx16(&mut geometry, -0.5, 0.0, 0.0);
        vtx16(&mut geometry, 0.0, -0.5, 0.0);
        vtx16(&mut geometry, 0.0, 0.5, 0.0);
        vtx16(&mut geometry, 0.5, -0.5, 0.0);
        // Both triangles face the front once the second is re-wound.
        assert_eq!(geometry.ram_count(), 2 | 4 << 16);
    }

    #[test]
    fn polygons_crossing_the_far_plane_need_permission() {
        let mut geometry = Geometry::default();
        solid(&mut geometry);
        vtx16(&mut geometry, -0.5, 0.5, 2.0);
        vtx16(&mut geometry, -0.5, -0.5, 0.0);
        vtx16(&mut geometry, 0.5, -0.5, 0.0);
        assert_eq!(geometry.ram_count(), 0);
        geometry.execute(0x29, &[0x001F_10C0]);
        geometry.execute(0x40, &[0]);
        vtx16(&mut geometry, -0.5, 0.5, 2.0);
        vtx16(&mut geometry, -0.5, -0.5, 0.0);
        vtx16(&mut geometry, 0.5, -0.5, 0.0);
        assert_eq!(geometry.ram_count() & 0xFFF, 1);
    }

    #[test]
    fn box_test_sees_boxes_touching_the_view_volume() {
        let mut geometry = Geometry::default();
        let box_test = |geometry: &mut Geometry, x: f32, size: f32| {
            geometry.execute(
                0x70,
                &[
                    fixed(x) & 0xFFFF,
                    fixed(-0.25) & 0xFFFF | fixed(size) << 16,
                    fixed(size) & 0xFFFF | fixed(size) << 16,
                ],
            );
            geometry.status() & 2 != 0
        };
        assert!(box_test(&mut geometry, -0.25, 0.5), "inside");
        assert!(!box_test(&mut geometry, 2.0, 0.5), "off to the right");
        assert!(box_test(&mut geometry, 0.75, 0.5), "straddling the edge");
        // A box enclosing the whole volume has no face inside it.
        geometry.execute(
            0x70,
            &[
                fixed(-4.0) & 0xFFFF | fixed(-4.0) << 16,
                fixed(-4.0) & 0xFFFF | fixed(7.0) << 16,
                fixed(7.0) & 0xFFFF | fixed(7.0) << 16,
            ],
        );
        assert_eq!(geometry.status() & 2, 0);
    }

    #[test]
    fn position_and_vector_tests_read_back() {
        let mut geometry = Geometry::default();
        geometry.execute(0x10, &[2]);
        geometry.execute(0x1C, &[fixed(1.0), 0, 0]);
        geometry.execute(0x71, &[fixed(0.5) & 0xFFFF, 0]);
        assert_eq!(geometry.pos_result(0), fixed(1.5));
        assert_eq!(geometry.pos_result(3), fixed(1.0));
        // Y = -0.5, sign-extended through bits 12-15.
        geometry.execute(0x72, &[(0x3FF - 0xFF) << 10]);
        assert_eq!(geometry.vec_result(1), (-(0x100i32 << 3)) as u16);
        geometry.execute(0x15, &[]);
        geometry.execute(0x1B, &[fixed(2.0), fixed(2.0), fixed(2.0)]);
        geometry.execute(0x10, &[1]);
        geometry.execute(0x72, &[0x100]);
        assert_eq!(
            geometry.vec_result(0),
            0x0800,
            "scale skips the vector matrix"
        );
    }

    #[test]
    fn texcoord_source_transform_translates() {
        let mut geometry = Geometry::default();
        geometry.execute(0x10, &[3]);
        geometry.execute(0x16, &{
            let mut m = [0u32; 16];
            m[0] = ONE as u32;
            m[5] = ONE as u32;
            m[12] = 16 << 12; // +1 texel in S.
            m
        });
        geometry.execute(0x2A, &[1 << 30]);
        geometry.execute(0x22, &[0x0020_0010]);
        assert_eq!(geometry.texcoord, [0x20, 0x20]);
    }
}
