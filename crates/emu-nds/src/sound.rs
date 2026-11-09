//! The ARM7's sound unit: 16 channels, the mixer and two capture units
//! (GBATEK, "DS Sound").
//!
//! Every channel can play 8- or 16-bit PCM or IMA-ADPCM from memory;
//! channels 8–13 can instead make a square wave and 14–15 noise. A channel's
//! timer counts at half the bus clock and each overflow moves to the next
//! sample (no interpolation, as on hardware). The mixer runs at about
//! 32.7 kHz: the bus calls [`Sound::sample`] every 1,024 bus cycles with
//! the ARM7's view of [`Memory`], and collects stereo samples.
//!
//! Capture 0 records the left mixer (or channel 0) into memory and capture 1
//! the right mixer (or channel 2), at the rate of channel 1's or 3's timer.
//! Those channels are meant to play the recording back, for effects such as
//! reverb, so SOUNDCNT can send them straight to the speakers. Compared with
//! hardware, a capture records the mix of the output sample it falls in
//! (the real mixer runs at about 1 MHz), its timer counts from its own
//! start rather than sharing channel 1's or 3's count, and it writes memory
//! at once rather than through a 16-byte FIFO.

use emu_core::StereoSample;
use emu_state::payload;
use serde::{Deserialize, Serialize};

/// Memory as the sound unit sees it, the ARM7's view: channels read
/// samples from it and capture writes them.
pub(crate) trait Memory {
    fn read_8(&mut self, address: u32) -> u8;
    fn write_8(&mut self, address: u32, value: u8);
}

