//! Display timing (GBATEK, "DS Video Display Timings").
//!
//! A frame is 263 lines of 2,130 bus cycles (355 dots of 6 cycles); lines
//! 192–262 are VBlank. Each CPU has its own DISPSTAT, so each decides which
//! of the VBlank, HBlank and VCount-match interrupts it wants.
//!
//! With upscaled 3D (NDS-9), the screens are also drawn larger into
//! [`ScaledScreens`], for display only: engine B's lines as they are
//! drawn, engine A's all at once at VBlank, in bands on the 3D renderer's
//! threads, from copies taken as they were drawn.

use std::any::Any;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, mpsc};

use crate::capture::{self, Control};
use crate::dma::Timing;
use crate::gpu2d::{self, LineCopy, WIDTH};
use crate::gpu3d;
use crate::hardware::{Event, Hardware};
use crate::interrupt::irq;
use crate::ipc::Side;
use crate::vram::{Engine, Region};

/// Bus cycles per line.
pub(crate) const LINE_CYCLES: u64 = 2130;
/// Bus cycles from a line's start to its HBlank: 256 dots.
pub(crate) const HBLANK_START: u64 = 256 * 6;
/// Lines per frame.
pub(crate) const LINES: u16 = 263;
/// Engine A lines in each band drawn larger at VBlank.
const SCALED_LINES_PER_BAND: usize = 24;
/// Lines shown.
pub(crate) const VISIBLE_LINES: u16 = 192;
/// The line where the 3D engine renders the coming frame.
const RENDER_3D_LINE: u16 = 214;
/// Start-of-display DMA starts at each of these lines' starts, and its
/// channels switch off at the next line (melonDS's timing; GBATEK only
/// names the mode).
const DISPLAY_START_DMA_LINES: std::ops::Range<u16> = 2..194;
/// Bus cycles per frame.
pub(crate) const FRAME_CYCLES: u64 = LINE_CYCLES * LINES as u64;

/// Both engines' screens `scale` times larger than 256×192 in each
/// direction, for display only (NDS-9): engine A's around the 3D engine's
/// larger frame, engine B's enlarged. The game never reads them, and they
/// are not saved: what a game sees is the same at every scale.
#[derive(Debug, Clone)]
pub(crate) struct ScaledScreens {
    /// 1 when off; the screens are then empty.
    pub(crate) scale: usize,
    /// Engine A's screen, `256 * scale` pixels by `192 * scale`, in
    /// bands of [`SCALED_LINES_PER_BAND`] lines from the top: each goes
    /// to a thread to be drawn and comes back.
    a: Vec<Box<[u32]>>,
    /// Engine B's screen.
    pub(crate) b: Box<[u32]>,
    /// Engine A's lines as drawn this frame, to draw larger at VBlank.
    lines: Vec<LineCopy>,
    /// Which of `lines` were drawn since the last VBlank.
    fresh: Vec<bool>,
}

/// What every band of engine A's larger screen is drawn from, shared by
/// the threads drawing them.
struct BandSource {
    scale: usize,
    lines: Vec<LineCopy>,
    fresh: Vec<bool>,
    /// The 3D engine's larger frame, which the lines showing 3D are drawn
    /// around.
    three_d: gpu3d::LentFrame,
}

impl BandSource {
    /// Draws band `index` of the screen into `out`: each of its lines
    /// drawn since the last VBlank.
    fn draw(&self, index: usize, out: &mut [u32]) {
        let scale = self.scale;
        let first = index * SCALED_LINES_PER_BAND;
        let three_d = self.three_d.rows();
        let lines = self.lines.iter().zip(&self.fresh).enumerate().skip(first);
        for ((line, (copy, fresh)), out) in lines.zip(out.chunks_mut(WIDTH * scale * scale)) {
            if *fresh {
                copy.draw(scale, |row| three_d.row(line * scale + row), out);
            }
        }
    }
}

/// A band a thread has drawn, handed back with its share of the source,
/// so that once every band is back the source is this thread's alone.
struct DrawnBand {
    index: usize,
    out: Box<[u32]>,
    source: Arc<BandSource>,
    /// The panic that stopped the drawing, to raise on the machine's
    /// thread.
    panic: Option<Box<dyn Any + Send>>,
}

impl Default for ScaledScreens {
    fn default() -> Self {
        Self {
            scale: 1,
            a: Vec::new(),
            b: Box::default(),
            lines: Vec::new(),
            fresh: Vec::new(),
        }
    }
}

