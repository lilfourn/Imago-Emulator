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
//! reads it. A band of the larger frame also draws `scale` rows beyond its
//! own on either side, which is as far as edge marking looks, so every
//! band's rows come out as if the frame were drawn whole.
//!
//! Both frames draw in bands on a few [`workers`] threads, while the
//! machine runs on: textures are copied and polygons sorted first, so the
//! drawing reads nothing the machine can change. A native band is waited
//! for when its first line is needed, the first about 50 lines later, and
//! the larger frame when the screens are finished at the next VBlank. Each
//! comes out the same as if it were drawn at once.

mod pixel;
mod post;
mod raster;
mod shade;
mod texture;
mod workers;

use std::any::Any;
use std::ops::Range;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

use self::pixel::{Buffers, Kind, PixelAttr, PolygonTarget};
use self::raster::{Area, HEIGHT, WIDTH};
use self::shade::Shader;
use self::texture::TextureMemory;
pub(crate) use self::workers::Job;
use self::workers::Workers;
use super::color;
use super::polygon::{Mode, Polygon, PolygonList};
use super::registers::{RenderConfig, RenderRegisters, expand_depth};
use crate::vram::Vram;

/// Rear-plane bitmaps: colour in texture slot 2, depth in slot 3.
const REAR_COLOR: u32 = 0x4_0000;
const REAR_DEPTH: u32 = 0x6_0000;

/// The most times the native resolution the renderer draws at.
pub(crate) const MAX_SCALE: usize = 8;

/// Bands the larger frame is split into.
const BANDS: usize = 16;

/// Bands the native frame is split into, so its first lines are drawn
/// soon. On Platinum's overworld the machine waited 30–90 µs a frame for
/// them with 4 bands, and about 3 µs with 8.
const NATIVE_BANDS: usize = 8;

/// Everything a frame's polygons are drawn from, copied when the frame
/// starts.
#[derive(Debug, Clone)]
struct Scene {
    list: PolygonList,
    config: RenderConfig,
    /// Every texture the polygons use, copied from VRAM.
    memory: TextureMemory,
    /// The toon table, 6 bits per channel.
    toon: [[u8; 3]; 32],
    /// Polygon indices in drawing order.
    order: Vec<usize>,
}

impl Default for Scene {
    fn default() -> Self {
        Self {
            list: PolygonList::default(),
            config: RenderRegisters::default().config(),
            memory: TextureMemory::default(),
            toon: [[0; 3]; 32],
            order: Vec::new(),
        }
    }
}

/// Rows `owned` of a frame, drawn in buffers that reach a little beyond
/// them.
#[derive(Debug)]
struct Band {
    owned: Range<usize>,
    /// `None` while a worker draws the band.
    buffers: Option<Buffers>,
}

/// Which of the renderer's frames a band belongs to.
#[derive(Debug, Clone, Copy)]
enum Resolution {
    Native,
    Larger,
}

/// A band a worker has drawn, handed back.
struct Drawn {
    resolution: Resolution,
    index: usize,
    buffers: Buffers,
    /// The panic that stopped the drawing, to raise on the machine's
    /// thread.
    panic: Option<Box<dyn Any + Send>>,
}

/// The frame buffers and the threads drawing them.
#[derive(Debug)]
pub(crate) struct Renderer {
    /// What the last frame was drawn from, shared with the workers while
    /// they draw it.
    scene: Arc<Scene>,
    /// The native frame, in [`NATIVE_BANDS`] bands; none before the first
    /// non-empty frame.
    native: Vec<Band>,
    /// The larger frame, for the screen only; none at the native
    /// resolution or before the first non-empty frame.
    larger: Vec<Band>,
    /// When the last frame had no polygons and no rear-plane bitmap, every
    /// pixel holds this single colour, avoiding worker wakeups and buffer
    /// clears.
    uniform: Option<u32>,
    /// One native row filled with [`Self::uniform`].
    uniform_native: [u32; WIDTH],
    /// One scaled row filled with [`Self::uniform`] when `scale > 1`.
    uniform_larger: Vec<u32>,
    /// How many times larger than the native frame the larger one is.
    scale: usize,
    workers: Workers,
    /// Where workers hand bands back.
    sender: Sender<Drawn>,
    receiver: Receiver<Drawn>,
    /// Bands out with the workers.
    drawing: usize,
}

impl Default for Renderer {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            scene: Arc::default(),
            native: Vec::new(),
            larger: Vec::new(),
            uniform: None,
            uniform_native: [0; WIDTH],
            uniform_larger: Vec::new(),
            scale: 1,
            workers: Workers::default(),
            sender,
            receiver,
            drawing: 0,
        }
    }
}