/// The bus clock, 33.513982 MHz (GBATEK, "DS Technical Data").
const BUS_HZ: u32 = 33_513_982;
/// Bus cycles per output sample.
pub(crate) const CYCLES_PER_SAMPLE: u64 = 1024;
/// Output sample rate in Hz: 33,513,982 / 1,024 = 32,728.498 Hz, rounded
/// down to 32,728. The 0.0015% left over is far inside what a frontend's
/// rate control absorbs; the nominal 32,768 Hz was 0.12% fast.
pub(crate) const SAMPLE_RATE: u32 = BUS_HZ / CYCLES_PER_SAMPLE as u32;
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
    /// SOUNDxLEN's 22 bits.
    #[serde(with = "payload::Below::<0x40_0000>")]
    length: u32,
    /// Timer ticks into the current sample: less than a timer's longest
    /// period and a sample's ticks.
    #[serde(with = "payload::Below::<0x2_0000>")]
    ticks: u32,
    /// Samples played since the channel started (or looped). A PSG
    /// channel's goes on counting for as long as it plays.
    position: u32,
    /// The sample being output.
    current: i16,
    /// ADPCM decoder state, and its copy at the loop start.
    adpcm_value: i16,
    #[serde(with = "payload::Below::<89>")]
    adpcm_index: u8,
    /// The value and the index, which is below 89 too ([`Sound::in_range`]).
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

    /// Timer ticks per sample. Captures 0 and 1 run at channel 1's and
    /// 3's.
    fn period(&self) -> u32 {
        0x1_0000 - u32::from(self.timer)
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

    /// Moves to the next sample, reading `memory`.
    fn advance(&mut self, index: usize, memory: &mut impl Memory) {
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
            self.read_adpcm_header(memory);
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
            // The sums wrap: a save state may hold any position.
            Format::Pcm8 => {
                i16::from(memory.read_8(address.wrapping_add(self.position)) as i8) << 8
            }
            Format::Pcm16 => {
                let at = address.wrapping_add(self.position.wrapping_mul(2));
                i16::from_le_bytes([memory.read_8(at), memory.read_8(at.wrapping_add(1))])
            }
            Format::Adpcm => {
                let byte = memory.read_8(address.wrapping_add(self.position / 2));
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

    fn read_adpcm_header(&mut self, memory: &mut impl Memory) {
        let address = self.source & 0x07FF_FFFC;
        self.adpcm_value = i16::from_le_bytes([memory.read_8(address), memory.read_8(address + 1)]);
        self.adpcm_index = memory.read_8(address + 2).min(88);
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

    /// This channel's sample after its volume, scaled up by 2^7.
    fn level(&self) -> i64 {
        let volume = i64::from(self.control & 0x7F);
        let shift = [0, 1, 2, 4][((self.control >> 8) & 3) as usize];
        (i64::from(self.current) * volume) >> shift
    }

    /// `level` after this channel's panning (0 left, 64 centre, 127
    /// right): [left, right], scaled up by 2^7 × 2^7 from 16-bit samples.
    fn pan(&self, level: i64) -> [i64; 2] {
        let pan = i64::from((self.control >> 16) & 0x7F);
        [level * (128 - pan), level * pan]
    }
}

/// Drops `bits` fractional bits the way capture does (GBATEK, "DS Sound
/// Notes"): positive values round down, and negative ones round up (toward
/// zero) when the top fractional bit is set.
fn strip_fraction(value: i32, bits: u32) -> i32 {
    let whole = value >> bits;
    if value < 0 && value & (1 << (bits - 1)) != 0 {
        whole + 1
    } else {
        whole
    }
}

/// A capture unit: SNDCAPxCNT, SNDCAPxDAD and SNDCAPxLEN, and how far it
/// has got.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Capture {
    /// SNDCAPxCNT.
    control: u8,
    /// Where recording starts, and restarts when it loops.
    destination: u32,
    /// The buffer's length in words; 0 acts as 1.
    length: u16,
    /// Timer ticks into the current sample, as a channel's.
    #[serde(with = "payload::Below::<0x2_0000>")]
    ticks: u32,
    /// Bytes written since the capture started (or looped): within the
    /// longest buffer.
    #[serde(with = "payload::Below::<0x8_0000>")]
    position: u32,
}

impl Capture {
    fn running(&self) -> bool {
        self.control & 0x80 != 0
    }

    /// Bit 0, which counts only while the capture runs: channel 1 (or 3)
    /// is added into channel 0 (or 2) rather than output on its own.
    fn adds(&self) -> bool {
        self.control & 0x81 == 0x81
    }

    /// Bit 1: record channel 0 (or 2) rather than the mixer.
    fn records_channel(&self) -> bool {
        self.control & 2 != 0
    }

    fn write_control(&mut self, value: u8) {
        let was_running = self.running();
        self.control = value & 0x8F;
        if !was_running && self.running() {
            self.ticks = 0;
            self.position = 0;
        }
    }

    /// Advances one output sample at the timer's `period`, recording
    /// `input` (16.8 fixed point) at each overflow.
    fn run(&mut self, period: u32, input: i32, memory: &mut impl Memory) {
        if !self.running() {
            return;
        }
        self.ticks += TICKS_PER_SAMPLE;
        while self.ticks >= period && self.running() {
            self.ticks -= period;
            self.record(input, memory);
        }
    }

    fn record(&mut self, input: i32, memory: &mut impl Memory) {
        let address = self.destination.wrapping_add(self.position) & 0x07FF_FFFF;
        // Bit 3: 8-bit samples, else 16-bit.
        if self.control & 8 != 0 {
            memory.write_8(address, strip_fraction(input, 16) as u8);
            self.position += 1;
        } else {
            let [low, high] = (strip_fraction(input, 8) as i16).to_le_bytes();
            memory.write_8(address, low);
            memory.write_8(address.wrapping_add(1), high);
            self.position += 2;
        }
        if self.position >= u32::from(self.length.max(1)) * 4 {
            self.position = 0;
            // Bit 2: one-shot, else loop.
            if self.control & 4 != 0 {
                self.control &= !0x80;
            }
        }
    }
}

/// One output sample's mix before the master volume, [left, right] pairs
/// scaled up by 2^7 × 2^7 from 16-bit samples.
#[derive(Debug, Default)]
struct Mix {
    /// The left and right mixers.
    mixer: [i64; 2],
    /// Channel 1's and channel 3's own outputs, which SOUNDCNT can send
    /// straight to the speakers.
    direct: [[i64; 2]; 2],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Sound {
    channels: [Channel; 16],
    /// SOUNDCNT.
    control: u16,
    bias: u16,
    captures: [Capture; 2],
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
            captures: Default::default(),
            samples: Vec::new(),
        }
    }
}

impl Sound {
    /// Whether the channels, read from a save state, have ADPCM steps
    /// within the table for their loops to go back to.
    pub(crate) fn in_range(&self) -> bool {
        self.channels
            .iter()
            .all(|channel| channel.adpcm_loop.is_none_or(|(_, index)| index <= 88))
    }

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
            0x108 => u32::from(self.captures[0].control) | u32::from(self.captures[1].control) << 8,
            // SNDCAPxDAD; SNDCAPxLEN does not read back.
            0x110 | 0x118 => self.captures[((offset - 0x110) / 8) as usize].destination,
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
            0x108 => {
                let [first, second] = &mut self.captures;
                let both = merge(u32::from(first.control) | u32::from(second.control) << 8);
                first.write_control(both as u8);
                second.write_control((both >> 8) as u8);
            }
            0x110 | 0x118 => {
                let capture = &mut self.captures[((offset - 0x110) / 8) as usize];
                capture.destination = merge(capture.destination) & 0x07FF_FFFC;
            }
            0x114 | 0x11C => {
                let capture = &mut self.captures[((offset - 0x114) / 8) as usize];
                capture.length = merge(u32::from(capture.length)) as u16;
            }
            _ => {}
        }
    }

    /// Moves the samples produced so far into `out`.
    pub(crate) fn drain_samples(&mut self, out: &mut Vec<StereoSample>) {
        out.append(&mut self.samples);
    }

    /// Advances one output-sample period: the channels move on, reading
    /// `memory`, the mixer mixes one stereo sample, and running captures
    /// record into `memory`.
    pub(crate) fn sample(&mut self, memory: &mut impl Memory) {
        let levels = self.advance_channels(memory);
        let mix = self.mix(&levels);
        for unit in 0..2 {
            let input = self.capture_input(unit, &levels, &mix);
            let period = self.channels[unit * 2 + 1].period();
            self.captures[unit].run(period, input, memory);
        }
        let sample = self.output(&mix);
        // A caller that never drains keeps at most one second of audio.
        if self.samples.len() >= SAMPLE_RATE as usize {
            self.samples.clear();
        }
        self.samples.push(sample);
    }

    /// Moves every playing channel on by one output sample and returns
    /// their levels ([`Channel::level`]), `None` for channels that are off.
    fn advance_channels(&mut self, memory: &mut impl Memory) -> [Option<i64>; 16] {
        let mut levels = [None; 16];
        for (index, channel) in self.channels.iter_mut().enumerate() {
            if !channel.running() {
                continue;
            }
            let period = channel.period();
            channel.ticks += TICKS_PER_SAMPLE;
            while channel.ticks >= period && channel.running() {
                channel.ticks -= period;
                channel.advance(index, memory);
            }
            levels[index] = Some(channel.level());
        }
        levels
    }

    /// Whether SOUNDCNT lets channel 1 (`unit` 0) or 3 (`unit` 1) into
    /// the mixers: bits 12 and 13 keep them out.
    fn mixes_second_channel(&self, unit: usize) -> bool {
        self.control & (1 << (12 + unit)) == 0
    }

    /// Pans and sums the channels' `levels`. In capture's add mode,
    /// channel 1 (or 3) joins channel 0 (or 2) before its panning, and is
    /// silent while channel 0 (or 2) is off (GBATEK, "DS Sound Capture").
    fn mix(&self, levels: &[Option<i64>; 16]) -> Mix {
        let mut mix = Mix::default();
        for (index, (channel, level)) in self.channels.iter().zip(levels).enumerate() {
            let Some(mut level) = *level else {
                continue;
            };
            // Channels 0 and 1 belong with capture 0, 2 and 3 with capture 1.
            let unit = index / 2;
            match index {
                0 | 2 if self.captures[unit].adds() && self.mixes_second_channel(unit) => {
                    level += levels[index + 1].unwrap_or(0);
                }
                1 | 3 if self.captures[unit].adds() => continue,
                1 | 3 => {
                    mix.direct[unit] = channel.pan(level);
                    if !self.mixes_second_channel(unit) {
                        continue;
                    }
                }
                _ => {}
            }
            let [left, right] = channel.pan(level);
            mix.mixer[0] += left;
            mix.mixer[1] += right;
        }
        mix
    }

    /// What capture `unit` records this sample, as 16.8 fixed point.
    ///
    /// The mixer is clipped to 16 bits. Channel 0 (or 2) is taken after its
    /// volume, with two hardware bugs: in add mode the sum with channel 1
    /// (or 3) wraps at 16 bits instead of clipping, and otherwise both
    /// channels being negative records -8000h.
    fn capture_input(&self, unit: usize, levels: &[Option<i64>; 16], mix: &Mix) -> i32 {
        let capture = &self.captures[unit];
        if !capture.records_channel() {
            // From 2^14 scaling to 2^8.
            return (mix.mixer[unit] >> 6).clamp(-0x80_0000, 0x7F_FFFF) as i32;
        }
        // From 2^7 scaling to 2^8.
        let first = levels[unit * 2].unwrap_or(0) << 1;
        let second = levels[unit * 2 + 1].unwrap_or(0) << 1;
        if capture.adds() {
            // Keep the low 24 bits, sign-extended.
            (((first + second) as i32) << 8) >> 8
        } else if first < 0 && second < 0 {
            -0x80_0000
        } else {
            first as i32
        }
    }

    /// The speakers' sample: each side takes its mixer or channel 1's
    /// and/or 3's own output (SOUNDCNT bits 8–11), then the master volume.
    fn output(&self, mix: &Mix) -> StereoSample {
        if self.control & 0x8000 == 0 {
            return [0, 0];
        }
        let master = i64::from(self.control & 0x7F);
        let side = |side: usize| {
            let level = match (self.control >> (8 + 2 * side)) & 3 {
                0 => mix.mixer[side],
                1 => mix.direct[0][side],
                2 => mix.direct[1][side],
                _ => mix.direct[0][side] + mix.direct[1][side],
            };
            // Undo the channel scaling (2^14), apply the master volume
            // (2^7), and keep a little headroom for 16 channels.
            let scaled = (level * master) >> (14 + 7 + 2);
            scaled.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16
        };
        [side(0), side(1)]
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

    /// One sample per output sample: period 512 ticks.
    const EVERY_SAMPLE: u16 = (0x1_0000 - 512) as u16;

    /// Flat memory from address 0.
    impl Memory for Vec<u8> {
        fn read_8(&mut self, address: u32) -> u8 {
            self.get(address as usize).copied().unwrap_or(0)
        }

        fn write_8(&mut self, address: u32, value: u8) {
            if let Some(byte) = self.get_mut(address as usize) {
                *byte = value;
            }
        }
    }

    fn collect(sound: &mut Sound, memory: &mut Vec<u8>, count: usize) -> Vec<StereoSample> {
        for _ in 0..count {
            sound.sample(memory);
        }
        let mut out = Vec::new();
        sound.drain_samples(&mut out);
        out
    }

    /// Starts channel `index` looping one word of 16-bit samples from
    /// `source`, one per output sample, at full volume and panning `pan`.
    fn play(sound: &mut Sound, index: u32, source: u32, pan: u32) {
        let base = index * 16;
        sound.write(base + 4, source, FULL);
        sound.write(base + 8, u32::from(EVERY_SAMPLE), FULL);
        sound.write(base + 0xC, 1, FULL);
        sound.write(base, 1 << 31 | 1 << 29 | 1 << 27 | pan << 16 | 0x7F, FULL);
    }

    /// Starts capture `unit` with SNDCAPxCNT bits `control`, recording
    /// `words` words at `destination`, with channel 1's or 3's timer at
    /// `timer`.
    fn capture(
        sound: &mut Sound,
        unit: u32,
        control: u32,
        destination: u32,
        words: u32,
        timer: u16,
    ) {
        sound.write(0x18 + unit * 0x20, u32::from(timer), FULL);
        sound.write(0x110 + unit * 8, destination, FULL);
        sound.write(0x114 + unit * 8, words, FULL);
        sound.write(0x108, (0x80 | control) << (unit * 8), 0xFF << (unit * 8));
    }

    /// Memory holding 16-bit `samples` from 0, then zeros up to 0x80.
    fn memory_with(samples: &[i16]) -> Vec<u8> {
        let mut memory: Vec<u8> = samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        memory.resize(0x80, 0);
        memory
    }

    #[test]
    fn pcm8_one_shot_plays_then_stops() {
        let mut sound = Sound::default();
        start(&mut sound, 0, 2, 2, EVERY_SAMPLE);
        let out = collect(&mut sound, &mut vec![0x40; 8], 12);
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
        start(&mut sound, 0, 1, 1, EVERY_SAMPLE);
        collect(&mut sound, &mut vec![0x10; 16], 100);
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
        start(&mut sound, 0, 1, 1, EVERY_SAMPLE);
        sound.write(0x100, 0, FULL);
        let out = collect(&mut sound, &mut vec![0x7F; 8], 4);
        assert!(out.iter().all(|sample| *sample == [0, 0]));
    }

    #[test]
    fn capture_0_loops_over_the_left_mixer() {
        let mut sound = Sound::default();
        sound.write(0x100, 0x807F, FULL);
        let mut memory = memory_with(&[0x4000, 0x4000]);
        play(&mut sound, 0, 0, 0);
        // Loop, 16-bit, two words: four samples.
        capture(&mut sound, 0, 0, 0x40, 2, EVERY_SAMPLE);
        collect(&mut sound, &mut memory, 6);
        // 4000h at volume 127/128, hard left.
        assert_eq!(memory[0x40..0x48], [0x80, 0x3F].repeat(4));
        assert_eq!(memory[0x48], 0, "stays within its two words");
        assert_eq!(sound.read(0x108), 0x80, "still running");
        assert_eq!(sound.read(0x110), 0x40);
    }

    #[test]
    fn one_shot_pcm8_capture_runs_on_channel_3s_timer() {
        let mut sound = Sound::default();
        sound.write(0x100, 0x807F, FULL);
        let mut memory = memory_with(&[0x4000, 0x4000]);
        play(&mut sound, 0, 0, 127);
        // One-shot, 8-bit, one word, one sample every two output samples.
        capture(&mut sound, 1, 0x0C, 0x40, 1, (0x1_0000 - 1024) as u16);
        collect(&mut sound, &mut memory, 7);
        assert_eq!(memory[0x40..0x44], [0x3F, 0x3F, 0x3F, 0]);
        assert_ne!(sound.read(0x108) & 0x8000, 0, "busy");
        collect(&mut sound, &mut memory, 4);
        assert_eq!(memory[0x40..0x45], [0x3F, 0x3F, 0x3F, 0x3F, 0]);
        assert_eq!(sound.read(0x108), 0x0C00, "done");
    }

    #[test]
    fn capturing_channel_0_has_the_hardware_bugs() {
        // Without add mode, two negative channels record -8000h.
        let mut sound = Sound::default();
        sound.write(0x100, 0x807F, FULL);
        let mut memory = memory_with(&[-0x100, -0x100]);
        play(&mut sound, 0, 0, 64);
        play(&mut sound, 1, 0, 64);
        capture(&mut sound, 0, 0x02, 0x40, 1, EVERY_SAMPLE);
        collect(&mut sound, &mut memory, 1);
        assert_eq!(memory[0x40..0x42], [0x00, 0x80]);

        // With it, their sum wraps instead of clipping.
        let mut sound = Sound::default();
        sound.write(0x100, 0x807F, FULL);
        let mut memory = memory_with(&[0x7FFF, 0x7FFF]);
        play(&mut sound, 0, 0, 64);
        play(&mut sound, 1, 0, 64);
        capture(&mut sound, 0, 0x03, 0x40, 1, EVERY_SAMPLE);
        collect(&mut sound, &mut memory, 1);
        assert_eq!(memory[0x40..0x42], [0xFE, 0xFD]);
    }

    #[test]
    fn add_mode_pans_channel_1_with_channel_0() {
        let left_and_right = |add: u32| {
            let mut sound = Sound::default();
            sound.write(0x100, 0x807F, FULL);
            play(&mut sound, 0, 0, 0);
            play(&mut sound, 1, 0, 127);
            capture(&mut sound, 0, add, 0x40, 1, EVERY_SAMPLE);
            collect(&mut sound, &mut memory_with(&[0x1000, 0x1000]), 1)[0]
        };
        let [left, right] = left_and_right(0);
        assert!(right > 0);
        let [added_left, added_right] = left_and_right(1);
        assert_eq!(added_right, 0, "channel 1 follows channel 0 hard left");
        assert!(added_left > left);
    }

    #[test]
    fn soundcnt_can_send_channel_1_straight_to_a_speaker() {
        let mut sound = Sound::default();
        // Left from channel 1, which stays out of the mixers.
        sound.write(0x100, 0x907F | 1 << 8, FULL);
        let mut memory = memory_with(&[0x4000, 0x4000, 0x1000, 0x1000]);
        play(&mut sound, 0, 0, 0);
        play(&mut sound, 1, 4, 0);
        let out = collect(&mut sound, &mut memory, 1);
        // Channel 1 alone: 1000h at volume and master volume 127/128,
        // quartered for headroom.
        assert_eq!(out[0], [1008, 0]);
    }

    #[test]
    fn capture_drops_fractions_as_gbatek_describes() {
        assert_eq!(strip_fraction(0x1C0, 8), 1, "1.75");
        assert_eq!(strip_fraction(-0x140, 8), -1, "-1.25");
        assert_eq!(strip_fraction(-0x1C0, 8), -2, "-1.75");
        assert_eq!(strip_fraction(-0x80_0000, 16), -0x80);
    }
}
