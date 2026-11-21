//! The ARM7's SPI bus: power management, firmware and touchscreen (GBATEK,
//! "DS Serial Peripheral Interface Bus (SPI)").
//!
//! SPICNT (0x040001C0) picks a device and a baud rate; writing SPIDATA
//! (0x040001C2) exchanges one byte with it. The device stays selected while
//! SPICNT's hold bit is set and is released after the first transfer
//! without it, or when the bus is disabled. The exchange happens at once;
//! SPICNT reads busy for the byte's time on the wire, which the machine
//! schedules.

pub(crate) mod firmware;
pub(crate) mod flash;
mod power;
mod touch;

use serde::{Deserialize, Serialize};

use firmware::{Firmware, USER_SETTINGS_SIZE};
use power::Power;
use touch::Touch;

const BAUD: u16 = 0x0003;
const BUSY: u16 = 0x0080;
const DEVICE_SHIFT: u16 = 8;
const DEVICE: u16 = 0x0300;
/// Bit 10 asks for a 16-bit transfer, which the hardware botches into an
/// 8-bit one; it is kept but changes nothing.
const BUGGED_16_BIT: u16 = 0x0400;
const HOLD: u16 = 0x0800;
const IRQ_ENABLE: u16 = 0x4000;
const ENABLE: u16 = 0x8000;
const WRITABLE: u16 = BAUD | DEVICE | BUGGED_16_BIT | HOLD | IRQ_ENABLE | ENABLE;

/// System cycles per bit at baud rate 0 (4 MHz); each step of the rate
/// doubles it.
const CYCLES_PER_BIT: u64 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Device {
    Power,
    Firmware,
    Touch,
    /// Select value 3: nothing answers.
    Reserved,
}

