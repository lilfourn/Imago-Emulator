//! The rendering engine: draws a polygon list into a 256×192 image of
//! 6-bit colour and 5-bit alpha (GBATEK, "DS 3D Overview" and "DS 3D
//! Rear-Plane").
//!
//! The hardware renders each line shortly before it is shown; this draws
//! the whole frame at once. The rear plane comes first, then the opaque
//! polygons sorted by their Y range, then the translucent polygons, sorted
//! the same way unless SWAP_BUFFERS asked for manual order. Edge marking,
//! fog and anti-aliasing follow, in that order.
//!
//! Anti-aliasing applies to the edges of opaque polygons: each edge pixel
//! is drawn with the share of it the polygon covers, over the pixel it
//! hides, and at the end the two are blended by that share. Edge marking
//! replaces an edge's share with half. Simplifications: translucent
//! polygons keep the small-polygon rule rather than growing to full size;
//! line segments, dots and wire-frames stay solid, where the hardware
//! breaks them up (GBATEK suggests drawing them translucent); and
//! overlapping marked edges blend once, not twice.
//!
//! For upscaling (NDS-9) the renderer can also draw each frame `scale`
//! times larger in each direction, for the screen alone: the native frame
//! is still drawn, because display capture puts it in VRAM where the game
//! reads it. The larger frame is drawn in horizontal bands, each on its
//! own thread. A band also draws `scale` rows beyond its own on either
//! side, which is as far as edge marking looks, so every band's rows come
//! out as if the frame were drawn whole.
//!
//! The native frame is drawn in bands too, a few, each on its own thread.
//! At the native resolution alone, it draws while the machine runs on:
//! textures are copied and polygons sorted first, so the drawing reads
//! nothing the machine can change, and the frame is waited for when its
//! first line is needed, about 50 lines later. It comes out the same as
//! if it were drawn at once.

mod pixel;
mod post;
mod raster;
mod shade;
mod texture;

use std::ops::Range;
use std::thread::JoinHandle;

use self::pixel::{Buffers, Kind, PixelAttr, PolygonTarget};
use self::raster::{Area, HEIGHT, WIDTH};
use self::shade::Shader;
use self::texture::TextureMemory;
use super::color;
use super::polygon::{Mode, Polygon, PolygonList};
use super::registers::{RenderConfig, expand_depth};
use crate::vram::Vram;

/// Rear-plane bitmaps: colour in texture slot 2, depth in slot 3.
const REAR_COLOR: u32 = 0x4_0000;
const REAR_DEPTH: u32 = 0x6_0000;

/// The most times the native resolution the renderer draws at.
pub(crate) const MAX_SCALE: usize = 8;

/// Bands the larger frame is split into, each drawn on its own thread.
const BANDS: usize = 16;

/// Bands the native frame is split into, so it is drawn by the time its
/// first line is needed.
const NATIVE_BANDS: usize = 4;

/// What every polygon of a frame shares.
struct Frame<'a> {
    list: &'a PolygonList,
    config: &'a RenderConfig,
    /// Every texture the polygons use, already copied from VRAM.
    memory: &'a TextureMemory,
    /// The toon table, 6 bits per channel.
    toon: [[u8; 3]; 32],
    /// Polygon indices in drawing order.
    order: &'a [usize],
}

/// Rows `owned` of a frame, drawn in buffers that reach a little beyond
/// them.
#[derive(Debug, Clone)]
struct Band {
    owned: Range<usize>,
    buffers: Buffers,
}

/// The frame at the native resolution, which the game sees, and the
/// scratch space for drawing it.
#[derive(Debug)]
struct Canvas {
    /// The native frame, in [`NATIVE_BANDS`] bands.
    bands: Vec<Band>,
    memory: TextureMemory,
    /// Polygon indices in drawing order.
    order: Vec<usize>,
}

impl Default for Canvas {
    fn default() -> Self {
        Self {
            bands: bands(1, NATIVE_BANDS),
            memory: TextureMemory::default(),
            order: Vec::new(),
        }
    }
}

/// A native frame drawing on a worker thread: everything it reads, owned.
struct Job {
    canvas: Box<Canvas>,
    list: PolygonList,
    config: RenderConfig,
    toon: [[u8; 3]; 32],
}

/// The frame buffers and scratch space for drawing.
#[derive(Debug, Default)]
pub(crate) struct Renderer {
    /// The native frame, or `None` while `drawing` draws it.
    canvas: Option<Box<Canvas>>,
    /// The worker thread drawing the native frame, which hands the canvas
    /// back.
    drawing: Option<JoinHandle<Box<Canvas>>>,
    /// The larger frame, for the screen only; none at the native
    /// resolution.
    bands: Vec<Band>,
}

