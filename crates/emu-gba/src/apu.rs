//! GBA sound (GBATEK, "GBA Sound Controller", "GBA Sound Channel A and B -
//! DMA Sound").
//!
//! The Game Boy's four channels (shared with the GB core in `emu-psg`) plus
//! two 8-bit DMA sound channels. Each DMA channel plays from a 32-byte FIFO:
//! when its timer (0 or 1) overflows it takes the next sample, and when the
//! FIFO is half empty it asks DMA for more. The mixer adds everything to
//! SOUNDBIAS and clamps to the hardware's 10-bit output.

use emu_core::StereoSample;
use emu_psg::{Psg, Variant};
use emu_state::payload;
use serde::{Deserialize, Serialize};

/// Output sample rate in Hz.
pub(crate) const SAMPLE_RATE: u32 = 32_768;
/// System clock cycles per output sample.
pub(crate) const CYCLES_PER_SAMPLE: u64 = 16_777_216 / SAMPLE_RATE as u64;
/// System clock cycles per 512 Hz frame-sequencer step.
const CYCLES_PER_FRAME_STEP: u64 = 16_777_216 / 512;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Fifo {
    #[serde(with = "payload::AtMost::<32>")]
    samples: std::collections::VecDeque<i8>,
    /// The sample being played.
    current: i8,
}

