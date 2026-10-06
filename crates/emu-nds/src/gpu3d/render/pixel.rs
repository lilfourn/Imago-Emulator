//! Writing polygon pixels into the frame: depth test, alpha test, opaque
//! and translucent pixels, polygon IDs, fog flags and shadow polygons
//! (GBATEK, "DS 3D Polygon Attributes", "DS 3D Shadow Polygons" and "DS 3D
//! Toon, Edge, Fog, Alpha-Blending, Anti-Aliasing").

use super::raster::{HEIGHT, Target, WIDTH};
use super::shade::Shader;
use super::texture::TextureMemory;
use crate::gpu3d::color;

/// No translucent pixel has been drawn here.
pub(crate) const NO_ID: u8 = 0xFF;

/// The attribute buffer: what a pixel holds besides colour and depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PixelAttr {
    pub(crate) opaque_id: u8,
    /// Polygon ID of the last translucent pixel drawn here, or [`NO_ID`].
    pub(crate) translucent_id: u8,
    pub(crate) fog: bool,
    /// An opaque polygon drew here, so edge marking may apply.
    pub(crate) opaque: bool,
}

/// The frame being drawn, indexed by `y * 256 + x`.
#[derive(Debug, Clone)]
pub(crate) struct Buffers {
    /// Output pixels: 6-bit colour and 5-bit alpha.
    pub(crate) color: Box<[u32]>,
    /// 24-bit depth.
    pub(crate) depth: Box<[u32]>,
    pub(crate) attrs: Box<[PixelAttr]>,
    pub(crate) stencil: Box<[bool]>,
}

impl Default for Buffers {
    fn default() -> Self {
        let pixels = WIDTH * HEIGHT;
        let attr = PixelAttr {
            opaque_id: 0,
            translucent_id: NO_ID,
            fog: false,
            opaque: false,
        };
        Self {
            color: vec![0; pixels].into_boxed_slice(),
            depth: vec![0; pixels].into_boxed_slice(),
            attrs: vec![attr; pixels].into_boxed_slice(),
            stencil: vec![false; pixels].into_boxed_slice(),
        }
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
}

impl Target for PolygonTarget<'_> {
    fn test(&mut self, index: usize, depth: u32) -> bool {
        let passes = self.depth_passes(depth, self.buffers.depth[index]);
        match self.kind {
            Kind::Normal => passes,
            Kind::ShadowMask => {
                if !passes {
                    self.buffers.stencil[index] = true;
                }
                false
            }
            Kind::Shadow => {
                let marked = std::mem::take(&mut self.buffers.stencil[index]);
                marked && passes && self.buffers.attrs[index].opaque_id != self.id
            }
        }
    }

    fn shade(&mut self, index: usize, depth: u32, vertex: [u8; 3], texcoord: [i32; 2]) {
        let (rgb, alpha) = self.shader.shade(self.memory, vertex, texcoord);
        if alpha <= self.alpha_min {
            return;
        }
        let attr = &mut self.buffers.attrs[index];
        if alpha == 31 && self.kind == Kind::Normal {
            self.buffers.color[index] = color::pack(rgb, 31);
            self.buffers.depth[index] = depth;
            *attr = PixelAttr {
                opaque_id: self.id,
                translucent_id: NO_ID,
                fog: self.fog,
                opaque: true,
            };
            return;
        }
        // Translucent polygons with the same ID never cover each other.
        if attr.translucent_id == self.id {
            return;
        }
        attr.translucent_id = self.id;
        attr.fog &= self.fog;
        self.buffers.color[index] = self.blend(rgb, alpha, self.buffers.color[index]);
        if self.depth_update {
            self.buffers.depth[index] = depth;
        }
    }
}
