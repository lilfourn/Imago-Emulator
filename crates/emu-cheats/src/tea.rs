//! The encryption GameShark Advance and Pro Action Replay v3 codes share
//! (GBATEK, "Gameshark Encryption"): TEA, 32 rounds, on the code's left
//! and right words, with four seeds per device. A `DEADFACE 0000xxyy` code
//! rebuilds the seeds from the device's T1 and T2 tables.

use crate::codes::Item;
use crate::decode::halves;
use crate::seed_tables;

const DELTA: u32 = 0x9E37_79B9;

/// One device's encryption: its seeds and its reseeding tables.
#[derive(Debug, Clone)]
pub(crate) struct Cipher {
    seeds: [u32; 4],
    t1: &'static [u8; 256],
    t2: &'static [u8; 256],
}

impl Cipher {
    /// GameShark Advance and Action Replay v1/v2, as they power on.
    pub(crate) const GAMESHARK_ADVANCE: Self = Self {
        seeds: [0x09F4_FBBD, 0x9681_884A, 0x3520_27E9, 0xF3DE_E5A7],
        t1: &seed_tables::GSA_T1,
        t2: &seed_tables::GSA_T2,
    };

    /// Pro Action Replay v3, as it powers on.
    pub(crate) const PRO_ACTION_REPLAY: Self = Self {
        seeds: [0x7AA9_648F, 0x7FAE_6994, 0xC0EF_AAD5, 0x4271_2C57],
        t1: &seed_tables::PAR3_T1,
        t2: &seed_tables::PAR3_T2,
    };

    /// Decrypts one code: GBATEK's rounds run backwards.
    pub(crate) fn decrypt(&self, code: u64) -> u64 {
        let [s0, s1, s2, s3] = self.seeds;
        let (mut a, mut v) = halves(code);
        let mut sum = DELTA.wrapping_mul(32);
        for _ in 0..32 {
            v = v.wrapping_sub(
                (a << 4).wrapping_add(s2) ^ a.wrapping_add(sum) ^ (a >> 5).wrapping_add(s3),
            );
            a = a.wrapping_sub(
                (v << 4).wrapping_add(s0) ^ v.wrapping_add(sum) ^ (v >> 5).wrapping_add(s1),
            );
            sum = sum.wrapping_sub(DELTA);
        }
        u64::from(a) << 32 | u64::from(v)
    }

    /// Encrypts one code, exactly as GBATEK writes the rounds.
    #[cfg(test)]
    pub(crate) fn encrypt(&self, code: u64) -> u64 {
        let [s0, s1, s2, s3] = self.seeds;
        let (mut a, mut v) = halves(code);
        for round in 1..=32u32 {
            let sum = round.wrapping_mul(DELTA);
            a = a.wrapping_add(
                (v.wrapping_mul(16).wrapping_add(s0))
                    ^ v.wrapping_add(sum)
                    ^ (v / 32).wrapping_add(s1),
            );
            v = v.wrapping_add(
                (a.wrapping_mul(16).wrapping_add(s2))
                    ^ a.wrapping_add(sum)
                    ^ (a / 32).wrapping_add(s3),
            );
        }
        u64::from(a) << 32 | u64::from(v)
    }

    /// Rebuilds the seeds after `DEADFACE 0000xxyy`, `params` being
    /// `xxyy`.
    fn reseed(&mut self, params: u16) {
        let [xx, yy] = params.to_be_bytes();
        for (y, seed) in (0u8..).zip(&mut self.seeds) {
            for x in 0..4u8 {
                let z = self.t1[usize::from(xx.wrapping_add(x))]
                    .wrapping_add(self.t2[usize::from(yy.wrapping_add(y))]);
                *seed = *seed << 8 | u32::from(z);
            }
        }
    }

    /// Decrypts each code in `items` in place, reseeding after each
    /// `DEADFACE` code so the codes after it decrypt with the new seeds.
    pub(crate) fn decrypt_items(&mut self, items: &mut [(usize, Item)]) {
        for (_, item) in items {
            if let Item::Code(code) = item {
                *code = self.decrypt(*code);
                let (left, right) = halves(*code);
                if left == 0xDEAD_FACE {
                    self.reseed(right as u16);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decrypting_undoes_gbateks_rounds() {
        for cipher in [Cipher::GAMESHARK_ADVANCE, Cipher::PRO_ACTION_REPLAY] {
            for code in [0, 0x0202_5E30_0000_270F, u64::MAX, 0x1234_5678_9ABC_DEF0] {
                assert_eq!(cipher.decrypt(cipher.encrypt(code)), code);
            }
        }
    }

    /// Vectors from an independent transcription of GBATEK's formula.
    #[test]
    fn known_vectors() {
        assert_eq!(
            Cipher::GAMESHARK_ADVANCE.encrypt(0x4241_5850_001D_C0DE),
            0xD41A_4D50_EF6B_965D
        );
        assert_eq!(
            Cipher::PRO_ACTION_REPLAY.encrypt(0x0422_5E30_0000_270F),
            0x8B4A_925B_1C71_894C
        );
    }
}
