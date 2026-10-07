//! The computer's microphone, for DS games that listen (NDS-10).
//!
//! Off until the user turns it on (Emulation > Use Microphone). [`open`]
//! captures from the default input device. Like [`crate::audio`], it
//! returns two halves: [`Microphone`] owns the device stream and stays on
//! the UI thread; [`Loudness`] goes to the emulation thread. The device
//! callback keeps the loudest sample it hears, and the emulation thread
//! takes that once a frame as the frame's `Input::microphone`. Only the
//! loudness crosses: the core turns it into noise, so nothing said reaches
//! the game, and recorded input stays one byte a frame.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use anyhow::Context as _;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// The open input stream. Dropping it stops listening.
pub struct Microphone {
    _stream: cpal::Stream,
}

/// The loudest the microphone has heard since the emulation thread last
/// looked, 0 to 255.
#[derive(Debug, Clone, Default)]
pub struct Loudness(Arc<AtomicU8>);

impl Loudness {
    /// Notes `samples`, heard by the device, full scale at ±1.0.
    pub fn hear(&self, samples: &[f32]) {
        self.0.fetch_max(level(samples), Ordering::Relaxed);
    }

    /// The loudest heard since the last call, and starts again from
    /// silence.
    pub fn take(&self) -> u8 {
        self.0.swap(0, Ordering::Relaxed)
    }
}

impl PartialEq for Loudness {
    /// The same microphone.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Loudness {}

/// The loudest of `samples` as 0 to 255, where 255 is full scale.
fn level(samples: &[f32]) -> u8 {
    let peak = samples
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    // Saturating float-to-int conversion; NaN becomes 0.
    (peak.min(1.0) * 255.0).round() as u8
}

/// Starts listening to the default input device.
///
/// # Errors
///
/// Fails if there is no input device, it cannot record 32-bit float
/// samples, or the system refuses access to it.
pub fn open() -> anyhow::Result<(Microphone, Loudness)> {
    let device = cpal::default_host()
        .default_input_device()
        .context("no microphone")?;
    let supported = device
        .default_input_config()
        .context("cannot query the microphone")?;
    let loudness = Loudness::default();
    let heard = loudness.clone();
    let stream = device
        .build_input_stream::<f32, _, _>(
            supported.config(),
            move |data: &[f32], _| heard.hear(data),
            |error| eprintln!("ds-emulator: microphone stream error: {error}"),
            None,
        )
        .context("cannot open the microphone")?;
    stream.play().context("cannot start the microphone")?;
    Ok((Microphone { _stream: stream }, loudness))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_level_is_the_loudest_sample() {
        assert_eq!(level(&[]), 0);
        assert_eq!(level(&[0.1, -0.5, 0.25]), 128);
        assert_eq!(level(&[2.0]), 255, "clipped");
        assert_eq!(level(&[f32::NAN]), 0);
    }

    #[test]
    fn taking_the_loudness_starts_again() {
        let loudness = Loudness::default();
        loudness.hear(&[0.5]);
        loudness.hear(&[0.25]);
        assert_eq!(loudness.take(), 128, "the loudest since the last look");
        assert_eq!(loudness.take(), 0);
        assert_eq!(loudness.clone(), loudness);
        assert_ne!(Loudness::default(), loudness);
    }
}
