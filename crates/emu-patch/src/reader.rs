//! Reading a patch front to back. Every read past the end, and every
//! number too big to use, is a damaged patch.

use crate::PatchError;

/// Bytes read from the front.
pub struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// How many bytes are left.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn byte(&mut self) -> Result<u8, PatchError> {
        Ok(self.take(1)?[0])
    }

    /// The next `count` bytes.
    pub fn take(&mut self, count: usize) -> Result<&'a [u8], PatchError> {
        if count > self.bytes.len() {
            return Err(PatchError::Damaged);
        }
        let (taken, rest) = self.bytes.split_at(count);
        self.bytes = rest;
        Ok(taken)
    }

    /// Takes `expected` if the bytes start with it.
    pub fn expect(&mut self, expected: &[u8]) -> Result<(), PatchError> {
        if self.take(expected.len())? == expected {
            Ok(())
        } else {
            Err(PatchError::Damaged)
        }
    }

    /// A big-endian number `count` bytes long, at most 4.
    pub fn big_endian(&mut self, count: usize) -> Result<u32, PatchError> {
        Ok(self
            .take(count)?
            .iter()
            .fold(0, |number, &byte| number << 8 | u32::from(byte)))
    }

    /// A number as UPS and BPS write them: seven bits a byte, lowest
    /// first, the top bit set on the last byte, and each byte after the
    /// first adding one more of its place, so that every number has just
    /// one encoding.
    pub fn beat_number(&mut self) -> Result<usize, PatchError> {
        let mut number: u64 = 0;
        let mut place: u64 = 1;
        loop {
            let byte = self.byte()?;
            number = u64::from(byte & 0x7F)
                .checked_mul(place)
                .and_then(|digit| number.checked_add(digit))
                .ok_or(PatchError::Damaged)?;
            if byte & 0x80 != 0 {
                return usize::try_from(number).map_err(|_| PatchError::Damaged);
            }
            place = place.checked_mul(0x80).ok_or(PatchError::Damaged)?;
            number = number.checked_add(place).ok_or(PatchError::Damaged)?;
        }
    }

    /// A number as VCDIFF writes them: seven bits a byte, highest first,
    /// the top bit set on every byte but the last.
    pub fn vcdiff_number(&mut self) -> Result<usize, PatchError> {
        let mut number: usize = 0;
        loop {
            let byte = self.byte()?;
            number =
                number.checked_mul(0x80).ok_or(PatchError::Damaged)? | usize::from(byte & 0x7F);
            if byte & 0x80 == 0 {
                return Ok(number);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_beat_numbers() {
        // 0x7F + 1 needs a second byte, which adds a place: 128 is 00 80.
        let cases: [(&[u8], usize); 4] = [
            (&[0x80], 0),
            (&[0xFF], 0x7F),
            (&[0x00, 0x80], 0x80),
            (&[0x01, 0x80], 0x81),
        ];
        for (bytes, number) in cases {
            assert_eq!(Reader::new(bytes).beat_number(), Ok(number), "{bytes:02X?}");
        }
        assert_eq!(
            Reader::new(&[0x00; 16]).beat_number(),
            Err(PatchError::Damaged)
        );
    }

    #[test]
    fn reads_vcdiff_numbers() {
        // RFC 3284, section 2: 123456789 is the base-128 digits 58, 111,
        // 26 and 21.
        let mut reader = Reader::new(&[0xBA, 0xEF, 0x9A, 0x15, 0x05]);
        assert_eq!(reader.vcdiff_number(), Ok(123_456_789));
        assert_eq!(reader.vcdiff_number(), Ok(5));
        assert_eq!(
            Reader::new(&[0xFF; 11]).vcdiff_number(),
            Err(PatchError::Damaged)
        );
    }

    #[test]
    fn reading_past_the_end_is_damage() {
        let mut reader = Reader::new(b"ab");
        assert_eq!(reader.take(3), Err(PatchError::Damaged));
        assert_eq!(reader.big_endian(2), Ok(0x6162));
        assert_eq!(reader.byte(), Err(PatchError::Damaged));
    }
}
