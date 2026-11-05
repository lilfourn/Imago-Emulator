//! Timing for the benchmarks kept as ignored tests beside the code they
//! time. Run one in a release build:
//! `cargo test --release -p emu-app -- --ignored --nocapture NAME`.

use std::hint::black_box;
use std::time::{Duration, Instant};

/// The shortest of `runs` timed calls of `work`, after a few to warm up:
/// the least disturbed by whatever else the machine is doing.
pub fn best_of<T>(runs: usize, mut work: impl FnMut() -> T) -> Duration {
    for _ in 0..3 {
        black_box(work());
    }
    (0..runs)
        .map(|_| {
            let start = Instant::now();
            black_box(work());
            start.elapsed()
        })
        .min()
        .unwrap_or_default()
}
