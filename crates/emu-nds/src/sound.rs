//! The ARM7's sound unit: 16 channels and the mixer (GBATEK, "DS Sound").
//!
//! Every channel can play 8- or 16-bit PCM or IMA-ADPCM from memory;
//! channels 8–13 can instead make a square wave and 14–15 noise. A channel's
//! timer counts at half the bus clock and each overflow moves to the next
//! sample (no interpolation, as on hardware). The mixer runs at 32,768 Hz:
//! the bus calls [`Sound::sample`] every 1,024 bus cycles with a way to
//! read memory, and collects stereo samples.
//!
//! Sound capture (SNDCAPxCNT) is not emulated yet.

use emu_core::StereoSample;
use serde::{Deserialize, Serialize};

/// Output sample rate in Hz.
pub(crate) const SAMPLE_RATE: u32 = 32_768;
/// Bus cycles per output sample.
pub(crate) const CYCLES_PER_SAMPLE: u64 = 1024;
/// Channel timer ticks (half the bus clock) per output sample.
const TICKS_PER_SAMPLE: u32 = 512;

/// IMA-ADPCM step sizes.
const ADPCM_STEPS: [u16; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];
const ADPCM_INDEX_CHANGES: [i8; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Pcm8,
    Pcm16,
    Adpcm,
    /// Square wave on channels 8–13, noise on 14–15, silence elsewhere.
    Psg,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Channel {
    /// SOUNDxCNT.
    control: u32,
    source: u32,
    timer: u16,
    loop_start: u16,
    length: u32,
    /// Timer ticks into the current sample.
    ticks: u32,
    /// Samples played since the channel started (or looped).
    position: u32,
    /// The sample being output.
    current: i16,
    /// ADPCM decoder state, and its copy at the loop start.
    adpcm_value: i16,
    adpcm_index: u8,
    adpcm_loop: Option<(i16, u8)>,
    /// The noise generator's shift register.
    lfsr: u16,
}

impl Channel {
    fn running(&self) -> bool {
        self.control & (1 << 31) != 0
    }

    fn format(&self) -> Format {
        match (self.control >> 29) & 3 {
            0 => Format::Pcm8,
            1 => Format::Pcm16,
            2 => Format::Adpcm,
            _ => Format::Psg,
        }
    }

    fn start(&mut self) {
        self.ticks = 0;
        self.position = 0;
        self.current = 0;
        self.adpcm_loop = None;
        self.lfsr = 0x7FFF;
    }

    /// Samples per 32-bit word of data.
    fn samples_per_word(&self) -> u32 {
        match self.format() {
            Format::Pcm8 => 4,
            Format::Pcm16 => 2,
            Format::Adpcm => 8,
            Format::Psg => 1,
        }
    }

    /// Moves to the next sample, reading memory through `read`.
    fn advance(&mut self, index: usize, read: &mut impl FnMut(u32) -> u8) {
        if self.format() == Format::Psg {
            self.current = self.psg_sample(index);
            self.position = self.position.wrapping_add(1);
            return;
        }
        let per_word = self.samples_per_word();
        // ADPCM's first word is its header, not samples.
        let header = u32::from(self.format() == Format::Adpcm) * per_word;
        let loop_start = u32::from(self.loop_start) * per_word;
        let end = loop_start + self.length * per_word;
        if self.position < header {
            self.read_adpcm_header(read);
            self.position = header;
        }
        if self.position >= end.max(header + 1) {
            match (self.control >> 27) & 3 {
                1 => {
                    self.position = loop_start.max(header);
                    if let Some((value, step)) = self.adpcm_loop {
                        self.adpcm_value = value;
                        self.adpcm_index = step;
                    }
                }
                _ => {
                    // One-shot: stop, holding the last sample if asked.
                    self.control &= !(1 << 31);
                    if self.control & (1 << 15) == 0 {
                        self.current = 0;
                    }
                    return;
                }
            }
        }
        if self.format() == Format::Adpcm && self.position == loop_start.max(header) {
            self.adpcm_loop
                .get_or_insert((self.adpcm_value, self.adpcm_index));
        }
        let address = self.source & 0x07FF_FFFC;
        self.current = match self.format() {
            Format::Pcm8 => i16::from(read(address + self.position) as i8) << 8,
            Format::Pcm16 => {
                let at = address + self.position * 2;
                i16::from_le_bytes([read(at), read(at + 1)])
            }
            Format::Adpcm => {
                let byte = read(address + self.position / 2);
                let nibble = if self.position.is_multiple_of(2) {
                    byte & 0xF
                } else {
                    byte >> 4
                };
                self.decode_adpcm(nibble)
            }
            Format::Psg => 0,
        };
        self.position += 1;
    }

    fn read_adpcm_header(&mut self, read: &mut impl FnMut(u32) -> u8) {
        let address = self.source & 0x07FF_FFFC;
        self.adpcm_value = i16::from_le_bytes([read(address), read(address + 1)]);
        self.adpcm_index = read(address + 2).min(88);
    }

    fn decode_adpcm(&mut self, nibble: u8) -> i16 {
        let step = i32::from(ADPCM_STEPS[usize::from(self.adpcm_index)]);
        let mut difference = step / 8;
        if nibble & 1 != 0 {
            difference += step / 4;
        }
        if nibble & 2 != 0 {
            difference += step / 2;
        }
        if nibble & 4 != 0 {
            difference += step;
        }
        let value = if nibble & 8 == 0 {
            (i32::from(self.adpcm_value) + difference).min(0x7FFF)
        } else {
            (i32::from(self.adpcm_value) - difference).max(-0x7FFF)
        };
        self.adpcm_value = value as i16;
        let index =
            i32::from(self.adpcm_index) + i32::from(ADPCM_INDEX_CHANGES[usize::from(nibble & 7)]);
        self.adpcm_index = index.clamp(0, 88) as u8;
        self.adpcm_value
    }

    fn psg_sample(&mut self, index: usize) -> i16 {
        match index {
            8..=13 => {
                let duty = (self.control >> 24) & 7;
                // High for duty + 1 of every eight steps.
                if self.position % 8 > 6 - duty.min(6) || duty == 7 {
                    0x7FFF
                } else {
                    -0x7FFF
                }
            }
            14 | 15 => {
                let carry = self.lfsr & 1 != 0;
                self.lfsr >>= 1;
                if carry {
                    self.lfsr ^= 0x6000;
                    -0x7FFF
                } else {
                    0x7FFF
                }
            }
            _ => 0,
        }
    }

    /// This channel's output after its volume and panning (0 left, 64
    /// centre, 127 right): (left, right), scaled up by 2^7 × 2^7 from
    /// 16-bit samples.
    fn output(&self) -> (i64, i64) {
        let volume = i64::from(self.control & 0x7F);
        let shift = [0, 1, 2, 4][((self.control >> 8) & 3) as usize];
        let pan = i64::from((self.control >> 16) & 0x7F);
        let level = (i64::from(self.current) * volume) >> shift;
        (level * (128 - pan), level * pan)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Sound {
    channels: [Channel; 16],
    /// SOUNDCNT.
    control: u16,
    bias: u16,
    /// Samples produced but not yet collected. Not machine state.
    #[serde(skip)]
    samples: Vec<StereoSample>,
}

impl Default for Sound {
    fn default() -> Self {
        Self {
            channels: Default::default(),
            control: 0,
            bias: 0x200,
            samples: Vec::new(),
        }
    }
}

impl Sound {
    /// Reads the 32-bit register at `offset` from 0x04000400.
    pub(crate) fn read(&self, offset: u32) -> u32 {
        match offset {
            0x000..=0x0FF => {
                let channel = &self.channels[(offset / 16) as usize];
                // Only SOUNDxCNT reads back.
                if offset.is_multiple_of(16) {
                    channel.control
                } else {
                    0
                }
            }
            0x100 => u32::from(self.control),
            0x104 => u32::from(self.bias),
            _ => 0,
        }
    }

    /// Writes the 32-bit register at `offset` from 0x04000400, with
    /// `mask` marking which bytes the access covered.
    pub(crate) fn write(&mut self, offset: u32, value: u32, mask: u32) {
        let merge = |old: u32| (old & !mask) | (value & mask);
        match offset {
            0x000..=0x0FF => {
                let channel = &mut self.channels[(offset / 16) as usize];
                match offset % 16 {
                    0 => {
                        let was_running = channel.running();
                        channel.control = merge(channel.control) & 0xFF7F_837F;
                        if !was_running && channel.running() {
                            channel.start();
                        }
                    }
                    4 => channel.source = merge(channel.source) & 0x07FF_FFFC,
                    8 => {
                        let both =
                            merge(u32::from(channel.timer) | u32::from(channel.loop_start) << 16);
                        channel.timer = both as u16;
                        channel.loop_start = (both >> 16) as u16;
                    }
                    _ => channel.length = merge(channel.length) & 0x3F_FFFF,
                }
            }
            0x100 => self.control = merge(u32::from(self.control)) as u16 & 0xBF7F,
            0x104 => self.bias = merge(u32::from(self.bias)) as u16 & 0x3FF,
            _ => {}
        }
    }

    /// Moves the samples produced so far into `out`.
    pub(crate) fn drain_samples(&mut self, out: &mut Vec<StereoSample>) {
        out.append(&mut self.samples);
    }

    /// Advances one output-sample period and mixes that sample. `read`
    /// reads a byte of the ARM7's memory.
    pub(crate) fn sample(&mut self, mut read: impl FnMut(u32) -> u8) {
        let enabled = self.control & 0x8000 != 0;
        let (mut left, mut right) = (0i64, 0i64);
        for (index, channel) in self.channels.iter_mut().enumerate() {
            if !channel.running() {
                continue;
            }
            let period = 0x1_0000 - u32::from(channel.timer);
            channel.ticks += TICKS_PER_SAMPLE;
            while channel.ticks >= period && channel.running() {
                channel.ticks -= period;
                channel.advance(index, &mut read);
            }
            // SOUNDCNT bits 12 and 13 keep channels 1 and 3 out of the mix.
            let muted = (index == 1 && self.control & (1 << 12) != 0)
                || (index == 3 && self.control & (1 << 13) != 0);
            if !muted {
                let (l, r) = channel.output();
                left += l;
                right += r;
            }
        }
        let master = i64::from(self.control & 0x7F);
        // Undo the channel scaling (2^14), apply the master volume (2^7),
        // and keep a little headroom for 16 channels.
        let to_pcm = |mix: i64| {
            let scaled = (mix * master) >> (14 + 7 + 2);
            scaled.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16
        };
        let sample = if enabled {
            [to_pcm(left), to_pcm(right)]
        } else {
            [0, 0]
        };
        // A caller that never drains keeps at most one second of audio.
        if self.samples.len() >= SAMPLE_RATE as usize {
            self.samples.clear();
        }
        self.samples.push(sample);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: u32 = u32::MAX;

    /// Starts channel 0 playing `format` from 0 with `words` words, timer
    /// `timer`, full volume, centred.
    fn start(sound: &mut Sound, format: u32, repeat: u32, words: u32, timer: u16) {
        sound.write(0x100, 0x807F, FULL);
        sound.write(0x04, 0, FULL);
        sound.write(0x08, u32::from(timer), FULL);
        sound.write(0x0C, words, FULL);
        let control = 1 << 31 | format << 29 | repeat << 27 | 64 << 16 | 0x7F;
        sound.write(0x00, control, FULL);
    }

    fn collect(sound: &mut Sound, memory: &[u8], count: usize) -> Vec<StereoSample> {
        for _ in 0..count {
            sound.sample(|address| memory.get(address as usize).copied().unwrap_or(0));
        }
        let mut out = Vec::new();
        sound.drain_samples(&mut out);
        out
    }

    #[test]
    fn pcm8_one_shot_plays_then_stops() {
        let mut sound = Sound::default();
        let memory = [0x40u8; 8];
        // One sample per output sample: period 512 ticks.
        start(&mut sound, 0, 2, 2, (0x1_0000 - 512) as u16);
        let out = collect(&mut sound, &memory, 12);
        assert!(
            out[0][0] > 0 && out[0][0] == out[0][1],
            "centred: {:?}",
            out[0]
        );
        assert_eq!(sound.read(0) >> 31, 0, "stopped after 8 samples");
        assert_eq!(out[11], [0, 0]);
    }

    #[test]
    fn looping_restarts_at_the_loop_start() {
        let mut sound = Sound::default();
        let memory = [0x10u8; 16];
        start(&mut sound, 0, 1, 1, (0x1_0000 - 512) as u16);
        collect(&mut sound, &memory, 100);
        assert_ne!(sound.read(0) >> 31, 0, "still running");
    }

    #[test]
    fn adpcm_decodes_from_its_header() {
        let mut channel = Channel {
            adpcm_value: 0,
            adpcm_index: 0,
            ..Channel::default()
        };
        // Step 7: nibble 7 adds 7/8 + 7/4 + 7/2 + 7 = 0 + 1 + 3 + 7.
        assert_eq!(channel.decode_adpcm(7), 11);
        assert_eq!(channel.adpcm_index, 8);
        assert!(channel.decode_adpcm(0xF) < 11);
    }

    #[test]
    fn noise_comes_from_a_15_bit_lfsr() {
        let mut channel = Channel::default();
        channel.start();
        let first: Vec<i16> = (0..4).map(|_| channel.psg_sample(14)).collect();
        assert_eq!(first, [-0x7FFF, -0x7FFF, -0x7FFF, -0x7FFF]);
        assert_eq!(channel.psg_sample(5), 0, "channel 5 has no PSG");
    }

    #[test]
    fn master_disable_silences_everything() {
        let mut sound = Sound::default();
        start(&mut sound, 0, 1, 1, (0x1_0000 - 512) as u16);
        sound.write(0x100, 0, FULL);
        let out = collect(&mut sound, &[0x7F; 8], 4);
        assert!(out.iter().all(|sample| *sample == [0, 0]));
    }
}