impl Fifo {
    fn push(&mut self, byte: u8) {
        if self.samples.len() < 32 {
            self.samples.push_back(byte as i8);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Apu {
    psg: Psg,
    fifos: [Fifo; 2],
    /// SOUNDCNT_H: volumes, routing, timer selection.
    control: u16,
    bias: u16,
    /// Cycles into the current frame-sequencer step.
    #[serde(with = "payload::Below::<{ CYCLES_PER_FRAME_STEP }>")]
    frame_step_cycles: u64,
    /// Samples produced but not yet collected. Not machine state.
    #[serde(skip)]
    samples: Vec<StereoSample>,
}

/// The PSG register (Game Boy number) at each GBA sound byte offset from
/// 0x04000060.
fn psg_register(offset: u32) -> Option<u8> {
    Some(match offset {
        0x00 => 0x10,
        0x02 => 0x11,
        0x03 => 0x12,
        0x04 => 0x13,
        0x05 => 0x14,
        0x08 => 0x16,
        0x09 => 0x17,
        0x0C => 0x18,
        0x0D => 0x19,
        0x10 => 0x1A,
        0x12 => 0x1B,
        0x13 => 0x1C,
        0x14 => 0x1D,
        0x15 => 0x1E,
        0x18 => 0x20,
        0x19 => 0x21,
        0x1C => 0x22,
        0x1D => 0x23,
        0x20 => 0x24,
        0x21 => 0x25,
        0x24 => 0x26,
        _ => return None,
    })
}

impl Apu {
    pub(crate) fn new() -> Self {
        Self {
            psg: Psg::new(Variant::Agb),
            fifos: [Fifo::default(), Fifo::default()],
            control: 0,
            bias: 0x200,
            frame_step_cycles: 0,
            samples: Vec::new(),
        }
    }

    /// Whether the sound generator, read from a save state, holds only
    /// values its registers can ([`Psg::in_range`]).
    pub(crate) fn in_range(&self) -> bool {
        self.psg.in_range()
    }

    /// Moves the samples produced so far into `out`.
    pub(crate) fn drain_samples(&mut self, out: &mut Vec<StereoSample>) {
        out.append(&mut self.samples);
    }

    /// Reads the halfword at `offset` (even) from 0x04000060, with the bits
    /// the hardware does not read back as 0 (GBATEK, "GBA Sound
    /// Controller"). `None` for the write-only FIFOs and the unused
    /// registers after SOUNDBIAS, which read as open bus.
    pub(crate) fn read_16(&self, offset: u32) -> Option<u16> {
        let psg = |byte: u32| psg_register(byte).map_or(0, |register| self.psg.read(register));
        let wave = |byte: u32| self.psg.read_wave((byte - 0x30) as u8);
        let pair = |read: &dyn Fn(u32) -> u8| u16::from_le_bytes([read(offset), read(offset + 1)]);
        let readable = match offset {
            0x00 => 0x007F,               // SOUND1CNT_L: sweep
            0x02 | 0x08 => 0xFFC0,        // SOUND1CNT_H, SOUND2CNT_L: duty and envelope
            0x04 | 0x0C | 0x14 => 0x4000, // frequency registers: length enable
            0x10 => 0x00E0,               // SOUND3CNT_L: banks and enable
            0x12 => 0xE000,               // SOUND3CNT_H: volume
            0x18 => 0xFF00,               // SOUND4CNT_L: envelope
            0x1C => 0x40FF,               // SOUND4CNT_H: frequency and length enable
            0x20 => 0xFF77,               // SOUNDCNT_L: PSG volume and panning
            0x24 => 0x008F,               // SOUNDCNT_X: master enable and channel status
            0x22 => return Some(self.control & 0x770F),
            0x28 => return Some(self.bias),
            // The upper halves of 32-bit registers.
            0x06 | 0x0A | 0x0E | 0x16 | 0x1A | 0x1E | 0x26 | 0x2A => return Some(0),
            0x30..=0x3F => return Some(pair(&wave)),
            _ => return None,
        };
        Some(pair(&psg) & readable)
    }

    /// Writes the byte at `offset` from 0x04000060. SOUNDCNT_H's reset
    /// bits empty their FIFOs.
    pub(crate) fn write_8(&mut self, offset: u32, value: u8) {
        match offset {
            0x22 => self.control = (self.control & 0xFF00) | u16::from(value),
            0x23 => {
                self.control = (self.control & 0x00FF) | u16::from(value) << 8;
                for fifo in 0..2 {
                    if value & (0x08 << (fifo * 4)) != 0 {
                        self.fifos[fifo] = Fifo::default();
                    }
                }
                // The reset bits read back as zero.
                self.control &= !0x8800;
            }
            0x28 => self.bias = (self.bias & 0xFF00) | u16::from(value),
            0x29 => self.bias = (self.bias & 0x00FF) | u16::from(value) << 8,
            0x30..=0x3F => self.psg.write_wave((offset - 0x30) as u8, value),
            0x40..=0x47 => self.fifos[((offset - 0x40) / 4) as usize].push(value),
            _ => {
                if let Some(register) = psg_register(offset) {
                    self.psg.write(register, value);
                }
            }
        }
    }

    /// Timer `timer` (0 or 1) overflowed: each FIFO driven by it plays its
    /// next sample. Returns the FIFOs (bit 0 A, bit 1 B) that are at most
    /// half full and want a DMA refill.
    pub(crate) fn timer_overflow(&mut self, timer: usize) -> u8 {
        let mut refill = 0;
        for fifo in 0..2 {
            let selected = usize::from((self.control >> (10 + fifo * 4)) & 1);
            if selected != timer {
                continue;
            }
            let channel = &mut self.fifos[fifo];
            if let Some(sample) = channel.samples.pop_front() {
                channel.current = sample;
            }
            if channel.samples.len() <= 16 {
                refill |= 1 << fifo;
            }
        }
        refill
    }

    /// Advances one output-sample period and produces that sample.
    pub(crate) fn sample(&mut self) {
        // The Game Boy channels run at a quarter of the system clock.
        self.psg.tick((CYCLES_PER_SAMPLE / 4) as u32);
        self.frame_step_cycles += CYCLES_PER_SAMPLE;
        if self.frame_step_cycles >= CYCLES_PER_FRAME_STEP {
            self.frame_step_cycles -= CYCLES_PER_FRAME_STEP;
            self.psg.clock_frame_sequencer();
        }

        // SOUNDCNT_H bits 0-1: PSG at 25%, 50% or 100%.
        let psg_shift = match self.control & 3 {
            0 => 2,
            1 => 1,
            _ => 0,
        };
        let enabled = self.psg.powered();
        let mut sides = [0i32; 2];
        if enabled {
            sides = self.psg.mixed().map(|side| side >> psg_shift);
            for fifo in 0..2 {
                let full_volume = self.control & (1 << (2 + fifo)) != 0;
                let sample = i32::from(self.fifos[fifo].current) << if full_volume { 2 } else { 1 };
                // Bits 8/12 enable the right side, 9/13 the left.
                if self.control & (1 << (9 + fifo * 4)) != 0 {
                    sides[0] += sample;
                }
                if self.control & (1 << (8 + fifo * 4)) != 0 {
                    sides[1] += sample;
                }
            }
        }
        let bias = i32::from(self.bias & 0x3FE);
        let to_pcm = |value: i32| ((value + bias).clamp(0, 0x3FF) - 0x200) as i16 * 64;
        // A caller that never drains keeps at most one second of audio.
        if self.samples.len() >= SAMPLE_RATE as usize {
            self.samples.clear();
        }
        self.samples.push([to_pcm(sides[0]), to_pcm(sides[1])]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifo_plays_samples_on_its_timer_and_asks_for_more() {
        let mut apu = Apu::new();
        apu.write_8(0x23, 0x43); // FIFO A: timer 0, both sides; FIFO B: timer 1
        apu.write_8(0x22, 0x04); // FIFO A at 100%
        for byte in [10, 20, 30, 40] {
            apu.write_8(0x40, byte);
        }
        assert_eq!(apu.timer_overflow(1), 0b10); // empty FIFO B wants a refill
        assert_eq!(apu.timer_overflow(0), 0b01);
        assert_eq!(apu.fifos[0].current, 10);
        apu.sample();
        let mut out = Vec::new();
        apu.drain_samples(&mut out);
        assert_eq!(out, [[10 * 4 * 64, 10 * 4 * 64]]);
    }

    #[test]
    fn psg_registers_map_to_the_game_boy_numbers() {
        let mut apu = Apu::new();
        apu.write_8(0x20, 0x77); // SOUNDCNT_L low byte = NR50
        assert_eq!(apu.read_16(0x20), Some(0x0077));
        apu.write_8(0x24, 0x00); // SOUNDCNT_X = NR52: power off
        assert_eq!(apu.read_16(0x24), Some(0));
    }

    /// Hardware values from the mGBA suite's I/O read tests, after writing
    /// 0xFFFF with the sound circuit on.
    #[test]
    fn registers_read_back_only_their_readable_bits() {
        let mut apu = Apu::new();
        apu.write_8(0x24, 0x80);
        let mut write_ones = |offset: u32| {
            apu.write_8(offset, 0xFF);
            apu.write_8(offset + 1, 0xFF);
        };
        for offset in [0x00, 0x02, 0x08, 0x10, 0x12, 0x18, 0x1C, 0x20, 0x22] {
            write_ones(offset);
        }
        let read = |offset| apu.read_16(offset);
        assert_eq!(read(0x00), Some(0x007F));
        assert_eq!(read(0x02), Some(0xFFC0));
        assert_eq!(read(0x08), Some(0xFFC0));
        assert_eq!(read(0x10), Some(0x00E0));
        assert_eq!(read(0x12), Some(0xE000));
        assert_eq!(read(0x18), Some(0xFF00));
        assert_eq!(read(0x1C), Some(0x40FF));
        assert_eq!(read(0x20), Some(0xFF77));
        assert_eq!(read(0x22), Some(0x770F));
        assert_eq!(read(0x06), Some(0));
        assert_eq!(read(0x40), None);
        assert_eq!(read(0x2C), None);
    }

    #[test]
    fn makes_32_khz_audio() {
        let mut apu = Apu::new();
        for _ in 0..SAMPLE_RATE {
            apu.sample();
        }
        let mut out = Vec::new();
        apu.drain_samples(&mut out);
        assert_eq!(out.len(), SAMPLE_RATE as usize);
    }
}