impl Renderer {
    /// Also draws every frame `scale` times larger in each direction, from
    /// the next [`Renderer::render`] on; 1 draws the native frame alone.
    /// `scale` is clamped to 1–[`MAX_SCALE`].
    pub(crate) fn set_scale(&mut self, scale: usize) {
        let scale = scale.clamp(1, MAX_SCALE);
        if scale != self.scale {
            self.finish();
            self.scale = scale;
            self.larger.clear();
            if let Some(pixel) = self.uniform {
                if scale > 1 {
                    self.uniform_larger.resize(WIDTH * scale, pixel);
                    self.uniform_larger.fill(pixel);
                } else {
                    self.uniform_larger.clear();
                }
            }
        }
    }

    /// Draws the larger frame `scale` times larger in `count` bands.
    #[cfg(test)]
    pub(crate) fn split(&mut self, scale: usize, count: usize) {
        self.finish();
        self.scale = scale;
        self.uniform = None;
        self.larger = bands(scale, count);
    }

    /// Draws the native frame in `count` bands.
    #[cfg(test)]
    pub(crate) fn split_native(&mut self, count: usize) {
        self.finish();
        self.uniform = None;
        self.native = bands(1, count);
    }

    /// Draws `list` with the rendering registers in `config`, reading
    /// textures from `vram` now. The drawing finishes on the workers;
    /// [`Renderer::line`] and [`Renderer::take_scaled_frame`] wait for it.
    pub(crate) fn render(&mut self, list: &PolygonList, config: &RenderConfig, vram: &Vram) {
        self.finish();
        if list.polygons.is_empty() && !config.rear_bitmap {
            let pixel = uniform_clear_pixel(config);
            let scaled_len = if self.scale > 1 {
                WIDTH * self.scale
            } else {
                0
            };
            if self.uniform != Some(pixel) || self.uniform_larger.len() != scaled_len {
                self.uniform_native.fill(pixel);
                self.uniform_larger.resize(scaled_len, pixel);
                self.uniform_larger.fill(pixel);
                self.uniform = Some(pixel);
            }
            return;
        }
        self.uniform = None;
        if self.native.is_empty() {
            self.native = bands(1, NATIVE_BANDS);
        }
        if self.scale > 1 && self.larger.is_empty() {
            self.larger = bands(self.scale, BANDS);
        }
        // The workers have let go of the scene, so it is reused, not
        // copied.
        Arc::make_mut(&mut self.scene).load(list, config, vram);
        // Native bands first: their lines are needed first.
        let mut jobs = Vec::with_capacity(self.native.len() + self.larger.len());
        for (resolution, bands) in [
            (Resolution::Native, &mut self.native),
            (Resolution::Larger, &mut self.larger),
        ] {
            for (index, band) in bands.iter_mut().enumerate() {
                let buffers = band.buffers.take().expect("every band is back");
                jobs.push(draw_job(
                    Arc::clone(&self.scene),
                    buffers,
                    (resolution, index),
                    self.sender.clone(),
                ));
            }
        }
        self.drawing = jobs.len();
        self.workers.run(jobs);
    }

    /// Row `y` of the last frame drawn, waiting for it if it is still
    /// drawing.
    pub(crate) fn line(&mut self, y: usize) -> &[u32; WIDTH] {
        static BLANK: [u32; WIDTH] = [0; WIDTH];
        if self.uniform.is_some() {
            return if y < HEIGHT {
                &self.uniform_native
            } else {
                &BLANK
            };
        }
        let Some(index) = self.native.iter().position(|band| band.owned.contains(&y)) else {
            return &BLANK;
        };
        while self.native[index].buffers.is_none() {
            self.receive();
        }
        band_row(&self.native[index], y)
            .and_then(|row| row.try_into().ok())
            .unwrap_or(&BLANK)
    }