impl Renderer {
    /// Also draws every frame `scale` times larger in each direction, from
    /// the next [`Renderer::render`] on; 1 draws the native frame alone.
    /// `scale` is clamped to 1–[`MAX_SCALE`].
    pub(crate) fn set_scale(&mut self, scale: usize) {
        let scale = scale.clamp(1, MAX_SCALE);
        if scale != self.scale() {
            self.bands = if scale == 1 {
                Vec::new()
            } else {
                bands(scale, BANDS)
            };
        }
    }

    /// Draws the larger frame `scale` times larger in `count` bands.
    #[cfg(test)]
    pub(crate) fn split(&mut self, scale: usize, count: usize) {
        self.bands = bands(scale, count);
    }

    /// Draws the native frame in `count` bands.
    #[cfg(test)]
    pub(crate) fn split_native(&mut self, count: usize) {
        self.canvas().bands = bands(1, count);
    }

    /// How many times larger than the native frame the larger one is; 1
    /// when there is none.
    pub(crate) fn scale(&self) -> usize {
        self.bands.first().map_or(1, |band| band.buffers.area.scale)
    }

    /// The native frame, waiting for the worker thread drawing it if
    /// there is one.
    fn canvas(&mut self) -> &mut Canvas {
        if let Some(drawing) = self.drawing.take() {
            let canvas = drawing
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
            self.canvas = Some(canvas);
        }
        self.canvas.get_or_insert_default()
    }

    /// Draws `list` with the rendering registers in `config`, reading
    /// textures from `vram`. At the native resolution alone the drawing
    /// finishes on a worker thread; [`Renderer::line`] waits for it.
    pub(crate) fn render(&mut self, list: &PolygonList, config: &RenderConfig, vram: &Vram) {
        let toon = config.toon.map(|entry| color::rgb6(u32::from(entry)));
        let canvas = self.canvas();
        canvas.load_textures(list, config, &toon, vram);
        canvas.sort(list);
        if self.bands.is_empty() {
            let job = Job {
                canvas: self.canvas.take().expect("the canvas is back"),
                list: list.clone(),
                config: config.clone(),
                toon,
            };
            self.drawing = Some(std::thread::spawn(move || job.draw()));
            return;
        }
        let canvas = self.canvas.as_deref_mut().expect("the canvas is back");
        let frame = Frame {
            list,
            config,
            memory: &canvas.memory,
            toon,
            order: &canvas.order,
        };
        draw_bands(self.bands.iter_mut().chain(&mut canvas.bands), &frame);
    }

    /// Row `y` of the last frame drawn, once it is drawn.
    pub(crate) fn line(&mut self, y: usize) -> &[u32; WIDTH] {
        static BLANK: [u32; WIDTH] = [0; WIDTH];
        band_row(&self.canvas().bands, y)
            .and_then(|row| row.try_into().ok())
            .unwrap_or(&BLANK)
    }

    /// Row `row` of the last larger frame drawn, `256 * scale` pixels, if
    /// there is one.
    pub(crate) fn scaled_row(&self, row: usize) -> Option<&[u32]> {
        band_row(&self.bands, row)
    }
}

impl Job {
    /// Draws the frame and hands the canvas back.
    fn draw(mut self) -> Box<Canvas> {
        let canvas = &mut *self.canvas;
        let frame = Frame {
            list: &self.list,
            config: &self.config,
            memory: &canvas.memory,
            toon: self.toon,
            order: &canvas.order,
        };
        draw_bands(&mut canvas.bands, &frame);
        self.canvas
    }
}

/// Draws `frame` into every band, each on its own thread.
fn draw_bands<'a>(bands: impl IntoIterator<Item = &'a mut Band>, frame: &Frame<'_>) {
    std::thread::scope(|scope| {
        for band in bands {
            scope.spawn(move || draw_frame(&mut band.buffers, frame));
        }
    });
}

/// Row `row` of a frame drawn in `bands`, from the band that owns it.
fn band_row(bands: &[Band], row: usize) -> Option<&[u32]> {
    let band = bands.iter().find(|band| band.owned.contains(&row))?;
    let area = band.buffers.area;
    let start = area.index(0, row);
    band.buffers.color.get(start..start + area.width())
}

