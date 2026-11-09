//! Test support for the cores: loading states that are wrong in one
//! place.
//!
//! A core must take any bytes handed to `load_state` without a panic, a
//! hang or an allocation out of proportion: it refuses them, or it loads
//! them and runs. [`try_each`] makes wrong states from a good machine and
//! counts what a core does with them. What it changes is above all the
//! state's structure, the lengths, numbers, flags and enum tags that a
//! core's checks are about:
//!
//! - each byte outside the long byte strings, changed in up to five ways;
//! - each number, replaced by the largest 16-, 32- and 64-bit numbers,
//!   which finds the sums that overflow.
//!
//! A memory holds whatever a game put there, so of each byte string only
//! a few bytes spread through it are changed. A long list of numbers,
//! such as a screen's pixels, is a memory in all but how it is written:
//! its ends and a few of the numbers between are changed.
//!
//! Each core's tests call it with a machine of their own: a sample of the
//! places in every test run, and all of them, for several machines at
//! once ([`sweep`]), in a test that is run by hand.

use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::Serialize;

/// How long one wrong state may take to load and run before the test run
/// is stopped as hung.
const HANG: Duration = Duration::from_secs(60);

/// What a core did with the wrong states [`try_each`] handed it.
#[derive(Debug, Default)]
pub struct Outcome {
    /// How many states were tried.
    pub tried: usize,
    /// The places in the core that panicked, each with the first state
    /// that got there.
    pub panics: Vec<Panic>,
}

/// One place a wrong state made the core panic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Panic {
    /// The source location, then the first such state's message.
    pub what: String,
    /// The offset in the payload where the first such state differs.
    pub offset: usize,
    /// The bytes it has there instead.
    pub bytes: Vec<u8>,
    /// How many states panicked here.
    pub states: usize,
}

impl Panic {
    /// The source location: `what` up to its message. Panics are counted
    /// by it, since a message's numbers differ from state to state.
    fn place(&self) -> &str {
        self.what.split(": ").next().unwrap_or(&self.what)
    }

    fn describe(&self) -> String {
        format!(
            "{} (first with {:02X?} at offset {}, {} states)",
            self.what, self.bytes, self.offset, self.states
        )
    }
}

impl Outcome {
    /// Adds `other`'s states and panics to these, after saying on stderr
    /// what `other`, the outcome for the machine called `name`, came to:
    /// a sweep of several machines says how each went as it goes, and
    /// fails once at its end with every panic.
    pub fn add(&mut self, name: &str, other: Self) {
        eprintln!(
            "{name}: {} wrong states, {} panicked in {} places",
            other.tried,
            other.panics.iter().map(|panic| panic.states).sum::<usize>(),
            other.panics.len()
        );
        for panic in &other.panics {
            eprintln!("  {}", panic.describe());
        }
        self.tried += other.tried;
        for panic in other.panics {
            match self
                .panics
                .iter_mut()
                .find(|known| known.place() == panic.place())
            {
                Some(known) => known.states += panic.states,
                None => self.panics.push(panic),
            }
        }
    }

    /// Fails the test, listing the panics, if there were any.
    ///
    /// # Panics
    ///
    /// Panics if a state made the core panic.
    pub fn assert_no_panics(&self) {
        let list: Vec<String> = self.panics.iter().map(Panic::describe).collect();
        assert!(
            list.is_empty(),
            "{} of {} wrong states made the core panic, in {} places:\n{}",
            self.panics.iter().map(|panic| panic.states).sum::<usize>(),
            self.tried,
            list.len(),
            list.join("\n")
        );
    }
}