impl ScaledScreens {
    /// Screens `scale` times larger showing `a` and `b`, two native
    /// screens, enlarged. They are a screen's size times the scale
    /// squared whatever length `a` and `b` are; lines those lack stay
    /// black.
    fn enlarged(scale: usize, a: &[u32], b: &[u32]) -> Self {
        if scale == 1 {
            return Self::default();
        }
        let lines = usize::from(VISIBLE_LINES);
        let line_size = WIDTH * scale * scale;
        // `lines` of `native`, from `first`, enlarged.
        let enlarge = |native: &[u32], first: usize, lines: usize| {
            let mut out = vec![0; lines * line_size].into_boxed_slice();
            for (line, out) in native
                .as_chunks()
                .0
                .iter()
                .skip(first)
                .zip(out.chunks_exact_mut(line_size))
            {
                gpu2d::enlarge_line(line, scale, out);
            }
            out
        };
        Self {
            scale,
            a: (0..lines)
                .step_by(SCALED_LINES_PER_BAND)
                .map(|first| enlarge(a, first, SCALED_LINES_PER_BAND.min(lines - first)))
                .collect(),
            b: enlarge(b, 0, lines),
            lines: vec![LineCopy::default(); lines],
            fresh: vec![false; lines],
        }
    }

    /// Copies engine A's screen into `out`, which is its size.
    pub(crate) fn copy_a(&self, out: &mut [u32]) {
        let mut rest = out;
        for band in &self.a {
            let (out, after) = rest.split_at_mut(band.len().min(rest.len()));
            out.copy_from_slice(&band[..out.len()]);
            rest = after;
        }
    }
}

/// What a display event did, for the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Moment {
    /// Line `n` reached HBlank; visible lines are ready to draw.
    HBlank(u16),
    /// Line `n` started; line 192 starts VBlank and ends a frame.
    LineStart(u16),
}

impl Hardware {
    /// Schedules the first HBlank. Call once at power-on.
    pub(crate) fn start_display(&mut self) {
        let time = self.scheduler.now() + HBLANK_START;
        self.scheduler.schedule_at(time, Event::HBlank);
        let sample = self.scheduler.now() + crate::sound::CYCLES_PER_SAMPLE;
        self.scheduler.schedule_at(sample, Event::SoundSample);
    }

    fn request_display_interrupt(&mut self, enable_bit: u16, request: u32) {
        for side in [Side::Arm9, Side::Arm7] {
            if self.cpu(side).dispstat & enable_bit != 0 {
                self.cpu_mut(side).interrupts.request(request);
            }
        }
    }

    /// The HBlank event at `time`.
    pub(crate) fn hblank(&mut self, time: u64) -> Moment {
        self.hblank = true;
        self.request_display_interrupt(1 << 4, irq::HBLANK);
        if self.vcount < VISIBLE_LINES {
            self.draw_line(self.vcount);
            self.arm9.dma.trigger(Timing::HBlank);
        }
        self.scheduler
            .schedule_at(time + LINE_CYCLES - HBLANK_START, Event::LineStart);
        Moment::HBlank(self.vcount)
    }

    /// Both 2D engines draw `line` into their screens; a running capture
    /// takes engine A's line.
    pub(crate) fn draw_line(&mut self, line: u16) {
        let index = usize::from(line);
        let vram = &self.vram;
        // POWCNT1 bit 2 powers the 3D rendering engine.
        let three_d = (self.powcnt1 & (1 << 2) != 0).then(|| self.gpu3d.line(line));
        self.engine_a.render_line(
            line,
            &vram.engine(Engine::A),
            |offset| vram.read_16(Region::Lcdc, offset),
            three_d,
            &mut self.screen_a.as_chunks_mut().0[index],
        );
        if self.capturing {
            self.capture(line);
        }
        self.engine_b.render_line(
            line,
            &self.vram.engine(Engine::B),
            |_| 0,
            None,
            &mut self.screen_b.as_chunks_mut().0[index],
        );
        if self.scaled.scale > 1 {
            self.draw_scaled_line(index);
        }
    }

    /// Takes line `line` for the [`ScaledScreens`], as engines A and B
    /// just drew it: B's enlarged now, A's copied to draw at VBlank.
    fn draw_scaled_line(&mut self, line: usize) {
        let scaled = &mut self.scaled;
        let size = WIDTH * scaled.scale * scaled.scale;
        let b = &self.screen_b.as_chunks().0[line];
        gpu2d::enlarge_line(b, scaled.scale, &mut scaled.b[line * size..][..size]);
        self.engine_a
            .copy_line(&self.screen_a.as_chunks().0[line], &mut scaled.lines[line]);
        scaled.fresh[line] = true;
    }

