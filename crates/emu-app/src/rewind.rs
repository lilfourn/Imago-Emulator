//! A bounded history of save states for rewinding.
//!
//! The newest state is kept whole. Each older state is stored as the XOR
//! of itself with the state after it, LZ4-compressed: consecutive frames
//! differ in few bytes, so deltas compress to a small fraction of a full
//! state. Stepping back decompresses one delta and XORs it into the newest
//! state. When the history exceeds its byte budget, the oldest deltas are
//! dropped.

use std::collections::VecDeque;

/// Save states, newest first, within a memory budget.
#[derive(Debug)]
pub struct Rewind {
    newest: Option<Vec<u8>>,
    /// Compressed deltas, oldest at the front.
    deltas: VecDeque<Vec<u8>>,
    delta_bytes: usize,
    budget_bytes: usize,
}

impl Rewind {
    /// An empty history that keeps at most `budget_bytes` of deltas.
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            newest: None,
            deltas: VecDeque::new(),
            delta_bytes: 0,
            budget_bytes,
        }
    }

    /// Records `state` as the newest.
    pub fn push(&mut self, state: &[u8]) {
        if let Some(previous) = self.newest.replace(state.to_vec()) {
            let delta = lz4_flex::compress_prepend_size(&xor(&previous, state));
            self.delta_bytes += delta.len();
            self.deltas.push_back(delta);
            while self.delta_bytes > self.budget_bytes {
                let Some(oldest) = self.deltas.pop_front() else {
                    break;
                };
                self.delta_bytes -= oldest.len();
            }
        }
    }

    /// Removes and returns the newest state.
    pub fn pop(&mut self) -> Option<Vec<u8>> {
        let newest = self.newest.take()?;
        if let Some(delta) = self.deltas.pop_back() {
            self.delta_bytes -= delta.len();
            // Our own compressed data always decompresses.
            if let Ok(delta) = lz4_flex::decompress_size_prepended(&delta) {
                self.newest = Some(xor(&delta, &newest));
            }
        }
        Some(newest)
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
}

/// `older` XOR `newer`, sized like `older`. Since XOR is its own inverse,
/// `xor(&xor(older, newer), newer) == older` even when lengths differ.
fn xor(older: &[u8], newer: &[u8]) -> Vec<u8> {
    older
        .iter()
        .enumerate()
        .map(|(index, byte)| byte ^ newer.get(index).copied().unwrap_or(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
            rewind.push(&state(frame));
        }
        for frame in (0..10).rev() {
            assert_eq!(rewind.pop(), Some(state(frame)));
        }
        assert_eq!(rewind.pop(), None);
    }

    #[test]
    fn deltas_are_much_smaller_than_states() {
        let mut rewind = Rewind::new(1 << 20);
        for frame in 0..100 {
            rewind.push(&state(frame));
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
            rewind.push(&state(frame));
        }
        assert!(rewind.len() < 100);
        assert_eq!(rewind.pop(), Some(state(99)));
    }

    #[test]
    fn handles_states_of_different_lengths() {
        let mut rewind = Rewind::new(1 << 20);
        rewind.push(&[1, 2, 3]);
        rewind.push(&[9; 6]);
        rewind.push(&[4]);
        assert_eq!(rewind.pop(), Some(vec![4]));
        assert_eq!(rewind.pop(), Some(vec![9; 6]));
        assert_eq!(rewind.pop(), Some(vec![1, 2, 3]));
    }
}
