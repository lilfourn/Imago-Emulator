//! One CPU's interrupt controller: IME, IE and IF (GBATEK, "DS Interrupts").
//!
//! Each CPU has its own. IF bits are set by hardware and cleared by writing
//! ones; the CPU's IRQ line is high while IME is set and IE & IF is non-zero.

use serde::{Deserialize, Serialize};

/// IF bits.
pub(crate) mod irq {
    pub(crate) const VBLANK: u32 = 1 << 0;
    pub(crate) const HBLANK: u32 = 1 << 1;
    pub(crate) const VCOUNT: u32 = 1 << 2;
    /// Timer `n`: bit 3 + n.
    pub(crate) const TIMER0: u32 = 1 << 3;
    /// DMA channel `n`: bit 8 + n.
    pub(crate) const DMA0: u32 = 1 << 8;
    pub(crate) const KEYPAD: u32 = 1 << 12;
    pub(crate) const IPC_SYNC: u32 = 1 << 16;
    pub(crate) const IPC_SEND_EMPTY: u32 = 1 << 17;
    pub(crate) const IPC_RECEIVE_NOT_EMPTY: u32 = 1 << 18;
    pub(crate) const CART_TRANSFER: u32 = 1 << 19;
    /// ARM9 only.
    pub(crate) const GEOMETRY_FIFO: u32 = 1 << 21;
    /// ARM7 only.
    pub(crate) const SPI: u32 = 1 << 23;
    /// ARM7 only: W_IF & W_IE became non-zero.
    pub(crate) const WIFI: u32 = 1 << 24;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Interrupts {
    pub(crate) master_enable: bool,
    pub(crate) enable: u32,
    pub(crate) flags: u32,
}

impl Interrupts {
    pub(crate) fn request(&mut self, bits: u32) {
        self.flags |= bits;
    }

    /// Writing ones to IF clears those bits.
    pub(crate) fn acknowledge(&mut self, bits: u32) {
        self.flags &= !bits;
    }

    /// Whether an enabled interrupt is requested, whatever IME says: this is
    /// what wakes a halted CPU.
    pub(crate) fn pending(&self) -> bool {
        self.enable & self.flags != 0
    }

    /// The CPU's IRQ input.
    pub(crate) fn line(&self) -> bool {
        self.master_enable && self.pending()
    }
}
