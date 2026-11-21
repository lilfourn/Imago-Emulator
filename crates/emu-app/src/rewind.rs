//! A bounded history of save states for rewinding.
//!
//! The newest state is kept whole. Each older state is stored as the XOR
//! of itself with the state after it, LZ4-compressed: consecutive frames
//! differ in few bytes, so deltas compress to a small fraction of a full
//! state. Stepping back decompresses one delta and XORs it into the newest
//! state. When the history exceeds its byte budget, the oldest deltas are
//! dropped.
//!
//! A state's numbers take as many bytes as their values need, so one
//! register that grows moves everything after it, and a byte-for-byte XOR
//! of two DS states differed in two thirds of their 6 MB. Each stretch of
//! the older state is therefore XORed with where it went in the newer one
//! ([`line_up`]): a delta of Pokémon Platinum fell from 2.8 MB to 41 KB.
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
    /// Scratch buffer holding only the segments that changed, kept between pushes.
    delta_buf: Vec<u8>,
    /// Room for compressing one delta, kept between pushes.
    compressed: Vec<u8>,
    /// A buffer the next [`Rewind::pop`] rebuilds the state before it in.
    spare: Option<Vec<u8>>,
}

/// One older state, as the XOR of its changed segments with the state after it,
/// compressed.
#[derive(Debug)]
struct Delta {
    /// The older state's length.
    len: usize,
    /// Where its segments went in the state after it.
    shifts: Box<[Shift]>,
    /// Bit `segment` is set when that `SEGMENT`-byte stretch differed from
    /// where it moved in the newer state and was written into `compressed`.
    dirty: Box<[u64]>,
    /// Uncompressed byte length of the dirty segments stored in `compressed`.
    dirty_len: usize,
    compressed: Box<[u8]>,
}

/// From segment `from` of an older state on, until the next shift: how
/// many bytes its segments moved in the state after it. Segments before
/// the first shift stayed where they were.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Shift {
    from: usize,
    by: isize,
}

/// Bytes in each stretch of a state that is lined up on its own.
const SEGMENT: usize = 4096;
/// Bytes from the start of a segment that are looked for in the next
/// state.
const ANCHOR: usize = 32;
/// How far a segment is looked for from where the one before it went.
const REACH: usize = 64 * 1024;
/// The most segments looked for per delta, so states that differ
/// everywhere cost no more than a few hundred microseconds to line up.
const SEARCHES: usize = 64;

