//! The rendering engine: draws a polygon list into a 256×192 image of
//! 6-bit colour and 5-bit alpha (GBATEK, "DS 3D Overview" and "DS 3D
//! Rear-Plane").
//!
//! The hardware renders each line shortly before it is shown; this draws
//! the whole frame at once. The rear plane comes first, then the opaque
//! polygons sorted by their Y range, then the translucent polygons, sorted
//! the same way unless SWAP_BUFFERS asked for manual order. Edge marking
//! and fog follow. Anti-aliasing is not emulated: edges stay hard.

mod pixel;
mod post;
mod raster;
mod shade;
mod texture;

use self::pixel::{Buffers, Kind, NO_ID, PixelAttr, PolygonTarget};
use self::raster::WIDTH;
use self::shade::Shader;
use self::texture::TextureMemory;
use super::color;
use super::polygon::{Mode, Polygon, PolygonList};
use super::registers::{RenderConfig, expand_depth};
use crate::vram::Vram;

/// Rear-plane bitmaps: colour in texture slot 2, depth in slot 3.
const REAR_COLOR: u32 = 0x4_0000;
const REAR_DEPTH: u32 = 0x6_0000;

/// What every polygon of a frame shares.
struct Frame<'a> {
    list: &'a PolygonList,
    config: &'a RenderConfig,
    vram: &'a Vram,
    /// The toon table, 6 bits per channel.
    toon: [[u8; 3]; 32],
}

/// The frame buffers and scratch space for drawing.
#[derive(Debug, Clone, Default)]
pub(crate) struct Renderer {
    buffers: Buffers,
    memory: TextureMemory,
    /// Polygon indices in drawing order.
    order: Vec<usize>,
}

impl Renderer {
    /// Draws `list` with the rendering registers in `config`, reading
    /// textures from `vram`.
    pub(crate) fn render(&mut self, list: &PolygonList, config: &RenderConfig, vram: &Vram) {
        self.memory.invalidate();
        self.clear(config, vram);
        self.sort(list);
        let frame = Frame {
            list,
            config,
            vram,
            toon: config.toon.map(|entry| color::rgb6(u32::from(entry))),
        };
        let order = std::mem::take(&mut self.order);
        let mut shadow_drawn = false;
        for &index in &order {
            self.draw(&list.polygons[index], &frame, &mut shadow_drawn);
        }
        self.order = order;
        if config.edge_marking {
            post::mark_edges(&mut self.buffers, config);
        }
        if config.fog {
            post::apply_fog(&mut self.buffers, config);
        }
    }

    /// Row `y` of the last frame drawn.
    pub(crate) fn line(&self, y: usize) -> &[u32; WIDTH] {
        static BLANK: [u32; WIDTH] = [0; WIDTH];
        self.buffers
            .color
            .get(y * WIDTH..(y + 1) * WIDTH)
            .and_then(|row| row.try_into().ok())
            .unwrap_or(&BLANK)
    }

    /// Fills the buffers with the rear plane.
    fn clear(&mut self, config: &RenderConfig, vram: &Vram) {
        let clear_id = ((config.clear_color >> 24) & 0x3F) as u8;
        let buffers = &mut self.buffers;
        buffers.stencil.fill(false);
        if !config.rear_bitmap {
            let alpha = ((config.clear_color >> 16) & 0x1F) as u8;
            buffers
                .color
                .fill(color::pack(color::rgb6(config.clear_color), alpha));
            buffers.depth.fill(config.clear_depth);
            buffers.attrs.fill(PixelAttr {
                opaque_id: clear_id,
                translucent_id: NO_ID,
                fog: config.clear_color & 0x8000 != 0,
                opaque: false,
            });
            return;
        }
        self.memory.load_texture(vram, REAR_COLOR, 0x4_0000);
        let (offset_x, offset_y) = config.clear_offset;
        for (index, ((pixel, depth), attr)) in buffers
            .color
            .iter_mut()
            .zip(buffers.depth.iter_mut())
            .zip(buffers.attrs.iter_mut())
            .enumerate()
        {
            let x = (index as u32 % 256 + offset_x) & 0xFF;
            let y = (index as u32 / 256 + offset_y) & 0xFF;
            let offset = (y * 256 + x) * 2;
            let rear_color = self.memory.texture_16(REAR_COLOR + offset);
            let rear_depth = self.memory.texture_16(REAR_DEPTH + offset);
            let alpha = if rear_color & 0x8000 != 0 { 31 } else { 0 };
            *pixel = color::pack(color::rgb6(u32::from(rear_color)), alpha);
            *depth = expand_depth(u32::from(rear_depth));
            *attr = PixelAttr {
                opaque_id: clear_id,
                translucent_id: NO_ID,
                fog: rear_depth & 0x8000 != 0,
                opaque: false,
            };
        }
    }

    /// Opaque polygons by their Y range, then translucent ones.
    fn sort(&mut self, list: &PolygonList) {
        let polygons = &list.polygons;
        let key = |&index: &usize| (polygons[index].bottom, polygons[index].top);
        self.order.clear();
        self.order
            .extend((0..polygons.len()).filter(|&i| !polygons[i].translucent));
        self.order.sort_by_key(key);
        let opaque = self.order.len();
        self.order
            .extend((0..polygons.len()).filter(|&i| polygons[i].translucent));
        if !list.manual_sort {
            self.order[opaque..].sort_by_key(key);
        }
    }

    /// Draws one polygon. `shadow_drawn` tracks whether a shadow polygon
    /// has drawn since the last shadow mask.
    fn draw(&mut self, polygon: &Polygon, frame: &Frame<'_>, shadow_drawn: &mut bool) {
        let (list, config) = (frame.list, frame.config);
        let attr = polygon.attr;
        let shader = Shader::new(polygon, config, &frame.toon);
        if let Some(sampler) = shader.sampler() {
            sampler.load(&mut self.memory, frame.vram);
        }
        let kind = match (attr.mode(), attr.id()) {
            (Mode::Shadow, 0) => {
                // A new shadow volume starts with a clean stencil.
                if std::mem::take(shadow_drawn) {
                    self.buffers.stencil.fill(false);
                }
                Kind::ShadowMask
            }
            (Mode::Shadow, _) => {
                *shadow_drawn = true;
                Kind::Shadow
            }
            _ => Kind::Normal,
        };
        let mut target = PolygonTarget {
            buffers: &mut self.buffers,
            memory: &self.memory,
            shader: &shader,
            kind,
            id: attr.id(),
            depth_equal: attr.depth_equal(),
            depth_update: attr.depth_update(),
            fog: attr.fog(),
            alpha_min: if config.alpha_test {
                config.alpha_ref
            } else {
                0
            },
            blending: config.blending,
        };
        let vertices = list.vertices_of(polygon);
        if polygon.line || attr.alpha() == 0 {
            raster::outline(vertices, list.w_buffer, &mut target);
        } else {
            raster::fill(vertices, list.w_buffer, &mut target);
        }
    }
}
