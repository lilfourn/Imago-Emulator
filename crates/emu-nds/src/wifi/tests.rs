use super::frame::{HEADER, RATE_2_MBIT};
use super::*;

const HOST: Mac = [0x00, 0x16, 0x56, 0x2D, 0x53, 0x01];
const CLIENT: Mac = [0x00, 0x16, 0x56, 0x2D, 0x53, 0x02];
/// The RX ring the tests use: 0x4C00-0x5F60, as in GBATEK's example.
const RX_BEGIN: u16 = 0x4C00;
const RX_END: u16 = 0x5F60;

fn register(offset: u32) -> u32 {
    0x0480_8000 + offset
}

fn write(wifi: &mut Wifi, offset: u32, value: u16, now: u64) {
    wifi.write_16(register(offset), value, now);
}

fn read(wifi: &mut Wifi, offset: u32, now: u64) -> u16 {
    wifi.read_16(register(offset), now)
}

/// Bus cycles for `us` microseconds.
fn us(us: u64) -> u64 {
    clock::cycles(us)
}

/// A wifi block with power and its clock running, as the firmware leaves
/// a DS Lite.
fn powered() -> Wifi {
    let mut settings = vec![0; 0x200];
    settings[0x1D] = 0x20;
    settings[0x40] = 3;
    settings[0x41] = 0x94;
    settings[0x42] = 0x29;
    settings[0x64 + 1] = 0x9E;
    let mut wifi = Wifi::new(&settings);
    wifi.set_enabled(true, 0);
    write(&mut wifi, W_POWER_US, 0, 0);
    wifi
}

/// A powered wifi block, awake and receiving, with `mac`, on the BSS
/// `bssid`, as Nintendo's library sets one up (GBATEK, "DS Wifi
/// Initialization").
fn station(mac: Mac, bssid: Mac) -> Wifi {
    let mut wifi = powered();
    for (index, pair) in mac.chunks_exact(2).enumerate() {
        let offset = 2 * index as u32;
        write(
            &mut wifi,
            W_MACADDR + offset,
            u16::from_le_bytes([pair[0], pair[1]]),
            0,
        );
    }
    for (index, pair) in bssid.chunks_exact(2).enumerate() {
        let offset = 2 * index as u32;
        write(
            &mut wifi,
            W_BSSID + offset,
            u16::from_le_bytes([pair[0], pair[1]]),
            0,
        );
    }
    write(&mut wifi, W_MODE_WEP, 2, 0);
    write(&mut wifi, W_MODE_RST, 1, 0);
    wifi.advance(us(WAKE_MICROS));
    write(&mut wifi, W_RXBUF_BEGIN, RX_BEGIN, us(WAKE_MICROS));
    write(&mut wifi, W_RXBUF_END, RX_END, us(WAKE_MICROS));
    write(
        &mut wifi,
        W_RXBUF_WR_ADDR,
        (RX_BEGIN & 0x1FFF) / 2,
        us(WAKE_MICROS),
    );
    write(
        &mut wifi,
        W_RXBUF_READCSR,
        (RX_BEGIN & 0x1FFF) / 2,
        us(WAKE_MICROS),
    );
    write(&mut wifi, W_RXCNT, 0x8001, us(WAKE_MICROS));
    write(&mut wifi, W_RXFILTER, 0x0581, us(WAKE_MICROS));
    write(&mut wifi, W_RXFILTER2, 0, us(WAKE_MICROS));
    write(&mut wifi, W_WEP_CNT, 0x8000, us(WAKE_MICROS));
    write(&mut wifi, W_PREAMBLE, 0x0006, us(WAKE_MICROS));
    write(&mut wifi, W_IE, 0xFFFF, us(WAKE_MICROS));
    write(&mut wifi, W_IF, 0xFFFF, us(WAKE_MICROS));
    wifi.take_irq();
    wifi
}

