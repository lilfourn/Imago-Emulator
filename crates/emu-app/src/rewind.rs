//! A bounded history of save states for rewinding.
//!
//! The newest state is kept whole. Each older state is stored as the XOR
//! of itself with the state after it, LZ4-compressed: consecutive frames
//! differ in few bytes, so deltas compress to a small fraction of a full
//! state. Stepping back decompresses one delta and XORs it into the newest
//! state. When the history exceeds its byte budget, the oldest deltas are
//! dropped.
//!
//! A DS state is several megabytes, so the history works in buffers it is
//! handed and hands back rather than allocating its own each frame, and
//! [`RewindThread`](crate::rewind_thread::RewindThread) runs it off the
//! emulation thread. [`capture_due`] decides which frames go in.

use std::collections::VecDeque;
use std::time::Duration;

use crate::speed::Speed;

/// Save states, newest first, within a memory budget.
#[derive(Debug)]
pub struct Rewind {
    newest: Option<Vec<u8>>,
    /// Compressed deltas, oldest at the front.
    deltas: VecDeque<Delta>,
    delta_bytes: usize,
    budget_bytes: usize,
    /// Room for compressing one delta, kept between pushes.
    compressed: Vec<u8>,
    /// A buffer the next [`Rewind::pop`] rebuilds the state before it in.
    spare: Option<Vec<u8>>,
}

/// One older state, as its XOR with the state after it, compressed.
#[derive(Debug)]
struct Delta {
    /// The older state's length.
    len: usize,
    compressed: Box<[u8]>,
}

impl Rewind {
    /// An empty history that keeps at most `budget_bytes` of deltas.
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            newest: None,
            deltas: VecDeque::new(),
            delta_bytes: 0,
            budget_bytes,
            compressed: Vec::new(),
            spare: None,
        }
    }

    /// Records `state` as the newest. Returns a buffer the history no
    /// longer needs, whose contents are garbage, to serialize a later
    /// state into.
    pub fn push(&mut self, state: Vec<u8>) -> Option<Vec<u8>> {
        let Some(mut previous) = self.newest.take() else {
            self.newest = Some(state);
            return None;
        };
        // `previous` becomes its XOR with the new state: the delta.
        xor_into(&mut previous, &state);
        self.newest = Some(state);
        let delta = self.compress(&previous);
        self.delta_bytes += delta.compressed.len();
        self.deltas.push_back(delta);
        while self.delta_bytes > self.budget_bytes {
            let Some(oldest) = self.deltas.pop_front() else {
                break;
            };
            self.delta_bytes -= oldest.compressed.len();
        }
        Some(previous)
    }

    /// Removes and returns the newest state.
    pub fn pop(&mut self) -> Option<Vec<u8>> {
        let newest = self.newest.take()?;
        if let Some(delta) = self.deltas.pop_back() {
            self.delta_bytes -= delta.compressed.len();
            let mut older = self.spare.take().unwrap_or_default();
            older.clear();
            older.resize(delta.len, 0);
            // Our own compressed data always decompresses.
            if lz4_flex::block::decompress_into(&delta.compressed, &mut older).is_ok() {
                xor_into(&mut older, &newest);
                self.newest = Some(older);
            }
        }
        Some(newest)
    }

    /// Takes a buffer, such as a state [`Rewind::pop`] returned and the
    /// caller has finished with, for the next pop to rebuild a state in.
    pub fn reuse(&mut self, buffer: Vec<u8>) {
        self.spare = Some(buffer);
    }

    /// Forgets everything, e.g. after loading a save state.
    pub fn clear(&mut self) {
        self.newest = None;
        self.deltas.clear();
        self.delta_bytes = 0;
    }

    /// How many states can be stepped back through.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.deltas.len() + usize::from(self.newest.is_some())
    }

    /// `delta` compressed.
    fn compress(&mut self, delta: &[u8]) -> Delta {
        self.compressed
            .resize(lz4_flex::block::get_maximum_output_size(delta.len()), 0);
        let written = lz4_flex::block::compress_into(delta, &mut self.compressed)
            .expect("the buffer has room for the largest output");
        Delta {
            len: delta.len(),
            compressed: self.compressed[..written].into(),
        }
    }
}

