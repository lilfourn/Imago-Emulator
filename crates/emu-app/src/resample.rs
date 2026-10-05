//! Converts a core's audio to the output device's rate.
//!
//! The emulator's clock and the sound card's clock never agree exactly, so
//! the output buffer slowly fills or drains. Dynamic rate control fixes
//! that: the resampling ratio is nudged by up to ±0.5% towards keeping the
//! buffer half full, which is too small a pitch change to hear.

use emu_core::StereoSample;

/// Largest relative change to the resampling ratio.
const MAX_ADJUSTMENT: f64 = 0.005;

/// A stereo resampler using linear interpolation.
#[derive(Debug, Clone, Default)]
pub struct Resampler {
    /// Position of the next output sample between `previous` (0.0) and the
    /// next input sample (1.0).
    position: f64,
    previous: [f32; 2],
}

impl Resampler {
    /// Appends `input` to `out`, taking `step` input samples per output
    /// sample.
    pub fn process(&mut self, input: &[StereoSample], step: f64, out: &mut Vec<[f32; 2]>) {
        for sample in input {
            let current = sample.map(|value| f32::from(value) / 32_768.0);
            while self.position < 1.0 {
                let t = self.position as f32;
                out.push([
                    self.previous[0] + (current[0] - self.previous[0]) * t,
                    self.previous[1] + (current[1] - self.previous[1]) * t,
                ]);
                self.position += step;
            }
            self.position -= 1.0;
            self.previous = current;
        }
    }
}

/// Input samples per output sample to convert `source_rate` to
/// `device_rate`, corrected towards a half-full buffer. `fill` is how full
/// the output buffer is, from 0.0 to 1.0.
pub fn step_for(source_rate: u32, device_rate: u32, fill: f64) -> f64 {
    let base = f64::from(source_rate) / f64::from(device_rate);
    // Too full: consume input faster, producing fewer output samples.
    let correction = ((fill - 0.5) * 2.0).clamp(-1.0, 1.0) * MAX_ADJUSTMENT;
    base * (1.0 + correction)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_rates_pass_samples_through() {
        let mut resampler = Resampler::default();
        let mut out = Vec::new();
        resampler.process(&[[16_384, -16_384]; 4], 1.0, &mut out);
        assert_eq!(out.len(), 4);
        assert_eq!(out[3], [0.5, -0.5]);
    }

    #[test]
    fn halving_the_rate_halves_the_samples() {
        let mut resampler = Resampler::default();
        let mut out = Vec::new();
        resampler.process(&[[0, 0]; 1000], 2.0, &mut out);
        assert_eq!(out.len(), 500);
    }

    #[test]
    fn step_is_corrected_towards_half_full() {
        assert_eq!(step_for(48_000, 48_000, 0.5), 1.0);
        assert!(step_for(48_000, 48_000, 0.9) > 1.0);
        assert!(step_for(48_000, 48_000, 0.1) < 1.0);
        assert!((step_for(48_000, 48_000, 1.0) - 1.005).abs() < 1e-12);
    }
}