/// Writes a TX header and `frame` (an IEEE header and body, without the
/// checksum) at byte `address` of RAM, at 2 Mbit/s.
fn put_frame(wifi: &mut Wifi, address: u16, frame: &[u8], now: u64) {
    let base = 0x0480_4000 + u32::from(address);
    let mut bytes = vec![0; HEADER];
    bytes[8] = RATE_2_MBIT;
    bytes[0xA..0xC].copy_from_slice(&(frame.len() as u16 + 4).to_le_bytes());
    bytes.extend_from_slice(frame);
    bytes.push(0);
    for (index, pair) in bytes.chunks_exact(2).enumerate() {
        let value = u16::from_le_bytes([pair[0], pair[1]]);
        wifi.write_16(base + 2 * index as u32, value, now);
    }
}

/// An IEEE frame with `control`, three addresses and `body` (halfwords).
fn ieee(control: u16, addresses: [Mac; 3], body: &[u16]) -> Vec<u8> {
    let mut frame = vec![0; 24];
    frame[..2].copy_from_slice(&control.to_le_bytes());
    for (index, mac) in addresses.iter().enumerate() {
        frame[4 + 6 * index..10 + 6 * index].copy_from_slice(mac);
    }
    for value in body {
        frame.extend_from_slice(&value.to_le_bytes());
    }
    frame
}

/// Runs both stations to bus cycle `until`, a few microseconds at a time,
/// carrying each one's frames to the other as `link.rs` does.
fn run(pair: &mut [Wifi; 2], from: u64, until: u64) {
    let mut now = from;
    while now < until {
        now = (now + us(4)).min(until);
        for index in 0..2 {
            pair[index].advance(now);
            for frame in pair[index].take_transmissions() {
                pair[1 - index].receive(frame);
            }
        }
    }
}

/// The halfword at byte `address` of RAM.
fn ram(wifi: &mut Wifi, address: u16, now: u64) -> u16 {
    wifi.read_16(0x0480_4000 + u32::from(address), now)
}

#[test]
fn registers_start_as_gbatek_lists_and_mask_their_writes() {
    let mut wifi = powered();
    assert_eq!(read(&mut wifi, W_ID, 0), ID_DS_LITE);
    assert_eq!(read(&mut wifi, W_RXBUF_BEGIN, 0), 0x4000);
    assert_eq!(read(&mut wifi, W_TXREQ_READ, 0), 0x0010);
    assert_eq!(read(&mut wifi, W_RF_CNT, 0), 0x0114, "from firmware[041h]");
    write(&mut wifi, W_ID, 0x1234, 0);
    assert_eq!(read(&mut wifi, W_ID, 0), ID_DS_LITE, "read-only");
    write(&mut wifi, W_AID_LOW, 0xFFFF, 0);
    write(&mut wifi, W_AID_FULL, 0xFFFF, 0);
    write(&mut wifi, W_RXBUF_WR_ADDR, 0xFFFF, 0);
    assert_eq!(read(&mut wifi, W_AID_LOW, 0), 0x000F);
    assert_eq!(read(&mut wifi, W_AID_FULL, 0), 0x07FF);
    assert_eq!(read(&mut wifi, W_RXBUF_WR_ADDR, 0), 0x0FFF);
    // The page repeats at 0x1000, 0x6000 and 0x7000, and in WS0.
    assert_eq!(wifi.read_16(0x0480_9028, 0), 0x000F);
    assert_eq!(wifi.read_16(0x0480_7028, 0), 0x000F);
    assert_eq!(wifi.read_16(0x0480_0028, 0), 0x000F);
}

#[test]
fn ram_takes_halfwords_at_both_regions_and_nothing_while_unpowered() {
    let mut wifi = powered();
    wifi.write_16(0x0480_4010, 0xBEEF, 0);
    assert_eq!(wifi.read_16(0x0480_C010, 0), 0xBEEF);
    assert_eq!(wifi.read_16(0x0480_2010, 0), 0xFFFF, "unused");
    wifi.set_enabled(false, 0);
    assert_eq!(wifi.read_16(0x0480_4010, 0), 0, "POWCNT2 bit 1 is off");
    wifi.write_16(0x0480_4010, 0x1234, 0);
    wifi.set_enabled(true, 0);
    assert_eq!(wifi.read_16(0x0480_4010, 0), 0xBEEF);
    assert_eq!(wifi.peek(0x0480_4011), Some(0xBE));
}