/// Runs each of `machines`, a name and what tries its wrong states, on a
/// thread of its own, and adds the outcomes up ([`Outcome::add`]): a
/// sweep of several machines takes as long as its slowest.
///
/// # Panics
///
/// Panics if one of `machines` panics outside the states it tries.
#[must_use]
pub fn sweep(machines: Vec<(String, Box<dyn FnOnce() -> Outcome + Send + '_>)>) -> Outcome {
    let outcomes: Vec<(String, Outcome)> = thread::scope(|scope| {
        let running: Vec<_> = machines
            .into_iter()
            .map(|(name, tries)| scope.spawn(move || (name, tries())))
            .collect();
        running
            .into_iter()
            .map(|machine| machine.join().expect("the machine's own test passes"))
            .collect()
    });
    let mut all = Outcome::default();
    for (name, outcome) in outcomes {
        all.add(&name, outcome);
    }
    all
}

/// The bytes a byte is changed to: all zeros and all ones, the two that
/// turn a length's last byte into the middle of a longer one or back, and
/// its neighbours, which put an index one past its range.
fn changes(byte: u8) -> impl Iterator<Item = u8> {
    let candidates = [
        0x00,
        0xFF,
        byte ^ 0x80,
        byte.wrapping_add(1),
        byte.wrapping_sub(1),
    ];
    (0..candidates.len())
        .filter(move |&index| {
            candidates[index] != byte && !candidates[..index].contains(&candidates[index])
        })
        .map(move |index| candidates[index])
}

/// The largest 16-, 32- and 64-bit numbers as postcard writes them. One
/// in the place of a smaller number of the same width reads as that
/// number at its largest; a wider one is refused as too wide.
const LARGEST: [&[u8]; 3] = [
    &[0xFF, 0xFF, 0x03],
    &[0xFF, 0xFF, 0xFF, 0xFF, 0x0F],
    &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01],
];

/// A machine's payload, and where its structure is.
#[derive(Default)]
struct Structure {
    payload: Vec<u8>,
    /// What postcard wrote outside long byte strings, in order.
    pieces: Vec<Piece>,
}

/// Bytes postcard wrote in one go: a lone byte (a `u8`, a flag, an
/// `Option`'s tag) or a piece that may be a number (every number wider
/// than a byte, an enum's tag, a length, a short string).
#[derive(Debug, Clone, Copy)]
struct Piece {
    offset: usize,
    length: usize,
    number: bool,
}

impl postcard::ser_flavors::Flavor for Structure {
    type Output = Self;

    fn try_push(&mut self, data: u8) -> postcard::Result<()> {
        self.pieces.push(Piece {
            offset: self.payload.len(),
            length: 1,
            number: false,
        });
        self.payload.push(data);
        Ok(())
    }

    fn try_extend(&mut self, data: &[u8]) -> postcard::Result<()> {
        /// The longest piece that is a number: a 128-bit one.
        const LONGEST_NUMBER: usize = 19;
        let at = self.payload.len();
        if data.len() <= LONGEST_NUMBER {
            self.pieces.push(Piece {
                offset: at,
                length: data.len(),
                number: true,
            });
        } else {
            // Postcard writes a byte string's bytes in one piece: a
            // memory, of which a few bytes spread through it stand for
            // all.
            let step = data.len().div_ceil(Self::OF_A_MEMORY);
            self.pieces
                .extend((0..data.len()).step_by(step).map(|offset| Piece {
                    offset: at + offset,
                    length: 1,
                    number: false,
                }));
        }
        self.payload.extend_from_slice(data);
        Ok(())
    }

    fn finalize(mut self) -> postcard::Result<Self> {
        self.thin_long_lists();
        Ok(self)
    }
}

impl Structure {
    /// More lone bytes than this, or more numbers than that, written one
    /// straight after another are a list: no machine has so many fields
    /// of a kind in a row.
    const LONG_BYTES: usize = 256;
    const LONG_NUMBERS: usize = 64;
    /// How many of a long list's pieces stay at each end, and about how
    /// many between.
    const ENDS: usize = 8;
    const BETWEEN: usize = 16;
    /// How many of a byte string's bytes stand for it.
    const OF_A_MEMORY: usize = 48;

