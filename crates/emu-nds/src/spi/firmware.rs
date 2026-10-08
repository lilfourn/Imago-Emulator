//! The firmware flash (GBATEK, "DS Firmware Header", "DS Firmware Wifi
//! Calibration Data", "DS Firmware Wifi Internet Access Points", "DS
//! Firmware User Settings").
//!
//! The user's own `firmware.bin` is used when given. Otherwise the chip
//! holds a synthesised 256 KiB image of a DS Lite with what games read: the
//! header with the user settings location, wifi settings with a valid CRC,
//! three unconfigured access points, and two copies of the user settings.
//! It has no boot code, as the machine boots games directly.
//!
//! The wifi settings are a DS Lite's: its MAC address, the MAC's port
//! values GBATEK lists after initialisation, and tables for its baseband
//! and radio chip (an MM3218, "type 3"). The emulated chips only remember
//! what is written to them, so their tables hold GBATEK's documented values
//! and zeros elsewhere rather than a real console's calibration.

use serde::{Deserialize, Serialize};

use super::flash::Flash;

/// Bytes of user settings copied to RAM at boot (0x027FFC80).
pub(crate) const USER_SETTINGS_SIZE: usize = 0x70;
/// One user settings area: the settings, an update counter, the CRC and
/// unused bytes.
const AREA_SIZE: usize = 0x100;

const IMAGE_SIZE: usize = 0x4_0000;
/// The two user settings areas fill the last 512 bytes.
const USER_SETTINGS_OFFSET: usize = IMAGE_SIZE - 2 * AREA_SIZE;
/// The three access points sit 400h bytes below the user settings.
const ACCESS_POINTS_OFFSET: usize = USER_SETTINGS_OFFSET - 0x400;
const ACCESS_POINT_SIZE: usize = 0x100;

const CONSOLE_DS_LITE: u8 = 0x20;
/// Wifi data from 0x2C that the CRC at 0x2A covers.
const WIFI_CONFIG_LENGTH: usize = 0x138;
const WIFI_VERSION_DS_LITE: u8 = 5;
/// A made-up address with the prefix DS Lite consoles use.
const MAC_ADDRESS: [u8; 6] = [0x00, 0x16, 0x56, 0x2D, 0x53, 0x01];
/// Wifi channels 1-13.
const CHANNELS: u16 = 0x3FFE;
const RF_CHIP_TYPE_3: u8 = 3;
/// 20-bit serial words for the radio; bit 7 becomes W_RF_CNT bit 8.
const RF_BITS_TYPE_3: u8 = 0x94;
/// Radio registers the boot code sets, from 0xCE.
const RF_REGISTERS: usize = 0x29;
/// Values for the MAC's ports (W_CONFIG_xxx and W_POWER_TX) at 0x44-0x63:
/// GBATEK's values after initialisation, and W_POWER_TX's power-on value.
const MAC_PORTS: [u16; 16] = [
    0x0002, 0x0017, 0x0026, 0x1818, 0x0048, 0x4840, 0x0058, 0x0042, 0x0140, 0x8064, 0xE0E0, 0x2443,
    0x0003, 0x0032, 0x01F4, 0x0101,
];
/// Baseband registers GBATEK documents (the others start at zero): the
/// DS Lite's 03h and 12h at 00h and 66h, the reset bit in 01h, carrier
/// sense in 13h, the gain in 1Eh and the energy threshold in 35h.
const BASEBAND: [(usize, u8); 6] = [
    (0x00, 0x03),
    (0x01, 0x9E),
    (0x13, 0x00),
    (0x1E, 0xBB),
    (0x35, 0x1F),
    (0x66, 0x12),
];
/// The baseband gain (register 1Eh) for channels 1-14, written on each
/// channel change.
const CHANNEL_GAINS: [u8; 14] = [
    0xB1, 0xB1, 0xB2, 0xB2, 0xB3, 0xB3, 0xB4, 0xB4, 0xB5, 0xB5, 0xB6, 0xB6, 0xB7, 0xB7,
];

const SETTINGS_VERSION: u16 = 5;
const NICKNAME: &str = "Player";
const FAVORITE_COLOR: u8 = 0; // grey
const BIRTHDAY: (u8, u8) = (1, 1); // month, day
const LANGUAGE_ENGLISH: u16 = 1;
/// Language and flags bits 10, 11, 13, 14 and 15: the settings are
/// complete, so nothing prompts for them.
const SETTINGS_COMPLETE: u16 = 0xEC00;
const BACKLIGHT_MAX: u16 = 3 << 4;

