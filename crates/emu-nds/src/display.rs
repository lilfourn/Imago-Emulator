//! Display timing (GBATEK, "DS Video Display Timings").
//!
//! A frame is 263 lines of 2,130 bus cycles (355 dots of 6 cycles); lines
//! 192–262 are VBlank. Each CPU has its own DISPSTAT, so each decides which
//! of the VBlank, HBlank and VCount-match interrupts it wants.

use crate::dma::Timing;
use crate::gpu2d::WIDTH;
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
/// Lines shown.
pub(crate) const VISIBLE_LINES: u16 = 192;
/// Bus cycles per frame.
pub(crate) const FRAME_CYCLES: u64 = LINE_CYCLES * LINES as u64;

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

    /// Both 2D engines draw `line` into their screens.
    fn draw_line(&mut self, line: u16) {
        let start = usize::from(line) * WIDTH;
        let mut out = [0; WIDTH];
        let vram = &self.vram;
        self.engine_a.render_line(
            line,
            &vram.engine(Engine::A),
            |offset| vram.read_16(Region::Lcdc, offset),
            None,
            &mut out,
        );
        self.screen_a[start..start + WIDTH].copy_from_slice(&out);
        self.engine_b
            .render_line(line, &vram.engine(Engine::B), |_| 0, None, &mut out);
        self.screen_b[start..start + WIDTH].copy_from_slice(&out);
    }

    /// The line-start event at `time`.
    pub(crate) fn line_start(&mut self, time: u64) -> Moment {
        self.hblank = false;
        self.vcount = (self.vcount + 1) % LINES;
        if self.vcount == VISIBLE_LINES {
            self.request_display_interrupt(1 << 3, irq::VBLANK);
            self.trigger_dma(Timing::VBlank);
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
    fn vcount_match_uses_nine_bits() {
        let mut hw = Hardware::new(vec![0; 0x1000].into(), vec![0; 0x4000].into());
        // Line 200: 0xC8 in bits 8–15, bit 8 of the line in bit 7 clear.
        hw.arm7.dispstat = 0xC8 << 8 | 1 << 5;
        hw.start_display();
        run_events(&mut hw, LINE_CYCLES * 200 + 10);
        assert_ne!(hw.arm7.interrupts.flags & irq::VCOUNT, 0);
    }
}