    /// Leaves out most of each long list, of numbers or of bytes not
    /// written as a byte string: its first and last [`Self::ENDS`] pieces
    /// stay (a list's own length comes first, and what follows a list may
    /// be an index into it), and about [`Self::BETWEEN`] spread between.
    fn thin_long_lists(&mut self) {
        let mut kept = Vec::with_capacity(self.pieces.len());
        let mut start = 0;
        while start < self.pieces.len() {
            let number = self.pieces[start].number;
            let mut end = start + 1;
            while end < self.pieces.len() && self.pieces[end].number == number && {
                let before = self.pieces[end - 1];
                self.pieces[end].offset == before.offset + before.length
            } {
                end += 1;
            }
            let list = &self.pieces[start..end];
            let long = if number {
                Self::LONG_NUMBERS
            } else {
                Self::LONG_BYTES
            };
            if list.len() <= long {
                kept.extend_from_slice(list);
            } else {
                let step = list.len() / Self::BETWEEN;
                kept.extend(list.iter().enumerate().filter_map(|(index, piece)| {
                    let at_an_end = index < Self::ENDS || index + Self::ENDS >= list.len();
                    (at_an_end || index % step == 0).then_some(*piece)
                }));
            }
            start = end;
        }
        self.pieces = kept;
    }
}