    /// Draws engine A's lines taken this frame into its larger screen,
    /// around the 3D engine's larger frame: a band to a job on the 3D
    /// renderer's threads ([`gpu3d::Gpu3d::run_jobs`]), which start none
    /// of their own for it. Waits for them all.
    fn finish_scaled_screens(&mut self) {
        let scaled = &mut self.scaled;
        // What the bands read moves into the source the jobs share, and
        // each band into its job; everything is back before this returns.
        let source = Arc::new(BandSource {
            scale: scaled.scale,
            lines: std::mem::take(&mut scaled.lines),
            fresh: std::mem::take(&mut scaled.fresh),
            three_d: self.gpu3d.take_scaled_frame(),
        });
        let (sender, receiver) = mpsc::channel();
        let jobs: Vec<gpu3d::Job> = scaled
            .a
            .iter_mut()
            .enumerate()
            .map(|(index, band)| {
                let mut out = std::mem::take(band);
                let source = Arc::clone(&source);
                let sender = sender.clone();
                Box::new(move || {
                    let drawing =
                        std::panic::catch_unwind(AssertUnwindSafe(|| source.draw(index, &mut out)));
                    // Nobody is waiting only if the machine's thread has
                    // itself panicked.
                    let _ = sender.send(DrawnBand {
                        index,
                        out,
                        source,
                        panic: drawing.err(),
                    });
                }) as gpu3d::Job
            })
            .collect();
        let bands = jobs.len();
        self.gpu3d.run_jobs(jobs);
        let mut panic = None;
        for drawn in receiver.iter().take(bands) {
            scaled.a[drawn.index] = drawn.out;
            panic = panic.or(drawn.panic);
            // The job's share of the source ends here, on this thread.
            drop(drawn.source);
        }
        // Every job has handed its share back, so this is the only one.
        let source = Arc::into_inner(source).expect("every band is back");
        scaled.lines = source.lines;
        scaled.fresh = source.fresh;
        scaled.fresh.fill(false);
        self.gpu3d.put_scaled_frame(source.three_d);
        if let Some(panic) = panic {
            std::panic::resume_unwind(panic);
        }
    }

    /// Draws the screens `scale` times larger too (clamped to 1 to
    /// [`gpu3d::MAX_SCALE`]; 1 stops), 3D included from the next 3D frame.
    /// Until each line is drawn again they show the native screens
    /// enlarged.
    pub(crate) fn set_render_scale(&mut self, scale: usize) {
        let scale = scale.clamp(1, gpu3d::MAX_SCALE);
        self.gpu3d.set_render_scale(scale);
        self.scaled = ScaledScreens::enlarged(scale, &self.screen_a, &self.screen_b);
    }

    /// Takes over the display setup of `other`, the hardware this replaces
    /// on a reset or state load: the 3D renderer with its buffers, and the
    /// larger screens, which show this hardware's screens enlarged until
    /// they are drawn again.
    pub(crate) fn take_display_from(&mut self, other: &mut Self) {
        self.gpu3d.take_renderer_from(&mut other.gpu3d);
        self.scaled = ScaledScreens::enlarged(other.scaled.scale, &self.screen_a, &self.screen_b);
    }

    /// DISPCAPCNT, kept with the raw I/O registers.
    fn capture_control(&self) -> Control {
        Control(u32::from_le_bytes([
            self.io9[0x64],
            self.io9[0x65],
            self.io9[0x66],
            self.io9[0x67],
        ]))
    }

    /// Captures `line`, and finishes the capture after its last line.
    fn capture(&mut self, line: u16) {
        let control = self.capture_control();
        let display_bank = u32::from(self.engine_a.read_register(0x02) >> 2) & 3;
        capture::capture_line(
            control,
            line,
            self.engine_a.capture_line(),
            Some(self.gpu3d.line(line)),
            display_bank,
            &mut self.vram,
        );
        if line + 1 == control.size().1 {
            self.capturing = false;
            self.io9[0x67] &= 0x7F;
        }
    }

