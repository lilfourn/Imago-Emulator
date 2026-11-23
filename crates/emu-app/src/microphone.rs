//! The computer's microphone, for DS games that listen (NDS-10).
//!
//! Off until the user turns it on (Emulation > Use Microphone). [`open`]
//! captures from the default input device, and returns two halves:
//! [`Microphone`] owns the device stream, and [`Loudness`] is shared with
//! its callback, which keeps the loudest sample it hears. The window takes
//! that once a frame and sends it to the game as the active player's
//! `Input::microphone`. Only the loudness crosses: the core turns it into
//! noise, so nothing said reaches the game, and recorded input stays one
//! byte a frame.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use anyhow::Context as _;
use cpal::ErrorKind;
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
/// samples, or the system refuses access to it, which on Linux and
/// Windows says how to allow it.
pub fn open() -> anyhow::Result<(Microphone, Loudness)> {
    let device = cpal::default_host()
        .default_input_device()
        .context("no microphone")?;
    let supported = device
        .default_input_config()
        .map_err(|error| explain(error, "cannot query the microphone"))?;
    let loudness = Loudness::default();
    let heard = loudness.clone();
    let stream = device
        .build_input_stream::<f32, _, _>(
            supported.config(),
            move |data: &[f32], _| heard.hear(data),
            |error| eprintln!("ds-emulator: microphone stream error: {error}"),
            None,
        )
        .map_err(|error| explain(error, "cannot open the microphone"))?;
    stream
        .play()
        .map_err(|error| explain(error, "cannot start the microphone"))?;
    Ok((Microphone { _stream: stream }, loudness))
}

/// What to tell the player of `error`, met while `doing` something: how to
/// allow the microphone, if the system refused it, or else what went wrong.
fn explain(error: cpal::Error, doing: &'static str) -> anyhow::Error {
    match refusal(&error) {
        Some(how) => anyhow::anyhow!(how),
        None => anyhow::Error::new(error).context(doing),
    }
}

/// How to let the app use the microphone, if `error` says the system
/// refused it. macOS asks the player itself, and is left as it was.
fn refusal(error: &cpal::Error) -> Option<&'static str> {
    let refused = error.kind() == ErrorKind::PermissionDenied
        // cpal passes WASAPI's E_ACCESSDENIED on as a plain error, whose
        // message ends with the code (0x80070005) as a signed number.
        || (cfg!(windows) && error.to_string().contains("os error -2147024891"));
    if !refused {
        return None;
    }
    if cfg!(windows) {
        Some(
            "Windows is keeping the microphone from apps like this one. Turn on \
             Settings > Privacy & security > Microphone > Let desktop apps access your \
             microphone, then try again",
        )
    } else if cfg!(target_os = "linux") {
        Some(
            "Linux won't let ds-emulator open the microphone. Your account needs the sound \
             devices: sign in to a desktop session, or join the audio group, then try again",
        )
    } else {
        None
    }
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
    fn a_refusal_says_how_to_allow_the_microphone() {
        let denied = cpal::Error::new(ErrorKind::PermissionDenied);
        let wasapi = cpal::Error::with_message(
            ErrorKind::BackendError,
            "Access is denied. (os error -2147024891)",
        );
        let busy = cpal::Error::with_message(ErrorKind::DeviceBusy, "in use");
        if cfg!(windows) {
            for error in [&denied, &wasapi] {
                let how = refusal(error).expect("explained");
                assert!(how.contains("Privacy & security > Microphone"), "{how}");
            }
        } else if cfg!(target_os = "linux") {
            let how = refusal(&denied).expect("explained");
            assert!(how.contains("audio group"), "{how}");
            assert_eq!(refusal(&wasapi), None);
        } else {
            assert_eq!(refusal(&denied), None, "macOS asks for itself");
        }
        assert_eq!(refusal(&busy), None);
        let other = format!("{:#}", explain(busy, "cannot open the microphone"));
        assert_eq!(other, "cannot open the microphone: in use");
    }

    #[test]
    fn taking_the_loudness_starts_again() {
        let loudness = Loudness::default();
        loudness.hear(&[0.5]);
        loudness.hear(&[0.25]);
        assert_eq!(loudness.take(), 128, "the loudest since the last look");
        assert_eq!(loudness.take(), 0);
    }
}