#[test]
fn the_microsecond_counter_runs_while_enabled_and_can_be_set() {
    let mut wifi = powered();
    write(&mut wifi, W_US_COUNTCNT, 1, 0);
    let count = |wifi: &mut Wifi, now| {
        (0..4).fold(0u64, |value, part| {
            value | u64::from(read(wifi, W_US_COUNT + 2 * part, now)) << (16 * part)
        })
    };
    assert_eq!(count(&mut wifi, us(1500)), 1500);
    write(&mut wifi, W_US_COUNT + 2, 0x0001, us(1500));
    assert_eq!(count(&mut wifi, us(1600)), 0x1_0000 + 1600);
    write(&mut wifi, W_US_COUNTCNT, 0, us(1600));
    assert_eq!(count(&mut wifi, us(9000)), 0x1_0000 + 1600, "stopped");
    // W_CMD_COUNT counts down in tens of microseconds.
    write(&mut wifi, W_CMD_COUNT, 100, us(9000));
    assert_eq!(read(&mut wifi, W_CMD_COUNT, us(9000 + 255)), 75);
    assert_eq!(read(&mut wifi, W_CMD_COUNT, us(20_000)), 0);
}

#[test]
fn beacon_timers_raise_pre_beacon_beacon_and_post_beacon_irqs() {
    let mut wifi = powered();
    write(&mut wifi, W_IE, 0xE000, 0);
    write(&mut wifi, W_PRE_BEACON, 0x0100, 0);
    write(&mut wifi, W_BEACONINT, 3, 0);
    write(&mut wifi, W_BEACON_COUNT, 2, 0);
    write(&mut wifi, W_US_COMPARECNT, 1, 0);
    write(&mut wifi, W_US_COUNTCNT, 1, 0);
    // IRQ15 256 µs before the second millisecond boundary (2048 µs).
    wifi.advance(us(2048 - 256) - 1);
    assert_eq!(read(&mut wifi, W_IF, us(2048 - 256) - 1), 0);
    wifi.advance(us(2048 - 256));
    assert_eq!(read(&mut wifi, W_IF, us(2048 - 256)), 0x8000);
    assert!(wifi.take_irq(), "IF bit 24");
    wifi.advance(us(2048));
    assert_eq!(read(&mut wifi, W_IF, us(2048)), 0xC000);
    assert_eq!(read(&mut wifi, W_BEACON_COUNT, us(2048)), 3, "reloaded");
    assert_eq!(read(&mut wifi, W_POST_BEACON, us(2048)), 0xFFFF);
    assert!(!wifi.take_irq(), "only when W_IF & W_IE was zero");
    // Acknowledge, and set the post-beacon count as the IRQ14 handler does.
    write(&mut wifi, W_IF, 0xFFFF, us(2048));
    write(&mut wifi, W_POST_BEACON, 2, us(2048));
    wifi.advance(us(2048 + 2 * 1024));
    assert_eq!(read(&mut wifi, W_IF, us(2048 + 2 * 1024)), 0x2000);
    // The next beacon is 3 ms after the last.
    write(&mut wifi, W_IF, 0xFFFF, us(4096));
    wifi.advance(us(2048 + 3 * 1024));
    assert_eq!(read(&mut wifi, W_IF, us(2048 + 3 * 1024)) & 0x4000, 0x4000);
}