impl Canvas {
    /// Copies every texture the frame reads from VRAM: the rear-plane
    /// bitmap and each polygon's texture and palette.
    fn load_textures(
        &mut self,
        list: &PolygonList,
        config: &RenderConfig,
        toon: &[[u8; 3]; 32],
        vram: &Vram,
    ) {
        self.memory.invalidate();
        if config.rear_bitmap {
            self.memory.load_texture(vram, REAR_COLOR, 0x4_0000);
        }
        for polygon in &list.polygons {
            if let Some(sampler) = Shader::new(polygon, config, toon).sampler() {
                sampler.load(&mut self.memory, vram);
            }
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
}

/// Splits a frame `scale` times larger than the screen into `count`
/// bands, each drawing `scale` rows past its own on both sides.
fn bands(scale: usize, count: usize) -> Vec<Band> {
    let height = HEIGHT * scale;
    (0..count)
        .map(|band| {
            let owned = height * band / count..height * (band + 1) / count;
            let top = owned.start.saturating_sub(scale);
            let bottom = (owned.end + scale).min(height);
            let area = Area {
                scale,
                top,
                rows: bottom - top,
            };
            Band {
                owned,
                buffers: Buffers::new(area),
            }
        })
        .collect()
}

/// Draws `frame` into `buffers`, then marks edges, adds fog and smooths
/// edges as the registers ask.
fn draw_frame(buffers: &mut Buffers, frame: &Frame<'_>) {
    clear(buffers, frame);
    let mut shadow_drawn = false;
    for &index in frame.order {
        draw(
            buffers,
            &frame.list.polygons[index],
            frame,
            &mut shadow_drawn,
        );
    }
    let config = frame.config;
    if config.edge_marking {
        post::mark_edges(buffers, config);
    }
    if config.fog {
        post::apply_fog(buffers, config);
    }
    if config.antialiasing {
        post::antialias(buffers);
    }
}

/// Fills the top layer of `buffers` with the rear plane. The layer beneath
/// is only read under anti-aliased edges, which fill it.
fn clear(buffers: &mut Buffers, frame: &Frame<'_>) {
    let config = frame.config;
    let clear_id = ((config.clear_color >> 24) & 0x3F) as u8;
    let area = buffers.area;
    let layer = buffers.layer;
    let pixels = &mut buffers.color[..layer];
    let depths = &mut buffers.depth[..layer];
    let attrs = &mut buffers.attrs[..layer];
    buffers.stencil.fill(false);
    if !config.rear_bitmap {
        let alpha = ((config.clear_color >> 16) & 0x1F) as u8;
        pixels.fill(color::pack(color::rgb6(config.clear_color), alpha));
        depths.fill(config.clear_depth);
        attrs.fill(PixelAttr::rear_plane(
            clear_id,
            config.clear_color & 0x8000 != 0,
        ));
        return;
    }
    let (offset_x, offset_y) = config.clear_offset;
    let width = area.width();
    for (index, ((pixel, depth), attr)) in pixels
        .iter_mut()
        .zip(depths.iter_mut())
        .zip(attrs.iter_mut())
        .enumerate()
    {
        // The screen pixel under this one.
        let x = ((index % width / area.scale) as u32 + offset_x) & 0xFF;
        let y = (((area.top + index / width) / area.scale) as u32 + offset_y) & 0xFF;
        let offset = (y * 256 + x) * 2;
        let rear_color = frame.memory.texture_16(REAR_COLOR + offset);
        let rear_depth = frame.memory.texture_16(REAR_DEPTH + offset);
        let alpha = if rear_color & 0x8000 != 0 { 31 } else { 0 };
        *pixel = color::pack(color::rgb6(u32::from(rear_color)), alpha);
        *depth = expand_depth(u32::from(rear_depth));
        *attr = PixelAttr::rear_plane(clear_id, rear_depth & 0x8000 != 0);
    }
}

/// Draws one polygon into `buffers`. `shadow_drawn` tracks whether a
/// shadow polygon has drawn since the last shadow mask.
fn draw(buffers: &mut Buffers, polygon: &Polygon, frame: &Frame<'_>, shadow_drawn: &mut bool) {
    let (list, config) = (frame.list, frame.config);
    let attr = polygon.attr;
    let shader = Shader::new(polygon, config, &frame.toon);
    let kind = match (attr.mode(), attr.id()) {
        (Mode::Shadow, 0) => {
            // A new shadow volume starts with a clean stencil.
            if std::mem::take(shadow_drawn) {
                buffers.stencil.fill(false);
            }
            Kind::ShadowMask
        }
        (Mode::Shadow, _) => {
            *shadow_drawn = true;
            Kind::Shadow
        }
        _ => Kind::Normal,
    };
    let area = buffers.area;
    let mut target = PolygonTarget {
        buffers,
        memory: frame.memory,
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
        antialiasing: config.antialiasing,
    };
    let vertices = list.vertices_of(polygon);
    if polygon.line || attr.alpha() == 0 {
        raster::outline(vertices, list.w_buffer, area, &mut target);
    } else if config.antialiasing && kind == Kind::Normal && !polygon.translucent {
        raster::fill_antialiased(vertices, list.w_buffer, area, &mut target);
    } else {
        raster::fill(vertices, list.w_buffer, area, &mut target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_share_out_every_row_once() {
        for scale in 2..=MAX_SCALE {
            let bands = bands(scale, BANDS);
            let mut next = 0;
            for band in &bands {
                assert_eq!(band.owned.start, next);
                next = band.owned.end;
                let area = band.buffers.area;
                // Edge marking reaches `scale` rows: one to the neighbour,
                // then up to `scale - 1` back over the polygon.
                assert!(area.top + scale <= band.owned.start || area.top == 0);
                assert!(band.owned.end + scale <= area.bottom() || area.bottom() == HEIGHT * scale);
            }
            assert_eq!(next, HEIGHT * scale);
        }
    }
}
