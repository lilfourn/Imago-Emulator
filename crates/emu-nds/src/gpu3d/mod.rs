//! The 3D engine: geometry and rendering (GBATEK, "DS 3D Video").
//!
//! The CPU, or DMA in geometry-FIFO mode, writes commands to GXFIFO or the
//! command ports. [`fifo`] queues them and the [`geometry`] engine runs
//! each as soon as it reaches the head, filling Polygon RAM. SWAP_BUFFERS
//! halts command processing until [`Gpu3d::vblank`], which hands the
//! polygon list to the rendering engine and resumes. [`Gpu3d::render_frame`]
//! draws that list with the rendering registers, and [`Gpu3d::line`] reads
//! the result for engine A's BG0: per pixel, 6-bit red, green and blue in
//! bits 0–17 and a 5-bit alpha in bits 18–22 (0 where there is no 3D
//! pixel). With [`Gpu3d::set_render_scale`], it also draws each frame
//! larger for the screen ([`Gpu3d::take_scaled_frame`]), unless nobody
//! looks at it ([`Gpu3d::show`]); the game only ever sees the native
//! frame.
//!
//! Commands execute instantly, so the FIFO only fills while a swap waits
//! for VBlank, and the test and matrix busy flags never read as busy.

mod color;
mod fifo;
mod geometry;
mod polygon;
mod registers;
mod render;

#[cfg(test)]
use self::render::ScaledFrame;
pub(crate) use self::render::{Job, LentFrame, MAX_SCALE};

use serde::{Deserialize, Serialize};

use self::fifo::{Assembler, FIFO_CAPACITY, Fifo, SWAP_BUFFERS};
use self::geometry::Geometry;
use self::polygon::PolygonList;
use self::registers::RenderRegisters;
use self::render::Renderer;
use crate::vram::Vram;

/// RDLINES_COUNT: the renderer never falls behind, so the line buffer
/// always holds its maximum.
const RDLINES_COUNT: u32 = 46;

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Gpu3d {
    registers: RenderRegisters,
    fifo: Fifo,
    assembler: Assembler,
    geometry: Geometry,
    /// SWAP_BUFFERS' parameter while the engine waits for VBlank.
    swap_pending: Option<u32>,
    /// GXSTAT bits 30–31: 1 interrupts while the FIFO is less than half
    /// full, 2 while it is empty.
    fifo_irq: u8,
    /// The polygon list the rendering engine draws.
    rendering: PolygonList,
    /// Frame buffers; redrawn by the next `render_frame`. The scale they
    /// draw at is a display setting, so it is not saved either.
    #[serde(skip)]
    renderer: Renderer,
}

impl Gpu3d {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// A 32-bit write, for tests.
    #[cfg(test)]
    pub(crate) fn write_register(&mut self, offset: u32, value: u32) {
        self.write(offset & !3, value, u32::MAX);
    }

    /// A 16-bit write, for tests.
    #[cfg(test)]
    pub(crate) fn write_register_16(&mut self, offset: u32, value: u16) {
        let shift = (offset & 2) * 8;
        self.write(offset & !3, u32::from(value) << shift, 0xFFFF << shift);
    }

    /// Writes the bytes of `value` selected by `mask` (a byte, a halfword
    /// or all four) to the word at `offset` from 0x04000000: DISP3DCNT
    /// (0x060), 0x320–0x3BF, GXFIFO, the command ports, GXSTAT and
    /// DISP_1DOT_DEPTH. GXFIFO and the command ports take only 32-bit
    /// writes; a narrower one counts as a whole word.
    pub(crate) fn write(&mut self, offset: u32, value: u32, mask: u32) {
        match offset {
            0x060 => {
                let (value, mask) = (value as u16, mask as u16);
                if value & mask & (1 << 13) != 0 {
                    self.geometry.overflow = false;
                }
                self.registers.write_control(value, mask);
            }
            0x320..=0x3BC => {
                for lane in 0..4 {
                    if (mask >> (lane * 8)) & 0xFF != 0 {
                        self.registers
                            .write_byte(offset + lane, (value >> (lane * 8)) as u8);
                    }
                }
            }
            0x400..=0x43C => {
                self.fifo.write_packed(value);
                self.run();
            }
            0x440..=0x5FC => {
                self.fifo.write_port(((offset - 0x400) >> 2) as u8, value);
                self.run();
            }
            0x600 => {
                if value & mask & (1 << 15) != 0 {
                    self.geometry.acknowledge_stack_error();
                }
                if mask & (3 << 30) != 0 {
                    self.fifo_irq = (value >> 30) as u8;
                }
            }
            0x610 => {
                let depth = self.geometry.one_dot_depth;
                self.geometry.one_dot_depth = (depth & !mask | value & mask) & 0x7FFF;
            }
            _ => {}
        }
    }

    /// The 32-bit word containing `offset`. Write-only registers read 0.
    pub(crate) fn read_register(&self, offset: u32) -> u32 {
        let offset = offset & !3;
        let index = |base: u32, size: u32| ((offset - base) / size) as usize;
        match offset {
            0x060 => u32::from(self.registers.control()) | u32::from(self.geometry.overflow) << 13,
            0x320 => RDLINES_COUNT,
            0x600 => self.gxstat(),
            0x604 => self.geometry.ram_count(),
            0x620..=0x62C => self.geometry.pos_result(index(0x620, 4)),
            0x630..=0x634 => {
                let first = index(0x630, 2);
                u32::from(self.geometry.vec_result(first))
                    | u32::from(self.geometry.vec_result(first + 1)) << 16
            }
            0x640..=0x67C => self.geometry.clip_matrix_word(index(0x640, 4)),
            0x680..=0x6A0 => self.geometry.vector_matrix_word(index(0x680, 4)),
            _ => 0,
        }
    }

