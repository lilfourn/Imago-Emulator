//! The timing trace, for `scripts/macos/bench.sh`. With `DS_EMULATOR_TRACE`
//! naming a file, the app writes a line there for each step a key press
//! and a frame take on their way to the window: `<microseconds> <event>
//! <values…>`, the microseconds counted from the app's start. Without the
//! variable, nothing is written.
//!
//! The events, by thread:
//! - main: `send <buttons>` as a key's buttons go to the emulation thread;
//!   `take <frame>` as the window takes a new frame to draw, and `drawn
//!   <frame> <µs>` once its images are made, with the time that took;
//!   `render` for each redraw, new frames or not.
//! - emulation: `run <frame> <buttons> <µs>` for each frame run, numbered
//!   as it is published, with the buttons it ran with and how long it
//!   took; `busy <µs>` for all the work of one turn of the loop; `late
//!   <µs>` for how long after its deadline the thread woke.

use std::fs::File;
use std::io::Write as _;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

struct Trace {
    start: Instant,
    file: Mutex<File>,
}

fn trace() -> Option<&'static Trace> {
    static TRACE: OnceLock<Option<Trace>> = OnceLock::new();
    TRACE
        .get_or_init(|| {
            let path = std::env::var_os("DS_EMULATOR_TRACE")?;
            let file = File::create(path).ok()?;
            Some(Trace {
                start: Instant::now(),
                file: Mutex::new(file),
            })
        })
        .as_ref()
}

/// Whether the trace is on: work done only to trace can be skipped.
pub fn on() -> bool {
    trace().is_some()
}

/// Writes `event` with `values`, at the time it is now.
pub fn event(event: &str, values: &[u64]) {
    let Some(trace) = trace() else {
        return;
    };
    let mut line = format!("{} {event}", trace.start.elapsed().as_micros());
    for value in values {
        line.push(' ');
        line.push_str(&value.to_string());
    }
    line.push('\n');
    if let Ok(mut file) = trace.file.lock() {
        file.write_all(line.as_bytes()).ok();
    }
}

/// `duration` in whole microseconds, as events give times.
pub fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}
