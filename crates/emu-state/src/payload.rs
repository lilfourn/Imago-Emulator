//! The payload: a core's own state bytes, written and read back with
//! [postcard](https://docs.rs/postcard), and checked on the way in.
//!
//! A state file is input like any other. Its checksum catches a damaged
//! file, not one made to be wrong, so whatever a core later uses as a
//! length or an index has to be checked as the state is read, and a state
//! that fails a check is refused with a [`StateError`] rather than
//! installed. A core's machine says what it holds in its fields:
//!
//! - [`mod@bytes`] on a byte buffer whose length varies with the game
//!   (cartridge RAM, a save chip), which the core then checks is one its
//!   kind comes in;
//! - [`Exact`] on a buffer of a fixed length: `#[serde(with =
//!   "payload::Exact::<0x8000>")]`;
//! - [`Below`] on a number used as an index or a count:
//!   `#[serde(with = "payload::Below::<8>")]`;
//! - [`AtMost`] on a list that only ever holds a few entries.
//!
//! What no one field can say, a core asks of the machine once it is read,
//! with [`check`]: that parts agree with each other, and that clocks and
//! counts are ones a loop can catch up on. Where a value is only data, a
//! running total or a clock, the arithmetic on it is made total instead.
//!
//! [`load`] reads a machine and refuses bytes left over after it;
//! [`append`] writes one. None of the helpers changes the bytes: a buffer
//! is laid out as postcard lays out a sequence of bytes, a length and then
//! the bytes, and the checks only read.

use std::fmt;
use std::marker::PhantomData;

use emu_core::StateError;
use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Serializes `value` onto the end of `out`, laid out as postcard lays it
/// out. It goes straight into the caller's buffer: the app's rewind
/// snapshot reuses one each frame that already has room, where postcard's
/// own buffer would grow several times over, a whole state each frame, and
/// then be copied.
///
/// # Panics
///
/// Panics if `value` cannot be serialized, which a machine's plain data
/// always can.
pub fn append<T: Serialize + ?Sized>(value: &T, out: &mut Vec<u8>) {
    postcard::serialize_with_flavor(value, Append(out)).expect("machine state always serializes");
}

/// A postcard output that appends to a `Vec` it borrows.
struct Append<'a>(&'a mut Vec<u8>);

impl postcard::ser_flavors::Flavor for Append<'_> {
    type Output = ();

    fn try_push(&mut self, data: u8) -> postcard::Result<()> {
        self.0.push(data);
        Ok(())
    }

    fn try_extend(&mut self, data: &[u8]) -> postcard::Result<()> {
        self.0.extend_from_slice(data);
        Ok(())
    }

    fn finalize(self) -> postcard::Result<()> {
        Ok(())
    }
}

/// Reads a `T` that takes up the whole of `bytes`.
///
/// Bytes left over are as wrong as bytes missing: a linked pair's state
/// begins with one console's, and used to load into a console on its own.
///
/// # Errors
///
/// Returns [`StateError::Malformed`] with `not` (such as "not a Game Boy
/// state") if the bytes are not a `T`, fail one of its fields' checks, or
/// go on after it.
pub fn load<'a, T: Deserialize<'a>>(bytes: &'a [u8], not: &'static str) -> Result<T, StateError> {
    match postcard::take_from_bytes(bytes) {
        Ok((value, [])) => Ok(value),
        _ => Err(StateError::Malformed(not)),
    }
}

/// `Ok` if `sound`, else [`StateError::Malformed`] with `not`: for the
/// checks a core makes on a machine once it is read, which no one field
/// can make alone.
///
/// # Errors
///
/// Returns the error if `sound` is false.
pub fn check(sound: bool, not: &'static str) -> Result<(), StateError> {
    if sound {
        Ok(())
    } else {
        Err(StateError::Malformed(not))
    }
}

/// Byte buffers written as one byte string rather than a byte at a time:
/// `#[serde(with = "payload::bytes")]` on a `Box<[u8]>` or a `Vec<u8>`,
/// and [`array`](bytes::array) on an array of them.
///
/// Postcard lays out a byte string exactly as it does a sequence of bytes,
/// so states keep their format and only save and load faster. The DS's
/// main RAM alone is 4 MiB. The length is whatever the state says: use
/// [`Exact`](super::Exact) where it is fixed.
pub mod bytes {
    use serde::{Deserializer, Serializer};

    use super::ByteBuf;

