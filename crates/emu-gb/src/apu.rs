//! Game Boy sound: the four-channel generator shared with the GBA
//! (`emu-psg`), plus the Game Boy's mixer and output filter.
//!
//! The mixer applies NR51 panning and NR50 volume, averages the result over
//! each output sample period, and high-pass filters it like the hardware's
//! output capacitor.

use emu_core::StereoSample;
use emu_psg::{Psg, Variant};
use serde::{Deserialize, Serialize};

use crate::model::Model;

const CLOCK_HZ: u32 = 4_194_304;
/// Output sample rate in Hz.
pub(crate) const SAMPLE_RATE: u32 = 48_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Apu {
    psg: Psg,
    /// Sums of the mixed output since the last sample was emitted.
    accumulated: (i32, i32),
    accumulated_cycles: u32,
    /// Fractional progress towards the next output sample, in units of
    /// 1 / `CLOCK_HZ` samples.
    sample_clock: u32,
    /// High-pass filter capacitor charge, per side.
    capacitor: (f32, f32),
    /// Samples produced but not yet collected. Not machine state.
    #[serde(skip)]
    samples: Vec<StereoSample>,
}

impl Apu {
    /// The state the boot ROM leaves: sound on, channel 1 having just
    /// played the boot chime.
    pub(crate) fn after_boot(model: Model) -> Self {
        let variant = match model {
            Model::Dmg => Variant::Dmg,
            Model::Cgb => Variant::Cgb,
        };
        let mut psg = Psg::new(variant);
        for (register, value) in [
            (0x10, 0x80),
            (0x11, 0xBF),
            (0x12, 0xF3),
            (0x13, 0xFF),
            (0x14, 0x3F),
            (0x16, 0x3F),
            (0x18, 0xFF),
            (0x19, 0x3F),
            (0x1A, 0x7F),
            (0x1B, 0xFF),
            (0x1C, 0x9F),
            (0x1D, 0xFF),
            (0x1E, 0x3F),
            (0x20, 0xFF),
            (0x23, 0x3F),
            (0x24, 0x77),
            (0x25, 0xF3),
        ] {
            psg.write(register, value);
        }
        psg.set_channel1_playing();
        Self {
            psg,
            accumulated: (0, 0),
            accumulated_cycles: 0,
            sample_clock: 0,
            capacitor: (0.0, 0.0),
            samples: Vec::new(),
        }
    }

    /// Moves the samples produced so far into `out`.
    pub(crate) fn drain_samples(&mut self, out: &mut Vec<StereoSample>) {
        out.append(&mut self.samples);
    }

    /// Advances `cycles` real-time clock cycles (one CPU M-cycle: 4, or 2
    /// in CGB double speed). `frame_sequencer` is true when the DIV bit that
    /// clocks it at 512 Hz fell.
    pub(crate) fn tick(&mut self, cycles: u8, frame_sequencer: bool) {
        if frame_sequencer {
            self.psg.clock_frame_sequencer();
        }
        let cycles = u32::from(cycles);
        self.psg.tick(cycles);
        self.mix(cycles);
    }

    /// One frame-sequencer step, for a DIV write that makes its bit fall.
    pub(crate) fn clock_frame_sequencer(&mut self) {
        self.psg.clock_frame_sequencer();
    }

