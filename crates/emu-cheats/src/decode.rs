//! Turns a flat format's codes into operations, one code at a time.

use crate::codes::Item;
use crate::ops::Op;

/// One flat format's reader. It keeps whatever state spans codes, such as
/// a multi-line code in progress.
pub(crate) trait Decoder {
    /// Reads the next code, appending its operation, if it has one.
    fn code(&mut self, code: u64, ops: &mut Vec<Op>) -> Result<(), String>;

    /// What an unfinished multi-line code still needs, if one is open.
    fn waiting_for(&self) -> Option<&'static str> {
        None
    }

    /// Ends the cheat, closing anything still open.
    fn finish(&mut self, ops: &mut Vec<Op>) {
        let _ = ops;
    }
}

/// Runs `items` through `decoder`; raw patches go straight through.
///
/// # Errors
///
/// Returns the line and reason for the first code the decoder rejects, a
/// patch inside a multi-line code, or a multi-line code cut off by the end.
pub(crate) fn decode(
    items: &[(usize, Item)],
    decoder: &mut impl Decoder,
) -> Result<Vec<Op>, (usize, String)> {
    let mut ops = Vec::new();
    for (line, item) in items {
        match item {
            Item::Patch(op) => {
                if let Some(what) = decoder.waiting_for() {
                    return Err((*line, format!("expected {what} before this patch")));
                }
                ops.push(op.clone());
            }
            Item::Code(code) => decoder.code(*code, &mut ops).map_err(|e| (*line, e))?,
        }
    }
    if let Some(what) = decoder.waiting_for() {
        let last = items.last().map_or(0, |(line, _)| *line);
        return Err((last, format!("the cheat ends before {what}")));
    }
    decoder.finish(&mut ops);
    Ok(ops)
}

/// A code's two 32-bit halves.
pub(crate) fn halves(code: u64) -> (u32, u32) {
    ((code >> 32) as u32, code as u32)
}
