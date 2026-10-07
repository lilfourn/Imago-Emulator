//! What the geometry engine hands to the rendering engine: Polygon RAM and
//! Vertex RAM (GBATEK, "DS 3D Overview", "DS 3D Polygon Attributes" and
//! "DS 3D Texture Attributes").
//!
//! Vertices are stored in screen space, as the hardware does: integer pixel
//! positions, a 24-bit depth (Z or W, chosen by SWAP_BUFFERS), the W used
//! for perspective-correct interpolation, a 6-bit colour and a 12.4 texture
//! coordinate. Each also keeps its position to a 256th of a pixel, which
//! the hardware drops, for drawing at a higher resolution (NDS-9).

use serde::{Deserialize, Serialize};

/// Polygon RAM holds 2048 polygons.
pub(crate) const MAX_POLYGONS: usize = 2048;
/// Vertex RAM holds 6144 vertices.
pub(crate) const MAX_VERTEX_RAM: usize = 6144;
/// A quad clipped by all six planes of the view volume has at most ten
/// vertices.
pub(crate) const MAX_POLYGON_VERTICES: usize = 10;

/// How a polygon combines vertex and texture colours (POLYGON_ATTR bits
/// 4–5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Modulation,
    Decal,
    /// Toon or highlight shading, as DISP3DCNT bit 1 selects.
    Toon,
    Shadow,
}

/// POLYGON_ATTR, as latched by BEGIN_VTXS.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PolygonAttr(pub(crate) u32);

impl PolygonAttr {
    fn bit(self, n: u32) -> bool {
        self.0 & (1 << n) != 0
    }

    /// Lights 0–3 enabled, one bit each.
    pub(crate) fn lights(self) -> u32 {
        self.0 & 0xF
    }

    pub(crate) fn mode(self) -> Mode {
        match (self.0 >> 4) & 3 {
            0 => Mode::Modulation,
            1 => Mode::Decal,
            2 => Mode::Toon,
            _ => Mode::Shadow,
        }
    }

    pub(crate) fn render_back(self) -> bool {
        self.bit(6)
    }

    pub(crate) fn render_front(self) -> bool {
        self.bit(7)
    }

    /// Translucent pixels write their depth.
    pub(crate) fn depth_update(self) -> bool {
        self.bit(11)
    }

    /// Polygons crossing the far plane are clipped rather than dropped.
    pub(crate) fn render_far(self) -> bool {
        self.bit(12)
    }

    /// Dot-sized polygons beyond DISP_1DOT_DEPTH are kept.
    pub(crate) fn render_one_dot(self) -> bool {
        self.bit(13)
    }

    pub(crate) fn depth_equal(self) -> bool {
        self.bit(14)
    }

    pub(crate) fn fog(self) -> bool {
        self.bit(15)
    }

    /// 0 is wire-frame, 31 solid.
    pub(crate) fn alpha(self) -> u8 {
        ((self.0 >> 16) & 0x1F) as u8
    }

    pub(crate) fn id(self) -> u8 {
        ((self.0 >> 24) & 0x3F) as u8
    }
}

/// TEXIMAGE_PARAM.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TextureParam(pub(crate) u32);

impl TextureParam {
    /// Byte address in texture VRAM.
    pub(crate) fn address(self) -> u32 {
        (self.0 & 0xFFFF) << 3
    }

    pub(crate) fn repeat_s(self) -> bool {
        self.0 & (1 << 16) != 0
    }

    pub(crate) fn repeat_t(self) -> bool {
        self.0 & (1 << 17) != 0
    }

    pub(crate) fn flip_s(self) -> bool {
        self.0 & (1 << 18) != 0
    }

    pub(crate) fn flip_t(self) -> bool {
        self.0 & (1 << 19) != 0
    }

    /// Log2 of the width in texels (3–10).
    pub(crate) fn width_shift(self) -> u32 {
        3 + ((self.0 >> 20) & 7)
    }

    /// Log2 of the height in texels (3–10).
    pub(crate) fn height_shift(self) -> u32 {
        3 + ((self.0 >> 23) & 7)
    }

