//! Display capture: engine A's output (or the 3D layer), optionally
//! blended with VRAM, written back into a VRAM bank one line at a time
//! (GBATEK, "DS Display Capture").
//!
//! Games use it for motion blur, screen transitions and showing 3D on both
//! screens. Setting DISPCAPCNT bit 31 starts a capture at the next frame's
//! first line; the hardware clears the bit when the last line is written.
//! Main-memory display FIFO as source B is not emulated (it reads zero).

use crate::gpu2d::WIDTH;
use crate::vram::{Region, Vram};

/// DISPCAPCNT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Control(pub(crate) u32);

impl Control {
    pub(crate) fn enabled(self) -> bool {
        self.0 & (1 << 31) != 0
    }

    /// (width, lines) captured.
    pub(crate) fn size(self) -> (usize, u16) {
        match (self.0 >> 20) & 3 {
            0 => (128, 128),
            1 => (256, 64),
            2 => (256, 128),
            _ => (256, 192),
        }
    }

    fn eva(self) -> u32 {
        (self.0 & 0x1F).min(16)
    }

    fn evb(self) -> u32 {
        ((self.0 >> 8) & 0x1F).min(16)
    }

    /// The LCDC offset of the bank written, plus the write offset.
    fn write_base(self) -> (u32, u32) {
        let bank = (self.0 >> 16) & 3;
        (bank * 0x2_0000, ((self.0 >> 18) & 3) * 0x8000)
    }

    fn read_offset(self) -> u32 {
        ((self.0 >> 26) & 3) * 0x8000
    }

    /// 0: source A, 1: source B, 2 and 3: both, blended.
    fn mode(self) -> u32 {
        (self.0 >> 29) & 3
    }

    fn three_d_only(self) -> bool {
        self.0 & (1 << 24) != 0
    }
}

/// A 3D pixel (6-bit channels, 5-bit alpha) as BGR555 with bit 15 set
/// where it is drawn.
fn from_3d(pixel: u32) -> u16 {
    let channel = |shift: u32| ((pixel >> shift) & 0x3F) as u16 >> 1;
    let alpha = (pixel >> 18) & 0x1F != 0;
    channel(0) | channel(6) << 5 | channel(12) << 10 | u16::from(alpha) << 15
}

fn blend(a: u16, b: u16, eva: u32, evb: u32) -> u16 {
    let a_on = u32::from(a >> 15);
    let b_on = u32::from(b >> 15);
    let channel = |shift: u32| {
        let a = u32::from(a >> shift) & 0x1F;
        let b = u32::from(b >> shift) & 0x1F;
        ((a * a_on * eva + b * b_on * evb) / 16).min(31) as u16
    };
    let alpha = (a_on != 0 && eva > 0) || (b_on != 0 && evb > 0);
    channel(0) | channel(5) << 5 | channel(10) << 10 | u16::from(alpha) << 15
}