    fn gxstat(&self) -> u32 {
        let count = self.fifo.len() as u32;
        let busy = self.swap_pending.is_some() || self.fifo.has_entries();
        self.geometry.status()
            | count << 16
            | u32::from(count < FIFO_CAPACITY as u32 / 2) << 25
            | u32::from(count == 0) << 26
            | u32::from(busy) << 27
            | u32::from(self.fifo_irq) << 30
    }

    /// Runs queued commands until the FIFO is empty or a swap waits.
    fn run(&mut self) {
        while self.swap_pending.is_none() {
            let Some(entry) = self.fifo.pop() else {
                break;
            };
            if let Some((command, params)) = self.assembler.push(entry) {
                if command == SWAP_BUFFERS {
                    self.swap_pending = Some(params[0]);
                } else {
                    self.geometry.execute(command, params);
                }
            }
        }
    }

    /// The FIFO is less than half full: geometry-FIFO DMA should run (it
    /// sends up to 112 words, then waits for this again).
    pub(crate) fn fifo_wants_dma(&self) -> bool {
        self.fifo.len() < FIFO_CAPACITY / 2
    }

    /// The GXFIFO interrupt condition (IF bit 21). It is a level: while
    /// true, IF bit 21 stays set however often the CPU acknowledges it.
    pub(crate) fn irq_requested(&self) -> bool {
        let count = self.fifo.len();
        match self.fifo_irq {
            1 => count < FIFO_CAPACITY / 2,
            2 => count == 0,
            _ => false,
        }
    }

    /// VBlank begins (line 192): a pending SWAP_BUFFERS takes effect, the
    /// new polygon list goes to the rendering engine, and commands resume.
    pub(crate) fn vblank(&mut self) {
        let Some(params) = self.swap_pending.take() else {
            return;
        };
        let building = self.geometry.list_mut();
        std::mem::swap(&mut self.rendering, building);
        building.restart(params);
        self.run();
    }

    /// Draws the current polygon list with the rendering registers as they
    /// are now. Call once per frame, when the hardware starts rendering
    /// (line 214), and before reading [`Self::line`]. VRAM is read now; the
    /// drawing finishes on worker threads, which [`Self::line`] and
    /// [`Self::take_scaled_frame`] wait for.
    pub(crate) fn render_frame(&mut self, vram: &Vram) {
        let config = self.registers.config();
        self.renderer.render(&self.rendering, &config, vram);
    }

    /// A count that goes up whenever [`Gpu3d::render_frame`] leaves a
    /// frame other than the one before. A game drawing 30 frames a second
    /// leaves each for two, and a still scene for longer.
    pub(crate) fn frame_count(&self) -> u64 {
        self.renderer.frame_count()
    }

    /// Row `y` (0–191) of the last frame drawn, waiting for the frame if
    /// it is still drawing.
    pub(crate) fn line(&mut self, y: u16) -> &[u32; 256] {
        self.renderer.line(usize::from(y))
    }

    /// Also draws every frame `scale` times larger in each direction, for
    /// the screen, from the next [`Gpu3d::render_frame`] on; 1 stops.
    /// `scale` is clamped to 1–[`MAX_SCALE`].
    pub(crate) fn set_render_scale(&mut self, scale: usize) {
        self.renderer.set_scale(scale);
    }

    /// Says whether the frames drawn from now on are looked at. While
    /// they are not, [`Gpu3d::render_frame`] draws the native frame
    /// alone: the larger one keeps an earlier frame's picture.
    pub(crate) fn show(&mut self, shown: bool) {
        self.renderer.show(shown);
    }

    /// Whether frames are looked at just now, and so have larger frames
    /// of their own.
    pub(crate) fn shown(&self) -> bool {
        self.renderer.shown()
    }

    /// Whether this engine, read from a save state, holds polygon lists
    /// the geometry engine could have stored, which the renderer draws
    /// without checking again.
    pub(crate) fn in_range(&self) -> bool {
        self.geometry.in_range() && self.rendering.in_range()
    }

    /// How many polygons the rendering engine is drawing.
    #[cfg(test)]
    pub(crate) fn polygons_drawn(&self) -> usize {
        self.rendering.polygons.len()
    }

    /// Moves `other`'s renderer, with its buffers and scale, into this
    /// engine, which replaces it. The next [`Gpu3d::render_frame`]
    /// redraws them.
    pub(crate) fn take_renderer_from(&mut self, other: &mut Self) {
        self.renderer = std::mem::take(&mut other.renderer);
    }