#[test]
fn the_compare_register_raises_irq14_on_its_millisecond() {
    let mut wifi = powered();
    write(&mut wifi, W_IE, 0x4000, 0);
    write(&mut wifi, W_US_COMPARE, 0x1400, 0); // 5 × 1024 µs
    for part in 1..4 {
        write(&mut wifi, W_US_COMPARE + 2 * part, 0, 0);
    }
    write(&mut wifi, W_US_COMPARECNT, 1, 0);
    write(&mut wifi, W_US_COUNTCNT, 1, 0);
    wifi.advance(us(5 * 1024) - 1);
    assert_eq!(read(&mut wifi, W_IF, us(5 * 1024) - 1), 0);
    wifi.advance(us(5 * 1024));
    assert_eq!(read(&mut wifi, W_IF, us(5 * 1024)), 0x4000);
    // Bit 1 forces one.
    write(&mut wifi, W_IF, 0xFFFF, us(5 * 1024));
    write(&mut wifi, W_US_COMPARECNT, 3, us(5 * 1024));
    assert_eq!(read(&mut wifi, W_IF, us(5 * 1024)), 0x4000);
}

#[test]
fn waking_takes_2048_microseconds_and_raises_irq11() {
    let mut wifi = powered();
    write(&mut wifi, W_IE, 0x0800, 0);
    write(&mut wifi, W_MODE_WEP, 2, 0);
    assert_eq!(read(&mut wifi, W_POWER_UNKNOWN, 0), 0x0003);
    write(&mut wifi, W_MODE_RST, 1, 0);
    assert_eq!(read(&mut wifi, W_POWERSTATE, 0) & 0x0100, 0x0100, "waking");
    wifi.advance(us(2047));
    assert_eq!(read(&mut wifi, W_IF, us(2047)), 0);
    wifi.advance(us(2048));
    assert_eq!(read(&mut wifi, W_IF, us(2048)), 0x0800);
    assert_eq!(read(&mut wifi, W_POWERSTATE, us(2048)), 0);
    // W_POWERFORCE puts it back to sleep at once.
    write(&mut wifi, W_POWERFORCE, 0x8001, us(2048));
    assert_eq!(read(&mut wifi, W_POWERSTATE, us(2048)), 0x0200);
    assert_eq!(read(&mut wifi, W_RF_STATUS, us(2048)), 9);
}

#[test]
fn the_receive_ring_wraps_and_counts_down() {
    let mut wifi = powered();
    write(&mut wifi, W_IE, 0x0200, 0);
    write(&mut wifi, W_RXBUF_BEGIN, 0x4000, 0);
    write(&mut wifi, W_RXBUF_END, 0x4008, 0);
    for (index, value) in [0x1111, 0x2222, 0x3333, 0x4444].into_iter().enumerate() {
        wifi.write_16(0x0480_4000 + 2 * index as u32, value, 0);
    }
    write(&mut wifi, W_RXBUF_RD_ADDR, 0x0004, 0);
    write(&mut wifi, W_RXBUF_COUNT, 3, 0);
    let reads: Vec<u16> = (0..3)
        .map(|_| read(&mut wifi, W_RXBUF_RD_DATA, 0))
        .collect();
    assert_eq!(reads, [0x3333, 0x4444, 0x1111]);
    assert_eq!(read(&mut wifi, W_IF, 0), 0x0200, "the count ran out");
    // The mirror at 0x1060 repeats the last read without moving on.
    assert_eq!(wifi.read_16(0x0480_9060, 0), 0x1111);
    assert_eq!(read(&mut wifi, W_RXBUF_RD_ADDR, 0), 0x0002);
}

#[test]
fn transmit_writes_skip_the_gap() {
    let mut wifi = powered();
    write(&mut wifi, W_TXBUF_WR_ADDR, 0x0010, 0);
    write(&mut wifi, W_TXBUF_GAP, 0x0012, 0);
    write(&mut wifi, W_TXBUF_GAPDISP, 0x0008, 0);
    write(&mut wifi, W_TXBUF_COUNT, 2, 0);
    write(&mut wifi, W_IE, 0x0100, 0);
    write(&mut wifi, W_TXBUF_WR_DATA, 0xAAAA, 0);
    write(&mut wifi, W_TXBUF_WR_DATA, 0xBBBB, 0);
    assert_eq!(ram(&mut wifi, 0x10, 0), 0xAAAA);
    assert_eq!(ram(&mut wifi, 0x22, 0), 0xBBBB);
    assert_eq!(read(&mut wifi, W_TXBUF_WR_ADDR, 0), 0x0024);
    assert_eq!(read(&mut wifi, W_IF, 0), 0x0100);
}

