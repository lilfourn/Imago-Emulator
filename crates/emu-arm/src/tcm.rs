//! Tightly coupled memory: RAM inside the ARM946E-S that answers without
//! the bus (GBATEK, "ARM CP15 Tightly Coupled Memory"). CP15 places each
//! block at a base and a virtual size; a block smaller than its virtual size
//! repeats across it.

use std::fmt;

use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Bytes of instruction TCM in the DS's ARM946E-S.
pub(crate) const ITCM_SIZE: usize = 0x8000;
/// Bytes of data TCM in the DS's ARM946E-S.
pub(crate) const DTCM_SIZE: usize = 0x4000;

/// A base that no masked address can equal: masks always clear bit 0.
const NEVER: u32 = 1;

/// Which addresses a TCM block answers, from a CP15 region register (base
/// in bits 12–31, virtual size 512 << N in bits 1–5) and the control
/// register's enable and load-mode bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Window {
    /// Address bits above the virtual size.
    mask: u32,
    /// The base while the block is enabled, else [`NEVER`].
    base: u32,
    /// The base while the block answers reads (enabled and not in load
    /// mode, which makes it write-only), else [`NEVER`].
    read_base: u32,
}

impl Window {
    pub(crate) fn new(region: u32, enabled: bool, load_mode: bool) -> Self {
        let size_shift = 9 + ((region >> 1) & 0x1F);
        // Virtual sizes of 4 GiB and up cover every address.
        let mask = if size_shift >= 32 {
            0
        } else {
            !((1 << size_shift) - 1)
        };
        // The base must be size-aligned; bits below the size are ignored.
        let base = region & 0xFFFF_F000 & mask;
        Self {
            mask,
            base: if enabled { base } else { NEVER },
            read_base: if enabled && !load_mode { base } else { NEVER },
        }
    }

    /// Whether the block is enabled at `address` (for writes, and for
    /// instruction fetches, which load mode does not affect).
    pub(crate) fn contains(self, address: u32) -> bool {
        address & self.mask == self.base
    }

    /// Whether data reads at `address` come from the block.
    pub(crate) fn readable(self, address: u32) -> bool {
        address & self.mask == self.read_base
    }
}

/// One block of TCM. Addresses wrap at its size, which is a power of two.
/// Saved as one byte string, which postcard lays out as it would the
/// bytes one by one, only faster.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Block(Box<[u8]>);

impl Block {
    pub(crate) fn new(size: usize) -> Self {
        debug_assert!(size.is_power_of_two() && size >= 4);
        Self(vec![0; size].into_boxed_slice())
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.0
    }

    fn index(&self, address: u32) -> usize {
        address as usize & (self.0.len() - 1)
    }

    // Every instruction fetched from TCM comes through these, so they are
    // inlined, and multi-byte reads take their bytes as one slice: one
    // bounds check and one load.

    #[inline]
    pub(crate) fn read_32(&self, address: u32) -> u32 {
        let i = self.index(address & !3);
        let bytes = &self.0[i..i + 4];
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    }

    #[inline]
    pub(crate) fn read_16(&self, address: u32) -> u16 {
        let i = self.index(address & !1);
        let bytes = &self.0[i..i + 2];
        u16::from_le_bytes([bytes[0], bytes[1]])
    }

    #[inline]
    pub(crate) fn read_8(&self, address: u32) -> u8 {
        self.0[self.index(address)]
    }

    #[inline]
    pub(crate) fn write_32(&mut self, address: u32, value: u32) {
        let i = self.index(address & !3);
        self.0[i..i + 4].copy_from_slice(&value.to_le_bytes());
    }

    #[inline]
    pub(crate) fn write_16(&mut self, address: u32, value: u16) {
        let i = self.index(address & !1);
        self.0[i..i + 2].copy_from_slice(&value.to_le_bytes());
    }

    #[inline]
    pub(crate) fn write_8(&mut self, address: u32, value: u8) {
        let i = self.index(address);
        self.0[i] = value;
    }
}

impl TryFrom<Box<[u8]>> for Block {
    type Error = String;

    /// Rejects a saved block whose size is not a power of two, which would
    /// break the address wrapping.
    fn try_from(bytes: Box<[u8]>) -> Result<Self, String> {
        if bytes.len().is_power_of_two() && bytes.len() >= 4 {
            Ok(Self(bytes))
        } else {
            Err(format!("TCM block of {} bytes", bytes.len()))
        }
    }
}

impl Serialize for Block {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for Block {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bytes = deserializer.deserialize_byte_buf(ByteBuf)?;
        Self::try_from(bytes.into_boxed_slice()).map_err(de::Error::custom)
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

impl fmt::Debug for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Block({} bytes)", self.0.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_covers_its_virtual_size() {
        // Base 0x027C0000, 16 KiB (N = 5).
        let window = Window::new(0x027C_000A, true, false);
        assert!(window.contains(0x027C_0000));
        assert!(window.contains(0x027C_3FFF));
        assert!(!window.contains(0x027C_4000));
        assert!(!window.contains(0x027B_FFFF));
    }

    #[test]
    fn window_ignores_base_bits_below_its_size() {
        // A 32 MiB window (N = 16) with a misaligned base starts at 0.
        let window = Window::new(0x0123_4020, true, false);
        assert!(window.contains(0));
        assert!(window.contains(0x01FF_FFFF));
        assert!(!window.contains(0x0200_0000));
    }

    #[test]
    fn window_of_4_gib_covers_everything() {
        let window = Window::new(23 << 1, true, false);
        assert!(window.contains(0));
        assert!(window.contains(0xFFFF_FFFF));
    }

    #[test]
    fn disabled_and_load_mode_windows() {
        let off = Window::new(0x0000_0020, false, false);
        assert!(!off.contains(0) && !off.readable(0));
        let load = Window::new(0x0000_0020, true, true);
        assert!(load.contains(0) && !load.readable(0));
    }

    #[test]
    fn block_mirrors_and_aligns() {
        let mut block = Block::new(DTCM_SIZE);
        block.write_32(0x0000_0102, 0x4433_2211);
        assert_eq!(block.read_32(0x0000_0100), 0x4433_2211);
        assert_eq!(block.read_32(0x0000_4100), 0x4433_2211);
        assert_eq!(block.read_16(0x0000_0103), 0x4433);
        assert_eq!(block.read_8(0x0000_0101), 0x22);
        block.write_8(0x0000_4101, 0x99);
        assert_eq!(block.read_32(0x100), 0x4433_9911);
    }

    #[test]
    fn saved_blocks_must_be_a_power_of_two() {
        assert!(Block::try_from(vec![0; 12].into_boxed_slice()).is_err());
        assert!(Block::try_from(vec![0; 16].into_boxed_slice()).is_ok());
    }
}