    /// The last larger frame drawn, waiting for it if it is still drawing:
    /// rows 0 to `192 * scale - 1` of `256 * scale` pixels, as
    /// [`Gpu3d::line`]'s. Its rows are `None` at the native scale.
    #[cfg(test)]
    pub(crate) fn scaled_frame(&mut self) -> ScaledFrame<'_> {
        self.renderer.scaled_frame()
    }

    /// Takes the last larger frame drawn out of the engine, waiting for
    /// it if it is still drawing, for jobs given to [`Gpu3d::run_jobs`]
    /// to read: rows 0 to `192 * scale - 1` of `256 * scale` pixels, as
    /// [`Gpu3d::line`]'s, and none at the native scale. It must be put
    /// back, with [`Gpu3d::put_scaled_frame`], before the engine draws
    /// again.
    pub(crate) fn take_scaled_frame(&mut self) -> LentFrame {
        self.renderer.take_scaled_frame()
    }

    /// Puts back the frame [`Gpu3d::take_scaled_frame`] took.
    pub(crate) fn put_scaled_frame(&mut self, frame: LentFrame) {
        self.renderer.put_scaled_frame(frame);
    }

    /// Runs `jobs` on the threads that draw the 3D frames: as many as
    /// half the processors, in the scheduling class of the thread that
    /// runs the core, or here and now if no thread will start.
    pub(crate) fn run_jobs(&mut self, jobs: Vec<Job>) {
        self.renderer.run(jobs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vram::Region;

    const ONE: i32 = 1 << 12;

    // Command ports.
    const MTX_MODE: u32 = 0x440;
    const MTX_IDENTITY: u32 = 0x454;
    const MTX_SCALE: u32 = 0x46C;
    const COLOR: u32 = 0x480;
    const TEXCOORD: u32 = 0x488;
    const VTX_16: u32 = 0x48C;
    const POLYGON_ATTR: u32 = 0x4A4;
    const TEXIMAGE_PARAM: u32 = 0x4A8;
    const BEGIN_VTXS: u32 = 0x500;
    const SWAP_BUFFERS_PORT: u32 = 0x540;
    const BOX_TEST: u32 = 0x5C0;

    const SOLID: u32 = 31 << 16 | 0xC0;

    fn send(gpu: &mut Gpu3d, port: u32, params: &[u32]) {
        if params.is_empty() {
            gpu.write_register(port, 0);
        }
        for &param in params {
            gpu.write_register(port, param);
        }
    }

    fn vertex(gpu: &mut Gpu3d, x: i32, y: i32, z: i32) {
        send(
            gpu,
            VTX_16,
            &[(x as u32 & 0xFFFF) | (y as u32) << 16, z as u32 & 0xFFFF],
        );
    }

    /// A quad from (x0, y0) to (x1, y1) in clip space at depth `z`.
    fn quad(gpu: &mut Gpu3d, [x0, y0, x1, y1]: [i32; 4], z: i32) {
        send(gpu, BEGIN_VTXS, &[1]);
        vertex(gpu, x0, y1, z);
        vertex(gpu, x0, y0, z);
        vertex(gpu, x1, y0, z);
        vertex(gpu, x1, y1, z);
    }

    /// DISP3DCNT, CLEAR_COLOR and CLEAR_DEPTH (farthest).
    fn configure(gpu: &mut Gpu3d, control: u32, clear_color: u32) {
        gpu.write_register(0x060, control);
        gpu.write_register(0x350, clear_color);
        gpu.write_register_16(0x354, 0x7FFF);
    }

    /// Swaps, starts VBlank and renders.
    fn finish(gpu: &mut Gpu3d, vram: &Vram) {
        send(gpu, SWAP_BUFFERS_PORT, &[0]);
        gpu.vblank();
        gpu.render_frame(vram);
    }

    fn pixel(gpu: &mut Gpu3d, x: usize, y: u16) -> u32 {
        gpu.line(y)[x]
    }

    /// An opaque output pixel: red, green and blue in bits 0–17, alpha 31
    /// in bits 18–22.
    fn opaque([red, green, blue]: [u8; 3]) -> u32 {
        u32::from(red) | u32::from(green) << 6 | u32::from(blue) << 12 | 31 << 18
    }

    #[test]
    fn a_flat_triangle_covers_the_expected_pixels() {
        let mut gpu = Gpu3d::new();
        let vram = Vram::default();
        configure(&mut gpu, 0, 0);
        // Packed through GXFIFO: POLYGON_ATTR, COLOR, BEGIN_VTXS.
        gpu.write_register(0x400, 0x0040_2029);
        gpu.write_register(0x400, SOLID);
        gpu.write_register(0x400, 0x001F);
        gpu.write_register(0x400, 0);
        // The lower-left half of the screen.
        vertex(&mut gpu, -ONE, ONE, 0);
        vertex(&mut gpu, -ONE, -ONE, 0);
        vertex(&mut gpu, ONE, -ONE, 0);
        finish(&mut gpu, &vram);
        assert_eq!(pixel(&mut gpu, 10, 100), opaque([63, 0, 0]));
        assert_eq!(pixel(&mut gpu, 0, 191), opaque([63, 0, 0]));
        assert_eq!(pixel(&mut gpu, 200, 10), 0, "no 3D pixel");
        // Row 96 is covered up to the diagonal at x = 128.
        assert_eq!(pixel(&mut gpu, 127, 96), opaque([63, 0, 0]));
        assert_eq!(pixel(&mut gpu, 129, 96), 0);
    }

    #[test]
    fn nearer_pixels_win_the_depth_test() {
        let mut gpu = Gpu3d::new();
        let vram = Vram::default();
        configure(&mut gpu, 0, 0);
        send(&mut gpu, POLYGON_ATTR, &[SOLID]);
        send(&mut gpu, COLOR, &[0x001F]);
        quad(&mut gpu, [-ONE / 2, -ONE / 2, ONE / 2, ONE / 2], -ONE / 2);
        send(&mut gpu, COLOR, &[0x7C00]);
        quad(&mut gpu, [-ONE, -ONE, ONE, ONE], ONE / 2);
        finish(&mut gpu, &vram);
        assert_eq!(
            pixel(&mut gpu, 128, 96),
            opaque([63, 0, 0]),
            "near red stays"
        );
        assert_eq!(
            pixel(&mut gpu, 10, 10),
            opaque([0, 0, 63]),
            "far blue elsewhere"
        );
    }

    #[test]
    fn a_four_colour_texture_maps_across_a_quad() {
        let mut gpu = Gpu3d::new();
        let mut vram = Vram::default();
        vram.set_control(0, 0x80 | 3); // A: texture slot 0.
        vram.set_control(4, 0x80 | 3); // E: texture palettes.
        // 8×8 texels, each row 0, 1, 2, 3, 0, 1, 2, 3.
        for offset in 0..16 {
            vram.write_8(Region::Texture, offset, 0b1110_0100);
        }
        for (index, color) in [0x0000, 0x001F, 0x03E0, 0x7C00].into_iter().enumerate() {
            vram.write_16(Region::TexturePalette, index as u32 * 2, color);
        }
        configure(&mut gpu, 1, 0);
        send(&mut gpu, POLYGON_ATTR, &[SOLID]);
        send(&mut gpu, TEXIMAGE_PARAM, &[2 << 26]);
        send(&mut gpu, COLOR, &[0x7FFF]);
        send(&mut gpu, BEGIN_VTXS, &[1]);
        // Eight texels across 256 pixels: 32 pixels each.
        for (s, t, x, y) in [(0, 0, -1, 1), (0, 8, -1, -1), (8, 8, 1, -1), (8, 0, 1, 1)] {
            send(&mut gpu, TEXCOORD, &[(s << 4) | (t << 4) << 16]);
            vertex(&mut gpu, x * ONE, y * ONE, 0);
        }
        finish(&mut gpu, &vram);
        assert_eq!(pixel(&mut gpu, 16, 50), opaque([0, 0, 0]));
        assert_eq!(pixel(&mut gpu, 48, 50), opaque([63, 0, 0]));
        assert_eq!(pixel(&mut gpu, 80, 50), opaque([0, 63, 0]));
        assert_eq!(pixel(&mut gpu, 112, 50), opaque([0, 0, 63]));
        assert_eq!(pixel(&mut gpu, 176, 150), opaque([63, 0, 0]));
    }

    #[test]
    fn a_frame_is_drawn_again_only_when_it_changes() {
        let mut gpu = Gpu3d::new();
        gpu.set_render_scale(2);
        let mut vram = Vram::default();
        vram.set_control(0, 0x80 | 3); // A: texture slot 0.
        vram.set_control(4, 0x80 | 3); // E: texture palettes.
        // 8×8 texels of colour 1, red.
        for offset in 0..16 {
            vram.write_8(Region::Texture, offset, 0b0101_0101);
        }
        vram.write_16(Region::TexturePalette, 2, 0x001F);
        // The left half of the screen, textured, over a blue rear plane.
        let textured = |gpu: &mut Gpu3d| {
            send(gpu, POLYGON_ATTR, &[SOLID]);
            send(gpu, TEXIMAGE_PARAM, &[2 << 26]);
            send(gpu, COLOR, &[0x7FFF]);
            quad(gpu, [-ONE, -ONE, 0, ONE], 0);
        };
        configure(&mut gpu, 1, 0x7C00 | 31 << 16);
        textured(&mut gpu);
        finish(&mut gpu, &vram);
        let drawn = gpu.frame_count();
        let frame = scaled_frame(&mut gpu, 2);
        assert_eq!(pixel(&mut gpu, 64, 96), opaque([63, 0, 0]));
        // The same polygons, registers and texels: the frame it holds.
        gpu.render_frame(&vram);
        gpu.render_frame(&vram);
        assert_eq!(gpu.frame_count(), drawn);
        assert_eq!(scaled_frame(&mut gpu, 2), frame);
        // The same polygons sent again are the same frame too.
        textured(&mut gpu);
        finish(&mut gpu, &vram);
        assert_eq!(gpu.frame_count(), drawn);
        // A texel changes: green.
        vram.write_16(Region::TexturePalette, 2, 0x03E0);
        gpu.render_frame(&vram);
        assert_eq!(gpu.frame_count(), drawn + 1);
        assert_eq!(pixel(&mut gpu, 64, 96), opaque([0, 63, 0]));
        assert_ne!(scaled_frame(&mut gpu, 2), frame);
        // The bank leaves texture memory: every texel is colour 0, black.
        vram.set_control(0, 0);
        gpu.render_frame(&vram);
        assert_eq!(gpu.frame_count(), drawn + 2);
        assert_eq!(pixel(&mut gpu, 64, 96), opaque([0, 0, 0]));
        // A rendering register changes: no texturing, the vertex colour.
        gpu.write_register(0x060, 0);
        gpu.render_frame(&vram);
        assert_eq!(gpu.frame_count(), drawn + 3);
        assert_eq!(pixel(&mut gpu, 64, 96), opaque([63, 63, 63]));
        let untextured = scaled_frame(&mut gpu, 2);
        // No polygons: the rear plane alone, once.
        finish(&mut gpu, &vram);
        gpu.render_frame(&vram);
        assert_eq!(gpu.frame_count(), drawn + 4);
        assert_eq!(pixel(&mut gpu, 64, 96), opaque([0, 0, 63]));
        // The polygons from before it are shown again without drawing.
        textured(&mut gpu);
        finish(&mut gpu, &vram);
        assert_eq!(gpu.frame_count(), drawn + 5);
        assert_eq!(pixel(&mut gpu, 64, 96), opaque([63, 63, 63]));
        assert_eq!(scaled_frame(&mut gpu, 2), untextured);
        // Another scale needs its own larger frame.
        gpu.set_render_scale(3);
        gpu.render_frame(&vram);
        assert_eq!(scaled_frame(&mut gpu, 3).len(), 256 * 3 * 192 * 3);
    }

    #[test]
    fn a_frame_nobody_looks_at_is_drawn_only_if_it_is_read() {
        let vram = Vram::default();
        let start = |color: u32| {
            let mut gpu = Gpu3d::new();
            gpu.set_render_scale(2);
            configure(&mut gpu, 0, 0x7C00 | 31 << 16);
            send(&mut gpu, POLYGON_ATTR, &[SOLID]);
            send(&mut gpu, COLOR, &[color]);
            quad(&mut gpu, [-ONE, -ONE, 0, ONE], 0);
            finish(&mut gpu, &vram);
            gpu
        };
        let mut gpu = start(0x001F);
        let red = scaled_frame(&mut gpu, 2);
        let drawn = gpu.frame_count();
        // A green frame nobody looks at: nothing is drawn. The game's own
        // frame is once it is read, as a capture reads it, and the larger
        // one keeps the red.
        gpu.show(false);
        send(&mut gpu, COLOR, &[0x03E0]);
        quad(&mut gpu, [-ONE, -ONE, 0, ONE], 0);
        finish(&mut gpu, &vram);
        assert!(!gpu.shown());
        assert_eq!(gpu.frame_count(), drawn + 1);
        assert_eq!(gpu.renderer.drawing(), 0);
        assert_eq!(pixel(&mut gpu, 64, 96), opaque([0, 63, 0]));
        assert!(scaled_frame(&mut gpu, 2) == red);
        // One never read is not drawn at all: the frame after it, looked
        // at, is the same as if it had been.
        send(&mut gpu, COLOR, &[0x7C00]);
        quad(&mut gpu, [-ONE, -ONE, 0, ONE], 0);
        finish(&mut gpu, &vram);
        assert_eq!(gpu.frame_count(), drawn + 2);
        assert_eq!(gpu.renderer.drawing(), 0);
        send(&mut gpu, COLOR, &[0x03E0]);
        quad(&mut gpu, [-ONE, -ONE, 0, ONE], 0);
        finish(&mut gpu, &vram);
        assert_eq!(gpu.frame_count(), drawn + 3);
        let drawn = drawn + 2;
        // The same frame, looked at: its larger frame is drawn now, as
        // one looked at from the start is.
        gpu.show(true);
        gpu.render_frame(&vram);
        assert!(gpu.shown());
        assert_eq!(gpu.frame_count(), drawn + 1);
        assert!(scaled_frame(&mut gpu, 2) == scaled_frame(&mut start(0x03E0), 2));
        // And only the once.
        gpu.render_frame(&vram);
        assert_eq!(gpu.frame_count(), drawn + 1);
    }

    #[test]
    fn translucent_polygons_blend_once_per_id() {
        let mut gpu = Gpu3d::new();
        let vram = Vram::default();
        // Alpha blending on; an opaque blue rear plane.
        configure(&mut gpu, 1 << 3, 0x7C00 | 31 << 16);
        send(&mut gpu, POLYGON_ATTR, &[15 << 16 | 0xC0 | 1 << 24]);
        send(&mut gpu, COLOR, &[0x001F]);
        quad(&mut gpu, [-ONE, -ONE, 0, ONE], 0);
        // Same ID again over the left half: no second blend.
        quad(&mut gpu, [-ONE, -ONE, ONE, ONE], -ONE / 2);
        finish(&mut gpu, &vram);
        // (63 * 16 + 0 * 16) / 32 = 31 red, (0 + 63 * 16) / 32 = 31 blue.
        assert_eq!(pixel(&mut gpu, 64, 96), opaque([31, 0, 31]));
        assert_eq!(pixel(&mut gpu, 192, 96), opaque([31, 0, 31]));
    }

    #[test]
    fn fog_covers_fogged_polygons() {
        let mut gpu = Gpu3d::new();
        let vram = Vram::default();
        configure(&mut gpu, 1 << 7, 0);
        gpu.write_register(0x358, 0x03E0 | 31 << 16); // Green fog.
        for offset in (0x360..0x380).step_by(4) {
            gpu.write_register(offset, 0x7F7F_7F7F); // Full density.
        }
        send(&mut gpu, POLYGON_ATTR, &[SOLID | 1 << 15]);
        send(&mut gpu, COLOR, &[0x001F]);
        quad(&mut gpu, [-ONE, -ONE, 0, ONE], 0);
        send(&mut gpu, POLYGON_ATTR, &[SOLID]);
        quad(&mut gpu, [0, -ONE, ONE, ONE], 0);
        finish(&mut gpu, &vram);
        assert_eq!(pixel(&mut gpu, 64, 96), opaque([0, 63, 0]));
        assert_eq!(pixel(&mut gpu, 192, 96), opaque([63, 0, 0]), "fog disabled");
    }

    #[test]
    fn edge_marking_outlines_polygon_ids() {
        let mut gpu = Gpu3d::new();
        let vram = Vram::default();
        configure(&mut gpu, 1 << 5, 0);
        gpu.write_register(0x330, 0x7FFF << 16); // Edge colour 1: white.
        send(&mut gpu, POLYGON_ATTR, &[SOLID | 8 << 24]);
        send(&mut gpu, COLOR, &[0x001F]);
        quad(&mut gpu, [-ONE / 2, -ONE / 2, ONE / 2, ONE / 2], 0);
        finish(&mut gpu, &vram);
        // The quad spans x 64..192, y 48..144.
        assert_eq!(pixel(&mut gpu, 64, 96), opaque([63, 63, 63]), "left edge");
        assert_eq!(pixel(&mut gpu, 100, 48), opaque([63, 63, 63]), "top edge");
        assert_eq!(pixel(&mut gpu, 100, 96), opaque([63, 0, 0]), "interior");
    }

    /// The lower-left half of the screen in red over a blue rear plane,
    /// drawn with DISP3DCNT `control`.
    fn red_half_over_blue(control: u32) -> Gpu3d {
        let mut gpu = Gpu3d::new();
        configure(&mut gpu, control, 0x7C00 | 31 << 16);
        send(&mut gpu, POLYGON_ATTR, &[SOLID]);
        send(&mut gpu, COLOR, &[0x001F]);
        send(&mut gpu, BEGIN_VTXS, &[0]);
        vertex(&mut gpu, -ONE, ONE, 0);
        vertex(&mut gpu, -ONE, -ONE, 0);
        vertex(&mut gpu, ONE, -ONE, 0);
        finish(&mut gpu, &Vram::default());
        gpu
    }

    #[test]
    fn anti_aliasing_softens_edges_against_the_rear_plane() {
        let mut sharp = red_half_over_blue(0);
        let mut smooth = red_half_over_blue(1 << 4);
        for y in 0..192 {
            // The diagonal crosses the middle of row y at (y + 0.5) * 4 / 3.
            let crossing = (f64::from(y) + 0.5) * 4.0 / 3.0;
            let mut blended = 0;
            for x in 0..256 {
                let (before, after) = (pixel(&mut sharp, x, y), pixel(&mut smooth, x, y));
                assert!(before == opaque([63, 0, 0]) || before == opaque([0, 0, 63]));
                let [red, _, blue] = color::unpack(after);
                if red > 0 && blue > 0 {
                    blended += 1;
                }
                if (x as f64 + 0.5 - crossing).abs() > 1.0 {
                    assert_eq!(after, before, "({x}, {y}) is off the edge");
                }
            }
            assert!(blended > 0, "row {y} blends its edge");
        }
    }

    #[test]
    fn anti_aliased_neighbours_leave_no_seam() {
        let mut gpu = Gpu3d::new();
        configure(&mut gpu, 1 << 4, 0x7C00 | 31 << 16);
        send(&mut gpu, POLYGON_ATTR, &[SOLID]);
        send(&mut gpu, COLOR, &[0x001F]);
        // x 64..192, y 48..144, as two triangles split along a diagonal.
        let [x0, y0, x1, y1] = [-ONE / 2, -ONE / 2, ONE / 2, ONE / 2];
        send(&mut gpu, BEGIN_VTXS, &[0]);
        for (x, y) in [(x0, y1), (x0, y0), (x1, y0), (x0, y1), (x1, y0), (x1, y1)] {
            vertex(&mut gpu, x, y, 0);
        }
        finish(&mut gpu, &Vram::default());
        for y in 48..144 {
            for x in 64..192 {
                assert_eq!(pixel(&mut gpu, x, y), opaque([63, 0, 0]), "({x}, {y})");
            }
        }
    }

    #[test]
    fn edge_marking_halves_anti_aliased_edges() {
        let mut gpu = Gpu3d::new();
        let vram = Vram::default();
        configure(&mut gpu, 1 << 5 | 1 << 4, 0x7C00 | 31 << 16);
        gpu.write_register(0x330, 0x7FFF << 16); // Edge colour 1: white.
        send(&mut gpu, POLYGON_ATTR, &[SOLID | 8 << 24]);
        send(&mut gpu, COLOR, &[0x001F]);
        quad(&mut gpu, [-ONE / 2, -ONE / 2, ONE / 2, ONE / 2], 0);
        finish(&mut gpu, &vram);
        // White at 17/32 over the blue rear plane.
        assert_eq!(pixel(&mut gpu, 64, 96), opaque([33, 33, 63]), "left edge");
        assert_eq!(pixel(&mut gpu, 100, 48), opaque([33, 33, 63]), "top edge");
        assert_eq!(pixel(&mut gpu, 100, 96), opaque([63, 0, 0]), "interior");
    }

    /// Two quads with different polygon IDs, one fogged and one with a
    /// slanted edge, under a translucent strip: everything the renderer
    /// post-processes, with DISP3DCNT `control`.
    fn busy_scene(gpu: &mut Gpu3d, control: u32) {
        configure(gpu, control, 0x7C00 | 31 << 16 | 1 << 15);
        gpu.write_register(0x330, 0x7FFF << 16 | 0x001F); // Edge colours.
        gpu.write_register(0x358, 0x03E0 | 31 << 16); // Green fog.
        for offset in (0x360..0x380).step_by(4) {
            gpu.write_register(offset, 0x4040_4040); // Half density.
        }
        send(gpu, POLYGON_ATTR, &[SOLID | 1 << 15 | 8 << 24]);
        send(gpu, COLOR, &[0x001F]);
        quad(gpu, [-ONE / 2, -ONE / 2, ONE / 2, ONE / 2], 0);
        send(gpu, POLYGON_ATTR, &[SOLID | 16 << 24]);
        send(gpu, COLOR, &[0x03FF]);
        send(gpu, BEGIN_VTXS, &[0]);
        vertex(gpu, -ONE / 4, ONE, -ONE / 4);
        vertex(gpu, -ONE, -ONE / 3, -ONE / 4);
        vertex(gpu, ONE / 3, -ONE, -ONE / 4);
        send(gpu, POLYGON_ATTR, &[15 << 16 | 0xC0 | 24 << 24]);
        send(gpu, COLOR, &[0x7FFF]);
        quad(gpu, [-ONE, -ONE / 8, ONE, ONE / 8], -ONE / 2);
    }

    /// Every row of `gpu`'s larger frame.
    fn scaled_frame(gpu: &mut Gpu3d, scale: usize) -> Vec<u32> {
        let frame = gpu.scaled_frame();
        (0..192 * scale)
            .flat_map(|row| frame.row(row).expect("drawn larger").to_vec())
            .collect()
    }

    #[test]
    fn the_native_frame_is_the_same_at_any_scale() {
        let vram = Vram::default();
        let everything = 1 << 4 | 1 << 5 | 1 << 7 | 1 << 3;
        let mut native = Gpu3d::new();
        busy_scene(&mut native, everything);
        finish(&mut native, &vram);
        assert!(native.scaled_frame().row(0).is_none());
        for scale in [2, 3] {
            let mut gpu = Gpu3d::new();
            gpu.set_render_scale(scale);
            busy_scene(&mut gpu, everything);
            finish(&mut gpu, &vram);
            for y in 0..192 {
                assert_eq!(gpu.line(y), native.line(y), "row {y} at {scale}x");
            }
        }
    }

    #[test]
    fn the_larger_frame_matches_the_native_one_away_from_edges() {
        let vram = Vram::default();
        // No anti-aliasing or edge marking: only edges may differ.
        let mut native = Gpu3d::new();
        busy_scene(&mut native, 1 << 7 | 1 << 3);
        finish(&mut native, &vram);
        let mut gpu = Gpu3d::new();
        gpu.set_render_scale(2);
        busy_scene(&mut gpu, 1 << 7 | 1 << 3);
        finish(&mut gpu, &vram);
        let larger = scaled_frame(&mut gpu, 2);
        let mut compared = 0;
        for y in 1..191 {
            for x in 1..255 {
                let here = pixel(&mut native, x, y);
                let flat = (y - 1..=y + 1)
                    .all(|y| (x - 1..=x + 1).all(|x| pixel(&mut native, x, y) == here));
                if !flat {
                    continue;
                }
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let cell = larger[(2 * usize::from(y) + dy) * 512 + 2 * x + dx];
                    assert_eq!(cell, here, "({x}, {y})");
                }
                compared += 1;
            }
        }
        assert!(compared > 40_000, "{compared} flat pixels");
    }

    #[test]
    fn bands_draw_the_frame_as_one_band_would() {
        let vram = Vram::default();
        let everything = 1 << 4 | 1 << 5 | 1 << 7 | 1 << 3;
        for scale in [2, 3] {
            let mut whole = Gpu3d::new();
            whole.renderer.split(scale, 1);
            busy_scene(&mut whole, everything);
            finish(&mut whole, &vram);
            let mut banded = Gpu3d::new();
            banded.renderer.split(scale, 13);
            busy_scene(&mut banded, everything);
            finish(&mut banded, &vram);
            assert!(
                scaled_frame(&mut whole, scale) == scaled_frame(&mut banded, scale),
                "at {scale}x"
            );
        }
    }

    #[test]
    fn the_native_frame_is_the_same_in_any_number_of_bands() {
        let vram = Vram::default();
        let everything = 1 << 4 | 1 << 5 | 1 << 7 | 1 << 3;
        let frame = |bands: usize| {
            let mut gpu = Gpu3d::new();
            gpu.renderer.split_native(bands);
            busy_scene(&mut gpu, everything);
            finish(&mut gpu, &vram);
            (0..192).flat_map(|y| *gpu.line(y)).collect::<Vec<u32>>()
        };
        let whole = frame(1);
        for bands in [2, 4, 7, 192] {
            assert!(frame(bands) == whole, "in {bands} bands");
        }
    }

    #[test]
    fn edge_marking_keeps_its_on_screen_thickness() {
        let mut gpu = Gpu3d::new();
        gpu.set_render_scale(2);
        configure(&mut gpu, 1 << 5, 0);
        gpu.write_register(0x330, 0x7FFF << 16); // Edge colour 1: white.
        send(&mut gpu, POLYGON_ATTR, &[SOLID | 8 << 24]);
        send(&mut gpu, COLOR, &[0x001F]);
        quad(&mut gpu, [-ONE / 2, -ONE / 2, ONE / 2, ONE / 2], 0);
        finish(&mut gpu, &Vram::default());
        // The quad spans x 128..384, y 96..288: edges two cells thick.
        let frame = gpu.scaled_frame();
        let cell = |x: usize, y: usize| frame.row(y).expect("drawn larger")[x];
        let white = opaque([63, 63, 63]);
        let red = opaque([63, 0, 0]);
        assert_eq!(
            [cell(128, 192), cell(129, 192), cell(130, 192)],
            [white, white, red]
        );
        assert_eq!(
            [cell(382, 192), cell(383, 192), cell(381, 192)],
            [white, white, red]
        );
        assert_eq!(
            [cell(200, 96), cell(200, 97), cell(200, 98)],
            [white, white, red]
        );
        assert_eq!(
            [cell(200, 287), cell(200, 286), cell(200, 285)],
            [white, white, red]
        );
    }

    #[test]
    fn swap_buffers_halts_the_fifo_until_vblank() {
        let mut gpu = Gpu3d::new();
        gpu.write_register(0x600, 2 << 30); // IRQ when empty.
        assert!(gpu.irq_requested());
        send(&mut gpu, SWAP_BUFFERS_PORT, &[0]);
        let gxstat = gpu.read_register(0x600);
        assert_ne!(gxstat & 1 << 27, 0, "busy while waiting");
        assert_ne!(gxstat & 1 << 26, 0, "nothing queued yet");
        // A triangle waits behind the swap.
        send(&mut gpu, POLYGON_ATTR, &[SOLID]);
        send(&mut gpu, BEGIN_VTXS, &[0]);
        vertex(&mut gpu, -ONE, ONE, 0);
        vertex(&mut gpu, -ONE, -ONE, 0);
        vertex(&mut gpu, ONE, -ONE, 0);
        for _ in 0..200 {
            send(&mut gpu, MTX_IDENTITY, &[]);
        }
        // 2 + 3 * 2 + 200 entries, four of them in the PIPE.
        let gxstat = gpu.read_register(0x600);
        assert_eq!((gxstat >> 16) & 0x1FF, 204);
        assert_eq!(gxstat & (3 << 25), 0, "neither empty nor below half");
        assert!(!gpu.fifo_wants_dma() && !gpu.irq_requested());
        assert_eq!(gpu.read_register(0x604), 0, "no polygon yet");
        gpu.vblank();
        let gxstat = gpu.read_register(0x600);
        assert_eq!(gxstat & (1 << 27 | 0x1FF << 16), 0);
        assert_eq!(gxstat & (3 << 25), 3 << 25);
        assert!(gpu.fifo_wants_dma() && gpu.irq_requested());
        assert_eq!(gpu.read_register(0x604), 1 | 3 << 16);
    }

    #[test]
    fn the_fifo_fills_at_260_entries() {
        let mut gpu = Gpu3d::new();
        send(&mut gpu, SWAP_BUFFERS_PORT, &[0]);
        // 259 entries wait behind the swap: four in the PIPE, 255 in the
        // FIFO.
        for _ in 0..259 {
            send(&mut gpu, MTX_IDENTITY, &[]);
        }
        assert_eq!((gpu.read_register(0x600) >> 16) & 0x1FF, 255);
        send(&mut gpu, MTX_IDENTITY, &[]);
        assert_eq!((gpu.read_register(0x600) >> 16) & 0x1FF, 256, "full");
    }

    #[test]
    fn box_test_and_matrix_reads_through_registers() {
        let mut gpu = Gpu3d::new();
        send(&mut gpu, MTX_MODE, &[1]);
        send(&mut gpu, MTX_SCALE, &[2 << 12, 3 << 12, 4 << 12]);
        assert_eq!(gpu.read_register(0x640), 2 << 12);
        assert_eq!(gpu.read_register(0x654), 3 << 12);
        assert_eq!(gpu.read_register(0x668), 4 << 12);
        assert_eq!(gpu.read_register(0x680), 1 << 12, "mode 1 leaves vectors");
        // A box at the origin, 0.25 on each side: scaled, still inside.
        let quarter = (ONE / 4) as u32;
        send(
            &mut gpu,
            BOX_TEST,
            &[0, quarter << 16, quarter | quarter << 16],
        );
        assert_ne!(gpu.read_register(0x600) & 2, 0);
        // Far off to the side.
        send(
            &mut gpu,
            BOX_TEST,
            &[(3 * ONE) as u32, quarter << 16, quarter | quarter << 16],
        );
        assert_eq!(gpu.read_register(0x600) & 2, 0);
    }

    #[test]
    fn state_round_trips_through_postcard() {
        let mut gpu = Gpu3d::new();
        let vram = Vram::default();
        configure(&mut gpu, 0, 0);
        send(&mut gpu, POLYGON_ATTR, &[SOLID]);
        send(&mut gpu, COLOR, &[0x03E0]);
        quad(&mut gpu, [-ONE, -ONE, ONE, ONE], 0);
        send(&mut gpu, SWAP_BUFFERS_PORT, &[0]);
        let bytes = postcard::to_stdvec(&gpu).expect("serializes");
        let mut restored: Gpu3d = postcard::from_bytes(&bytes).expect("deserializes");
        restored.vblank();
        restored.render_frame(&vram);
        assert_eq!(pixel(&mut restored, 100, 100), opaque([0, 63, 0]));
    }
}
