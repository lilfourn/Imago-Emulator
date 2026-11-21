//! The ARM946E-S protection unit (GBATEK, "ARM CP15 Protection Unit";
//! ARM946E-S Technical Reference Manual, chapter 3): eight regions, each a
//! power-of-two block of 4 KiB to 4 GiB with its own data and instruction
//! permissions for privileged and User mode. Where regions overlap, the
//! higher-numbered one decides; an address no enabled region covers allows
//! nothing.
//!
//! CP15 compiles its region and permission registers into a [`Map`] of
//! every 4 KiB page, so checking an access is one table lookup.

use std::fmt;

/// An access kind, as a bit of a page's permissions.
pub(crate) const READ: u8 = 1 << 0;
pub(crate) const WRITE: u8 = 1 << 1;
pub(crate) const EXECUTE: u8 = 1 << 2;

/// Privileged permissions are bits 0–2 of a page's; User mode's sit here.
const USER_SHIFT: u32 = 4;

/// Regions are at least 4 KiB, so permissions change at most per 4 KiB.
const PAGE_SHIFT: u32 = 12;
const PAGES: usize = 1 << (32 - PAGE_SHIFT);

/// Each page's permissions for privileged and User mode, from the
/// protection regions (c6) and their permissions (c5).
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Map(Box<[u8; PAGES]>);

impl Default for Map {
    /// A map that allows nothing, as with no region enabled.
    fn default() -> Self {
        let pages = vec![0; PAGES].into_boxed_slice();
        Self(pages.try_into().expect("PAGES bytes"))
    }
}

impl Map {
    /// Recompiles the map from the eight region registers and the
    /// extended data and instruction permissions (4 bits per region).
    pub(crate) fn rebuild(&mut self, regions: &[u32; 8], data: u32, code: u32) {
        self.0.fill(0);
        // Later regions overwrite earlier ones: the higher number wins.
        for (index, &region) in (0u32..).zip(regions) {
            if let Some(pages) = pages(region) {
                let field = |permissions: u32| (permissions >> (index * 4)) & 0xF;
                self.0[pages].fill(page_permissions(field(data), field(code)));
            }
        }
    }

    /// Whether an access of kind `need` ([`READ`], [`WRITE`] or
    /// [`EXECUTE`]) at `address` is allowed, with User mode's permissions
    /// if `user` is set and privileged ones otherwise.
    #[inline]
    pub(crate) fn allows(&self, address: u32, need: u8, user: bool) -> bool {
        let need = if user { need << USER_SHIFT } else { need };
        self.0[(address >> PAGE_SHIFT) as usize] & need != 0
    }
}

impl fmt::Debug for Map {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Map(..)")
    }
}

/// The pages an enabled region covers: size 2 << N for N in bits 1–5, base
/// in bits 12–31. The base must be size-aligned; bits below the size are
/// ignored, as they are for the TCM regions. Sizes below 4 KiB are
/// reserved and cover nothing, as in melonDS.
fn pages(region: u32) -> Option<std::ops::Range<usize>> {
    let size_shift = ((region >> 1) & 0x1F) + 1;
    if region & 1 == 0 || size_shift < PAGE_SHIFT {
        return None;
    }
    let count = 1usize << (size_shift - PAGE_SHIFT);
    let first = (region >> PAGE_SHIFT) as usize & !(count - 1);
    Some(first..first + count)
}

/// A page's permission bits from its region's data and instruction access
/// permission fields (GBATEK): 0 no access, 1 privileged only, 2 privileged
/// read-write and User read-only, 3 full access, 5 privileged read-only, 6
/// read-only for both. The other values are reserved and allow nothing.
/// Instruction fetches need only read permission.
fn page_permissions(data: u32, code: u32) -> u8 {
    const RW: u8 = READ | WRITE;
    let (privileged, user) = match data {
        1 => (RW, 0),
        2 => (RW, READ),
        3 => (RW, RW),
        5 => (READ, 0),
        6 => (READ, READ),
        _ => (0, 0),
    };
    let (privileged_code, user_code) = match code {
        1 | 5 => (EXECUTE, 0),
        2 | 3 | 6 => (EXECUTE, EXECUTE),
        _ => (0, 0),
    };
    (privileged | privileged_code) | (user | user_code) << USER_SHIFT
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A region at `base` of 1 << `size_log2` bytes, enabled.
    fn region(base: u32, size_log2: u32) -> u32 {
        base | (size_log2 - 1) << 1 | 1
    }

    #[test]
    fn region_sizes_and_bases() {
        let mut map = Map::default();
        // 4 GiB covers everything; a misaligned base is rounded down.
        map.rebuild(&[region(0x1234_5000, 32), 0, 0, 0, 0, 0, 0, 0], 3, 3);
        assert!(map.allows(0, READ, false));
        assert!(map.allows(0xFFFF_FFFF, READ, false));
        map.rebuild(&[region(0x0200_3000, 14), 0, 0, 0, 0, 0, 0, 0], 3, 3);
        assert!(map.allows(0x0200_0000, READ, false));
        assert!(!map.allows(0x0200_4000, READ, false));
        // Disabled regions and reserved sizes cover nothing.
        map.rebuild(&[0x0200_0016, region(0, 11), 0, 0, 0, 0, 0, 0], 3, 3);
        assert!(!map.allows(0x0200_0000, READ, false));
        assert!(!map.allows(0, READ, false));
    }
}