    /// The last larger frame drawn, waiting for it if it is still drawing.
    #[cfg(test)]
    pub(crate) fn scaled_frame(&mut self) -> ScaledFrame<'_> {
        if self.uniform.is_some() {
            return ScaledFrame(ScaledInner::Uniform(&self.uniform_larger));
        }
        while self.larger.iter().any(|band| band.buffers.is_none()) {
            self.receive();
        }
        ScaledFrame(ScaledInner::Bands(&self.larger))
    }

    /// Takes the last larger frame drawn out of the renderer, waiting for
    /// it if it is still drawing, so that jobs on its threads can read
    /// it. Until [`Renderer::put_scaled_frame`] puts it back the renderer
    /// has none, and must not be asked to draw.
    pub(crate) fn take_scaled_frame(&mut self) -> LentFrame {
        if self.uniform.is_some() {
            return LentFrame(LentInner::Uniform(std::mem::take(&mut self.uniform_larger)));
        }
        while self.larger.iter().any(|band| band.buffers.is_none()) {
            self.receive();
        }
        LentFrame(LentInner::Bands(std::mem::take(&mut self.larger)))
    }

    /// Puts back the frame [`Renderer::take_scaled_frame`] took.
    pub(crate) fn put_scaled_frame(&mut self, frame: LentFrame) {
        match frame.0 {
            LentInner::Bands(bands) => self.larger = bands,
            LentInner::Uniform(row) => self.uniform_larger = row,
        }
    }

    /// Runs `jobs` on the renderer's threads ([`Workers::run`]), for work
    /// that goes with its frames: the screen drawn around the larger one.
    /// They take their turn behind any bands still drawing.
    pub(crate) fn run(&mut self, jobs: Vec<Job>) {
        self.workers.run(jobs);
    }

    /// Waits for every band out with the workers.
    fn finish(&mut self) {
        while self.drawing > 0 {
            self.receive();
        }
    }

    /// Waits for a worker to hand a band back, and puts it in place. A
    /// panic in the worker carries on here.
    fn receive(&mut self) {
        let drawn = self.receiver.recv().expect("the renderer keeps a sender");
        self.drawing -= 1;
        let bands = match drawn.resolution {
            Resolution::Native => &mut self.native,
            Resolution::Larger => &mut self.larger,
        };
        bands[drawn.index].buffers = Some(drawn.buffers);
        if let Some(panic) = drawn.panic {
            std::panic::resume_unwind(panic);
        }
    }
}

/// A job drawing `scene` into `buffers`, band `index` of a frame, that
/// hands them back through `sender`.
fn draw_job(
    scene: Arc<Scene>,
    mut buffers: Buffers,
    (resolution, index): (Resolution, usize),
    sender: Sender<Drawn>,
) -> Job {
    Box::new(move || {
        let drawing = std::panic::catch_unwind(AssertUnwindSafe(|| {
            draw_frame(&mut buffers, &scene);
        }));
        // Let go of the scene first, so the next frame can reuse it.
        drop(scene);
        // Nobody is waiting once the renderer is gone.
        let _ = sender.send(Drawn {
            resolution,
            index,
            buffers,
            panic: drawing.err(),
        });
    })
}

/// A larger frame out of the renderer for a while
/// ([`Renderer::take_scaled_frame`]), for jobs to read.
#[derive(Debug)]
pub(crate) struct LentFrame(LentInner);

#[derive(Debug)]
enum LentInner {
    Bands(Vec<Band>),
    Uniform(Vec<u32>),
}

impl Default for LentFrame {
    fn default() -> Self {
        Self(LentInner::Bands(Vec::new()))
    }
}

impl LentFrame {
    /// Its rows.
    pub(crate) fn rows(&self) -> ScaledFrame<'_> {
        match &self.0 {
            LentInner::Bands(bands) => ScaledFrame(ScaledInner::Bands(bands)),
            LentInner::Uniform(row) => ScaledFrame(ScaledInner::Uniform(row)),
        }
    }
}

/// The rows of a larger frame, which several threads can read at once.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScaledFrame<'a>(ScaledInner<'a>);

#[derive(Debug, Clone, Copy)]
enum ScaledInner<'a> {
    Bands(&'a [Band]),
    Uniform(&'a [u32]),
}

impl<'a> ScaledFrame<'a> {
    /// Row `row`, `256 * scale` pixels; `None` without a larger frame.
    pub(crate) fn row(self, row: usize) -> Option<&'a [u32]> {
        match self.0 {
            ScaledInner::Bands(bands) => {
                let band = bands.iter().find(|band| band.owned.contains(&row))?;
                band_row(band, row)
            }
            ScaledInner::Uniform(pixels) => {
                let scale = pixels.len() / WIDTH;
                (scale > 1 && row < HEIGHT * scale).then_some(pixels)
            }
        }
    }
}

/// The single pixel value of a uniform rear-plane frame (no polygons and no
/// rear-plane bitmap), including fog if the rear plane enables it.
fn uniform_clear_pixel(config: &RenderConfig) -> u32 {
    let alpha = ((config.clear_color >> 16) & 0x1F) as u8;
    let pixel = color::pack(color::rgb6(config.clear_color), alpha);
    if config.fog && config.clear_color & 0x8000 != 0 {
        post::fog_pixel(pixel, config.clear_depth, config)
    } else {
        pixel
    }
}