impl Rewind {
    /// An empty history that keeps at most `budget_bytes` of deltas.
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            newest: None,
            deltas: VecDeque::new(),
            delta_bytes: 0,
            budget_bytes,
            delta_buf: Vec::new(),
            compressed: Vec::new(),
            spare: None,
        }
    }

    /// Records `state` as the newest. Returns a buffer the history no
    /// longer needs, whose contents are garbage, to serialize a later
    /// state into.
    pub fn push(&mut self, state: Vec<u8>) -> Option<Vec<u8>> {
        let Some(previous) = self.newest.take() else {
            self.newest = Some(state);
            return None;
        };
        let shifts = line_up(&previous, &state);
        self.delta_buf.clear();
        let dirty = collect_dirty_xor(&previous, &state, &shifts, &mut self.delta_buf);
        self.newest = Some(state);
        let delta = self.compress(previous.len(), shifts, dirty);
        self.delta_bytes += delta.byte_size();
        self.deltas.push_back(delta);
        while self.delta_bytes > self.budget_bytes {
            let Some(oldest) = self.deltas.pop_front() else {
                break;
            };
            self.delta_bytes -= oldest.byte_size();
        }
        Some(previous)
    }

    /// Removes and returns the newest state.
    pub fn pop(&mut self) -> Option<Vec<u8>> {
        let newest = self.newest.take()?;
        if let Some(delta) = self.deltas.pop_back() {
            self.delta_bytes -= delta.byte_size();
            let mut older = self.spare.take().unwrap_or_default();
            older.clear();
            older.resize(delta.len, 0);
            let ok = if delta.dirty_len == 0 {
                self.delta_buf.clear();
                true
            } else {
                self.delta_buf.resize(delta.dirty_len, 0);
                // Our own compressed data always decompresses.
                lz4_flex::block::decompress_into(
                    &delta.compressed,
                    &mut self.delta_buf[..delta.dirty_len],
                )
                .is_ok()
            };
            if ok {
                apply_dirty_xor(
                    &mut older,
                    &newest,
                    &delta.shifts,
                    &delta.dirty,
                    &self.delta_buf[..delta.dirty_len],
                );
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

    /// Compresses the dirty segment XORs in `self.delta_buf`.
    fn compress(&mut self, len: usize, shifts: Vec<Shift>, dirty: Vec<u64>) -> Delta {
        let dirty_len = self.delta_buf.len();
        let compressed = if dirty_len == 0 {
            Box::default()
        } else {
            let needed = lz4_flex::block::get_maximum_output_size(dirty_len);
            if self.compressed.len() < needed {
                self.compressed.resize(needed, 0);
            }
            let written = lz4_flex::block::compress_into(&self.delta_buf, &mut self.compressed)
                .expect("the buffer has room for the largest output");
            self.compressed[..written].into()
        };
        Delta {
            len,
            shifts: shifts.into(),
            dirty: dirty.into(),
            dirty_len,
            compressed,
        }
    }
}

impl Delta {
    fn byte_size(&self) -> usize {
        self.compressed.len() + self.dirty.len() * std::mem::size_of::<u64>()
    }
}

/// Where each segment of `older` went in `newer`: found by looking for
/// its first bytes near where the segment before it went. A segment whose
/// first bytes changed, or that is not found, is taken to have moved as
/// the one before it did. That costs only a larger delta if it is wrong,
/// since stepping back XORs at the same shifts.
fn line_up(older: &[u8], newer: &[u8]) -> Vec<Shift> {
    let mut shifts = Vec::new();
    let mut by = 0;
    let mut searches = SEARCHES;
    for (segment, start) in (0..older.len()).step_by(SEGMENT).enumerate() {
        let anchor = &older[start..older.len().min(start + ANCHOR)];
        let expected = start.checked_add_signed(by);
        let there = expected.and_then(|at| newer.get(at..at + anchor.len()));
        if there == Some(anchor) || searches == 0 {
            continue;
        }
        // A run of one byte value would match anywhere.
        if anchor.iter().all(|&byte| byte == anchor[0]) {
            continue;
        }
        searches -= 1;
        let near = expected.unwrap_or(0);
        let low = near.saturating_sub(REACH);
        let high = newer.len().min(near.saturating_add(REACH + anchor.len()));
        let Some(found) = newer
            .get(low..high)
            .and_then(|window| memchr::memmem::find(window, anchor))
        else {
            continue;
        };
        // Both are below isize::MAX, as they index slices.
        let moved = (low + found) as isize - start as isize;
        if moved != by {
            by = moved;
            shifts.push(Shift { from: segment, by });
        }
    }
    shifts
}

/// Appends the XOR of every changed segment of `older` with its shifted
/// position in `newer` into `out`, skipping segments whose bytes are
/// identical. Returns the bitset of changed segments.
fn collect_dirty_xor(older: &[u8], newer: &[u8], shifts: &[Shift], out: &mut Vec<u8>) -> Vec<u64> {
    let segment_count = older.len().div_ceil(SEGMENT);
    let mut dirty = vec![0u64; segment_count.div_ceil(64)];
    let mut shifts = shifts.iter().peekable();
    let mut by = 0isize;
    for (segment, chunk) in older.chunks(SEGMENT).enumerate() {
        if let Some(shift) = shifts.next_if(|shift| shift.from == segment) {
            by = shift.by;
        }
        let start = segment * SEGMENT;
        let skip = if by < 0 {
            by.unsigned_abs().saturating_sub(start)
        } else {
            0
        };
        let other = (start + skip)
            .checked_add_signed(by)
            .and_then(|from| newer.get(from..));
        if skip == 0
            && let Some(other) = other
            && other.len() >= chunk.len()
            && chunk == &other[..chunk.len()]
        {
            continue;
        }
        dirty[segment / 64] |= 1u64 << (segment % 64);
        let base = out.len();
        out.extend_from_slice(chunk);
        if let Some(other) = other
            && let Some(dst) = out.get_mut(base + skip..)
        {
            xor_into(dst, other);
        }
    }
    dirty
}

/// Rebuilds `older` from `newer`, `shifts`, the `dirty` segment bitset,
/// and the concatenated dirty segment XORs in `dirty_data`.
fn apply_dirty_xor(
    older: &mut [u8],
    newer: &[u8],
    shifts: &[Shift],
    dirty: &[u64],
    dirty_data: &[u8],
) {
    let mut shifts = shifts.iter().peekable();
    let mut by = 0isize;
    let mut cursor = 0usize;
    for (segment, chunk) in older.chunks_mut(SEGMENT).enumerate() {
        if let Some(shift) = shifts.next_if(|shift| shift.from == segment) {
            by = shift.by;
        }
        let start = segment * SEGMENT;
        let is_dirty = (dirty[segment / 64] >> (segment % 64)) & 1 != 0;
        if !is_dirty {
            if let Some(from) = start.checked_add_signed(by)
                && let Some(src) = newer.get(from..from + chunk.len())
            {
                chunk.copy_from_slice(src);
            }
            continue;
        }
        let end = cursor + chunk.len();
        chunk.copy_from_slice(&dirty_data[cursor..end]);
        cursor = end;
        let skip = if by < 0 {
            by.unsigned_abs().saturating_sub(start)
        } else {
            0
        };
        let Some(from) = (start + skip).checked_add_signed(by) else {
            continue;
        };
        if let (Some(dst), Some(other)) = (chunk.get_mut(skip..), newer.get(from..)) {
            xor_into(dst, other);
        }
    }
}

/// XORs `other` into `bytes`, as far as both go. Since XOR is its own
/// inverse, applying the same `other` twice gives back `bytes` even when
/// the lengths differ.
///
/// Slicing both to their common length lets the compiler vectorize cleanly.
fn xor_into(bytes: &mut [u8], other: &[u8]) {
    let len = bytes.len().min(other.len());
    for (byte, other) in bytes[..len].iter_mut().zip(&other[..len]) {
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

    /// How many states `rewind` steps back through, taking them all.
    fn steps_back(rewind: &mut Rewind) -> usize {
        std::iter::from_fn(|| rewind.pop()).count()
    }

    #[test]
    fn deltas_are_much_smaller_than_states() {
        // A twentieth of the states' size keeps them all.
        let mut rewind = Rewind::new(100 * 4096 / 20);
        for frame in 0..100 {
            rewind.push(state(frame));
        }
        assert_eq!(steps_back(&mut rewind), 100);
    }

    #[test]
    fn budget_drops_the_oldest_states() {
        let mut rewind = Rewind::new(200);
        for frame in 0..100 {
            rewind.push(state(frame));
        }
        let kept: Vec<Vec<u8>> = std::iter::from_fn(|| rewind.pop()).collect();
        assert!(kept.len() < 100, "{} kept", kept.len());
        let newest: Vec<Vec<u8>> = (0..100).rev().take(kept.len()).map(state).collect();
        assert_eq!(kept, newest);
    }

    #[test]
    fn deltas_stay_small_when_a_field_grows() {
        // A twentieth of the states' size keeps them all.
        let mut rewind = Rewind::new(100 * 65536 / 20);
        for frame in 0..100u8 {
            // A field near the start takes one byte more every few frames,
            // moving the 64 KB after it, as a varint register does.
            let mut state = vec![frame; usize::from(frame / 8) + 1];
            state.extend((0..65536u32).map(|index| (index * 7 % 251) as u8));
            state[40000] = frame;
            rewind.push(state);
        }
        assert_eq!(steps_back(&mut rewind), 100);
    }

    #[test]
    fn shifted_states_come_back_as_they_were() {
        let mut states = Vec::new();
        let mut rewind = Rewind::new(1 << 24);
        for frame in 0..40u32 {
            // Grows, shrinks and changes in places, with long plain runs
            // and stretches that repeat.
            let mut state = vec![frame as u8; (frame % 7) as usize];
            state.extend((0..20000u32).map(|index| (index % 97) as u8));
            state.extend(vec![0; (frame * 300) as usize % 9000]);
            state.extend((0..30000u32).map(|index| (index.wrapping_mul(frame + 3) >> 3) as u8));
            states.push(state.clone());
            rewind.push(state);
        }
        for state in states.iter().rev() {
            assert_eq!(rewind.pop().as_ref(), Some(state));
        }
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