    /// Writes `bytes` as one byte string.
    ///
    /// # Errors
    ///
    /// Returns the serializer's error.
    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(bytes)
    }

    /// Reads a byte string (or a sequence of bytes) into a `Box<[u8]>` or
    /// a `Vec<u8>`.
    ///
    /// # Errors
    ///
    /// Returns the deserializer's error.
    pub fn deserialize<'de, D, T>(deserializer: D) -> Result<T, D::Error>
    where
        D: Deserializer<'de>,
        T: From<Vec<u8>>,
    {
        deserializer
            .deserialize_byte_buf(ByteBuf { exactly: None })
            .map(T::from)
    }

    /// Arrays of byte buffers, each a byte string of any length.
    pub mod array {
        use std::fmt;

        use serde::de::{self, SeqAccess, Visitor};
        use serde::ser::SerializeTuple;
        use serde::{Deserialize, Deserializer, Serialize, Serializer};

        /// Writes each of `buffers` as a byte string.
        ///
        /// # Errors
        ///
        /// Returns the serializer's error.
        pub fn serialize<S: Serializer, const N: usize>(
            buffers: &[Box<[u8]>; N],
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            let mut tuple = serializer.serialize_tuple(N)?;
            for buffer in buffers {
                tuple.serialize_element(&Bytes(buffer))?;
            }
            tuple.end()
        }

        /// Reads `N` byte strings.
        ///
        /// # Errors
        ///
        /// Returns the deserializer's error.
        pub fn deserialize<'de, D: Deserializer<'de>, const N: usize>(
            deserializer: D,
        ) -> Result<[Box<[u8]>; N], D::Error> {
            struct Buffers<const N: usize>;

            impl<'de, const N: usize> Visitor<'de> for Buffers<N> {
                type Value = [Box<[u8]>; N];

                fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                    write!(formatter, "{N} byte strings")
                }

                fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                    let mut buffers = Vec::with_capacity(N);
                    for index in 0..N {
                        let OwnedBytes(buffer) = seq
                            .next_element()?
                            .ok_or_else(|| de::Error::invalid_length(index, &self))?;
                        buffers.push(buffer);
                    }
                    Ok(buffers.try_into().expect("N buffers"))
                }
            }

            deserializer.deserialize_tuple(N, Buffers::<N>)
        }

        /// A borrowed byte buffer that serializes as a byte string.
        struct Bytes<'a>(&'a [u8]);

        impl Serialize for Bytes<'_> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_bytes(self.0)
            }
        }

        /// A byte buffer read from a byte string.
        struct OwnedBytes(Box<[u8]>);

        impl<'de> Deserialize<'de> for OwnedBytes {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                super::deserialize(deserializer).map(Self)
            }
        }
    }
}

/// A byte buffer of exactly `N` bytes, a byte string in the state:
/// `#[serde(with = "payload::Exact::<N>")]` on a `Box<[u8]>` or a
/// `Vec<u8>`. A state that gives it another length is refused.
pub struct Exact<const N: usize>;

impl<const N: usize> Exact<N> {
    /// Writes `bytes` as one byte string.
    ///
    /// # Errors
    ///
    /// Returns the serializer's error.
    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(bytes)
    }

    /// Reads a byte string of `N` bytes.
    ///
    /// # Errors
    ///
    /// Returns the deserializer's error, or its own for another length.
    pub fn deserialize<'de, D, T>(deserializer: D) -> Result<T, D::Error>
    where
        D: Deserializer<'de>,
        T: From<Vec<u8>>,
    {
        deserializer
            .deserialize_byte_buf(ByteBuf { exactly: Some(N) })
            .map(T::from)
    }
}

/// A list of exactly `N` values that are not bytes, such as a screen's
/// pixels: `#[serde(with = "payload::ExactList::<N>")]` on a `Box<[T]>`
/// or a `Vec<T>`. A state that gives it another length is refused.
pub struct ExactList<const N: usize>;

impl<const N: usize> ExactList<N> {
    /// Writes `list` as serde writes a sequence.
    ///
    /// # Errors
    ///
    /// Returns the serializer's error.
    pub fn serialize<S: Serializer, T: Serialize>(
        list: &[T],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        list.serialize(serializer)
    }

    /// Reads a sequence of `N` values.
    ///
    /// # Errors
    ///
    /// Returns the deserializer's error, or its own for another length.
    pub fn deserialize<'de, D, T, L>(deserializer: D) -> Result<L, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de>,
        L: From<Vec<T>>,
    {
        deserializer
            .deserialize_seq(List {
                lengths: N..=N,
                of: PhantomData::<T>,
            })
            .map(L::from)
    }
}

/// A list of at most `N` values: `#[serde(with = "payload::AtMost::<N>")]`
/// on a `Vec<T>` or a `VecDeque<T>` that the machine itself never lets
/// grow past `N`. A state that gives it more is refused.
pub struct AtMost<const N: usize>;