/// Makes wrong states from `machine`'s own, taking every `every`th byte
/// and number of its structure (1 for all of them), and hands each to
/// `load_and_run`, which loads it into a core and, if the core takes it,
/// runs the core: a frame or two, a save, a reset.
///
/// A panic in `load_and_run` is caught and counted, so one run finds
/// them all. A call that has not returned after a minute is a hang: the
/// whole test process stops there, saying which state it was, since a
/// hung thread cannot be stopped any other way.
///
/// # Panics
///
/// Panics if `machine` cannot be serialized or `every` is 0.
pub fn try_each<T: Serialize + ?Sized>(
    machine: &T,
    every: usize,
    mut load_and_run: impl FnMut(&[u8]),
) -> Outcome {
    let structure = postcard::serialize_with_flavor(machine, Structure::default())
        .expect("machine state always serializes");
    let payload = &structure.payload;
    // Each wrong state: where it differs, and the bytes it has there in
    // place of `replaced` of the good one's.
    let changed_bytes = structure
        .pieces
        .iter()
        .flat_map(|piece| piece.offset..piece.offset + piece.length)
        .step_by(every)
        .flat_map(|offset| changes(payload[offset]).map(move |byte| (offset, 1, vec![byte])));
    let largest_numbers = structure
        .pieces
        .iter()
        .filter(|piece| piece.number)
        .step_by(every)
        .flat_map(|&Piece { offset, length, .. }| {
            LARGEST
                .iter()
                .filter(move |largest| ***largest != payload[offset..offset + length])
                .map(move |largest| (offset, length, largest.to_vec()))
        });

    // Where the panic hook leaves what it saw of a panic raised while
    // this thread is in `load_and_run`.
    thread_local! {
        static TRYING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        static LAST: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    }
    // Other tests' panics go to the hook that was there. Hooks are for
    // the whole process, so this one stays for good, harmlessly: it is
    // installed once, and passes on what is not this function's.
    static HOOKED: Mutex<bool> = Mutex::new(false);
    {
        let mut hooked = HOOKED
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !*hooked {
            *hooked = true;
            let previous = panic::take_hook();
            panic::set_hook(Box::new(move |info| {
                if !TRYING.with(std::cell::Cell::get) {
                    return previous(info);
                }
                let place = info.location().map_or_else(
                    || "an unknown place".to_owned(),
                    |location| format!("{}:{}", location.file(), location.line()),
                );
                let payload = info.payload();
                let message = payload
                    .downcast_ref::<&str>()
                    .map(|message| (*message).to_owned())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_default();
                // The place says which bug; the message's first words are
                // kept to go with it.
                let message: String = message.chars().take(60).collect();
                LAST.with(|last| *last.borrow_mut() = Some(format!("{place}: {message}")));
            }));
        }
    }

    // The watchdog: `progress` counts the states finished, and `at` says
    // where the one under way differs.
    let progress = Arc::new(AtomicU64::new(0));
    let at = Arc::new(AtomicU64::new(0));
    let done = Arc::new(AtomicBool::new(false));
    let watchdog = {
        let (progress, at, done) = (progress.clone(), at.clone(), done.clone());
        thread::spawn(move || {
            let mut seen = u64::MAX;
            let mut waited = Duration::ZERO;
            let nap = Duration::from_millis(250);
            while !done.load(Ordering::Relaxed) {
                thread::sleep(nap);
                let now = progress.load(Ordering::Relaxed);
                if now != seen {
                    seen = now;
                    waited = Duration::ZERO;
                    continue;
                }
                waited += nap;
                if waited >= HANG {
                    let at = at.load(Ordering::Relaxed);
                    eprintln!(
                        "a wrong state hung the core: the one that differs at offset {} \
                         (its first byte there {:#04X})",
                        at >> 8,
                        at & 0xFF
                    );
                    std::process::abort();
                }
            }
        })
    };

    let mut outcome = Outcome::default();
    let mut wrong = Vec::with_capacity(payload.len() + 16);
    for (offset, replaced, bytes) in changed_bytes.chain(largest_numbers) {
        wrong.clear();
        wrong.extend_from_slice(&payload[..offset]);
        wrong.extend_from_slice(&bytes);
        wrong.extend_from_slice(&payload[offset + replaced..]);
        at.store(
            (offset as u64) << 8 | u64::from(bytes[0]),
            Ordering::Relaxed,
        );
        TRYING.with(|trying| trying.set(true));
        let result = panic::catch_unwind(AssertUnwindSafe(|| load_and_run(&wrong)));
        TRYING.with(|trying| trying.set(false));
        progress.fetch_add(1, Ordering::Relaxed);
        outcome.tried += 1;
        if result.is_err() {
            let what = LAST
                .with(|last| last.borrow_mut().take())
                .unwrap_or_else(|| "a panic the hook did not see".to_owned());
            let place = what.split(": ").next().unwrap_or(&what);
            match outcome
                .panics
                .iter_mut()
                .find(|panic| panic.place() == place)
            {
                Some(panic) => panic.states += 1,
                None => {
                    let panic = Panic {
                        what,
                        offset,
                        bytes,
                        states: 1,
                    };
                    // Said at once: a sweep takes its time to finish.
                    eprintln!("a wrong state panicked: {}", panic.describe());
                    outcome.panics.push(panic);
                }
            }
        }
    }
    done.store(true, Ordering::Relaxed);
    let _ = watchdog.join();
    outcome
}

#[cfg(test)]
mod tests {
    use serde::Serialize;

    use super::*;

    #[test]
    fn a_byte_is_changed_to_each_other_value_once() {
        assert_eq!(changes(0x00).collect::<Vec<_>>(), [0xFF, 0x80, 0x01]);
        assert_eq!(changes(0xFF).collect::<Vec<_>>(), [0x00, 0x7F, 0xFE]);
        assert_eq!(
            changes(0x12).collect::<Vec<_>>(),
            [0x00, 0xFF, 0x92, 0x13, 0x11]
        );
        assert_eq!(changes(0x01).collect::<Vec<_>>(), [0x00, 0xFF, 0x81, 0x02]);
    }

    #[derive(Serialize)]
    struct Machine {
        flag: bool,
        #[serde(with = "crate::payload::bytes")]
        memory: Vec<u8>,
        count: u32,
        small: u8,
    }

    const MACHINE: fn() -> Machine = || Machine {
        flag: true,
        memory: vec![0xEE; 100],
        count: 300,
        small: 7,
    };