/// XORs `other` into `bytes`, as far as both go. Since XOR is its own
/// inverse, applying the same `other` twice gives back `bytes` even when
/// the lengths differ.
///
/// Zipping two slices lets the compiler XOR many bytes per instruction.
fn xor_into(bytes: &mut [u8], other: &[u8]) {
    for (byte, other) in bytes.iter_mut().zip(other) {
        *byte ^= other;
    }
}

/// Whether to record a frame that finished `since_last` after the last
/// one recorded, running at `speed` a system whose frames last `frame` at
/// normal speed.
///
/// At normal speed and slower, every frame is recorded, so rewinding steps
/// back through each one. Faster, a frame is recorded about as often as at
/// normal speed, in wall time: fast-forward costs no more to record than
/// normal play, and rewinding goes back through it as fast as it went by.
/// The quarter-frame allowance keeps 2x from slipping to every third frame
/// when a frame comes a little early.
pub fn capture_due(speed: Speed, frame: Duration, since_last: Duration) -> bool {
    match speed {
        Speed::Times(rate) if rate.get() <= 1.0 => true,
        _ => since_last >= frame * 3 / 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::speed::Rate;

    fn state(frame: u8) -> Vec<u8> {
        let mut state = vec![0xAB; 4096];
        state[100] = frame;
        state[2000] = frame.wrapping_mul(3);
        state
    }

    #[test]
    fn pops_states_newest_first() {
        let mut rewind = Rewind::new(1 << 20);
        for frame in 0..10 {
            rewind.push(state(frame));
        }
        for frame in (0..10).rev() {
            assert_eq!(rewind.pop(), Some(state(frame)));
        }
        assert_eq!(rewind.pop(), None);
    }

    #[test]
    fn reused_buffers_do_not_leak_into_states() {
        let mut rewind = Rewind::new(1 << 20);
        let mut spare = None;
        for frame in 0..10 {
            // Serialize into whatever the history handed back last.
            let mut buffer: Vec<u8> = spare.take().unwrap_or_default();
            buffer.clear();
            buffer.extend_from_slice(&state(frame));
            spare = rewind.push(buffer);
        }
        for frame in (0..10).rev() {
            let popped = rewind.pop().expect("a state");
            assert_eq!(popped, state(frame));
            rewind.reuse(vec![0x55; 9000]);
        }
    }

    #[test]
    fn deltas_are_much_smaller_than_states() {
        let mut rewind = Rewind::new(1 << 20);
        for frame in 0..100 {
            rewind.push(state(frame));
        }
        assert!(
            rewind.delta_bytes < 100 * 4096 / 20,
            "{} bytes",
            rewind.delta_bytes
        );
    }

    #[test]
    fn budget_drops_the_oldest_states() {
        let mut rewind = Rewind::new(200);
        for frame in 0..100 {
            rewind.push(state(frame));
        }
        assert!(rewind.len() < 100);
        assert_eq!(rewind.pop(), Some(state(99)));
    }

    #[test]
    fn handles_states_of_different_lengths() {
        let mut rewind = Rewind::new(1 << 20);
        rewind.push(vec![1, 2, 3]);
        rewind.push(vec![9; 6]);
        rewind.push(vec![4]);
        assert_eq!(rewind.pop(), Some(vec![4]));
        assert_eq!(rewind.pop(), Some(vec![9; 6]));
        assert_eq!(rewind.pop(), Some(vec![1, 2, 3]));
    }

    #[test]
    fn every_frame_is_recorded_up_to_normal_speed() {
        let frame = Duration::from_millis(16);
        for rate in [0.25, 0.5, 1.0] {
            let speed = Speed::Times(Rate::new(rate).expect("valid"));
            assert!(capture_due(speed, frame, Duration::ZERO), "{rate}");
        }
    }

    #[test]
    fn faster_records_about_once_a_normal_frame() {
        let frame = Duration::from_millis(16);
        let double = Speed::Times(Rate::new(2.0).expect("valid"));
        // At 2x, frames come 8 ms apart, give or take.
        assert!(!capture_due(double, frame, Duration::from_millis(9)));
        assert!(capture_due(double, frame, Duration::from_millis(15)));
        assert!(!capture_due(
            Speed::Unlimited,
            frame,
            Duration::from_millis(5)
        ));
        assert!(capture_due(
            Speed::Unlimited,
            frame,
            Duration::from_millis(16)
        ));
    }
}
