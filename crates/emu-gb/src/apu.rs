//! The audio processing unit (Pan Docs, "Audio", "Audio Registers",
//! "Audio Details"; gbdev wiki, "Game Boy Sound Hardware").
//!
//! Four channels feed a mixer: two pulse channels (the first with a
//! frequency sweep), a wave channel playing 32 four-bit samples from wave
//! RAM, and a noise channel driven by a linear-feedback shift register. A
//! frame sequencer clocked at 512 Hz by DIV drives length counters, the
//! sweep and volume envelopes. Output is averaged over each sample period
//! and high-pass filtered like the hardware's output capacitor.

use emu_core::StereoSample;
use serde::{Deserialize, Serialize};

const CLOCK_HZ: u32 = 4_194_304;
/// Output sample rate in Hz.
pub(crate) const SAMPLE_RATE: u32 = 48_000;

const DUTY_PATTERNS: [[u8; 8]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 1],
    [1, 0, 0, 0, 0, 0, 0, 1],
    [1, 0, 0, 0, 0, 1, 1, 1],
    [0, 1, 1, 1, 1, 1, 1, 0],
];
const NOISE_DIVISORS: [u32; 8] = [8, 16, 32, 48, 64, 80, 96, 112];

/// Bits that always read as 1, for 0xFF10–0xFF2F.
const READ_MASKS: [u8; 0x20] = [
    0x80, 0x3F, 0x00, 0xFF, 0xBF, // NR10–NR14
    0xFF, 0x3F, 0x00, 0xFF, 0xBF, // unused, NR21–NR24
    0x7F, 0xFF, 0x9F, 0xFF, 0xBF, // NR30–NR34
    0xFF, 0xFF, 0x00, 0x00, 0xBF, // unused, NR41–NR44
    0x00, 0x00, 0x70, // NR50–NR52
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, // unused
];