/// Captures `line` as `control` says. `graphics` is engine A's composed
/// line (BGR555, bit 15 where drawn), `three_d` the 3D layer if any, and
/// `display_bank` DISPCNT bits 18–19, which pick the bank source B reads.
pub(crate) fn capture_line(
    control: Control,
    line: u16,
    graphics: &[u16; WIDTH],
    three_d: Option<&[u32; WIDTH]>,
    display_bank: u32,
    vram: &mut Vram,
) {
    let (width, lines) = control.size();
    if line >= lines {
        return;
    }
    let source_a = if control.three_d_only() {
        three_d.map_or([0; WIDTH], |pixels| pixels.map(from_3d))
    } else {
        *graphics
    };
    // A line read or written starts at a multiple of its length, so lies
    // within one 8 KiB page of its 128 KiB bank, and moves in one go.
    let row = u32::from(line) * WIDTH as u32 * 2;
    let read_at = display_bank * 0x2_0000 + ((control.read_offset() + row) & 0x1_FFFF);
    let source_b = || -> [u16; WIDTH] {
        let bytes: [u8; WIDTH * 2] = vram.read_bytes(Region::Lcdc, read_at);
        std::array::from_fn(|x| u16::from_le_bytes([bytes[2 * x], bytes[2 * x + 1]]))
    };
    let captured = match control.mode() {
        0 => source_a,
        1 => source_b(),
        _ => {
            let (eva, evb) = (control.eva(), control.evb());
            let source_b = source_b();
            std::array::from_fn(|x| blend(source_a[x], source_b[x], eva, evb))
        }
    };
    let mut bytes = [0; WIDTH * 2];
    for (pair, value) in bytes.as_chunks_mut().0.iter_mut().zip(captured) {
        *pair = value.to_le_bytes();
    }
    let (write_bank, write_offset) = control.write_base();
    let write_at = (write_offset + u32::from(line) * width as u32 * 2) & 0x1_FFFF;
    vram.write_page(Region::Lcdc, write_bank + write_at, &bytes[..width * 2]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_source_a_into_the_chosen_bank() {
        let mut vram = Vram::default();
        vram.set_control(1, 0x80); // B to LCDC
        let graphics = [0x801F; WIDTH];
        // Enable, bank B, full size, source A.
        let control = Control(1 << 31 | 1 << 16 | 3 << 20);
        capture_line(control, 5, &graphics, None, 0, &mut vram);
        assert_eq!(vram.read_16(Region::Lcdc, 0x2_0000 + 5 * 512), 0x801F);
        assert_eq!(vram.read_16(Region::Lcdc, 0x2_0000 + 5 * 512 + 510), 0x801F);
    }

    #[test]
    fn blending_weighs_each_source() {
        let red = 0x8000 | 16;
        let blue = 0x8000 | 16 << 10;
        assert_eq!(blend(red, blue, 8, 8), 0x8000 | 8 | 8 << 10);
        assert_eq!(blend(red, 0, 16, 16), red, "an undrawn source adds nothing");
    }

    #[test]
    fn three_d_pixels_narrow_to_five_bits() {
        let white_opaque = 0x3F | 0x3F << 6 | 0x3F << 12 | 31 << 18;
        assert_eq!(from_3d(white_opaque), 0xFFFF);
        assert_eq!(from_3d(0x3F), 0x001F, "no alpha, no bit 15");
    }

    /// Captures `line` a pixel at a time: read source B, then write the
    /// pixel, as the hardware moves them.
    fn capture_each_pixel(
        control: Control,
        line: u16,
        graphics: &[u16; WIDTH],
        three_d: Option<&[u32; WIDTH]>,
        display_bank: u32,
        vram: &mut Vram,
    ) {
        let (width, lines) = control.size();
        if line >= lines {
            return;
        }
        let (write_bank, write_offset) = control.write_base();
        let row = u32::from(line) * WIDTH as u32 * 2;
        for x in 0..width {
            let source_a = if control.three_d_only() {
                three_d.map_or(0, |pixels| from_3d(pixels[x]))
            } else {
                graphics[x]
            };
            let read_at = (control.read_offset() + row + x as u32 * 2) & 0x1_FFFF;
            let source_b = vram.read_16(Region::Lcdc, display_bank * 0x2_0000 + read_at);
            let pixel = match control.mode() {
                0 => source_a,
                1 => source_b,
                _ => blend(source_a, source_b, control.eva(), control.evb()),
            };
            let write_at =
                (write_offset + u32::from(line) * width as u32 * 2 + x as u32 * 2) & 0x1_FFFF;
            vram.write_16(Region::Lcdc, write_bank + write_at, pixel);
        }
    }

    #[test]
    fn capturing_a_line_at_once_matches_capturing_each_pixel() {
        let mut random = crate::vram::tests::Random(0xCA97);
        let mut vram = Vram::default();
        for bank in 0..4 {
            vram.set_control(bank, 0x80);
        }
        for offset in (0..0x8_0000).step_by(2) {
            vram.write_16(Region::Lcdc, offset, random.next() as u16);
        }
        let mut expected = vram.clone();
        for _ in 0..2000 {
            let control = Control(random.next() | 1 << 31);
            let line = (random.next() % 192) as u16;
            let graphics = [0; WIDTH].map(|_| random.next() as u16);
            let three_d = [0; WIDTH].map(|_| random.next());
            let three_d = (random.next() & 1 != 0).then_some(&three_d);
            let bank = random.next() & 3;
            capture_line(control, line, &graphics, three_d, bank, &mut vram);
            capture_each_pixel(control, line, &graphics, three_d, bank, &mut expected);
        }
        for offset in (0..0x8_0000).step_by(2) {
            let (got, want) = (
                vram.read_16(Region::Lcdc, offset),
                expected.read_16(Region::Lcdc, offset),
            );
            assert_eq!(got, want, "LCDC {offset:#X}");
        }
    }

    #[test]
    fn small_captures_stop_at_their_height() {
        let mut vram = Vram::default();
        vram.set_control(0, 0x80);
        let control = Control(1 << 31); // 128×128 into bank A
        capture_line(control, 130, &[0xFFFF; WIDTH], None, 0, &mut vram);
        assert_eq!(vram.read_16(Region::Lcdc, 130 * 256), 0);
    }
}