impl<const N: usize> AtMost<N> {
    /// Writes `list` as serde writes it.
    ///
    /// # Errors
    ///
    /// Returns the serializer's error.
    pub fn serialize<S: Serializer, L: Serialize>(
        list: &L,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        list.serialize(serializer)
    }

    /// Reads a sequence of `N` values or fewer.
    ///
    /// # Errors
    ///
    /// Returns the deserializer's error, or its own for a longer one.
    pub fn deserialize<'de, D, T, L>(deserializer: D) -> Result<L, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de>,
        L: From<Vec<T>>,
    {
        deserializer
            .deserialize_seq(List {
                lengths: 0..=N,
                of: PhantomData::<T>,
            })
            .map(L::from)
    }
}

/// A number below `N`, for a field used as an index or a count:
/// `#[serde(with = "payload::Below::<N>")]` on any unsigned integer. A
/// state that gives it `N` or more is refused.
pub struct Below<const N: u64>;

impl<const N: u64> Below<N> {
    /// Writes `value` as serde writes it.
    ///
    /// # Errors
    ///
    /// Returns the serializer's error.
    pub fn serialize<S: Serializer, T: Serialize>(
        value: &T,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.serialize(serializer)
    }

    /// Reads a number below `N`.
    ///
    /// # Errors
    ///
    /// Returns the deserializer's error, or its own for a larger number.
    pub fn deserialize<'de, D, T>(deserializer: D) -> Result<T, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de> + Copy + TryInto<u64>,
    {
        let value = T::deserialize(deserializer)?;
        match value.try_into() {
            Ok(number) if number < N => Ok(value),
            _ => Err(de::Error::custom(format_args!("a number below {N}"))),
        }
    }
}

/// Reads a byte string, or a sequence of bytes from formats that write
/// one, of `exactly` so many bytes if that is given.
struct ByteBuf {
    exactly: Option<usize>,
}

impl ByteBuf {
    fn checked<E: de::Error>(&self, length: usize) -> Result<(), E> {
        match self.exactly {
            Some(exactly) if exactly != length => Err(E::invalid_length(length, self)),
            _ => Ok(()),
        }
    }
}

impl<'de> Visitor<'de> for ByteBuf {
    type Value = Vec<u8>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.exactly {
            Some(length) => write!(formatter, "a byte string of {length} bytes"),
            None => formatter.write_str("a byte string"),
        }
    }

    fn visit_bytes<E: de::Error>(self, bytes: &[u8]) -> Result<Vec<u8>, E> {
        self.checked(bytes.len())?;
        Ok(bytes.to_vec())
    }

    fn visit_byte_buf<E: de::Error>(self, bytes: Vec<u8>) -> Result<Vec<u8>, E> {
        self.checked(bytes.len())?;
        Ok(bytes)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<u8>, A::Error> {
        // Never more room than the length asked for, whatever the format
        // says is coming.
        let room = seq.size_hint().unwrap_or(0);
        let mut bytes = Vec::with_capacity(self.exactly.map_or(room, |exactly| room.min(exactly)));
        while let Some(byte) = seq.next_element()? {
            bytes.push(byte);
        }
        self.checked(bytes.len())?;
        Ok(bytes)
    }
}

/// Reads a sequence of `T` whose length is within `lengths`.
struct List<T> {
    lengths: std::ops::RangeInclusive<usize>,
    of: PhantomData<T>,
}