#[test]
fn the_baseband_and_radio_answer_over_their_serial_ports() {
    let mut wifi = powered();
    write(&mut wifi, W_BB_CNT, 0x6001, 0);
    assert_eq!(
        read(&mut wifi, W_BB_READ, 0),
        0x9E,
        "the firmware's BB[01h]"
    );
    write(&mut wifi, W_BB_WRITE, 0x1F, 0);
    write(&mut wifi, W_BB_CNT, 0x5035, 0);
    write(&mut wifi, W_BB_CNT, 0x6035, 0);
    assert_eq!(read(&mut wifi, W_BB_READ, 0), 0x1F);
    assert_eq!(read(&mut wifi, W_BB_BUSY, 0), 0);
    // A DS Lite's radio: index in DATA1's high byte, command in DATA2.
    write(&mut wifi, W_RF_DATA1, 0x0742, 0);
    write(&mut wifi, W_RF_DATA2, 5, 0);
    write(&mut wifi, W_RF_DATA1, 0x0700, 0);
    write(&mut wifi, W_RF_DATA2, 6, 0);
    assert_eq!(read(&mut wifi, W_RF_DATA1, 0), 0x0742);
    assert_eq!(read(&mut wifi, W_RF_BUSY, 0), 0);
}

#[test]
fn a_frame_sent_from_loc1_arrives_in_the_other_consoles_ring() {
    let mut pair = [station(HOST, HOST), station(CLIENT, HOST)];
    let start = us(WAKE_MICROS);
    let frame = ieee(0x0008, [CLIENT, HOST, HOST], &[0x1234, 0x5678]);
    put_frame(&mut pair[0], 0x0100, &frame, start);
    write(&mut pair[0], W_TXBUF_LOC1, 0x8000 | 0x0080, start);
    write(&mut pair[0], W_TXREQ_SET, 0x0001, start);
    assert_eq!(read(&mut pair[0], W_TXBUSY, start), 0x0001);
    // Short preamble 96 µs, then 32 bytes at 4 µs each.
    let data = start + us(96);
    let end = data + clock::cycles(32 * 4);
    run(&mut pair, start, data + us(4));
    assert_eq!(read(&mut pair[0], W_IF, data + us(4)), 0x0080, "TX start");
    assert_eq!(read(&mut pair[1], W_IF, data + us(4)), 0x0040, "RX start");
    run(&mut pair, data + us(4), end + us(8));
    let now = end + us(8);
    assert_eq!(read(&mut pair[0], W_IF, now) & 0x0002, 0x0002, "TX done");
    assert_eq!(read(&mut pair[0], W_TXSTAT, now), 0x0001);
    assert_eq!(read(&mut pair[0], W_TXBUSY, now), 0);
    assert_eq!(read(&mut pair[0], W_TXBUF_LOC1, now), 0x0080);
    assert_eq!(ram(&mut pair[0], 0x0100, now), 0x0001, "TX header status");
    assert_eq!(read(&mut pair[1], W_IF, now) & 0x0001, 0x0001, "RX done");
    // The RX header, then the frame without its checksum.
    let ring = RX_BEGIN & 0x1FFF;
    assert_eq!(
        ram(&mut pair[1], ring, now),
        0x8018,
        "data frame from our BSS"
    );
    assert_eq!(ram(&mut pair[1], ring + 6, now), u16::from(RATE_2_MBIT));
    assert_eq!(ram(&mut pair[1], ring + 8, now), 28);
    assert_eq!(ram(&mut pair[1], ring + 12 + 24, now), 0x1234);
    assert_eq!(ram(&mut pair[1], ring + 12 + 26, now), 0x5678);
    let wrcsr = read(&mut pair[1], W_RXBUF_WRCSR, now);
    assert_eq!(wrcsr * 2, ring + 12 + 28, "past the frame, word-aligned");
}

