//! Sound output through the system's default device.
//!
//! [`open`] returns two halves. [`AudioOutput`] owns the device stream and
//! stays on the UI thread. [`AudioSink`] goes to the emulation thread,
//! which pushes each frame's samples into a lock-free ring buffer that the
//! device callback drains. An empty buffer plays silence, so pausing needs
//! no special case.
//!
//! On Linux and Windows, a stream whose device goes, as when headphones
//! are unplugged, is replaced by one on the system's new default device,
//! draining the same buffer at that device's rate; if none is left, the
//! game plays on silently until one comes. On macOS a stream on the
//! default device follows it to the next by itself.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError, TryLockError, Weak};
use std::thread;
use std::time::Duration;

use anyhow::Context as _;
use cpal::ErrorKind;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use emu_core::StereoSample;

use crate::resample::{self, Resampler};
use crate::trace;

/// Output buffer length. The sink keeps it about half full, so this is
/// roughly twice the added latency.
const BUFFER_SECONDS: f64 = 0.1;

/// How long to wait between tries at a new device while there is none.
const RETRY: Duration = Duration::from_secs(1);

/// The open output stream. Dropping it stops sound.
pub struct AudioOutput {
    shared: Arc<Shared>,
}

/// What the output's streams share, one after another.
struct Shared {
    /// The buffer's draining end, which each stream takes in turn.
    queue: Arc<Mutex<rtrb::Consumer<[f32; 2]>>>,
    /// The sample rate of the device playing, which the sink resamples to.
    device_rate: Arc<AtomicU32>,
    /// The stream playing, if any.
    streams: Mutex<Streams>,
}

/// The output's stream, and whether it wants one.
#[derive(Default)]
struct Streams {
    playing: Option<cpal::Stream>,
    /// Off once the output is dropped.
    open: bool,
    /// While a new stream is being sought.
    replacing: bool,
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        let stopped = {
            let mut streams = self
                .shared
                .streams
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            streams.open = false;
            streams.playing.take()
        };
        drop(stopped);
    }
}

/// The emulation thread's end: resamples and queues samples.
pub struct AudioSink {
    producer: rtrb::Producer<[f32; 2]>,
    resampler: Resampler,
    source_rate: u32,
    device_rate: Arc<AtomicU32>,
    capacity: usize,
    scratch: Vec<[f32; 2]>,
    /// How many times faster than its own clock the core runs.
    speed: f64,
}

impl AudioSink {
    /// A sink queueing a core's `source_rate` sound into `producer`, which
    /// holds `capacity` samples, for a device playing at `device_rate`.
    fn new(
        producer: rtrb::Producer<[f32; 2]>,
        capacity: usize,
        source_rate: u32,
        device_rate: Arc<AtomicU32>,
    ) -> Self {
        Self {
            producer,
            resampler: Resampler::default(),
            source_rate,
            device_rate,
            capacity,
            speed: 1.0,
            scratch: Vec::new(),
        }
    }