impl Device {
    fn from_control(control: u16) -> Self {
        match (control & DEVICE) >> DEVICE_SHIFT {
            0 => Self::Power,
            1 => Self::Firmware,
            2 => Self::Touch,
            _ => Self::Reserved,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Spi {
    /// SPICNT as written.
    control: u16,
    busy: bool,
    /// The byte the last transfer received.
    data: u8,
    /// The device whose chip select is held.
    selected: Option<Device>,
    power: Power,
    firmware: Firmware,
    touch: Touch,
}

impl Spi {
    /// The bus with `firmware`, the user's dump, or a synthesised image.
    pub(crate) fn new(firmware: Option<&[u8]>) -> Self {
        Self {
            control: 0,
            busy: false,
            data: 0,
            selected: None,
            power: Power::default(),
            firmware: Firmware::new(firmware),
            touch: Touch::default(),
        }
    }

    /// Whether the firmware chip, read from a save state, is one the DS
    /// could have.
    pub(crate) fn in_range(&self) -> bool {
        self.firmware.in_range()
    }

    /// Reads SPICNT (0x040001C0).
    pub(crate) fn control(&self) -> u16 {
        self.control | if self.busy { BUSY } else { 0 }
    }

    /// Writes SPICNT (0x040001C0). Disabling the bus releases the device.
    pub(crate) fn write_control(&mut self, value: u16) {
        self.control = value & WRITABLE;
        if value & ENABLE == 0 {
            self.release();
        }
    }

    /// Reads SPIDATA (0x040001C2): the byte the last transfer received.
    pub(crate) fn data(&self) -> u8 {
        self.data
    }

    /// Writes SPIDATA (0x040001C2) at the ARM7's bus cycle `now`,
    /// exchanging a byte with the selected device. Returns the cycles
    /// SPICNT stays busy, after which the machine calls [`Spi::finish`];
    /// `None` with the bus disabled.
    pub(crate) fn write_data(&mut self, value: u8, now: u64) -> Option<u64> {
        if self.control & ENABLE == 0 {
            return None;
        }
        let device = Device::from_control(self.control);
        if self.selected.is_some_and(|held| held != device) {
            self.release();
        }
        self.selected = Some(device);
        self.data = match device {
            Device::Power => self.power.transfer(value),
            Device::Firmware => self.firmware.transfer(value),
            Device::Touch => self.touch.transfer(value, now),
            Device::Reserved => 0,
        };
        if self.control & HOLD == 0 {
            self.release();
        }
        self.busy = true;
        Some((8 * CYCLES_PER_BIT) << (self.control & BAUD))
    }

    /// The transfer ends. Returns whether it requests the SPI interrupt
    /// (IF bit 23).
    pub(crate) fn finish(&mut self) -> bool {
        let was_busy = std::mem::take(&mut self.busy);
        was_busy && self.control & IRQ_ENABLE != 0
    }

    fn release(&mut self) {
        match self.selected.take() {
            Some(Device::Power) => self.power.release(),
            Some(Device::Firmware) => self.firmware.release(),
            Some(Device::Touch) => self.touch.release(),
            Some(Device::Reserved) | None => {}
        }
    }

    /// Puts the pen at screen pixel (x, y) of the touchscreen, or lifts it,
    /// converting through the firmware's calibration.
    pub(crate) fn set_touch(&mut self, point: Option<(u8, u8)>) {
        let calibration = self.firmware.calibration();
        self.touch
            .set_pen(point.map(|(x, y)| calibration.adc(x, y)));
    }

    /// Sets how loud the microphone hears the player, 0–255.
    pub(crate) fn set_microphone(&mut self, loudness: u8) {
        self.touch.set_microphone(loudness);
    }

    /// Whether the pen touches the screen. EXTKEYIN bit 6 reads 0 then.
    pub(crate) fn pen_down(&self) -> bool {
        self.touch.pen_down()
    }

    /// Whether the game shut the system down (power management register 0
    /// bit 6).
    pub(crate) fn powered_off(&self) -> bool {
        self.power.powered_off()
    }

    /// The firmware's current user settings, which direct boot copies to
    /// 0x027FFC80.
    pub(crate) fn user_settings(&self) -> [u8; USER_SETTINGS_SIZE] {
        self.firmware.user_settings()
    }

    /// The firmware's header and wifi settings, its first 0x200 bytes.
    pub(crate) fn wifi_settings(&self) -> &[u8] {
        self.firmware.wifi_settings()
    }

    /// The MAC address in the firmware's wifi settings.
    pub(crate) fn mac_address(&self) -> [u8; 6] {
        self.firmware.mac_address()
    }

    /// Changes the MAC address in the firmware's wifi settings.
    pub(crate) fn set_mac_address(&mut self, mac: [u8; 6]) {
        self.firmware.set_mac_address(mac);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIRMWARE: u16 = 1 << DEVICE_SHIFT;
    const TOUCH: u16 = 2 << DEVICE_SHIFT;

    /// Sends `bytes` to the device in `control`, holding it until the last.
    fn exchange(spi: &mut Spi, control: u16, bytes: &[u8]) -> Vec<u8> {
        bytes
            .iter()
            .enumerate()
            .map(|(i, &byte)| {
                let hold = if i + 1 < bytes.len() { HOLD } else { 0 };
                spi.write_control(ENABLE | control | hold);
                spi.write_data(byte, 0).expect("the bus is enabled");
                spi.finish();
                spi.data()
            })
            .collect()
    }

    #[test]
    fn transfers_take_time_and_may_interrupt() {
        let mut spi = Spi::new(None);
        assert_eq!(spi.write_data(0, 0), None, "the bus starts disabled");
        spi.write_control(ENABLE | IRQ_ENABLE | 2);
        assert_eq!(spi.write_data(0, 0), Some(256));
        assert_ne!(spi.control() & BUSY, 0);
        assert!(spi.finish());
        assert!(!spi.finish());
        assert_eq!(spi.control() & BUSY, 0);
    }

    #[test]
    fn reads_the_firmware_until_released() {
        let mut spi = Spi::new(None);
        let reply = exchange(&mut spi, FIRMWARE, &[0x03, 0x03, 0xFE, 0x00, 0, 0]);
        assert_eq!(reply[4..], [5, 0]);
        // libnds holds chip select through the read, then disables the bus.
        spi.write_control(ENABLE | FIRMWARE | HOLD);
        for byte in [0x03, 0x00, 0x00, 0x08] {
            spi.write_data(byte, 0);
        }
        spi.write_data(0, 0);
        assert_eq!(spi.data(), b'M');
        spi.write_control(0);
        assert_eq!(exchange(&mut spi, FIRMWARE, &[0x9F, 0])[1], 0x20);
    }

    #[test]
    fn touch_goes_through_the_calibration() {
        let mut spi = Spi::new(None);
        spi.set_touch(Some((100, 50)));
        assert!(spi.pen_down());
        let reply = exchange(&mut spi, TOUCH, &[0xD0, 0, 0]);
        let x = u16::from(reply[1]) << 5 | u16::from(reply[2] >> 3);
        assert_eq!(x, 0x100 + 14 * 100);
        spi.set_touch(None);
        let reply = exchange(&mut spi, TOUCH, &[0x90, 0, 0]);
        assert_eq!(u16::from(reply[1]) << 5 | u16::from(reply[2] >> 3), 0xFFF);
    }

    #[test]
    fn the_microphone_hears_the_frame_s_loudness() {
        let mut spi = Spi::new(None);
        // libnds's micReadData12: control byte 0xE4, two reads.
        let mic = |spi: &mut Spi| {
            let reply = exchange(spi, TOUCH, &[0xE4, 0, 0]);
            u16::from(reply[1] & 0x7F) << 5 | u16::from(reply[2] >> 3)
        };
        assert_eq!(mic(&mut spi), 0x800, "silence");
        spi.set_microphone(255);
        let mut readings: Vec<u16> = (0..64)
            .map(|n| {
                spi.write_control(ENABLE | TOUCH | HOLD);
                spi.write_data(0xE4, n * 1000).expect("the bus is enabled");
                spi.write_data(0, n * 1000);
                let high = spi.data();
                spi.write_control(ENABLE | TOUCH);
                spi.write_data(0, n * 1000);
                u16::from(high & 0x7F) << 5 | u16::from(spi.data() >> 3)
            })
            .collect();
        readings.sort_unstable();
        readings.dedup();
        assert!(readings.len() > 32, "noise, not a constant: {readings:X?}");
    }

    #[test]
    fn power_off_is_visible() {
        let mut spi = Spi::new(None);
        exchange(&mut spi, 0, &[0x00, 0x40]);
        assert!(spi.powered_off());
    }
}