#[test]
fn the_receiver_writes_a_frame_a_halfword_at_a_time() {
    let mut pair = [station(HOST, HOST), station(CLIENT, HOST)];
    let start = us(WAKE_MICROS);
    let frame = ieee(0x0008, [CLIENT, HOST, HOST], &[0xAAAA; 8]);
    put_frame(&mut pair[0], 0x0100, &frame, start);
    write(&mut pair[0], W_TXBUF_LOC1, 0x8080, start);
    write(&mut pair[0], W_TXREQ_SET, 0x0001, start);
    let data = start + us(96);
    run(&mut pair, start, data);
    // Two halfwords take 16 µs at 2 Mbit/s.
    let ring = RX_BEGIN & 0x1FFF;
    let rxtx = |wifi: &mut Wifi, now| read(wifi, W_RXTX_ADDR, now);
    assert_eq!(rxtx(&mut pair[1], data + us(1)) * 2, ring + 12);
    assert_eq!(rxtx(&mut pair[1], data + us(16)) * 2, ring + 16);
}

#[test]
fn frames_for_someone_else_stay_out_of_the_ring() {
    let mut pair = [station(HOST, HOST), station(CLIENT, HOST)];
    let start = us(WAKE_MICROS);
    let stranger = [0x00, 0x16, 0x56, 0x00, 0x00, 0x99];
    put_frame(
        &mut pair[0],
        0x0100,
        &ieee(0x0008, [stranger, HOST, HOST], &[]),
        start,
    );
    write(&mut pair[0], W_TXBUF_LOC1, 0x8080, start);
    write(&mut pair[0], W_TXREQ_SET, 0x0001, start);
    run(&mut pair, start, start + us(400));
    let now = start + us(400);
    assert_eq!(read(&mut pair[1], W_IF, now), 0x0040, "started, not kept");
    assert_eq!(
        read(&mut pair[1], W_RXBUF_WRCSR, now) * 2,
        RX_BEGIN & 0x1FFF
    );
    assert_eq!(read(&mut pair[1], W_RF_STATUS, now), 1, "listening again");
}

#[test]
fn beacons_carry_the_senders_clock_to_the_receiver() {
    let mut pair = [station(HOST, HOST), station(CLIENT, HOST)];
    let start = us(WAKE_MICROS);
    let host = &mut pair[0];
    write(host, W_US_COUNT + 2, 0x0010, start); // 0x100000 µs
    write(host, W_US_COUNTCNT, 1, start);
    write(host, W_US_COMPARECNT, 1, start);
    write(host, W_BEACONINT, 100, start);
    write(host, W_BEACON_COUNT, 1, start);
    write(host, W_TXSTATCNT, 0x8000, start);
    // Timestamp, interval, capability, then the rest of a beacon.
    let beacon = ieee(0x0080, [[0xFF; 6], HOST, HOST], &[0, 0, 0, 0, 100, 0x0021]);
    put_frame(host, 0x0200, &beacon, start);
    write(host, W_TXBUF_BEACON, 0x8100, start);
    write(&mut pair[1], W_US_COUNTCNT, 1, start);
    // The beacon goes at the count's next millisecond boundary.
    let boundary = start + us(1024);
    run(&mut pair, start, boundary + us(1000));
    let now = boundary + us(1000);
    assert_eq!(read(&mut pair[0], W_TXSTAT, now), 0x0301);
    assert_eq!(
        read(&mut pair[0], W_TXBUF_BEACON, now),
        0x8100,
        "kept for the next"
    );
    let ring = RX_BEGIN & 0x1FFF;
    assert_eq!(ram(&mut pair[1], ring, now), 0x8011, "our network's beacon");
    let count = |wifi: &mut Wifi| {
        (0..4).fold(0u64, |value, part| {
            value | u64::from(read(wifi, W_US_COUNT + 2 * part, now)) << (16 * part)
        })
    };
    let (host_count, client_count) = (count(&mut pair[0]), count(&mut pair[1]));
    assert!(host_count >= 0x10_0000 + 1000);
    assert!(
        host_count.abs_diff(client_count) <= 1,
        "{host_count} {client_count}"
    );
}