    /// Queues `samples`, produced at the core's `source_rate`. Samples that
    /// do not fit are dropped.
    pub fn push(&mut self, samples: &[StereoSample]) {
        let fill = 1.0 - self.producer.slots() as f64 / self.capacity as f64;
        // In thousandths; the fraction is from 0 to 1.
        trace::event("fill", &[(fill * 1000.0) as u64]);
        let device_rate = self.device_rate.load(Ordering::Relaxed);
        let step = resample::step_for(self.source_rate, device_rate, fill) * self.speed;
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

    /// Tops the buffer up to half full with silence, where rate control
    /// keeps it, so sound starting now can arrive unevenly without the
    /// buffer running dry.
    fn prime(&mut self) {
        let queued = self.capacity - self.producer.slots();
        for _ in queued..self.capacity / 2 {
            if self.producer.push([0.0; 2]).is_err() {
                break;
            }
        }
    }

    /// Plays sound that comes on a clock of its own as it comes, rather
    /// than with each frame, while `on` says it should: as an RPG Maker
    /// game's player makes it, whose music plays on through a frame that
    /// takes a tenth of a second to load a map. The sound arrives a
    /// twentieth of a second at a time, so whenever it starts again the
    /// buffer starts half full.
    pub fn play_live(mut self, on: Arc<AtomicBool>) -> impl FnMut(&[StereoSample]) + Send {
        let mut was_on = false;
        move |samples| {
            let is_on = on.load(Ordering::Relaxed);
            if is_on {
                if !was_on {
                    self.prime();
                }
                self.push(samples);
            }
            was_on = is_on;
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
    let config = device
        .default_output_config()
        .context("cannot query the audio device")?
        .config();
    let capacity = (f64::from(config.sample_rate) * BUFFER_SECONDS) as usize;
    let (producer, consumer) = rtrb::RingBuffer::<[f32; 2]>::new(capacity);
    let shared = Arc::new(Shared {
        queue: Arc::new(Mutex::new(consumer)),
        device_rate: Arc::new(AtomicU32::new(config.sample_rate)),
        streams: Mutex::new(Streams {
            open: true,
            ..Streams::default()
        }),
    });
    let stream = play(&shared, &device, config)?;
    shared
        .streams
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .playing = Some(stream);
    let sink = AudioSink::new(
        producer,
        capacity,
        source_rate,
        Arc::clone(&shared.device_rate),
    );
    Ok((AudioOutput { shared }, sink))
}

/// Starts a stream on `device`, playing `config`, that drains the output's
/// buffer, and has the sink resample to its rate.
fn play(
    shared: &Arc<Shared>,
    device: &cpal::Device,
    config: cpal::StreamConfig,
) -> anyhow::Result<cpal::Stream> {
    let channels = usize::from(config.channels);
    let device_rate = config.sample_rate;
    let queue = Arc::clone(&shared.queue);
    let output = Arc::downgrade(shared);
    let stream = device
        .build_output_stream::<f32, _, _>(
            config,
            move |data: &mut [f32], _| {
                // Only the stream before this one held the queue, and only
                // while it was being let go of.
                let mut consumer = match queue.try_lock() {
                    Ok(consumer) => consumer,
                    Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                    Err(TryLockError::WouldBlock) => {
                        data.fill(0.0);
                        return;
                    }
                };
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
            move |error| {
                eprintln!("ds-emulator: audio stream error: {error}");
                if ends_stream(error.kind()) {
                    replace(output.clone());
                }
            },
            None,
        )
        .context("cannot open an audio stream")?;
    stream.play().context("cannot start the audio stream")?;
    shared.device_rate.store(device_rate, Ordering::Relaxed);
    Ok(stream)
}

/// Whether an error of `kind` means the stream has stopped for good, its
/// device gone or no longer the system's default, so another must be
/// opened.
///
/// Not on macOS, where a stream on the default device follows it to the
/// next by itself (`DeviceChanged`), and which this port leaves as it was.
fn ends_stream(kind: ErrorKind) -> bool {
    cfg!(not(target_os = "macos"))
        && matches!(
            kind,
            ErrorKind::DeviceNotAvailable | ErrorKind::StreamInvalidated
        )
}

/// Replaces `output`'s stream with one on the system's default device, on
/// a thread of its own, as the stream that ended can't be let go of from
/// its own callback. While there is no device, it tries again every
/// [`RETRY`], until there is one or the output is dropped.
fn replace(output: Weak<Shared>) {
    let started = thread::Builder::new()
        .name("audio-device".to_owned())
        .spawn(move || {
            let Some(shared) = output.upgrade() else {
                return;
            };
            let ended = {
                let mut streams = shared
                    .streams
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if !streams.open || streams.replacing {
                    return;
                }
                streams.replacing = true;
                streams.playing.take()
            };
            drop(ended);
            let mut said = false;
            loop {
                let open = shared
                    .streams
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .open;
                if !open {
                    return;
                }
                let started = cpal::default_host()
                    .default_output_device()
                    .context("no audio output device")
                    .and_then(|device| {
                        let config = device
                            .default_output_config()
                            .context("cannot query the audio device")?
                            .config();
                        play(&shared, &device, config)
                    });
                let mut streams = shared
                    .streams
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                match started {
                    Ok(stream) => {
                        streams.replacing = false;
                        if streams.open {
                            streams.playing = Some(stream);
                            eprintln!("ds-emulator: sound moved to the default device");
                        }
                        return;
                    }
                    Err(_) if !streams.open => return,
                    Err(error) => {
                        if !said {
                            eprintln!("ds-emulator: no sound device, looking again: {error:#}");
                            said = true;
                        }
                    }
                }
                drop(streams);
                thread::sleep(RETRY);
            }
        });
    if let Err(error) = started {
        eprintln!("ds-emulator: cannot look for another sound device: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DS_RATE: u32 = 32_768;

    /// A sink for the DS whose buffer was sized for a device at `rate`,
    /// half full as the device has played a while, with the device's end.
    fn sink_at(rate: u32) -> (AudioSink, rtrb::Consumer<[f32; 2]>, Arc<AtomicU32>) {
        let capacity = (f64::from(rate) * BUFFER_SECONDS) as usize;
        let (producer, mut consumer) = rtrb::RingBuffer::new(capacity);
        let device_rate = Arc::new(AtomicU32::new(rate));
        let mut sink = AudioSink::new(producer, capacity, DS_RATE, Arc::clone(&device_rate));
        for _ in 0..capacity / 2 {
            consumer.pop().ok();
            sink.producer.push([0.0; 2]).ok();
        }
        (sink, consumer, device_rate)
    }

    /// Plays `seconds` of a DS whose frames last 2 refreshes of a 120 Hz
    /// display through `sink` to a device at `rate`, returning how full
    /// the buffer is then.
    fn play_for(
        sink: &mut AudioSink,
        consumer: &mut rtrb::Consumer<[f32; 2]>,
        rate: u32,
        seconds: u32,
    ) -> f64 {
        // A DS frame's sound, 16.715 ms of it, every 16.667 ms.
        let frame = vec![[0i16; 2]; 600];
        let (mut owed, mut produced, mut played) = (0.0, 0.0, 0.0);
        for _ in 0..seconds * 60 {
            owed += f64::from(DS_RATE) * 0.016_715;
            let samples = owed.floor();
            owed -= samples;
            sink.push(&frame[..samples as usize]);
            played += f64::from(rate) / 60.0;
            while produced < played {
                consumer.pop().ok();
                produced += 1.0;
            }
        }
        1.0 - sink.producer.slots() as f64 / sink.capacity as f64
    }

    /// How full the buffer of a sink at `speed` is after `seconds` against
    /// a device at `rate`.
    fn fill_after(rate: u32, speed: f64, seconds: u32) -> f64 {
        let (mut sink, mut consumer, _) = sink_at(rate);
        sink.set_speed(speed);
        play_for(&mut sink, &mut consumer, rate, seconds)
    }

    /// Frames two refreshes of a 120 Hz display long run this much faster
    /// than the DS's own.
    const LOCKED: f64 = 16.715 / (2.0 / 120.0 * 1000.0);

    #[test]
    fn live_sound_plays_while_on_and_starts_half_full() {
        const RATE: u32 = 48_000;
        let capacity = (f64::from(RATE) * BUFFER_SECONDS) as usize;
        let (producer, mut consumer) = rtrb::RingBuffer::new(capacity);
        let sink = AudioSink::new(producer, capacity, RATE, Arc::new(AtomicU32::new(RATE)));
        let on = Arc::new(AtomicBool::new(false));
        let mut play = sink.play_live(Arc::clone(&on));
        // As the player writes it: 1,024 samples, 21.3 ms, at a time.
        let piece = vec![[1000_i16; 2]; 1024];
        play(&piece);
        assert_eq!(consumer.slots(), 0, "paused or fast-forwarding");
        on.store(true, Ordering::Relaxed);
        play(&piece);
        assert_eq!(consumer.slots(), capacity / 2 + piece.len());
        // The device takes 512 samples every 10.7 ms; the pieces keep up,
        // a frame that stalls the game for a tenth of a second or not.
        let mut dry = 0;
        for _ in 0..1000 {
            for _ in 0..2 {
                dry += usize::from(consumer.slots() < 512);
                for _ in 0..512 {
                    consumer.pop().ok();
                }
            }
            play(&piece);
        }
        assert_eq!(dry, 0);
        // Off, the buffer plays out; on again, it starts half full again.
        on.store(false, Ordering::Relaxed);
        play(&piece);
        while consumer.pop().is_ok() {}
        on.store(true, Ordering::Relaxed);
        play(&piece);
        let silence = capacity / 2;
        assert_eq!(consumer.slots(), silence + piece.len());
        let first = consumer.pop().expect("primed");
        assert_eq!(first, [0.0, 0.0]);
    }

    #[test]
    fn frames_kept_in_step_with_the_display_keep_the_buffer_half_full() {
        let fill = fill_after(48_000, LOCKED, 60);
        assert!((fill - 0.5).abs() < 0.05, "{fill}");
        // Unaware of the speed, rate control holds it a good deal fuller.
        let unaware = fill_after(48_000, 1.0, 60);
        assert!(unaware > 0.75, "{unaware}");
    }

    #[test]
    fn devices_at_other_rates_keep_the_buffer_half_full() {
        for rate in [22_050, 44_100, 88_200, 96_000, 192_000] {
            let fill = fill_after(rate, LOCKED, 60);
            assert!((fill - 0.5).abs() < 0.05, "{rate} Hz: {fill}");
        }
    }

    #[test]
    fn a_new_device_at_another_rate_keeps_the_same_buffer_half_full() {
        // As when headphones at 48 kHz are unplugged and the speakers,
        // at 44.1 kHz, take over: the buffer is the same, now longer.
        let (mut sink, mut consumer, device_rate) = sink_at(48_000);
        sink.set_speed(LOCKED);
        play_for(&mut sink, &mut consumer, 48_000, 10);
        device_rate.store(44_100, Ordering::Relaxed);
        let fill = play_for(&mut sink, &mut consumer, 44_100, 60);
        assert!((fill - 0.5).abs() < 0.05, "{fill}");
    }

    #[test]
    fn only_a_stream_that_has_stopped_for_good_is_replaced() {
        let replaced = [ErrorKind::DeviceNotAvailable, ErrorKind::StreamInvalidated];
        for kind in replaced {
            assert_eq!(
                ends_stream(kind),
                cfg!(not(target_os = "macos")),
                "{kind:?}"
            );
        }
        // An underrun, a move to another device, or anything else: the
        // stream plays on.
        for kind in [
            ErrorKind::Xrun,
            ErrorKind::DeviceChanged,
            ErrorKind::DeviceBusy,
            ErrorKind::BackendError,
        ] {
            assert!(!ends_stream(kind), "{kind:?}");
        }
    }
}
