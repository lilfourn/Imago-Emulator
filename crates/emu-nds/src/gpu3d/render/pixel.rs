//! Writing polygon pixels into the frame: depth test, alpha test, opaque
//! and translucent pixels, polygon IDs, fog flags, shadow polygons and
//! anti-aliased edges (GBATEK, "DS 3D Polygon Attributes", "DS 3D Shadow
//! Polygons" and "DS 3D Toon, Edge, Fog, Alpha-Blending, Anti-Aliasing").
//!
//! With anti-aliasing, the frame keeps a second layer, as the hardware
//! does: an opaque edge pixel pushes the pixel it covers down into it, to
//! be blended with by the edge's coverage once the frame is drawn. A pixel
//! that fails the depth test against an edge is tested against the pixel
//! beneath it instead, so where two polygons share an edge, each shows
//! through the other rather than the rear plane. Translucent pixels on an
//! edge blend into both layers. Shadow polygons only darken the top layer.

use super::raster::{Area, Coverage, Target};
use super::shade::Shader;
use super::texture::TextureMemory;
use crate::gpu3d::color;

/// No translucent pixel has been drawn here.
pub(crate) const NO_ID: u8 = 0xFF;

/// The pixel is not on an opaque polygon's edge.
pub(crate) const NO_EDGE: u8 = 0xFF;

/// The attribute buffer: what a pixel holds besides colour and depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PixelAttr {
    pub(crate) opaque_id: u8,
    /// Polygon ID of the last translucent pixel drawn here, or [`NO_ID`].
    pub(crate) translucent_id: u8,
    pub(crate) fog: bool,
    /// On an opaque polygon's edge, where edge marking and anti-aliasing
    /// apply: how much of the pixel the polygon covers, in 32nds less one.
    /// [`NO_EDGE`] elsewhere.
    pub(crate) edge: u8,
}

impl PixelAttr {
    /// The rear plane's attributes, with its polygon ID and fog flag.
    pub(crate) fn rear_plane(opaque_id: u8, fog: bool) -> Self {
        Self {
            opaque_id,
            translucent_id: NO_ID,
            fog,
            edge: NO_EDGE,
        }
    }

    pub(crate) fn on_edge(self) -> bool {
        self.edge != NO_EDGE
    }

    /// How much of the pixel its opaque polygon covers, in 32nds less one:
    /// 31 but on anti-aliased edges, which blend with the pixel beneath.
    pub(crate) fn coverage(self) -> u8 {
        self.edge.min(31)
    }
}

/// The frame being drawn, or the part of it in `area`. Colour, depth and
/// attributes hold two layers: the visible pixels, indexed by
/// [`Area::index`], then at `index + layer` the pixels beneath
/// anti-aliased edges.
#[derive(Debug, Clone)]
pub(crate) struct Buffers {
    pub(crate) area: Area,
    /// Pixels in a layer: the area's cells.
    pub(crate) layer: usize,
    /// Output pixels: 6-bit colour and 5-bit alpha.
    pub(crate) color: Box<[u32]>,
    /// 24-bit depth.
    pub(crate) depth: Box<[u32]>,
    pub(crate) attrs: Box<[PixelAttr]>,
    /// Shadow volume marks, for the top layer only.
    pub(crate) stencil: Box<[bool]>,
}

impl Buffers {
    /// Cleared buffers for `area`.
    pub(crate) fn new(area: Area) -> Self {
        let layer = area.cells();
        let attr = PixelAttr::rear_plane(0, false);
        Self {
            area,
            layer,
            color: vec![0; 2 * layer].into_boxed_slice(),
            depth: vec![0; 2 * layer].into_boxed_slice(),
            attrs: vec![attr; 2 * layer].into_boxed_slice(),
            stencil: vec![false; layer].into_boxed_slice(),
        }
    }
}

impl Default for Buffers {
    /// The whole frame at the native resolution.
    fn default() -> Self {
        Self::new(Area::NATIVE)
    }
}

/// How a polygon's pixels reach the buffers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Normal,
    /// Shadow polygon with ID 0: marks the stencil where it is hidden.
    ShadowMask,
    /// Shadow polygon with another ID: draws only on marked pixels.
    Shadow,
}

