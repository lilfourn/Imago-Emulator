//! Interrupt request bits, shared by IF (0xFF0F) and IE (0xFFFF).

/// LCD entered VBlank.
pub const VBLANK: u8 = 0x01;
/// A STAT condition (LY=LYC or a PPU mode) became true.
pub const STAT: u8 = 0x02;
/// TIMA overflowed.
pub const TIMER: u8 = 0x04;
/// A serial transfer finished.
pub const SERIAL: u8 = 0x08;
/// A joypad line went low.
pub const JOYPAD: u8 = 0x10;
