//! Byte buffers in save states, written as one byte string rather than a
//! byte at a time: `#[serde(with = "crate::bytes")]` on a `Box<[u8]>` or a
//! `Vec<u8>`, and [`array`] on an array of them.
//!
//! Postcard lays out a byte string exactly as it does a sequence of bytes,
//! a length and then the bytes, so states keep their format and only save
//! and load faster. Main RAM alone is 4 MiB.

use std::fmt;

use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Serializes `value` onto the end of `out`, laid out as postcard lays it
/// out. It goes straight into the caller's buffer: the app's rewind
/// snapshot reuses one each frame that already has room, where postcard's
/// own buffer would grow several times over, a whole state each frame, and
/// then be copied.
pub(crate) fn append<T: Serialize + ?Sized>(value: &T, out: &mut Vec<u8>) {
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

/// Writes `bytes` as one byte string.
pub(crate) fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_bytes(bytes)
}

/// Reads a byte string (or a sequence of bytes) into a `Box<[u8]>` or a
/// `Vec<u8>`.
pub(crate) fn deserialize<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: From<Vec<u8>>,
{
    deserializer.deserialize_byte_buf(ByteBuf).map(T::from)
}

/// Arrays of byte buffers, each a byte string.
pub(crate) mod array {
    use std::fmt;

    use serde::de::{self, SeqAccess, Visitor};
    use serde::ser::SerializeTuple;
    use serde::{Deserializer, Serializer};

    use super::{Bytes, OwnedBytes};

    pub(crate) fn serialize<S: Serializer, const N: usize>(
        buffers: &[Box<[u8]>; N],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut tuple = serializer.serialize_tuple(N)?;
        for buffer in buffers {
            tuple.serialize_element(&Bytes(buffer))?;
        }
        tuple.end()
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>, const N: usize>(
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
        deserialize(deserializer).map(Self)
    }
}

/// Reads a byte string, or a sequence of bytes from formats that write
/// one.
struct ByteBuf;

impl<'de> Visitor<'de> for ByteBuf {
    type Value = Vec<u8>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a byte string")
    }

    fn visit_bytes<E: de::Error>(self, bytes: &[u8]) -> Result<Vec<u8>, E> {
        Ok(bytes.to_vec())
    }

    fn visit_byte_buf<E: de::Error>(self, bytes: Vec<u8>) -> Result<Vec<u8>, E> {
        Ok(bytes)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<u8>, A::Error> {
        let mut bytes = Vec::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(byte) = seq.next_element()? {
            bytes.push(byte);
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Plain {
        boxed: Box<[u8]>,
        vec: Vec<u8>,
        banks: [Box<[u8]>; 2],
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Strings {
        #[serde(with = "super")]
        boxed: Box<[u8]>,
        #[serde(with = "super")]
        vec: Vec<u8>,
        #[serde(with = "super::array")]
        banks: [Box<[u8]>; 2],
    }

    #[test]
    fn byte_strings_keep_postcards_layout_for_byte_sequences() {
        let plain = Plain {
            boxed: (0..=255).collect(),
            vec: vec![1, 2, 3],
            banks: [vec![9; 300].into(), Box::default()],
        };
        let strings = Strings {
            boxed: plain.boxed.clone(),
            vec: plain.vec.clone(),
            banks: plain.banks.clone(),
        };
        let plain_bytes = postcard::to_stdvec(&plain).expect("serializes");
        let string_bytes = postcard::to_stdvec(&strings).expect("serializes");
        assert_eq!(plain_bytes, string_bytes);
        let read: Strings = postcard::from_bytes(&plain_bytes).expect("deserializes");
        assert_eq!(read, strings);
    }

    #[test]
    fn appending_writes_what_postcard_writes_after_what_was_there() {
        let strings = Strings {
            boxed: vec![1, 2, 3].into_boxed_slice(),
            vec: vec![4; 300],
            banks: [vec![5].into_boxed_slice(), Vec::new().into_boxed_slice()],
        };
        let mut out = vec![0xAA, 0xBB];
        super::append(&strings, &mut out);
        let own = postcard::to_stdvec(&strings).expect("serializes");
        assert_eq!(out[..2], [0xAA, 0xBB]);
        assert_eq!(out[2..], own[..]);
    }
}