/// The synthesised touchscreen: X readings are 14 × pixel + 100h and Y
/// readings 19 × pixel + B0h, close to a real panel's ranges, recorded at
/// two calibration pixels like the firmware's own.
const ADC_PER_PIXEL: (i32, i32) = (14, 19);
const ADC_AT_ZERO: (i32, i32) = (0x100, 0xB0);
const CALIBRATION_PIXELS: [(i32, i32); 2] = [(32, 32), (224, 160)];

/// CRC-16 as the BIOS's GetCRC16 computes it (GBATEK, "BIOS Misc
/// Functions"): reflected polynomial A001h from `initial`.
pub(crate) fn crc16(initial: u16, data: &[u8]) -> u16 {
    data.iter().fold(initial, |crc, &byte| {
        (0..8).fold(crc ^ u16::from(byte), |crc, _| {
            if crc & 1 != 0 {
                crc >> 1 ^ 0xA001
            } else {
                crc >> 1
            }
        })
    })
}

fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

/// Maps touchscreen pixels to ADC readings through two calibration points
/// (user settings 0x58-0x63).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Calibration {
    /// ADC readings at the two points.
    adc: [(i32, i32); 2],
    /// The two points in pixels (the settings count from 1).
    pixels: [(i32, i32); 2],
}

impl Calibration {
    pub(crate) fn from_settings(settings: &[u8; USER_SETTINGS_SIZE]) -> Self {
        let adc = |offset| {
            let reading = |offset| i32::from(read_u16(settings, offset) & 0xFFF);
            (reading(offset), reading(offset + 2))
        };
        let pixel = |offset: usize| {
            let coordinate = |offset: usize| i32::from(settings[offset]) - 1;
            (coordinate(offset), coordinate(offset + 1))
        };
        Self {
            adc: [adc(0x58), adc(0x5E)],
            pixels: [pixel(0x5C), pixel(0x62)],
        }
    }

    /// The ADC readings a pen at pixel (`x`, `y`) produces: GBATEK's
    /// pixel formula solved for the reading, rounded, within 12 bits.
    pub(crate) fn adc(&self, x: u8, y: u8) -> (u16, u16) {
        let [(adc_x1, adc_y1), (adc_x2, adc_y2)] = self.adc;
        let [(x1, y1), (x2, y2)] = self.pixels;
        (
            axis(i32::from(x), x1, x2, adc_x1, adc_x2),
            axis(i32::from(y), y1, y2, adc_y1, adc_y2),
        )
    }
}

