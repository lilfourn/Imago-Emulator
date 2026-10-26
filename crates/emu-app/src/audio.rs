//! Sound output through the system's default device.
//!
//! [`open`] returns two halves. [`AudioOutput`] owns the device stream and
//! stays on the UI thread (streams cannot move between threads on every
//! platform). [`AudioSink`] goes to the emulation thread, which pushes each
//! frame's samples into a lock-free ring buffer that the device callback
//! drains. An empty buffer plays silence, so pausing needs no special case.

use anyhow::Context as _;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use emu_core::StereoSample;

use crate::resample::{self, Resampler};
use crate::trace;

/// Output buffer length. The sink keeps it about half full, so this is
/// roughly twice the added latency.
const BUFFER_SECONDS: f64 = 0.1;

/// The open output stream. Dropping it stops sound.
pub struct AudioOutput {
    _stream: cpal::Stream,
}

/// The emulation thread's end: resamples and queues samples.
pub struct AudioSink {
    producer: rtrb::Producer<[f32; 2]>,
    resampler: Resampler,
    source_rate: u32,
    device_rate: u32,
    capacity: usize,
    scratch: Vec<[f32; 2]>,
    /// How many times faster than its own clock the core runs.
    speed: f64,
}

impl AudioSink {
    /// Queues `samples`, produced at the core's `source_rate`. Samples that
    /// do not fit are dropped.
    pub fn push(&mut self, samples: &[StereoSample]) {
        let fill = 1.0 - self.producer.slots() as f64 / self.capacity as f64;
        // In thousandths; the fraction is from 0 to 1.
        trace::event("fill", &[(fill * 1000.0) as u64]);
        let step = resample::step_for(self.source_rate, self.device_rate, fill) * self.speed;
        self.scratch.clear();
        self.resampler.process(samples, step, &mut self.scratch);
        for frame in &self.scratch {
            if self.producer.push(*frame).is_err() {
                break;
            }
        }
    }

    /// Takes how many times faster than its own clock the core runs, as
    /// when its frames keep to the display's refreshes: the samples play
    /// that much faster, so the buffer stays half full.
    pub fn set_speed(&mut self, speed: f64) {
        self.speed = speed;
    }
}

/// Opens the default output device for a core producing `source_rate` Hz.
///
/// # Errors
///
/// Fails if there is no output device or it cannot play 32-bit float
/// samples.
pub fn open(source_rate: u32) -> anyhow::Result<(AudioOutput, AudioSink)> {
    let device = cpal::default_host()
        .default_output_device()
        .context("no audio output device")?;
    let supported = device
        .default_output_config()
        .context("cannot query the audio device")?;
    let config = supported.config();
    let channels = usize::from(config.channels);
    let device_rate = config.sample_rate;
    let capacity = (f64::from(device_rate) * BUFFER_SECONDS) as usize;
    let (producer, mut consumer) = rtrb::RingBuffer::<[f32; 2]>::new(capacity);

    let stream = device
        .build_output_stream::<f32, _, _>(
            config,
            move |data: &mut [f32], _| {
                for frame in data.chunks_mut(channels) {
                    let [left, right] = consumer.pop().unwrap_or([0.0, 0.0]);
                    match frame {
                        [mono] => *mono = (left + right) / 2.0,
                        [first, second, rest @ ..] => {
                            *first = left;
                            *second = right;
                            rest.fill(0.0);
                        }
                        [] => {}
                    }
                }
            },
            |error| eprintln!("ds-emulator: audio stream error: {error}"),
            None,
        )
        .context("cannot open an audio stream")?;
    stream.play().context("cannot start the audio stream")?;

    Ok((
        AudioOutput { _stream: stream },
        AudioSink {
            producer,
            resampler: Resampler::default(),
            source_rate,
            device_rate,
            capacity,
            speed: 1.0,
            scratch: Vec::new(),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How full the buffer of a sink at `speed` is after `seconds` of a DS
    /// whose frames last 2 refreshes of a 120 Hz display, against a 48 kHz
    /// device.
    fn fill_after(speed: f64, seconds: u32) -> f64 {
        const DEVICE_RATE: u32 = 48_000;
        const DS_RATE: u32 = 32_768;
        let capacity = (f64::from(DEVICE_RATE) * BUFFER_SECONDS) as usize;
        let (producer, mut consumer) = rtrb::RingBuffer::new(capacity);
        let mut sink = AudioSink {
            producer,
            resampler: Resampler::default(),
            source_rate: DS_RATE,
            device_rate: DEVICE_RATE,
            capacity,
            speed: 1.0,
            scratch: Vec::new(),
        };
        sink.set_speed(speed);
        // Half full to start, as the device has played a while.
        for _ in 0..capacity / 2 {
            consumer.pop().ok();
            sink.producer.push([0.0; 2]).ok();
        }
        // A DS frame's sound, 16.715 ms of it, every 16.667 ms.
        let frame = vec![[0i16; 2]; 600];
        let (mut owed, mut produced, mut played) = (0.0, 0.0, 0.0);
        for _ in 0..seconds * 60 {
            owed += f64::from(DS_RATE) * 0.016_715;
            let samples = owed.floor();
            owed -= samples;
            sink.push(&frame[..samples as usize]);
            played += f64::from(DEVICE_RATE) / 60.0;
            while produced < played {
                consumer.pop().ok();
                produced += 1.0;
            }
        }
        1.0 - sink.producer.slots() as f64 / capacity as f64
    }

    #[test]
    fn frames_kept_in_step_with_the_display_keep_the_buffer_half_full() {
        let speed = 16.715 / (2.0 / 120.0 * 1000.0);
        let fill = fill_after(speed, 60);
        assert!((fill - 0.5).abs() < 0.05, "{fill}");
        // Unaware of the speed, rate control holds it a good deal fuller.
        let unaware = fill_after(1.0, 60);
        assert!(unaware > 0.75, "{unaware}");
    }
}