/// Sets up an MP exchange: the host sends a CMD naming `clients` with a
/// per-client reply time of `reply_time` µs, the client (AID 1) has a
/// 2-halfword reply queued. Returns the pair and when the CMD was asked
/// for.
fn mp_exchange(clients: u16, reply_time: u16) -> ([Wifi; 2], u64) {
    let mut pair = [station(HOST, HOST), station(CLIENT, HOST)];
    let start = us(WAKE_MICROS);
    let client = &mut pair[1];
    write(client, W_AID_LOW, 1, start);
    write(client, W_RXFILTER, 0x0581 | 0x0080, start);
    let reply = ieee(0x0118, [HOST, CLIENT, frame::MP_REPLY], &[0xC0DE, 0xF00D]);
    put_frame(client, 0x0400, &reply, start);
    write(client, W_TXBUF_REPLY1, 0x8200, start);
    let host = &mut pair[0];
    write(host, W_RXFILTER, 0x0581 | 0x0100, start);
    write(host, W_CMD_REPLYTIME, reply_time, start);
    write(host, W_TXSTATCNT, 0x6000, start);
    let cmd = ieee(
        0x0228,
        [frame::MP_CMD, HOST, HOST],
        &[reply_time, clients, 0xABCD],
    );
    put_frame(host, 0x0100, &cmd, start);
    write(host, W_CMD_COUNT, 200, start);
    write(host, W_TXBUF_CMD, 0x8080, start);
    write(host, W_TXREQ_SET, 0x0002, start);
    (pair, start)
}

#[test]
fn an_mp_client_answers_a_cmd_in_its_slot_and_the_host_acks() {
    let (mut pair, start) = mp_exchange(0x0002, 0x0100);
    // CMD: 96 µs preamble + 34 bytes; then the client's slot after 16 µs
    // and a preamble; then the ACK.
    let cmd_end = start + us(96 + 34 * 4);
    run(&mut pair, start, cmd_end + us(4));
    assert_eq!(
        read(&mut pair[0], W_TXSTAT, cmd_end + us(4)),
        0x0800,
        "CMD sent"
    );
    assert_eq!(
        read(&mut pair[0], W_RF_STATUS, cmd_end + us(4)),
        5,
        "awaiting replies"
    );
    let client_if = read(&mut pair[1], W_IF, cmd_end + us(4));
    assert_eq!(client_if & 0x0041, 0x0041, "the client heard the CMD");
    assert_eq!(
        read(&mut pair[1], W_TXBUF_REPLY2, cmd_end + us(4)),
        0x8200,
        "REPLY1 moved on"
    );
    assert_eq!(read(&mut pair[1], W_TXBUF_REPLY1, cmd_end + us(4)), 0);
    let done = cmd_end + us(16 + 96 + 266 + 96 + 32 * 4 + 8);
    run(&mut pair, cmd_end + us(4), done);
    let host = &mut pair[0];
    assert_eq!(
        read(host, W_IF, done) & 0x1000,
        0x1000,
        "IRQ12: exchange over"
    );
    assert_eq!(read(host, W_TXSTAT, done), 0x0B01, "ACK sent");
    assert_eq!(ram(host, 0x0100, done), 0x0001, "every client answered");
    assert_eq!(ram(host, 0x0102, done), 0x0000);
    assert_eq!(read(host, W_TXBUF_CMD, done), 0x0080);
    // The reply is in the host's ring after the CMD... the host's ring
    // has only the reply.
    let ring = RX_BEGIN & 0x1FFF;
    assert_eq!(ram(host, ring, done), 0x801E, "an MP reply");
    assert_eq!(ram(host, ring + 12 + 24, done), 0xC0DE);
    // The client kept the CMD, then the ACK listing no failures.
    let client = &mut pair[1];
    assert_eq!(ram(client, ring, done), 0x801C, "the CMD");
    // The ACK's 32 bytes are kept without their checksum.
    let ack = read(client, W_RXBUF_WRCSR, done) * 2 - 12 - 28;
    assert_eq!(ram(client, ack, done), 0x801D, "the ACK");
    assert_eq!(ram(client, ack + 12 + 26, done), 0, "nobody failed");
    assert_eq!(ram(client, 0x0404, done), 0x0001, "the reply was sent once");
}