    /// The line-start event at `time`.
    pub(crate) fn line_start(&mut self, time: u64) -> Moment {
        self.hblank = false;
        self.vcount = (self.vcount + 1) % LINES;
        if self.vcount == 0 {
            self.capturing = self.capture_control().enabled();
        }
        match self.vcount {
            VISIBLE_LINES => {
                if self.scaled.scale > 1 {
                    self.finish_scaled_screens();
                }
                self.request_display_interrupt(1 << 3, irq::VBLANK);
                self.trigger_dma(Timing::VBlank);
                // Swap buffers takes effect and geometry resumes.
                self.gpu3d.vblank();
            }
            // The 3D engine starts drawing the next frame here, with the
            // registers the VBlank handler set.
            RENDER_3D_LINE => self.gpu3d.render_frame(&self.vram),
            _ => {}
        }
        if DISPLAY_START_DMA_LINES.contains(&self.vcount) {
            self.arm9.dma.trigger(Timing::DisplayStart);
        } else if self.vcount == DISPLAY_START_DMA_LINES.end {
            self.arm9.dma.stop(Timing::DisplayStart);
        }
        for side in [Side::Arm9, Side::Arm7] {
            let stored = self.cpu(side).dispstat;
            let setting = (stored >> 8) | (stored & 0x80) << 1;
            if self.vcount == setting && stored & (1 << 5) != 0 {
                self.cpu_mut(side).interrupts.request(irq::VCOUNT);
            }
        }
        self.scheduler
            .schedule_at(time + HBLANK_START, Event::HBlank);
        Moment::LineStart(self.vcount)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_events(hw: &mut Hardware, until: u64) -> Vec<Moment> {
        let mut moments = Vec::new();
        let now = hw.scheduler.now();
        hw.scheduler.advance(until - now);
        while let Some(due) = hw.scheduler.pop_due() {
            moments.push(match due.event {
                Event::HBlank => hw.hblank(due.time),
                Event::LineStart => hw.line_start(due.time),
                _ => continue,
            });
        }
        moments
    }

    #[test]
    fn a_frame_has_263_lines_and_vblank_at_192() {
        let mut hw = Hardware::new(vec![0; 0x1000].into(), vec![0; 0x4000].into());
        hw.arm9.dispstat = 1 << 3;
        hw.start_display();
        let moments = run_events(&mut hw, FRAME_CYCLES);
        let starts = moments
            .iter()
            .filter(|moment| matches!(moment, Moment::LineStart(_)))
            .count();
        assert_eq!(starts, 263);
        assert!(moments.contains(&Moment::LineStart(192)));
        assert_ne!(hw.arm9.interrupts.flags & irq::VBLANK, 0);
        assert_eq!(
            hw.arm7.interrupts.flags & irq::VBLANK,
            0,
            "the ARM7 did not ask"
        );
        assert_eq!(hw.vcount, 0);
    }

    #[test]
    fn engine_a_draws_a_direct_colour_bitmap_like_pokemon_white() {
        let mut hw = Hardware::new(vec![0; 0x1000].into(), vec![0; 0x4000].into());
        hw.arm9_write_8(0x0400_0241, 0x81); // B to engine A BG at 0
        hw.arm9_write_16(0x0600_0000, 0x801F); // an opaque red pixel
        hw.arm9_write_32(0x0400_0000, 0x4001_0C05);
        hw.arm9_write_16(0x0400_000C, 0x4084);
        hw.arm9_write_16(0x0400_000E, 0x4885);
        hw.draw_line(0);
        assert_eq!(hw.screen_a[0], 0xFFFF_0000, "{:08X}", hw.screen_a[0]);
    }

    #[test]
    fn start_of_display_dma_runs_on_lines_2_to_193_then_switches_off() {
        let mut hw = Hardware::new(vec![0; 0x1000].into(), vec![0; 0x4000].into());
        hw.start_display();
        // ARM9 DMA 0: start of display, repeating, 4 halfwords a line.
        hw.arm9_write_32(0x0400_00B8, 1 << 31 | 1 << 25 | 3 << 27 | 4);
        let mut lines = Vec::new();
        for line in 1..LINES {
            run_events(&mut hw, LINE_CYCLES * u64::from(line));
            if hw.arm9.dma.next_pending() == Some(0) {
                lines.push(line);
                hw.arm9.dma.finish(0, 4, 0, 0);
            }
        }
        assert_eq!(lines, (2..194).collect::<Vec<_>>());
        assert_eq!(hw.arm9_read_32(0x0400_00B8) & 1 << 31, 0, "switched off");
    }

    #[test]
    fn vcount_match_uses_nine_bits() {
        let mut hw = Hardware::new(vec![0; 0x1000].into(), vec![0; 0x4000].into());
        // Line 200: 0xC8 in bits 8–15, bit 8 of the line in bit 7 clear.
        hw.arm7.dispstat = 0xC8 << 8 | 1 << 5;
        hw.start_display();
        run_events(&mut hw, LINE_CYCLES * 200 + 10);
        assert_ne!(hw.arm7.interrupts.flags & irq::VCOUNT, 0);
    }
}