    fn mix(&mut self, cycles: u32) {
        let channels = self.psg.outputs();
        let panning = self.psg.panning();
        let volume = self.psg.master_volume();
        let side = |shift: u8, master: u8| -> i32 {
            let sum: i32 = channels
                .iter()
                .enumerate()
                .filter(|(index, _)| panning & (1 << (index + usize::from(shift))) != 0)
                .map(|(_, value)| *value)
                .sum();
            sum * (i32::from(master & 7) + 1)
        };
        if self.psg.powered() {
            self.accumulated.0 += side(4, volume >> 4);
            self.accumulated.1 += side(0, volume);
        }
        self.accumulated_cycles += 1;

        self.sample_clock += cycles * SAMPLE_RATE;
        if self.sample_clock < CLOCK_HZ {
            return;
        }
        self.sample_clock -= CLOCK_HZ;
        // Full scale is 4 channels x 15 x volume 8.
        let scale = 1.0 / (480.0 * self.accumulated_cycles as f32);
        let left = self.accumulated.0 as f32 * scale;
        let right = self.accumulated.1 as f32 * scale;
        self.accumulated = (0, 0);
        self.accumulated_cycles = 0;
        let left = high_pass(&mut self.capacitor.0, left);
        let right = high_pass(&mut self.capacitor.1, right);
        let to_pcm = |value: f32| (value * 0.7 * f32::from(i16::MAX)) as i16;
        // A caller that never drains (a test stepping instructions) keeps at
        // most one second of audio.
        if self.samples.len() >= SAMPLE_RATE as usize {
            self.samples.clear();
        }
        self.samples.push([to_pcm(left), to_pcm(right)]);
    }

    pub(crate) fn read(&self, address: u16) -> u8 {
        if (0xFF30..=0xFF3F).contains(&address) {
            self.psg.read_wave((address - 0xFF30) as u8)
        } else {
            self.psg.read((address & 0xFF) as u8)
        }
    }

    pub(crate) fn write(&mut self, address: u16, value: u8) {
        if (0xFF30..=0xFF3F).contains(&address) {
            self.psg.write_wave((address - 0xFF30) as u8, value);
        } else {
            self.psg.write((address & 0xFF) as u8, value);
        }
    }
}

/// One step of the output high-pass filter, which removes the DC offset of
/// the DACs as the hardware's coupling capacitor does.
fn high_pass(capacitor: &mut f32, input: f32) -> f32 {
    // 0.999958 per clock cycle, raised to the clocks per sample.
    const CHARGE: f32 = 0.996_337;
    let output = input - *capacitor;
    *capacitor = input - output * CHARGE;
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_cycles(apu: &mut Apu, m_cycles: u32) {
        for _ in 0..m_cycles {
            apu.tick(4, false);
        }
    }

    #[test]
    fn produces_48_khz_output() {
        let mut apu = Apu::after_boot(Model::Dmg);
        run_cycles(&mut apu, CLOCK_HZ / 4);
        let mut samples = Vec::new();
        apu.drain_samples(&mut samples);
        assert_eq!(samples.len(), SAMPLE_RATE as usize);
    }

    #[test]
    fn a_triggered_square_wave_is_audible() {
        let mut apu = Apu::after_boot(Model::Dmg);
        apu.write(0xFF25, 0x11); // channel 1 to both sides
        apu.write(0xFF12, 0xF0); // full volume, DAC on
        apu.write(0xFF13, 0x00);
        apu.write(0xFF14, 0x87); // trigger, frequency 0x700 (about 1 kHz)
        run_cycles(&mut apu, CLOCK_HZ / 40);
        let mut samples = Vec::new();
        apu.drain_samples(&mut samples);
        let peak = samples
            .iter()
            .map(|[left, _]| left.unsigned_abs())
            .max()
            .unwrap_or(0);
        assert!(peak > 5_000, "peak {peak}");
        assert_eq!(apu.read(0xFF26) & 0x01, 0x01);
    }

    #[test]
    fn length_counter_silences_the_channel() {
        let mut apu = Apu::after_boot(Model::Dmg);
        apu.write(0xFF12, 0xF0);
        apu.write(0xFF11, 0x3F); // length 1
        apu.write(0xFF14, 0xC0); // trigger with length enabled
        assert_eq!(apu.read(0xFF26) & 0x01, 0x01);
        apu.tick(4, true);
        apu.tick(4, true);
        assert_eq!(apu.read(0xFF26) & 0x01, 0x00);
    }
}