#[test]
fn a_client_that_does_not_answer_is_reported() {
    // The CMD names clients 1 and 2; only 1 exists.
    let (mut pair, start) = mp_exchange(0x0006, 0x0100);
    let done = start + us(96 + 34 * 4 + 16 + 2 * (0x100 + 10) + 96 + 32 * 4 + 8);
    run(&mut pair, start, done);
    let host = &mut pair[0];
    assert_eq!(read(host, W_IF, done) & 0x1000, 0x1000);
    assert_eq!(ram(host, 0x0100, done), 0x0005, "failed");
    assert_eq!(ram(host, 0x0102, done), 0x0004, "client 2 failed");
    // A byte per client from 0x1D1.
    assert_eq!(read(host, W_CMD_STAT, done) >> 8, 0, "client 1 answered");
    assert_eq!(
        read(host, W_CMD_STAT + 2, done) & 0xFF,
        1,
        "client 2 did not"
    );
    assert_eq!(read(host, W_CMD_STAT + 2, done), 0, "reading clears it");
}

#[test]
fn a_client_without_a_queued_reply_answers_empty() {
    let (mut pair, start) = mp_exchange(0x0002, 0x0100);
    write(&mut pair[1], W_TXBUF_REPLY1, 0, start);
    let done = start + us(96 + 34 * 4 + 16 + 0x100 + 10 + 96 + 32 * 4 + 8);
    run(&mut pair, start, done);
    let host = &mut pair[0];
    assert_eq!(
        ram(host, 0x0100, done),
        0x0001,
        "an empty reply still counts"
    );
    let ring = RX_BEGIN & 0x1FFF;
    assert_eq!(ram(host, ring, done), 0x801F, "kept: W_RXFILTER bit 8");
}

#[test]
fn frames_wait_while_the_receiver_is_sending() {
    let mut pair = [station(HOST, HOST), station(CLIENT, HOST)];
    let start = us(WAKE_MICROS);
    for (wifi, to) in pair.iter_mut().zip([CLIENT, HOST]) {
        put_frame(
            wifi,
            0x0100,
            &ieee(0x0008, [to, HOST, HOST], &[1, 2, 3]),
            start,
        );
        write(wifi, W_TXBUF_LOC1, 0x8080, start);
        write(wifi, W_TXREQ_SET, 0x0001, start);
    }
    run(&mut pair, start, start + us(2000));
    for wifi in &mut pair {
        let now = start + us(2000);
        assert_eq!(read(wifi, W_TXSTAT, now), 0x0001);
        let kept = read(wifi, W_RXBUF_WRCSR, now) * 2 - (RX_BEGIN & 0x1FFF);
        assert_eq!(kept, 12 + 32, "the other's frame, received after sending");
    }
}

/// A console saved and loaded while it hears a CMD still answers it.
#[test]
fn a_state_saved_mid_frame_carries_on() {
    let (mut pair, start) = mp_exchange(0x0002, 0x0100);
    // Halfway through the CMD's 34 bytes.
    let saved = start + us(96 + 17 * 4);
    run(&mut pair, start, saved);
    let heard = read(&mut pair[1], W_IF, saved) & 0x0041;
    assert_eq!(heard, 0x0040, "receiving: started, not done");
    let bytes = postcard::to_stdvec(&pair[1]).expect("serializes");
    pair[1] = postcard::from_bytes(&bytes).expect("deserializes");
    let done = start + us(96 + 34 * 4 + 16 + 96 + 266 + 96 + 32 * 4 + 8);
    run(&mut pair, saved, done);
    let ring = RX_BEGIN & 0x1FFF;
    assert_eq!(ram(&mut pair[1], ring, done), 0x801C, "the CMD");
    assert_eq!(
        ram(&mut pair[0], 0x0100, done),
        0x0001,
        "the client answered"
    );
    assert_eq!(ram(&mut pair[0], ring + 12 + 24, done), 0xC0DE);
}
