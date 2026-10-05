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
}

impl AudioSink {
    /// Queues `samples`, produced at the core's `source_rate`. Samples that
    /// do not fit are dropped.
    pub fn push(&mut self, samples: &[StereoSample]) {
        let fill = 1.0 - self.producer.slots() as f64 / self.capacity as f64;
        let step = resample::step_for(self.source_rate, self.device_rate, fill);
        self.scratch.clear();
        self.resampler.process(samples, step, &mut self.scratch);
        for frame in &self.scratch {
            if self.producer.push(*frame).is_err() {
                break;
            }
        }
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
            scratch: Vec::new(),
        },
    ))
}
