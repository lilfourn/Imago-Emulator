//! Frames on the air, and what a receiving MAC makes of them (GBATEK, "DS
//! Wifi Hardware Headers", "DS Wifi Receive Control", "DS Wifi IEEE802.11
//! Frames").
//!
//! A sender's frame travels with its 12-byte TX header, which carries the
//! rate (byte 8) and the IEEE frame's length with its checksum (bytes
//! 0xA-0xB). The IEEE frame follows: frame control, duration, three
//! addresses, sequence control, body, and a 4-byte checksum (FCS) that is
//! not computed here, since nothing on the air corrupts frames.
//!
//! Nintendo's multiplayer (MP) protocol is built into the MAC. A host sends
//! a CMD frame to 03:09:BF:00:00:00 naming its clients in the second
//! halfword of the body; each named client's hardware answers in its own
//! time slot with the reply the game queued (or an empty one), sent to the
//! host with 03:09:BF:00:00:10 as the third address; then the host's
//! hardware sends an ACK to 03:09:BF:00:00:03 listing who failed to answer.

use serde::{Deserialize, Serialize};

use super::clock;

/// A MAC address, first byte first.
pub(super) type Mac = [u8; 6];

/// Where MP hosts send CMD frames.
pub(super) const MP_CMD: Mac = [0x03, 0x09, 0xBF, 0x00, 0x00, 0x00];
/// The third address of MP replies.
pub(super) const MP_REPLY: Mac = [0x03, 0x09, 0xBF, 0x00, 0x00, 0x10];
/// Where MP hosts send the ACK after the replies.
pub(super) const MP_ACK: Mac = [0x03, 0x09, 0xBF, 0x00, 0x00, 0x03];

/// Bytes in the TX and RX headers.
pub(super) const HEADER: usize = 12;
/// Bytes in an IEEE data or management frame's header.
const IEEE_HEADER: usize = 24;
/// The TX header's rate byte for 2 Mbit/s; anything else is 1 Mbit/s.
pub(super) const RATE_2_MBIT: u8 = 0x14;
/// The rate byte for 1 Mbit/s.
#[cfg(test)]
pub(super) const RATE_1_MBIT: u8 = 0x0A;

/// A frame one console sent, on its way to the others.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transmission {
    /// The bus cycle the frame's data started, after its preamble: when
    /// the sender raised IRQ7. Consoles in a link share one clock.
    pub(super) start: u64,
    /// The TX header, then the IEEE frame with its checksum.
    pub(super) bytes: Vec<u8>,
    /// For an MP reply, the client number it answers as, which on the air
    /// is the time slot it arrives in; 0 for other frames.
    pub(super) reply_slot: u8,
}

impl Transmission {
    /// Whether this frame, read from a save state, has its TX header and
    /// no more than the longest frame a header can give, and started at
    /// a time the clock can reach.
    pub(super) fn in_range(&self) -> bool {
        (HEADER..=HEADER + 0x3FFF).contains(&self.bytes.len()) && self.start < super::TIME_END
    }

    /// Microseconds each byte takes at the frame's rate.
    pub(super) fn micros_per_byte(&self) -> u64 {
        micros_per_byte(self.bytes[8])
    }

    /// Bytes of IEEE frame on the air, checksum included.
    pub(super) fn length(&self) -> usize {
        usize::from(half(&self.bytes, 0xA) & 0x3FFF)
    }

    /// The IEEE frame.
    pub(super) fn frame(&self) -> &[u8] {
        &self.bytes[HEADER..]
    }

    /// The bus cycles from the start of the data to its end.
    pub(super) fn duration(&self) -> u64 {
        clock::cycles(self.length() as u64 * self.micros_per_byte())
    }
}

/// Microseconds per byte at the rate in a TX header's byte 8.
pub(super) fn micros_per_byte(rate: u8) -> u64 {
    if rate == RATE_2_MBIT { 4 } else { 8 }
}

/// The little-endian halfword at `offset`, or 0 past the end.
pub(super) fn half(bytes: &[u8], offset: usize) -> u16 {
    match bytes.get(offset..offset + 2) {
        Some(pair) => u16::from_le_bytes([pair[0], pair[1]]),
        None => 0,
    }
}

/// The address at `offset` in an IEEE frame, or zeros past the end.
fn address(frame: &[u8], offset: usize) -> Mac {
    let mut mac = [0; 6];
    if let Some(bytes) = frame.get(offset..offset + 6) {
        mac.copy_from_slice(bytes);
    }
    mac
}