impl<'de, T: Deserialize<'de>> Visitor<'de> for List<T> {
    type Value = Vec<T>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (least, most) = (self.lengths.start(), self.lengths.end());
        if least == most {
            write!(formatter, "a list of {most}")
        } else {
            write!(formatter, "a list of {least} to {most}")
        }
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<T>, A::Error> {
        // The format says how many are coming, so a wrong count is refused
        // before any are read; a format that does not say is read to its
        // end, one past the most at the worst.
        if let Some(coming) = seq.size_hint()
            && !self.lengths.contains(&coming)
        {
            return Err(de::Error::invalid_length(coming, &self));
        }
        let most = *self.lengths.end();
        let mut list = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(most));
        while let Some(value) = seq.next_element()? {
            if list.len() == most {
                return Err(de::Error::invalid_length(most + 1, &self));
            }
            list.push(value);
        }
        if !self.lengths.contains(&list.len()) {
            return Err(de::Error::invalid_length(list.len(), &self));
        }
        Ok(list)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Plain {
        boxed: Box<[u8]>,
        vec: Vec<u8>,
        banks: [Box<[u8]>; 2],
        fixed: Box<[u8]>,
        pixels: Box<[u32]>,
        queue: VecDeque<u16>,
        index: u8,
        wide: usize,
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Checked {
        #[serde(with = "bytes")]
        boxed: Box<[u8]>,
        #[serde(with = "bytes")]
        vec: Vec<u8>,
        #[serde(with = "bytes::array")]
        banks: [Box<[u8]>; 2],
        #[serde(with = "Exact::<300>")]
        fixed: Box<[u8]>,
        #[serde(with = "ExactList::<3>")]
        pixels: Box<[u32]>,
        #[serde(with = "AtMost::<4>")]
        queue: VecDeque<u16>,
        #[serde(with = "Below::<8>")]
        index: u8,
        #[serde(with = "Below::<1000>")]
        wide: usize,
    }

    fn plain() -> Plain {
        Plain {
            boxed: (0..=255).collect(),
            vec: vec![1, 2, 3],
            banks: [vec![9; 300].into(), Box::default()],
            fixed: vec![7; 300].into(),
            pixels: vec![0xFF00_0000, 1, 0x1234_5678].into(),
            queue: VecDeque::from([1, 2, 300]),
            index: 7,
            wide: 999,
        }
    }

    fn checked(plain: &Plain) -> Checked {
        Checked {
            boxed: plain.boxed.clone(),
            vec: plain.vec.clone(),
            banks: plain.banks.clone(),
            fixed: plain.fixed.clone(),
            pixels: plain.pixels.clone(),
            queue: plain.queue.clone(),
            index: plain.index,
            wide: plain.wide,
        }
    }

    /// Whether `plain`'s bytes load as a [`Checked`].
    fn loads(plain: &Plain) -> bool {
        let bytes = postcard::to_stdvec(plain).expect("serializes");
        load::<Checked>(&bytes, "not that").is_ok()
    }

    #[test]
    fn the_helpers_keep_postcards_layout() {
        let plain = plain();
        let checked = checked(&plain);
        let plain_bytes = postcard::to_stdvec(&plain).expect("serializes");
        let checked_bytes = postcard::to_stdvec(&checked).expect("serializes");
        assert_eq!(plain_bytes, checked_bytes);
        let read: Checked = load(&plain_bytes, "not that").expect("loads");
        assert_eq!(read, checked);
    }

    #[test]
    fn appending_writes_what_postcard_writes_after_what_was_there() {
        let checked = checked(&plain());
        let mut out = vec![0xAA, 0xBB];
        append(&checked, &mut out);
        let own = postcard::to_stdvec(&checked).expect("serializes");
        assert_eq!(out[..2], [0xAA, 0xBB]);
        assert_eq!(out[2..], own[..]);
    }

    #[test]
    fn a_load_refuses_bytes_left_over_and_bytes_missing() {
        let mut bytes = postcard::to_stdvec(&plain()).expect("serializes");
        assert!(load::<Checked>(&bytes, "not that").is_ok());
        bytes.push(0);
        assert_eq!(
            load::<Checked>(&bytes, "not that"),
            Err(StateError::Malformed("not that"))
        );
        bytes.truncate(bytes.len() - 2);
        assert!(load::<Checked>(&bytes, "not that").is_err());
        // Two in a row, as a linked pair's state is, are not one.
        let pair = postcard::to_stdvec(&(plain(), plain())).expect("serializes");
        assert!(load::<Checked>(&pair, "not that").is_err());
        assert!(load::<(Checked, Checked)>(&pair, "not that").is_ok());
    }

    #[test]
    fn buffers_of_the_wrong_length_are_refused() {
        assert!(loads(&plain()));
        for length in [0, 299, 301] {
            let fixed = vec![7; length].into();
            assert!(!loads(&Plain { fixed, ..plain() }), "{length} bytes");
        }
        for length in [0, 2, 4] {
            let pixels = vec![5; length].into();
            assert!(!loads(&Plain { pixels, ..plain() }), "{length} pixels");
        }
        // A length the bytes do not back is refused before anything is
        // made room for.
        let mut bytes = postcard::to_stdvec(&plain()).expect("serializes");
        let at = bytes.len() - 20;
        assert_eq!(bytes[at], 3, "the pixels' length");
        bytes[at] = 0x7F;
        assert!(load::<Checked>(&bytes, "not that").is_err());
    }

    #[test]
    fn lists_longer_than_their_most_are_refused() {
        for length in 0..=4 {
            let queue = (0..length).collect();
            assert!(loads(&Plain { queue, ..plain() }), "{length} entries");
        }
        let queue = (0..5).collect();
        assert!(!loads(&Plain { queue, ..plain() }));
    }

    #[test]
    fn numbers_out_of_range_are_refused() {
        assert!(!loads(&Plain {
            index: 8,
            ..plain()
        }));
        assert!(!loads(&Plain {
            wide: 1000,
            ..plain()
        }));
        assert!(loads(&Plain {
            index: 0,
            wide: 0,
            ..plain()
        }));
    }
}