/// The reading at `pixel` on the line through (`pixel1`, `adc1`) and
/// (`pixel2`, `adc2`).
fn axis(pixel: i32, pixel1: i32, pixel2: i32, adc1: i32, adc2: i32) -> u16 {
    let mut span = pixel2 - pixel1;
    let mut offset = (pixel - pixel1) * (adc2 - adc1);
    if span < 0 {
        span = -span;
        offset = -offset;
    }
    let adc = if span == 0 {
        adc1
    } else {
        adc1 + (2 * offset + span).div_euclid(2 * span)
    };
    adc.clamp(0, 0xFFF) as u16
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Firmware {
    flash: Flash,
}

impl Firmware {
    /// The chip holding `image`, the user's firmware dump, or a
    /// synthesised image without one.
    pub(crate) fn new(image: Option<&[u8]>) -> Self {
        let image = image.map_or_else(synthesize, <[u8]>::to_vec);
        let id = match image.len().next_power_of_two() {
            0x2_0000 => [0x20, 0x40, 0x11],
            0x8_0000 => [0x20, 0x80, 0x13],
            _ => [0x20, 0x40, 0x12],
        };
        Self {
            flash: Flash::new(image, id),
        }
    }

    /// Shifts one byte in and returns the byte shifted out.
    pub(crate) fn transfer(&mut self, input: u8) -> u8 {
        self.flash.transfer(input)
    }

    /// Chip select goes high.
    pub(crate) fn release(&mut self) {
        self.flash.release();
    }

    /// The newest user settings with a valid CRC, or the synthesised
    /// defaults if neither copy is valid.
    pub(crate) fn user_settings(&self) -> [u8; USER_SETTINGS_SIZE] {
        let memory = self.flash.memory();
        // Header 0x20 holds the location divided by 8.
        let location = usize::from(read_u16(memory, 0x20)) * 8;
        let area = |index: usize| {
            let start = location + index * AREA_SIZE;
            memory
                .get(start..start + AREA_SIZE)
                .filter(|area| crc16(0xFFFF, &area[..USER_SETTINGS_SIZE]) == read_u16(area, 0x72))
        };
        let counter = |area: &[u8]| read_u16(area, 0x70) & 0x7F;
        let newest = match (area(0), area(1)) {
            (Some(first), Some(second)) => {
                if counter(second) == (counter(first) + 1) & 0x7F {
                    second
                } else {
                    first
                }
            }
            (Some(area), None) | (None, Some(area)) => area,
            (None, None) => return default_user_settings(),
        };
        let mut settings = [0; USER_SETTINGS_SIZE];
        settings.copy_from_slice(&newest[..USER_SETTINGS_SIZE]);
        settings
    }

    /// The touchscreen calibration in the current user settings.
    pub(crate) fn calibration(&self) -> Calibration {
        Calibration::from_settings(&self.user_settings())
    }

    /// The header and wifi settings: the image's first 0x200 bytes.
    pub(crate) fn wifi_settings(&self) -> &[u8] {
        let memory = self.flash.memory();
        &memory[..memory.len().min(0x200)]
    }

    /// The console's MAC address (0x36).
    pub(crate) fn mac_address(&self) -> [u8; 6] {
        let mut mac = [0; 6];
        if let Some(bytes) = self.flash.memory().get(0x36..0x3C) {
            mac.copy_from_slice(bytes);
        }
        mac
    }

    /// Gives the console another MAC address, updating the wifi settings'
    /// CRC, so two consoles from one image can tell each other apart.
    pub(crate) fn set_mac_address(&mut self, mac: [u8; 6]) {
        let memory = self.flash.memory_mut();
        if memory.len() < 0x200 {
            return;
        }
        memory[0x36..0x3C].copy_from_slice(&mac);
        let length = usize::from(read_u16(memory, 0x2C)).min(0x200 - 0x2C);
        let crc = crc16(0, &memory[0x2C..0x2C + length]);
        write_u16(memory, 0x2A, crc);
    }
}

fn default_user_settings() -> [u8; USER_SETTINGS_SIZE] {
    let mut settings = [0; USER_SETTINGS_SIZE];
    write_u16(&mut settings, 0x00, SETTINGS_VERSION);
    settings[0x02] = FAVORITE_COLOR;
    (settings[0x03], settings[0x04]) = BIRTHDAY;
    let mut length = 0;
    for (index, unit) in NICKNAME.encode_utf16().enumerate() {
        write_u16(&mut settings, 0x06 + 2 * index, unit);
        length += 1;
    }
    write_u16(&mut settings, 0x1A, length);
    for (point, offset) in CALIBRATION_PIXELS.iter().zip([0x58, 0x5E]) {
        let (x, y) = *point;
        let adc_x = ADC_AT_ZERO.0 + ADC_PER_PIXEL.0 * x;
        let adc_y = ADC_AT_ZERO.1 + ADC_PER_PIXEL.1 * y;
        write_u16(&mut settings, offset, adc_x as u16);
        write_u16(&mut settings, offset + 2, adc_y as u16);
        // Screen positions count from 1.
        settings[offset + 4] = (x + 1) as u8;
        settings[offset + 5] = (y + 1) as u8;
    }
    write_u16(
        &mut settings,
        0x64,
        LANGUAGE_ENGLISH | BACKLIGHT_MAX | SETTINGS_COMPLETE,
    );
    settings[0x6C..0x70].fill(0xFF);
    settings
}

/// A user settings area: the settings, update counter `counter` and CRC.
fn user_settings_area(counter: u16) -> [u8; AREA_SIZE] {
    let mut area = [0xFF; AREA_SIZE];
    let settings = default_user_settings();
    area[..USER_SETTINGS_SIZE].copy_from_slice(&settings);
    write_u16(&mut area, 0x70, counter);
    write_u16(&mut area, 0x72, crc16(0xFFFF, &settings));
    area
}

/// An access point entry marked as not configured.
fn empty_access_point() -> [u8; ACCESS_POINT_SIZE] {
    let mut entry = [0; ACCESS_POINT_SIZE];
    entry[0xE7] = 0xFF;
    let crc = crc16(0, &entry[..0xFE]);
    write_u16(&mut entry, 0xFE, crc);
    entry
}

/// The wifi settings at 0x2A-0x163 (GBATEK, "DS Firmware Wifi Calibration
/// Data"), with their CRC.
fn write_wifi_settings(header: &mut [u8]) {
    header[0x2C..0x2C + WIFI_CONFIG_LENGTH].fill(0);
    write_u16(header, 0x2C, WIFI_CONFIG_LENGTH as u16);
    header[0x2F] = WIFI_VERSION_DS_LITE;
    header[0x30..0x35].fill(0xFF);
    header[0x36..0x3C].copy_from_slice(&MAC_ADDRESS);
    write_u16(header, 0x3C, CHANNELS);
    write_u16(header, 0x3E, 0xFFFF);
    header[0x40] = RF_CHIP_TYPE_3;
    header[0x41] = RF_BITS_TYPE_3;
    header[0x42] = RF_REGISTERS as u8;
    // Radio writes per channel change: none.
    header[0x43] = 0;
    for (index, value) in MAC_PORTS.into_iter().enumerate() {
        write_u16(header, 0x44 + 2 * index, value);
    }
    for (register, value) in BASEBAND {
        header[0x64 + register] = value;
    }
    // After the radio's registers, the channel tables: one baseband write
    // per channel change, the gain.
    let channels = 0xCE + RF_REGISTERS;
    header[channels] = 1;
    header[channels + 1] = 0x1E;
    header[channels + 2..channels + 16].copy_from_slice(&CHANNEL_GAINS);
    header[0x134..0x162].fill(0xFF);
    header[0x162] = 0x1A;
    header[0x163] = 0xFF;
    let crc = crc16(0, &header[0x2C..0x2C + WIFI_CONFIG_LENGTH]);
    write_u16(header, 0x2A, crc);
}

fn synthesize() -> Vec<u8> {
    let mut image = vec![0xFF; IMAGE_SIZE];
    let header = &mut image[..0x200];
    header[..0x1D].fill(0);
    header[0x08..0x0C].copy_from_slice(b"MACP");
    // Bits 12-15: the chip size in 128 KiB units.
    write_u16(header, 0x14, ((IMAGE_SIZE / 0x2_0000) as u16) << 12);
    header[0x1D] = CONSOLE_DS_LITE;
    write_u16(header, 0x20, (USER_SETTINGS_OFFSET / 8) as u16);
    write_u16(header, 0x22, 0x7EC0);
    write_u16(header, 0x24, 0x7E40);
    write_u16(header, 0x26, 0);

    write_wifi_settings(header);

    for index in 0..3 {
        let start = ACCESS_POINTS_OFFSET + index * ACCESS_POINT_SIZE;
        image[start..start + ACCESS_POINT_SIZE].copy_from_slice(&empty_access_point());
    }
    // Both copies hold the same settings; the second is the newer.
    for counter in 0..2 {
        let start = USER_SETTINGS_OFFSET + usize::from(counter) * AREA_SIZE;
        image[start..start + AREA_SIZE].copy_from_slice(&user_settings_area(counter));
    }
    image
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spi::flash::tests::command;

    #[test]
    fn crc16_matches_the_logo_checksum_algorithm() {
        // The CRC-16/MODBUS check value.
        assert_eq!(crc16(0xFFFF, b"123456789"), 0x4B37);
    }

    #[test]
    fn synthesised_user_settings_have_valid_crcs() {
        let firmware = Firmware::new(None);
        let memory = firmware.flash.memory();
        assert_eq!(memory.len(), IMAGE_SIZE);
        assert_eq!(read_u16(memory, 0x20) as usize * 8, 0x3_FE00);
        for start in [0x3_FE00, 0x3_FF00] {
            let area = &memory[start..start + AREA_SIZE];
            assert_eq!(crc16(0xFFFF, &area[..0x70]), read_u16(area, 0x72));
        }
        let settings = firmware.user_settings();
        assert_eq!(read_u16(&settings, 0), 5);
        assert_eq!(read_u16(&settings, 0x1A), 6);
        assert_eq!(&settings[0x06..0x0A], &[b'P', 0, b'l', 0]);
        assert_eq!(read_u16(&settings, 0x64) & 7, LANGUAGE_ENGLISH);
    }

    #[test]
    fn synthesised_wifi_data_has_a_valid_crc_and_mac() {
        let firmware = Firmware::new(None);
        let memory = firmware.flash.memory();
        let length = usize::from(read_u16(memory, 0x2C));
        assert_eq!(
            crc16(0, &memory[0x2C..0x2C + length]),
            read_u16(memory, 0x2A)
        );
        assert_eq!(memory[0x36..0x3C], MAC_ADDRESS);
        let access_point = &memory[0x3_FA00..0x3_FB00];
        assert_eq!(
            crc16(0, &access_point[..0xFE]),
            read_u16(access_point, 0xFE)
        );
    }

    #[test]
    fn synthesised_wifi_settings_follow_gbatek() {
        let firmware = Firmware::new(None);
        let settings = firmware.wifi_settings();
        assert_eq!(settings.len(), 0x200);
        assert_eq!(settings[0x40], 3, "a DS Lite's radio");
        assert_eq!(read_u16(settings, 0x4C), 0x0048, "W_CONFIG_120h");
        assert_eq!(read_u16(settings, 0x62), 0x0101, "W_CONFIG_150h");
        assert_eq!(settings[0x64 + 0x01], 0x9E, "BB[01h]");
        // The channel tables follow the 29h radio registers.
        assert_eq!(&settings[0xF7..0xFA], &[1, 0x1E, 0xB1]);
        assert_eq!(settings[0x162], 0x1A);
    }

    #[test]
    fn a_second_console_gets_its_own_mac_address() {
        let mut firmware = Firmware::new(None);
        let mut mac = firmware.mac_address();
        assert_eq!(mac, MAC_ADDRESS);
        mac[5] = 0x02;
        firmware.set_mac_address(mac);
        let memory = firmware.flash.memory();
        assert_eq!(memory[0x36..0x3C], mac);
        let length = usize::from(read_u16(memory, 0x2C));
        assert_eq!(
            crc16(0, &memory[0x2C..0x2C + length]),
            read_u16(memory, 0x2A),
            "the CRC still matches"
        );
    }

    #[test]
    fn prefers_the_newer_valid_copy() {
        let mut image = synthesize();
        // Make the second copy newer with a different colour, then break it.
        let second = USER_SETTINGS_OFFSET + AREA_SIZE;
        image[second + 0x02] = 7;
        let crc = crc16(0xFFFF, &image[second..second + 0x70]);
        write_u16(&mut image, second + 0x72, crc);
        assert_eq!(Firmware::new(Some(&image)).user_settings()[0x02], 7);
        image[second + 0x72] ^= 1;
        assert_eq!(
            Firmware::new(Some(&image)).user_settings()[0x02],
            FAVORITE_COLOR
        );
    }

    #[test]
    fn reads_over_spi_and_reports_the_jedec_id() {
        let mut firmware = Firmware::new(None);
        let reply = command(&mut firmware.flash, &[0x9F, 0, 0, 0]);
        assert_eq!(reply[1..], [0x20, 0x40, 0x12]);
        let reply = command(&mut firmware.flash, &[0x03, 0x00, 0x00, 0x20, 0, 0]);
        assert_eq!(reply[4..], [0xC0, 0x7F]);
    }

    #[test]
    fn calibration_maps_pixels_to_readings_and_back() {
        let calibration = Firmware::new(None).calibration();
        assert_eq!(calibration.adc(0, 0), (0x100, 0xB0));
        assert_eq!(
            calibration.adc(255, 191),
            (0x100 + 14 * 255, 0xB0 + 19 * 191)
        );
        // GBATEK's formula takes every reading back to its pixel.
        let settings = default_user_settings();
        let reading = |offset| i32::from(read_u16(&settings, offset));
        let (adc_x1, adc_x2) = (reading(0x58), reading(0x5E));
        let (scr_x1, scr_x2) = (i32::from(settings[0x5C]), i32::from(settings[0x62]));
        for x in 0..=255u8 {
            let adc = i32::from(calibration.adc(x, 0).0);
            let pixel = (adc - adc_x1) * (scr_x2 - scr_x1) / (adc_x2 - adc_x1) + (scr_x1 - 1);
            assert_eq!(pixel, i32::from(x));
        }
    }
}