/// The receiver's settings that decide which frames it keeps.
pub(super) struct Station {
    /// W_MACADDR.
    pub(super) mac: Mac,
    /// W_BSSID.
    pub(super) bssid: Mac,
    /// W_RXFILTER and W_RXFILTER2.
    pub(super) filter: u16,
    pub(super) filter2: u16,
    /// W_WEP_CNT bit 15.
    pub(super) wep: bool,
}

/// The RX header's flags (its first halfword) for `frame`, or `None` if the
/// receiver drops it. Bits 0-3 give the kind (1 beacon, 0xC-0xF MP
/// frames), bit 4 is always set, and bit 15 says the frame's BSSID is the
/// receiver's.
///
/// Which W_RXFILTER bits let which frames through is only partly known;
/// this follows GBATEK and melonDS's hardware notes.
pub(super) fn classify(frame: &[u8], station: &Station) -> Option<u16> {
    let control = half(frame, 0);
    let destination = address(frame, 4);
    let group = destination[0] & 1 != 0;
    if !group && destination != station.mac {
        return None;
    }
    if control & 0x4000 != 0 && !station.wep {
        return None;
    }
    let filter = |bit: u16| station.filter & (1 << bit) != 0;
    let mut flags = 0x0010;
    let subtype = (control >> 4) & 0xF;
    match (control >> 2) & 3 {
        // Management.
        0 => {
            let ours = address(frame, 16) == station.bssid;
            if ours {
                flags |= 0x8000;
            }
            if subtype == 8 {
                if !ours && !filter(0) {
                    return None;
                }
                flags |= 0x0001;
            } else if (subtype <= 5 || (0xA..=0xC).contains(&subtype))
                && !ours
                && station.filter & (3 << 9) == 0
            {
                return None;
            }
        }
        // Control: only PS-Poll frames get through.
        1 => {
            if control & 0xF0 != 0xA0 {
                return None;
            }
            if destination == station.bssid {
                flags |= 0x8000;
            } else if !filter(11) {
                return None;
            }
            flags |= 0x0005;
        }
        // Data.
        2 => {
            let from_to = (control >> 8) & 3;
            if station.filter2 & (1 << from_to) != 0 {
                return None;
            }
            // The BSSID is the third, first or second address for frames
            // within a network, to it or from it.
            let bssid = [Some(16), Some(4), Some(10), None][usize::from(from_to)];
            if bssid.is_some_and(|offset| address(frame, offset) == station.bssid) {
                flags |= 0x8000;
            } else if !filter(11) {
                return None;
            }
            if control & 0x0800 != 0 && !filter(0) {
                return None;
            }
            let kind = if address(frame, 16) == MP_REPLY {
                if control & 0xF0 == 0x50 { 0xF } else { 0xE }
            } else if destination == MP_CMD {
                0xC
            } else if destination == MP_ACK {
                0xD
            } else {
                0x8
            };
            flags |= kind;
            let allowed = match subtype {
                0 | 4 => true,
                1 if kind == 0xD => filter(7),
                1 => kind == 0xE || filter(1),
                2 => kind == 0xC || filter(2),
                3 => filter(3),
                5 if kind == 0xF => filter(8),
                5 => filter(4),
                6 => filter(5),
                7 => filter(6),
                _ => false,
            };
            if !allowed {
                return None;
            }
        }
        _ => return None,
    }
    Some(flags)
}

/// Whether RX flags mark an MP CMD frame for the receiver's network.
pub(super) fn is_mp_cmd(flags: u16) -> bool {
    flags & 0x800F == 0x800C
}

/// Whether RX flags mark a beacon from the receiver's network.
pub(super) fn is_own_beacon(flags: u16) -> bool {
    flags & 0x800F == 0x8001
}

/// Whether `frame` is an MP reply, by its third address.
pub(super) fn is_mp_reply(frame: &[u8]) -> bool {
    (half(frame, 0) >> 2) & 3 == 2 && address(frame, 16) == MP_REPLY
}

/// An MP CMD frame's per-client reply time and client mask: the first two
/// halfwords of its body.
pub(super) fn mp_cmd_body(frame: &[u8]) -> (u16, u16) {
    (half(frame, IEEE_HEADER), half(frame, IEEE_HEADER + 2))
}