/// Runs a countdown `timer` for `cycles` clock cycles, reloading it with
/// `period` each time it expires. Returns the number of expirations.
fn run_timer(timer: &mut u32, period: u32, cycles: u32) -> u32 {
    let mut remaining = cycles;
    let mut expirations = 0;
    while remaining >= *timer {
        remaining -= *timer;
        *timer = period.max(1);
        expirations += 1;
    }
    *timer -= remaining;
    expirations
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Length {
    counter: u16,
    enabled: bool,
}

impl Length {
    /// A frame-sequencer length clock. Returns true if the counter just
    /// reached zero, which silences the channel.
    fn clock(&mut self) -> bool {
        if self.enabled && self.counter > 0 {
            self.counter -= 1;
            return self.counter == 0;
        }
        false
    }

    /// Applies the length-enable and trigger bits of an NRx4 write, with
    /// the DMG's extra length clock when length is enabled during a
    /// frame-sequencer step that will not clock it. Returns whether the
    /// write triggered the channel; clears `enabled` if length ran out.
    fn write_control(
        &mut self,
        value: u8,
        max: u16,
        next_step_clocks: bool,
        enabled: &mut bool,
    ) -> bool {
        let was_enabled = self.enabled;
        self.enabled = value & 0x40 != 0;
        let trigger = value & 0x80 != 0;
        if !next_step_clocks && !was_enabled && self.enabled && self.counter > 0 {
            self.counter -= 1;
            if self.counter == 0 && !trigger {
                *enabled = false;
            }
        }
        if trigger && self.counter == 0 {
            self.counter = max;
            if self.enabled && !next_step_clocks {
                self.counter -= 1;
            }
        }
        trigger
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Envelope {
    initial: u8,
    increase: bool,
    period: u8,
    volume: u8,
    timer: u8,
}

impl Envelope {
    fn write(&mut self, value: u8) {
        self.initial = value >> 4;
        self.increase = value & 0x08 != 0;
        self.period = value & 0x07;
    }

    /// The DAC is on whenever NRx2's top five bits are not all zero.
    fn dac_on(&self) -> bool {
        self.initial != 0 || self.increase
    }

    fn trigger(&mut self) {
        self.volume = self.initial;
        self.timer = if self.period == 0 { 8 } else { self.period };
    }

    fn clock(&mut self) {
        if self.period == 0 {
            return;
        }
        self.timer = self.timer.saturating_sub(1);
        if self.timer > 0 {
            return;
        }
        self.timer = self.period;
        if self.increase && self.volume < 15 {
            self.volume += 1;
        } else if !self.increase && self.volume > 0 {
            self.volume -= 1;
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Sweep {
    period: u8,
    negate: bool,
    shift: u8,
    timer: u8,
    enabled: bool,
    shadow: u16,
    /// A subtraction was calculated since the last trigger; clearing
    /// negate afterwards silences the channel.
    negate_used: bool,
}

impl Sweep {
    fn calculate(&mut self) -> u16 {
        let delta = self.shadow >> self.shift;
        if self.negate {
            self.negate_used = true;
            self.shadow.wrapping_sub(delta)
        } else {
            self.shadow + delta
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Pulse {
    enabled: bool,
    length: Length,
    envelope: Envelope,
    duty: u8,
    duty_step: u8,
    frequency: u16,
    timer: u32,
    sweep: Option<Sweep>,
}

impl Pulse {
    fn period(&self) -> u32 {
        (2048 - u32::from(self.frequency)) * 4
    }

    fn tick(&mut self, cycles: u32) {
        let period = self.period();
        let steps = run_timer(&mut self.timer, period, cycles);
        self.duty_step = ((u32::from(self.duty_step) + steps) % 8) as u8;
    }

    fn output(&self) -> u8 {
        if self.enabled {
            DUTY_PATTERNS[usize::from(self.duty)][usize::from(self.duty_step)]
                * self.envelope.volume
        } else {
            0
        }
    }

    fn trigger(&mut self) {
        self.timer = self.period();
        self.envelope.trigger();
        let frequency = self.frequency;
        let mut overflow = false;
        if let Some(sweep) = &mut self.sweep {
            sweep.shadow = frequency;
            sweep.timer = if sweep.period == 0 { 8 } else { sweep.period };
            sweep.enabled = sweep.period != 0 || sweep.shift != 0;
            sweep.negate_used = false;
            if sweep.shift != 0 {
                overflow = sweep.calculate() > 2047;
            }
        }
        if overflow {
            self.enabled = false;
        }
    }

    fn clock_sweep(&mut self) {
        let Some(sweep) = &mut self.sweep else { return };
        sweep.timer = sweep.timer.saturating_sub(1);
        if sweep.timer > 0 {
            return;
        }
        sweep.timer = if sweep.period == 0 { 8 } else { sweep.period };
        if !sweep.enabled || sweep.period == 0 {
            return;
        }
        let frequency = sweep.calculate();
        if frequency > 2047 {
            self.enabled = false;
            return;
        }
        if sweep.shift != 0 {
            sweep.shadow = frequency;
            self.frequency = frequency;
            if sweep.calculate() > 2047 {
                self.enabled = false;
            }
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Wave {
    enabled: bool,
    dac_on: bool,
    length: Length,
    volume_code: u8,
    frequency: u16,
    timer: u32,
    position: u8,
    sample: u8,
    /// The channel fetched from wave RAM during the current M-cycle; only
    /// then can the CPU reach wave RAM while the channel plays.
    just_read: bool,
}

impl Wave {
    fn period(&self) -> u32 {
        (2048 - u32::from(self.frequency)) * 2
    }

    fn tick(&mut self, cycles: u32, wave_ram: &[u8]) {
        self.just_read = false;
        if !self.enabled {
            return;
        }
        let period = self.period();
        for _ in 0..run_timer(&mut self.timer, period, cycles) {
            self.position = (self.position + 1) % 32;
            let byte = wave_ram[usize::from(self.position / 2)];
            self.sample = if self.position.is_multiple_of(2) {
                byte >> 4
            } else {
                byte & 0x0F
            };
            self.just_read = true;
        }
    }

    fn output(&self) -> u8 {
        if !self.enabled {
            return 0;
        }
        match self.volume_code {
            0 => 0,
            1 => self.sample,
            2 => self.sample >> 1,
            _ => self.sample >> 2,
        }
    }

    fn trigger(&mut self) {
        // The first sample is fetched one period plus a few cycles later.
        self.timer = self.period() + 6;
        self.position = 0;
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Noise {
    enabled: bool,
    length: Length,
    envelope: Envelope,
    shift: u8,
    narrow: bool,
    divisor_code: u8,
    lfsr: u16,
    timer: u32,
}

impl Noise {
    fn period(&self) -> u32 {
        NOISE_DIVISORS[usize::from(self.divisor_code)] << self.shift
    }

    fn tick(&mut self, cycles: u32) {
        // Shifts of 14 and 15 stop the clock entirely.
        if self.shift >= 14 {
            return;
        }
        let period = self.period();
        for _ in 0..run_timer(&mut self.timer, period, cycles) {
            let feedback = (self.lfsr ^ (self.lfsr >> 1)) & 1;
            self.lfsr = (self.lfsr >> 1) | feedback << 14;
            if self.narrow {
                self.lfsr = (self.lfsr & !0x40) | feedback << 6;
            }
        }
    }

    fn output(&self) -> u8 {
        if self.enabled && self.lfsr & 1 == 0 {
            self.envelope.volume
        } else {
            0
        }
    }

    fn trigger(&mut self) {
        self.timer = self.period();
        self.envelope.trigger();
        self.lfsr = 0x7FFF;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Apu {
    powered: bool,
    /// Raw register values as written, for reading back.
    registers: Box<[u8]>,
    wave_ram: Box<[u8]>,
    pulse1: Pulse,
    pulse2: Pulse,
    wave: Wave,
    noise: Noise,
    /// Index of the next frame-sequencer step, 0 to 7.
    frame_step: u8,
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
    /// The state the DMG boot ROM leaves: sound on, channel 1 having just
    /// played the boot chime.
    pub(crate) fn after_boot() -> Self {
        let mut apu = Self {
            powered: true,
            registers: vec![0; 0x20].into_boxed_slice(),
            wave_ram: vec![0; 0x10].into_boxed_slice(),
            pulse1: Pulse {
                sweep: Some(Sweep::default()),
                ..Pulse::default()
            },
            pulse2: Pulse::default(),
            wave: Wave::default(),
            noise: Noise::default(),
            frame_step: 0,
            accumulated: (0, 0),
            accumulated_cycles: 0,
            sample_clock: 0,
            capacitor: (0.0, 0.0),
            samples: Vec::new(),
        };
        for (address, value) in [
            (0xFF10, 0x80),
            (0xFF11, 0xBF),
            (0xFF12, 0xF3),
            (0xFF13, 0xFF),
            (0xFF14, 0x3F),
            (0xFF16, 0x3F),
            (0xFF18, 0xFF),
            (0xFF19, 0x3F),
            (0xFF1A, 0x7F),
            (0xFF1B, 0xFF),
            (0xFF1C, 0x9F),
            (0xFF1D, 0xFF),
            (0xFF1E, 0x3F),
            (0xFF20, 0xFF),
            (0xFF23, 0x3F),
            (0xFF24, 0x77),
            (0xFF25, 0xF3),
        ] {
            apu.write(address, value);
        }
        apu.pulse1.enabled = true;
        apu
    }

    /// Moves the samples produced so far into `out`.
    pub(crate) fn drain_samples(&mut self, out: &mut Vec<StereoSample>) {
        out.append(&mut self.samples);
    }

    /// Advances one M-cycle. `frame_sequencer` is true when DIV's bit 4
    /// fell during it (512 Hz).
    pub(crate) fn tick(&mut self, frame_sequencer: bool) {
        if frame_sequencer {
            self.clock_frame_sequencer();
        }
        if self.powered {
            self.pulse1.tick(4);
            self.pulse2.tick(4);
            self.wave.tick(4, &self.wave_ram);
            self.noise.tick(4);
        }
        self.mix();
    }

    /// One 512 Hz frame-sequencer step. Also called when a DIV write makes
    /// its bit 4 fall.
    pub(crate) fn clock_frame_sequencer(&mut self) {
        if !self.powered {
            return;
        }
        let step = self.frame_step;
        if step.is_multiple_of(2) {
            if self.pulse1.length.clock() {
                self.pulse1.enabled = false;
            }
            if self.pulse2.length.clock() {
                self.pulse2.enabled = false;
            }
            if self.wave.length.clock() {
                self.wave.enabled = false;
            }
            if self.noise.length.clock() {
                self.noise.enabled = false;
            }
        }
        if step == 2 || step == 6 {
            self.pulse1.clock_sweep();
        }
        if step == 7 {
            self.pulse1.envelope.clock();
            self.pulse2.envelope.clock();
            self.noise.envelope.clock();
        }
        self.frame_step = (step + 1) % 8;
    }

    /// Whether the next frame-sequencer step clocks length counters.
    fn next_step_clocks_length(&self) -> bool {
        self.frame_step.is_multiple_of(2)
    }

    fn mix(&mut self) {
        // Each DAC maps a digital 0–15 to -15..=15; a DAC that is off
        // contributes nothing.
        let dac = |digital: u8, on: bool| if on { i32::from(digital) * 2 - 15 } else { 0 };
        let channels = [
            dac(self.pulse1.output(), self.pulse1.envelope.dac_on()),
            dac(self.pulse2.output(), self.pulse2.envelope.dac_on()),
            dac(self.wave.output(), self.wave.dac_on),
            dac(self.noise.output(), self.noise.envelope.dac_on()),
        ];
        let panning = self.registers[0x15];
        let volume = self.registers[0x14];
        let side = |shift: u8, master: u8| -> i32 {
            let sum: i32 = channels
                .iter()
                .enumerate()
                .filter(|(index, _)| panning & (1 << (index + usize::from(shift))) != 0)
                .map(|(_, value)| *value)
                .sum();
            sum * (i32::from(master & 7) + 1)
        };
        if self.powered {
            self.accumulated.0 += side(4, volume >> 4);
            self.accumulated.1 += side(0, volume);
        }
        self.accumulated_cycles += 1;

        self.sample_clock += 4 * SAMPLE_RATE;
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
        match address {
            0xFF26 => {
                let status = u8::from(self.pulse1.enabled)
                    | u8::from(self.pulse2.enabled) << 1
                    | u8::from(self.wave.enabled) << 2
                    | u8::from(self.noise.enabled) << 3;
                u8::from(self.powered) << 7 | 0x70 | status
            }
            0xFF10..=0xFF2F => {
                let index = usize::from(address - 0xFF10);
                self.registers[index] | READ_MASKS[index]
            }
            _ => {
                // While the wave channel plays, the CPU only reaches the
                // byte the channel is reading, and only in that cycle.
                if self.wave.enabled {
                    if self.wave.just_read {
                        self.wave_ram[usize::from(self.wave.position / 2)]
                    } else {
                        0xFF
                    }
                } else {
                    self.wave_ram[usize::from(address - 0xFF30)]
                }
            }
        }
    }

    pub(crate) fn write(&mut self, address: u16, value: u8) {
        if (0xFF30..=0xFF3F).contains(&address) {
            if self.wave.enabled {
                if self.wave.just_read {
                    self.wave_ram[usize::from(self.wave.position / 2)] = value;
                }
            } else {
                self.wave_ram[usize::from(address - 0xFF30)] = value;
            }
            return;
        }
        if address == 0xFF26 {
            self.set_power(value & 0x80 != 0);
            return;
        }
        if !self.powered {
            // On DMG the length counters stay writable while powered off.
            match address {
                0xFF11 => self.pulse1.length.counter = 64 - u16::from(value & 0x3F),
                0xFF16 => self.pulse2.length.counter = 64 - u16::from(value & 0x3F),
                0xFF1B => self.wave.length.counter = 256 - u16::from(value),
                0xFF20 => self.noise.length.counter = 64 - u16::from(value & 0x3F),
                _ => {}
            }
            return;
        }
        if let Some(register) = self
            .registers
            .get_mut(usize::from(address.wrapping_sub(0xFF10)))
        {
            *register = value;
        }
        let next_step_clocks = self.next_step_clocks_length();
        match address {
            0xFF10 => {
                let pulse = &mut self.pulse1;
                if let Some(sweep) = &mut pulse.sweep {
                    sweep.period = (value >> 4) & 7;
                    let was_negate = sweep.negate;
                    sweep.negate = value & 0x08 != 0;
                    sweep.shift = value & 7;
                    if was_negate && !sweep.negate && sweep.negate_used {
                        pulse.enabled = false;
                    }
                }
            }
            0xFF11 | 0xFF16 => {
                let pulse = if address == 0xFF11 {
                    &mut self.pulse1
                } else {
                    &mut self.pulse2
                };
                pulse.duty = value >> 6;
                pulse.length.counter = 64 - u16::from(value & 0x3F);
            }
            0xFF12 | 0xFF17 => {
                let pulse = if address == 0xFF12 {
                    &mut self.pulse1
                } else {
                    &mut self.pulse2
                };
                pulse.envelope.write(value);
                if !pulse.envelope.dac_on() {
                    pulse.enabled = false;
                }
            }
            0xFF13 | 0xFF18 => {
                let pulse = if address == 0xFF13 {
                    &mut self.pulse1
                } else {
                    &mut self.pulse2
                };
                pulse.frequency = (pulse.frequency & 0x700) | u16::from(value);
            }
            0xFF14 | 0xFF19 => {
                let pulse = if address == 0xFF14 {
                    &mut self.pulse1
                } else {
                    &mut self.pulse2
                };
                pulse.frequency = (pulse.frequency & 0xFF) | u16::from(value & 7) << 8;
                let mut enabled = pulse.enabled;
                if pulse
                    .length
                    .write_control(value, 64, next_step_clocks, &mut enabled)
                {
                    enabled = pulse.envelope.dac_on();
                    pulse.enabled = enabled;
                    pulse.trigger();
                } else {
                    pulse.enabled = enabled;
                }
            }
            0xFF1A => {
                self.wave.dac_on = value & 0x80 != 0;
                if !self.wave.dac_on {
                    self.wave.enabled = false;
                }
            }
            0xFF1B => self.wave.length.counter = 256 - u16::from(value),
            0xFF1C => self.wave.volume_code = (value >> 5) & 3,
            0xFF1D => self.wave.frequency = (self.wave.frequency & 0x700) | u16::from(value),
            0xFF1E => {
                let wave = &mut self.wave;
                wave.frequency = (wave.frequency & 0xFF) | u16::from(value & 7) << 8;
                let mut enabled = wave.enabled;
                if wave
                    .length
                    .write_control(value, 256, next_step_clocks, &mut enabled)
                {
                    wave.enabled = wave.dac_on;
                    wave.trigger();
                } else {
                    wave.enabled = enabled;
                }
            }
            0xFF20 => self.noise.length.counter = 64 - u16::from(value & 0x3F),
            0xFF21 => {
                self.noise.envelope.write(value);
                if !self.noise.envelope.dac_on() {
                    self.noise.enabled = false;
                }
            }
            0xFF22 => {
                self.noise.shift = value >> 4;
                self.noise.narrow = value & 0x08 != 0;
                self.noise.divisor_code = value & 7;
            }
            0xFF23 => {
                let noise = &mut self.noise;
                let mut enabled = noise.enabled;
                if noise
                    .length
                    .write_control(value, 64, next_step_clocks, &mut enabled)
                {
                    noise.enabled = noise.envelope.dac_on();
                    noise.trigger();
                } else {
                    noise.enabled = enabled;
                }
            }
            _ => {}
        }
    }

    fn set_power(&mut self, on: bool) {
        if on == self.powered {
            return;
        }
        if on {
            self.powered = true;
            self.frame_step = 0;
            self.pulse1.duty_step = 0;
            self.pulse2.duty_step = 0;
            self.wave.sample = 0;
            return;
        }
        // Powering off clears every register, but on DMG the length
        // counters survive.
        let lengths = [
            self.pulse1.length.counter,
            self.pulse2.length.counter,
            self.wave.length.counter,
            self.noise.length.counter,
        ];
        for address in 0xFF10..=0xFF25 {
            self.write(address, 0);
        }
        self.pulse1 = Pulse {
            sweep: Some(Sweep::default()),
            ..Pulse::default()
        };
        self.pulse2 = Pulse::default();
        self.wave = Wave::default();
        self.noise = Noise::default();
        self.pulse1.length.counter = lengths[0];
        self.pulse2.length.counter = lengths[1];
        self.wave.length.counter = lengths[2];
        self.noise.length.counter = lengths[3];
        self.powered = false;
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
            apu.tick(false);
        }
    }

    #[test]
    fn produces_48_khz_output() {
        let mut apu = Apu::after_boot();
        run_cycles(&mut apu, CLOCK_HZ / 4);
        let mut samples = Vec::new();
        apu.drain_samples(&mut samples);
        assert_eq!(samples.len(), SAMPLE_RATE as usize);
    }

    #[test]
    fn a_triggered_square_wave_is_audible() {
        let mut apu = Apu::after_boot();
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
        let mut apu = Apu::after_boot();
        apu.write(0xFF12, 0xF0);
        apu.write(0xFF11, 0x3F); // length 1
        apu.write(0xFF14, 0xC0); // trigger with length enabled
        assert_eq!(apu.read(0xFF26) & 0x01, 0x01);
        apu.tick(true);
        apu.tick(true);
        assert_eq!(apu.read(0xFF26) & 0x01, 0x00);
    }
}
