//! The four-channel programmable sound generator of the Game Boy, Game Boy
//! Color and Game Boy Advance (Pan Docs, "Audio", "Audio Registers",
//! "Audio Details"; gbdev wiki, "Game Boy Sound Hardware"; GBATEK, "GBA
//! Sound Channel 1–4").
//!
//! Two pulse channels (the first with a frequency sweep), a wave channel
//! playing 4-bit samples from wave RAM, and a noise channel driven by a
//! linear-feedback shift register. A frame sequencer clocked at 512 Hz
//! drives length counters, the sweep and volume envelopes.
//!
//! Registers are addressed by their Game Boy number: 0x10 (NR10) to 0x26
//! (NR52), and wave RAM by its index. The GBA maps its own register layout
//! onto these. Mixing is left to each system, which reads the four DAC
//! outputs with [`Psg::outputs`].

use serde::{Deserialize, Serialize};

/// Which console's sound hardware, for the behaviours that differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Variant {
    /// Game Boy: length counters survive power-off; wave RAM is reachable
    /// while channel 3 plays only in the cycle it is read.
    Dmg,
    /// Game Boy Color.
    Cgb,
    /// Game Boy Advance: two banks of wave RAM and a 75% wave volume.
    Agb,
}

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

    /// Plays from `bank` (0 or 1), through both banks in turn if
    /// `two_banks` (GBA only).
    fn tick(&mut self, cycles: u32, wave_ram: &[u8], bank: usize, two_banks: bool) {
        self.just_read = false;
        if !self.enabled {
            return;
        }
        let period = self.period();
        let samples = if two_banks { 64 } else { 32 };
        for _ in 0..run_timer(&mut self.timer, period, cycles) {
            self.position = (self.position + 1) % samples;
            let byte = wave_ram[(bank * 16 + usize::from(self.position / 2)) % 32];
            self.sample = if self.position.is_multiple_of(2) {
                byte >> 4
            } else {
                byte & 0x0F
            };
            self.just_read = true;
        }
    }

    /// The current sample after volume. On the GBA, NR32 bit 7 forces 75%.
    fn output(&self, variant: Variant, nr32: u8) -> u8 {
        if !self.enabled {
            return 0;
        }
        if variant == Variant::Agb && nr32 & 0x80 != 0 {
            return self.sample * 3 / 4;
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

/// The sound generator's state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Psg {
    variant: Variant,
    powered: bool,
    /// Raw register values as written, for reading back (NR10 at 0).
    registers: Vec<u8>,
    /// Two 16-byte banks; the Game Boy uses only the first.
    wave_ram: Vec<u8>,
    pulse1: Pulse,
    pulse2: Pulse,
    wave: Wave,
    noise: Noise,
    /// Index of the next frame-sequencer step, 0 to 7.
    frame_step: u8,
}

impl Psg {
    /// Powered-on sound hardware with every register cleared.
    #[must_use]
    pub fn new(variant: Variant) -> Self {
        Self {
            variant,
            powered: true,
            registers: vec![0; 0x20],
            wave_ram: vec![0; 0x20],
            pulse1: Pulse {
                sweep: Some(Sweep::default()),
                ..Pulse::default()
            },
            pulse2: Pulse::default(),
            wave: Wave::default(),
            noise: Noise::default(),
            frame_step: 0,
        }
    }

    /// Whether the master power (NR52 bit 7) is on.
    #[must_use]
    pub fn powered(&self) -> bool {
        self.powered
    }

    /// NR50: master volume per side.
    #[must_use]
    pub fn master_volume(&self) -> u8 {
        self.registers[0x14]
    }

    /// NR51: which channels go to which side.
    #[must_use]
    pub fn panning(&self) -> u8 {
        self.registers[0x15]
    }

    /// Marks channel 1 as playing, as the Game Boy boot ROM's chime leaves
    /// it.
    pub fn set_channel1_playing(&mut self) {
        self.pulse1.enabled = true;
    }

    /// Each channel's DAC output, -15 to 15, or 0 with its DAC off.
    #[must_use]
    pub fn outputs(&self) -> [i32; 4] {
        let dac = |digital: u8, on: bool| if on { i32::from(digital) * 2 - 15 } else { 0 };
        [
            dac(self.pulse1.output(), self.pulse1.envelope.dac_on()),
            dac(self.pulse2.output(), self.pulse2.envelope.dac_on()),
            dac(
                self.wave.output(self.variant, self.registers[0x0C]),
                self.wave.dac_on,
            ),
            dac(self.noise.output(), self.noise.envelope.dac_on()),
        ]
    }

    /// Advances `cycles` cycles of the 4.19 MHz sound clock.
    pub fn tick(&mut self, cycles: u32) {
        if self.powered {
            self.pulse1.tick(cycles);
            self.pulse2.tick(cycles);
            let bank = self.wave_bank_played();
            let two_banks = self.variant == Variant::Agb && self.registers[0x0A] & 0x20 != 0;
            self.wave.tick(cycles, &self.wave_ram, bank, two_banks);
            self.noise.tick(cycles);
        }
    }

    /// The wave RAM bank channel 3 plays (always 0 on the Game Boy).
    fn wave_bank_played(&self) -> usize {
        if self.variant == Variant::Agb {
            usize::from((self.registers[0x0A] >> 6) & 1)
        } else {
            0
        }
    }

    /// The wave RAM byte the CPU reaches at `index` (0–15): on the GBA,
    /// the bank channel 3 is not playing.
    fn wave_index(&self, index: u8) -> usize {
        let bank = if self.variant == Variant::Agb {
            1 - self.wave_bank_played()
        } else {
            0
        };
        bank * 16 + usize::from(index & 0x0F)
    }

    /// Reads wave RAM byte `index` (0–15).
    #[must_use]
    pub fn read_wave(&self, index: u8) -> u8 {
        // While the wave channel plays, the Game Boy CPU only reaches the
        // byte the channel is reading; on DMG only in the very cycle it is
        // read. The GBA reaches the other bank freely.
        if self.wave.enabled && self.variant != Variant::Agb {
            if self.wave.just_read || self.variant == Variant::Cgb {
                self.wave_ram[usize::from(self.wave.position / 2)]
            } else {
                0xFF
            }
        } else {
            self.wave_ram[self.wave_index(index)]
        }
    }

    /// Writes wave RAM byte `index` (0–15).
    pub fn write_wave(&mut self, index: u8, value: u8) {
        if self.wave.enabled && self.variant != Variant::Agb {
            if self.wave.just_read || self.variant == Variant::Cgb {
                self.wave_ram[usize::from(self.wave.position / 2)] = value;
            }
        } else {
            let index = self.wave_index(index);
            self.wave_ram[index] = value;
        }
    }

    /// One 512 Hz frame-sequencer step.
    pub fn clock_frame_sequencer(&mut self) {
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

    /// Reads register `register` (0x10 for NR10 to 0x2F), with unused bits
    /// set.
    #[must_use]
    pub fn read(&self, register: u8) -> u8 {
        if register == 0x26 {
            let status = u8::from(self.pulse1.enabled)
                | u8::from(self.pulse2.enabled) << 1
                | u8::from(self.wave.enabled) << 2
                | u8::from(self.noise.enabled) << 3;
            return u8::from(self.powered) << 7 | 0x70 | status;
        }
        let index = usize::from(register.wrapping_sub(0x10)) & 0x1F;
        self.registers[index] | READ_MASKS[index]
    }

    /// Writes register `register` (0x10 for NR10 to 0x26 for NR52).
    pub fn write(&mut self, register: u8, value: u8) {
        if register == 0x26 {
            self.set_power(value & 0x80 != 0);
            return;
        }
        if !self.powered {
            // On DMG the length counters stay writable while powered off.
            if self.variant != Variant::Dmg {
                return;
            }
            match register {
                0x11 => self.pulse1.length.counter = 64 - u16::from(value & 0x3F),
                0x16 => self.pulse2.length.counter = 64 - u16::from(value & 0x3F),
                0x1B => self.wave.length.counter = 256 - u16::from(value),
                0x20 => self.noise.length.counter = 64 - u16::from(value & 0x3F),
                _ => {}
            }
            return;
        }
        if let Some(register) = self
            .registers
            .get_mut(usize::from(register.wrapping_sub(0x10)))
        {
            *register = value;
        }
        let next_step_clocks = self.next_step_clocks_length();
        match register {
            0x10 => {
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
            0x11 | 0x16 => {
                let pulse = if register == 0x11 {
                    &mut self.pulse1
                } else {
                    &mut self.pulse2
                };
                pulse.duty = value >> 6;
                pulse.length.counter = 64 - u16::from(value & 0x3F);
            }
            0x12 | 0x17 => {
                let pulse = if register == 0x12 {
                    &mut self.pulse1
                } else {
                    &mut self.pulse2
                };
                pulse.envelope.write(value);
                if !pulse.envelope.dac_on() {
                    pulse.enabled = false;
                }
            }
            0x13 | 0x18 => {
                let pulse = if register == 0x13 {
                    &mut self.pulse1
                } else {
                    &mut self.pulse2
                };
                pulse.frequency = (pulse.frequency & 0x700) | u16::from(value);
            }
            0x14 | 0x19 => {
                let pulse = if register == 0x14 {
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
            0x1A => {
                self.wave.dac_on = value & 0x80 != 0;
                if !self.wave.dac_on {
                    self.wave.enabled = false;
                }
            }
            0x1B => self.wave.length.counter = 256 - u16::from(value),
            0x1C => self.wave.volume_code = (value >> 5) & 3,
            0x1D => self.wave.frequency = (self.wave.frequency & 0x700) | u16::from(value),
            0x1E => {
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
            0x20 => self.noise.length.counter = 64 - u16::from(value & 0x3F),
            0x21 => {
                self.noise.envelope.write(value);
                if !self.noise.envelope.dac_on() {
                    self.noise.enabled = false;
                }
            }
            0x22 => {
                self.noise.shift = value >> 4;
                self.noise.narrow = value & 0x08 != 0;
                self.noise.divisor_code = value & 7;
            }
            0x23 => {
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
        let keep_lengths = self.variant == Variant::Dmg;
        let lengths = [
            self.pulse1.length.counter,
            self.pulse2.length.counter,
            self.wave.length.counter,
            self.noise.length.counter,
        ];
        for register in 0x10..=0x25 {
            self.write(register, 0);
        }
        self.pulse1 = Pulse {
            sweep: Some(Sweep::default()),
            ..Pulse::default()
        };
        self.pulse2 = Pulse::default();
        self.wave = Wave::default();
        self.noise = Noise::default();
        if keep_lengths {
            self.pulse1.length.counter = lengths[0];
            self.pulse2.length.counter = lengths[1];
            self.wave.length.counter = lengths[2];
            self.noise.length.counter = lengths[3];
        }
        self.powered = false;
    }
}