/// Writes `value` little-endian at `offset`.
fn put(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

/// A TX header and an IEEE header with `control`, `duration`, three
/// addresses and `sequence`, then `body` and a zero checksum.
fn build(
    rate: u8,
    control: u16,
    duration: u16,
    addresses: [Mac; 3],
    sequence: u16,
    body: &[u16],
) -> Vec<u8> {
    let length = IEEE_HEADER + 2 * body.len() + 4;
    let mut bytes = vec![0; HEADER + length];
    bytes[8] = rate;
    put(&mut bytes, 0xA, length as u16);
    let frame = &mut bytes[HEADER..];
    put(frame, 0, control);
    put(frame, 2, duration);
    for (index, mac) in addresses.iter().enumerate() {
        frame[4 + 6 * index..10 + 6 * index].copy_from_slice(mac);
    }
    put(frame, 22, sequence);
    for (index, value) in body.iter().enumerate() {
        put(frame, IEEE_HEADER + 2 * index, *value);
    }
    bytes
}

/// The ACK an MP host's hardware sends after the replies: a CF-Ack data
/// frame from the host (`bssid`, `mac`) whose body ends with the clients
/// that did not answer.
pub(super) fn mp_ack(rate: u8, bssid: Mac, mac: Mac, sequence: u16, failed: u16) -> Vec<u8> {
    build(
        rate,
        0x0218,
        0,
        [MP_ACK, bssid, mac],
        sequence,
        &[0x0033, failed],
    )
}

/// The empty reply an MP client's hardware sends when the game queued none
/// (or one too long for its slot): a CF-Ack with no data, 28 bytes at
/// 2 Mbit/s.
pub(super) fn mp_empty_reply(bssid: Mac, mac: Mac, sequence: u16) -> Vec<u8> {
    build(
        RATE_2_MBIT,
        0x0158,
        0x00F0,
        [bssid, mac, MP_REPLY],
        sequence,
        &[],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: Mac = [0x00, 0x16, 0x56, 0x2D, 0x53, 0x01];
    const CLIENT: Mac = [0x00, 0x16, 0x56, 0x2D, 0x53, 0x02];

    fn station(mac: Mac, bssid: Mac, filter: u16) -> Station {
        Station {
            mac,
            bssid,
            filter,
            filter2: 0,
            wep: true,
        }
    }

    fn frame(control: u16, addresses: [Mac; 3]) -> Vec<u8> {
        build(RATE_2_MBIT, control, 0, addresses, 0, &[0x0100, 0x0002])[HEADER..].to_vec()
    }

    #[test]
    fn beacons_from_other_networks_need_filter_bit_0() {
        let beacon = frame(0x0080, [[0xFF; 6], HOST, HOST]);
        assert_eq!(classify(&beacon, &station(CLIENT, [0; 6], 0x0000)), None);
        assert_eq!(
            classify(&beacon, &station(CLIENT, [0; 6], 0x0001)),
            Some(0x0011)
        );
        let flags = classify(&beacon, &station(CLIENT, HOST, 0x0000)).expect("kept");
        assert!(is_own_beacon(flags));
    }

    #[test]
    fn unicast_frames_for_someone_else_are_dropped() {
        let auth = frame(0x00B0, [HOST, CLIENT, HOST]);
        assert_eq!(classify(&auth, &station(CLIENT, HOST, 0xFFFF)), None);
        assert_eq!(classify(&auth, &station(HOST, HOST, 0)), Some(0x8010));
    }

    #[test]
    fn mp_frames_are_recognised_by_their_addresses() {
        let client = station(CLIENT, HOST, 0x0401);
        let cmd = frame(0x0228, [MP_CMD, HOST, HOST]);
        let flags = classify(&cmd, &client).expect("kept");
        assert!(is_mp_cmd(flags));
        assert_eq!(mp_cmd_body(&cmd), (0x0100, 0x0002));
        let ack = mp_ack(RATE_2_MBIT, HOST, HOST, 0, 0x0002);
        assert_eq!(
            classify(&ack[HEADER..], &client),
            None,
            "needs filter bit 7"
        );
        let flags = classify(&ack[HEADER..], &station(CLIENT, HOST, 0x0080));
        assert_eq!(flags, Some(0x801D));

        let host = station(HOST, HOST, 0x0000);
        let empty = mp_empty_reply(HOST, CLIENT, 0x10);
        assert!(is_mp_reply(&empty[HEADER..]));
        assert_eq!(
            classify(&empty[HEADER..], &host),
            None,
            "needs filter bit 8"
        );
        let flags = classify(&empty[HEADER..], &station(HOST, HOST, 0x0100));
        assert_eq!(flags, Some(0x801F));
        let reply = frame(0x0118, [HOST, CLIENT, MP_REPLY]);
        assert_eq!(classify(&reply, &host), Some(0x801E));
    }

    #[test]
    fn transmissions_know_their_length_and_rate() {
        let ack = Transmission {
            start: 0,
            bytes: mp_ack(RATE_1_MBIT, HOST, HOST, 0, 0),
            reply_slot: 0,
        };
        assert_eq!(ack.length(), 32);
        assert_eq!(ack.micros_per_byte(), 8);
        assert_eq!(ack.duration(), clock::cycles(256));
        assert_eq!(&ack.frame()[4..10], &MP_ACK);
    }
}