    #[test]
    fn the_structure_is_changed_and_a_little_of_the_memory() {
        // The payload: the flag, the memory's length, its 100 bytes, the
        // count in two bytes, the small number.
        let good = postcard::to_stdvec(&MACHINE()).expect("serializes");
        assert_eq!(good.len(), 105);
        let mut seen = Vec::new();
        let outcome = try_each(&MACHINE(), 1, |wrong| seen.push(wrong.to_vec()));
        assert_eq!(outcome.tried, seen.len());
        outcome.assert_no_panics();
        assert!(seen.iter().all(|wrong| wrong != &good));
        // The count at its largest, in place of its two bytes.
        let mut largest = good.clone();
        largest.splice(102..104, [0xFF, 0xFF, 0xFF, 0xFF, 0x0F]);
        assert!(seen.contains(&largest));
        // Every other place of the structure, a byte at a time.
        let mut flag = good.clone();
        flag[0] = 0xFF;
        let mut small = good.clone();
        small[104] = 8;
        assert!(seen.contains(&flag) && seen.contains(&small));
        // Of the memory, every third byte or so.
        let memory = changed_offsets(&MACHINE());
        let of_the_memory = memory.range(2..102).count();
        assert!((30..=50).contains(&of_the_memory), "{of_the_memory}");
        // Every second place is about half of them.
        let mut sampled = 0;
        try_each(&MACHINE(), 2, |_| sampled += 1);
        assert!(sampled < seen.len() * 2 / 3, "{sampled} of {}", seen.len());
    }

    /// Which of `value`'s bytes a state differs in, over all the wrong
    /// states made from it.
    fn changed_offsets<T: Serialize>(value: &T) -> std::collections::BTreeSet<usize> {
        let good = postcard::to_stdvec(value).expect("serializes");
        let mut changed = std::collections::BTreeSet::new();
        try_each(value, 1, |wrong| {
            let at = wrong.iter().zip(&good).position(|(a, b)| a != b);
            changed.insert(at.expect("differs"));
        });
        changed
    }

    #[test]
    fn a_long_list_is_changed_at_its_ends_and_here_and_there() {
        // 1,000 pixels of five bytes each after their two-byte length,
        // then a number that is not part of the list but follows it.
        let screen = (vec![0xFF00_0000u32; 1000], 300u16);
        let changed = changed_offsets(&screen);
        // The length and the first seven pixels; the last seven and the
        // number after them.
        assert!((0..37).all(|offset| changed.contains(&offset)));
        assert!((4967..5004).all(|offset| changed.contains(&offset)));
        // About sixteen pixels between, of the 986.
        let between = changed.range(37..4967).count() / 5;
        assert!((14..=18).contains(&between), "{between} pixels between");

        // Bytes written one at a time, as a memory without
        // `payload::bytes` is, go much the same way.
        let memory = (vec![0xEEu8; 1000], 7u8);
        let changed = changed_offsets(&memory);
        assert!(changed.contains(&0) && changed.contains(&1002));
        assert!(changed.len() < 40, "{} bytes", changed.len());

        // A hundred flags in a row are a struct's fields, all changed.
        let flags = [[true; 25]; 4];
        assert_eq!(changed_offsets(&flags).len(), 100);
    }

    #[test]
    fn panics_are_counted_by_place() {
        let outcome = try_each(&MACHINE(), 1, |wrong| {
            assert!(wrong[0] != 0xFF, "an index out of range");
        });
        let [panic] = &outcome.panics[..] else {
            panic!("one place: {:?}", outcome.panics);
        };
        assert!(panic.what.contains("mutation.rs"), "{}", panic.what);
        assert!(panic.what.contains("an index out of range"));
        assert_eq!((panic.offset, &panic.bytes[..]), (0, &[0xFF][..]));
        assert_eq!(panic.states, 1);
        let failed = panic::catch_unwind(|| outcome.assert_no_panics());
        assert!(failed.is_err());
        Outcome::default().assert_no_panics();
    }
}