    /// 0 none, 1 A3I5, 2 4-colour, 3 16-colour, 4 256-colour, 5 4×4
    /// compressed, 6 A5I3, 7 direct.
    pub(crate) fn format(self) -> u32 {
        (self.0 >> 26) & 7
    }

    pub(crate) fn color0_transparent(self) -> bool {
        self.0 & (1 << 29) != 0
    }

    /// Texture coordinate transformation: 0 none, 1 TexCoord, 2 Normal,
    /// 3 Vertex source.
    pub(crate) fn transform(self) -> u32 {
        self.0 >> 30
    }
}

/// A vertex after clipping and the viewport transform.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ScreenVertex {
    /// Pixel column, 0–256 (256 is the right edge of the viewport).
    pub(crate) x: i32,
    /// Pixel row from the top, 0–192.
    pub(crate) y: i32,
    /// 24-bit depth: Z mapped to 0–0xFFFFFF, or W.
    pub(crate) depth: u32,
    /// Clip-space W (20.12), for perspective correction.
    pub(crate) w: i32,
    /// Red, green, blue, 6 bits each.
    pub(crate) color: [u8; 3],
    /// S and T, 12.4 fixed point.
    pub(crate) texcoord: [i32; 2],
    /// Column and row with 8 fractional bits, for drawing above the native
    /// resolution only. Not saved: in a loaded state the vertex sits on
    /// its whole pixel.
    #[serde(skip)]
    pub(crate) fine: Option<[i32; 2]>,
}

impl ScreenVertex {
    /// Column and row on a grid `scale` times finer than the screen's
    /// pixels: [`ScreenVertex::x`] and [`ScreenVertex::y`] at 1, else the
    /// fine position, scaled and rounded down.
    pub(crate) fn position(&self, scale: i32) -> (i32, i32) {
        if scale == 1 {
            return (self.x, self.y);
        }
        let [x, y] = self.fine.unwrap_or([self.x << 8, self.y << 8]);
        ((x * scale) >> 8, (y * scale) >> 8)
    }
}

/// One entry of Polygon RAM.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Polygon {
    /// Index of the first vertex in [`PolygonList::vertices`].
    pub(crate) first: u32,
    pub(crate) len: u8,
    pub(crate) attr: PolygonAttr,
    pub(crate) texture: TextureParam,
    /// PLTT_BASE, bits 0–12.
    pub(crate) palette: u32,
    /// Drawn after the opaque polygons: alpha 1–30, or an A3I5 or A5I3
    /// texture.
    pub(crate) translucent: bool,
    /// Zero area on screen: drawn as its outline (line segments and dots).
    pub(crate) line: bool,
    /// Top-most and bottom-most rows, for Y-sorting.
    pub(crate) top: i32,
    pub(crate) bottom: i32,
}

/// Polygon and Vertex RAM: everything one frame draws.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PolygonList {
    pub(crate) polygons: Vec<Polygon>,
    pub(crate) vertices: Vec<ScreenVertex>,
    /// Vertex RAM entries in use; strips share vertices between polygons.
    pub(crate) vertex_ram: usize,
    /// SWAP_BUFFERS bit 0: translucent polygons keep their order.
    pub(crate) manual_sort: bool,
    /// SWAP_BUFFERS bit 1: depth is W rather than Z.
    pub(crate) w_buffer: bool,
}

impl PolygonList {
    /// The vertices of `polygon`, in order around its edge.
    pub(crate) fn vertices_of(&self, polygon: &Polygon) -> &[ScreenVertex] {
        let first = polygon.first as usize;
        &self.vertices[first..first + usize::from(polygon.len)]
    }

    /// Empties the list for a new frame with SWAP_BUFFERS parameter
    /// `params`, keeping the allocations.
    pub(crate) fn restart(&mut self, params: u32) {
        self.polygons.clear();
        self.vertices.clear();
        self.vertex_ram = 0;
        self.manual_sort = params & 1 != 0;
        self.w_buffer = params & 2 != 0;
    }
}