/// One polygon's view of the frame.
pub(crate) struct PolygonTarget<'a> {
    pub(crate) buffers: &'a mut Buffers,
    pub(crate) memory: &'a TextureMemory,
    pub(crate) shader: &'a Shader<'a>,
    pub(crate) kind: Kind,
    pub(crate) id: u8,
    pub(crate) depth_equal: bool,
    pub(crate) depth_update: bool,
    pub(crate) fog: bool,
    /// Pixels need an alpha above this: ALPHA_TEST_REF, or 0.
    pub(crate) alpha_min: u8,
    pub(crate) blending: bool,
    pub(crate) antialiasing: bool,
}

impl PolygonTarget<'_> {
    fn depth_passes(&self, new: u32, old: u32) -> bool {
        if self.depth_equal {
            new.abs_diff(old) <= 0x200
        } else {
            new < old
        }
    }

    fn blend(&self, rgb: [u8; 3], alpha: u8, old: u32) -> u32 {
        let old_alpha = color::alpha(old);
        if !self.blending || old_alpha == 0 {
            return color::pack(rgb, alpha);
        }
        let old_rgb = color::unpack(old);
        let weight = u32::from(alpha);
        let mixed = std::array::from_fn(|i| {
            ((u32::from(rgb[i]) * (weight + 1) + u32::from(old_rgb[i]) * (31 - weight)) >> 5) as u8
        });
        color::pack(mixed, alpha.max(old_alpha))
    }

    /// Blends a translucent pixel into `slot`.
    fn blend_into(&mut self, slot: usize, rgb: [u8; 3], alpha: u8, depth: u32) {
        let attr = &mut self.buffers.attrs[slot];
        // Translucent polygons with the same ID never cover each other.
        if attr.translucent_id == self.id {
            return;
        }
        attr.translucent_id = self.id;
        attr.fog &= self.fog;
        self.buffers.color[slot] = self.blend(rgb, alpha, self.buffers.color[slot]);
        if self.depth_update {
            self.buffers.depth[slot] = depth;
        }
    }
}

impl Target for PolygonTarget<'_> {
    fn test(&mut self, index: usize, depth: u32) -> Option<usize> {
        let passes = self.depth_passes(depth, self.buffers.depth[index]);
        match self.kind {
            Kind::Normal if passes => Some(index),
            // A pixel behind an anti-aliased edge may still show through.
            Kind::Normal if self.antialiasing && self.buffers.attrs[index].on_edge() => {
                let beneath = index + self.buffers.layer;
                self.depth_passes(depth, self.buffers.depth[beneath])
                    .then_some(beneath)
            }
            Kind::Normal => None,
            Kind::ShadowMask => {
                if !passes {
                    self.buffers.stencil[index] = true;
                }
                None
            }
            Kind::Shadow => {
                let marked = std::mem::take(&mut self.buffers.stencil[index]);
                (marked && passes && self.buffers.attrs[index].opaque_id != self.id)
                    .then_some(index)
            }
        }
    }

    fn shade(
        &mut self,
        slot: usize,
        depth: u32,
        vertex: [u8; 3],
        texcoord: [i32; 2],
        coverage: Coverage,
    ) {
        let (rgb, alpha) = self.shader.shade(self.memory, vertex, texcoord);
        if alpha <= self.alpha_min {
            return;
        }
        if alpha == 31 && self.kind == Kind::Normal {
            if self.antialiasing && coverage.edge && slot < self.buffers.layer {
                // Keep what the edge covers, to blend with later.
                let beneath = slot + self.buffers.layer;
                self.buffers.color[beneath] = self.buffers.color[slot];
                self.buffers.depth[beneath] = self.buffers.depth[slot];
                self.buffers.attrs[beneath] = self.buffers.attrs[slot];
            }
            self.buffers.color[slot] = color::pack(rgb, 31);
            self.buffers.depth[slot] = depth;
            self.buffers.attrs[slot] = PixelAttr {
                opaque_id: self.id,
                translucent_id: NO_ID,
                fog: self.fog,
                edge: if coverage.edge {
                    coverage.amount
                } else {
                    NO_EDGE
                },
            };
            return;
        }
        self.blend_into(slot, rgb, alpha, depth);
        // An anti-aliased edge shows the pixel beneath it too.
        if self.antialiasing
            && self.kind == Kind::Normal
            && slot < self.buffers.layer
            && self.buffers.attrs[slot].on_edge()
        {
            self.blend_into(slot + self.buffers.layer, rgb, alpha, depth);
        }
    }
}