/// Row `row` of a frame, from `band`, which owns it.
fn band_row(band: &Band, row: usize) -> Option<&[u32]> {
    let buffers = band.buffers.as_ref()?;
    let area = buffers.area;
    let start = area.index(0, row);
    buffers.color.get(start..start + area.width())
}

impl Scene {
    /// Copies everything a frame of `list` reads, with the rendering
    /// registers in `config`, and sorts its polygons.
    fn load(&mut self, list: &PolygonList, config: &RenderConfig, vram: &Vram) {
        self.list.copy_from(list);
        self.config.clone_from(config);
        self.toon = config.toon.map(|entry| color::rgb6(u32::from(entry)));
        self.load_textures(vram);
        self.sort();
    }

    /// Copies every texture the frame reads from VRAM: the rear-plane
    /// bitmap and each polygon's texture and palette.
    fn load_textures(&mut self, vram: &Vram) {
        self.memory.invalidate();
        if self.config.rear_bitmap {
            self.memory.load_texture(vram, REAR_COLOR, 0x4_0000);
        }
        for polygon in &self.list.polygons {
            if let Some(sampler) = Shader::new(polygon, &self.config, &self.toon).sampler() {
                sampler.load(&mut self.memory, vram);
            }
        }
    }

    /// Opaque polygons by their Y range, then translucent ones.
    fn sort(&mut self) {
        let polygons = &self.list.polygons;
        let key = |&index: &usize| (polygons[index].bottom, polygons[index].top);
        self.order.clear();
        self.order
            .extend((0..polygons.len()).filter(|&i| !polygons[i].translucent));
        self.order.sort_by_key(key);
        let opaque = self.order.len();
        self.order
            .extend((0..polygons.len()).filter(|&i| polygons[i].translucent));
        if !self.list.manual_sort {
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
                buffers: Some(Buffers::new(area)),
            }
        })
        .collect()
}

/// Draws `scene` into `buffers`, then marks edges, adds fog and smooths
/// edges as the registers ask.
fn draw_frame(buffers: &mut Buffers, scene: &Scene) {
    clear(buffers, scene);
    let mut shadow_drawn = false;
    for &index in &scene.order {
        draw(
            buffers,
            &scene.list.polygons[index],
            scene,
            &mut shadow_drawn,
        );
    }
    let config = &scene.config;
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
fn clear(buffers: &mut Buffers, scene: &Scene) {
    let config = &scene.config;
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
        let rear_color = scene.memory.texture_16(REAR_COLOR + offset);
        let rear_depth = scene.memory.texture_16(REAR_DEPTH + offset);
        let alpha = if rear_color & 0x8000 != 0 { 31 } else { 0 };
        *pixel = color::pack(color::rgb6(u32::from(rear_color)), alpha);
        *depth = expand_depth(u32::from(rear_depth));
        *attr = PixelAttr::rear_plane(clear_id, rear_depth & 0x8000 != 0);
    }
}

/// Draws one polygon into `buffers`. `shadow_drawn` tracks whether a
/// shadow polygon has drawn since the last shadow mask.
fn draw(buffers: &mut Buffers, polygon: &Polygon, scene: &Scene, shadow_drawn: &mut bool) {
    let (list, config) = (&scene.list, &scene.config);
    let attr = polygon.attr;
    let shader = Shader::new(polygon, config, &scene.toon);
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
        memory: &scene.memory,
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
                let area = band.buffers.as_ref().expect("not drawing").area;
                // Edge marking reaches `scale` rows: one to the neighbour,
                // then up to `scale - 1` back over the polygon.
                assert!(area.top + scale <= band.owned.start || area.top == 0);
                assert!(band.owned.end + scale <= area.bottom() || area.bottom() == HEIGHT * scale);
            }
            assert_eq!(next, HEIGHT * scale);
        }
    }

    #[test]
    fn a_panic_while_drawing_reaches_the_reader() {
        let mut renderer = Renderer {
            native: bands(1, 1),
            ..Renderer::default()
        };
        // A drawing order naming a polygon the list does not have.
        let broken = Scene {
            order: vec![7],
            ..Scene::default()
        };
        let buffers = renderer.native[0].buffers.take().expect("not drawing");
        renderer.drawing = 1;
        let job = draw_job(
            Arc::new(broken),
            buffers,
            (Resolution::Native, 0),
            renderer.sender.clone(),
        );
        renderer.workers.run(vec![job]);
        let reading = std::panic::catch_unwind(AssertUnwindSafe(|| {
            renderer.line(0);
        }));
        assert!(reading.is_err(), "the panic carries on");
        // The band is back, so the next frame draws.
        let config = RenderRegisters::default().config();
        renderer.render(&PolygonList::default(), &config, &Vram::default());
        assert_eq!(renderer.line(0)[0], 0);
    }
}
