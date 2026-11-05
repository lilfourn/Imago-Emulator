//! A polygon pixel's colour: the vertex colour combined with its texel by
//! the polygon mode (GBATEK, "DS 3D Texture Blending").
//!
//! Colours have 6 bits per channel and alpha 5 bits. Without a texture the
//! texel is opaque white, which makes modulation return the vertex colour.

use super::texture::{Sampler, Texel, TextureMemory};
use crate::gpu3d::color;
use crate::gpu3d::polygon::{Mode, Polygon};
use crate::gpu3d::registers::RenderConfig;

const WHITE: Texel = Texel {
    rgb: [63; 3],
    alpha: 31,
};

fn modulate(a: u8, b: u8) -> u8 {
    (((u32::from(a) + 1) * (u32::from(b) + 1) - 1) >> 6) as u8
}

fn modulate_alpha(a: u8, b: u8) -> u8 {
    (((u32::from(a) + 1) * (u32::from(b) + 1) - 1) >> 5) as u8
}

/// The texture, mode and tables one polygon's pixels share.
#[derive(Debug, Clone)]
pub(crate) struct Shader<'a> {
    mode: Mode,
    highlight: bool,
    /// Polygon alpha; wire-frames draw their edges solid.
    alpha: u8,
    sampler: Option<Sampler>,
    /// The toon table, 6 bits per channel.
    toon: &'a [[u8; 3]; 32],
}

impl<'a> Shader<'a> {
    pub(crate) fn new(polygon: &Polygon, config: &RenderConfig, toon: &'a [[u8; 3]; 32]) -> Self {
        let alpha = match polygon.attr.alpha() {
            0 => 31,
            alpha => alpha,
        };
        let sampler = if config.texturing {
            Sampler::new(polygon.texture, polygon.palette)
        } else {
            None
        };
        Self {
            mode: polygon.attr.mode(),
            highlight: config.highlight,
            alpha,
            sampler,
            toon,
        }
    }

    pub(crate) fn sampler(&self) -> Option<&Sampler> {
        self.sampler.as_ref()
    }

    /// Colour and alpha for the interpolated vertex colour and 12.4
    /// texture coordinates. Inlined into the rasterizer's loop, as
    /// [`Sampler::sample`] is.
    #[inline(always)]
    pub(crate) fn shade(
        &self,
        memory: &TextureMemory,
        vertex: [u8; 3],
        texcoord: [i32; 2],
    ) -> ([u8; 3], u8) {
        let texel = match &self.sampler {
            Some(sampler) => sampler.sample(memory, texcoord[0] >> 4, texcoord[1] >> 4),
            None => WHITE,
        };
        match self.mode {
            Mode::Decal if self.sampler.is_some() => {
                let rgb = match texel.alpha {
                    0 => vertex,
                    31 => texel.rgb,
                    alpha => {
                        let weight = u32::from(color::expand(u32::from(alpha)));
                        std::array::from_fn(|i| {
                            ((u32::from(texel.rgb[i]) * weight
                                + u32::from(vertex[i]) * (63 - weight))
                                >> 6) as u8
                        })
                    }
                };
                (rgb, self.alpha)
            }
            Mode::Decal => (vertex, self.alpha),
            // Highlight shading modulates by a grey of the vertex red, then
            // adds the table colour.
            Mode::Toon => {
                let toon = self.toon[usize::from(vertex[0] >> 1)];
                let rgb = if self.highlight {
                    std::array::from_fn(|i| (modulate(texel.rgb[i], vertex[0]) + toon[i]).min(63))
                } else {
                    std::array::from_fn(|i| modulate(texel.rgb[i], toon[i]))
                };
                (rgb, modulate_alpha(texel.alpha, self.alpha))
            }
            Mode::Modulation | Mode::Shadow => (
                std::array::from_fn(|i| modulate(texel.rgb[i], vertex[i])),
                modulate_alpha(texel.alpha, self.alpha),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu3d::polygon::PolygonAttr;
    use crate::gpu3d::registers::RenderRegisters;

    /// Shades one untextured pixel with POLYGON_ATTR `attr`, DISP3DCNT
    /// `control` and some toon table entries.
    fn shade(attr: u32, control: u16, toon: &[(usize, u16)], vertex: [u8; 3]) -> ([u8; 3], u8) {
        let mut registers = RenderRegisters::default();
        registers.write_control(control, 0xFFFF);
        for &(index, value) in toon {
            let [low, high] = value.to_le_bytes();
            registers.write_byte(0x380 + 2 * index as u32, low);
            registers.write_byte(0x381 + 2 * index as u32, high);
        }
        let config = registers.config();
        let table = config.toon.map(|entry| color::rgb6(u32::from(entry)));
        let polygon = Polygon {
            attr: PolygonAttr(attr),
            ..Polygon::default()
        };
        Shader::new(&polygon, &config, &table).shade(&TextureMemory::default(), vertex, [0, 0])
    }

    #[test]
    fn untextured_modulation_is_the_vertex_colour() {
        assert_eq!(shade(31 << 16, 0, &[], [10, 20, 30]), ([10, 20, 30], 31));
    }

    #[test]
    fn toon_replaces_the_colour_by_the_red_index() {
        // Red 11 indexes entry 5.
        let (rgb, _) = shade(31 << 16 | 2 << 4, 0, &[(5, 0x7C00)], [11, 0, 0]);
        assert_eq!(rgb, [0, 0, 63]);
    }

    #[test]
    fn highlight_adds_the_table_to_a_grey_base() {
        let (rgb, _) = shade(31 << 16 | 2 << 4, 1 << 1, &[(5, 0x001F)], [11, 0, 0]);
        assert_eq!(rgb, [63, 11, 11]);
    }

    #[test]
    fn wire_frames_are_solid() {
        assert_eq!(shade(0, 0, &[], [1, 2, 3]).1, 31);
    }
}
