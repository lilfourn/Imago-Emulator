//! A one-halfword read cache. Neighbouring pixels usually share a VRAM
//! halfword (two 256-colour texels, four 16-colour texels, or one map
//! entry for a whole tile), and each VRAM read walks the bank mapping, so
//! remembering the last halfword saves most reads.

pub(super) struct HalfwordCache {
    /// The cached halfword's address, or odd (never a halfword address)
    /// when empty.
    address: u32,
    value: u16,
}

impl HalfwordCache {
    pub(super) fn new() -> Self {
        Self {
            address: 1,
            value: 0,
        }
    }

    /// The halfword at `offset` (aligned down), read through `read` on a miss.
    pub(super) fn halfword(&mut self, offset: u32, read: impl Fn(u32) -> u16) -> u16 {
        let address = offset & !1;
        if address != self.address {
            self.address = address;
            self.value = read(address);
        }
        self.value
    }

    /// The byte at `offset`, read through `read` on a miss.
    pub(super) fn byte(&mut self, offset: u32, read: impl Fn(u32) -> u16) -> u8 {
        (self.halfword(offset, read) >> ((offset & 1) * 8)) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_bytes_and_halfwords_of_the_halfword_asked_for() {
        let read = |address: u32| address as u16 | 0x1100;
        let mut cache = HalfwordCache::new();
        assert_eq!(cache.byte(0x20, read), 0x20);
        assert_eq!(cache.byte(0x21, read), 0x11);
        assert_eq!(cache.halfword(0x21, read), 0x1120);
        // The next halfword is read, not the one before.
        assert_eq!(cache.byte(0x22, read), 0x22);
        assert_eq!(cache.halfword(0x20, read), 0x1120);
    }
}
