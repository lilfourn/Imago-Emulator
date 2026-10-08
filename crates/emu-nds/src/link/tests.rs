use emu_core::debug::{Breakpoint, StopReason};

use super::*;
use crate::hardware::Hardware;

/// A cartridge whose ARM9 and ARM7 programs both loop with `B .`.
fn rom() -> Vec<u8> {
    let mut rom = vec![0; 0x400];
    let mut put = |offset: usize, value: u32| {
        rom[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    };
    for (offset, value) in [
        (0x20, 0x200),
        (0x24, 0x0200_4000),
        (0x28, 0x0200_4000),
        (0x2C, 4),
    ] {
        put(offset, value);
    }
    for (offset, value) in [
        (0x30, 0x300),
        (0x34, 0x0238_0000),
        (0x38, 0x0238_0000),
        (0x3C, 4),
    ] {
        put(offset, value);
    }
    put(0x200, 0xEAFF_FFFE);
    put(0x300, 0xEAFF_FFFE);
    rom
}

fn linked() -> Linked {
    let rom = rom();
    Linked::new(&[Media::new(&rom), Media::new(&rom)]).expect("valid roms")
}

fn hw(linked: &mut Linked, player: usize) -> &mut Hardware {
    &mut linked.players[player].machine.hw
}

/// Writes a wifi register through the ARM7's bus.
fn wifi_write(hw: &mut Hardware, offset: u32, value: u16) {
    hw.arm7_write_16(0x0480_8000 + offset, value);
}

fn wifi_read(hw: &mut Hardware, offset: u32) -> u16 {
    hw.arm7_read_16(0x0480_8000 + offset)
}

/// Powers the console's radio and wakes it, with `mac` as its address,
/// in the network `bssid`, receiving into 0x4C00-0x5F60.
fn power_up(hw: &mut Hardware, mac: [u8; 6], bssid: [u8; 6]) {
    hw.arm7_write_16(0x0400_0304, 0x0003); // POWCNT2: sound and wifi
    wifi_write(hw, 0x036, 0); // W_POWER_US
    for (index, (mac, bssid)) in mac.chunks_exact(2).zip(bssid.chunks_exact(2)).enumerate() {
        let offset = 2 * index as u32;
        wifi_write(hw, 0x018 + offset, u16::from_le_bytes([mac[0], mac[1]]));
        wifi_write(hw, 0x020 + offset, u16::from_le_bytes([bssid[0], bssid[1]]));
    }
    wifi_write(hw, 0x006, 2); // W_MODE_WEP
    wifi_write(hw, 0x004, 1); // W_MODE_RST: wake
    for (offset, value) in [
        (0x050, 0x4C00), // W_RXBUF_BEGIN
        (0x052, 0x5F60), // W_RXBUF_END
        (0x056, 0x0600), // W_RXBUF_WR_ADDR
        (0x05A, 0x0600), // W_RXBUF_READCSR
        (0x030, 0x8001), // W_RXCNT
        (0x0D0, 0x0581), // W_RXFILTER
        (0x0E0, 0x0000), // W_RXFILTER2
        (0x0BC, 0x0006), // W_PREAMBLE: short
        (0x012, 0x0001), // W_IE: RX done
    ] {
        wifi_write(hw, offset, value);
    }
}

#[test]
fn the_second_console_gets_another_mac_address() {
    let linked = linked();
    let [first, second] = linked
        .players
        .each_ref()
        .map(|nds| nds.machine.hw.spi.mac_address());
    assert_ne!(first, second);
    assert_eq!(first[..5], second[..5]);
}

#[test]
fn a_linked_frame_takes_one_frame_on_both_consoles() {
    let mut linked = linked();
    linked.run_frame(&[], &mut Vec::new());
    let start = linked.players.each_ref().map(|nds| nds.cycles());
    linked.run_frame(&[], &mut Vec::new());
    for (nds, start) in linked.players.iter().zip(start) {
        assert_eq!(nds.cycles() - start, FRAME_CYCLES);
    }
    assert_eq!(linked.screens().len(), 4);
    assert_eq!(linked.info().player_for_screen(3), 1);
}

#[test]
fn a_frame_one_console_sends_reaches_the_other() {
    let mut linked = linked();
    linked.run_frame(&[], &mut Vec::new());
    let host = [0x00, 0x16, 0x56, 0x2D, 0x53, 0x01];
    for player in 0..2 {
        let mac = [0x00, 0x16, 0x56, 0x2D, 0x53, 0x01 + player as u8];
        power_up(hw(&mut linked, player), mac, host);
    }
    // Let both wake (2048 µs), then send a broadcast data frame from the
    // first console's LOC1 at 0x4100.
    linked.run_frame(&[], &mut Vec::new());
    let sender = hw(&mut linked, 0);
    let mut frame = vec![0u8; 12];
    frame[8] = 0x14;
    frame[0xA] = 24 + 4 + 4;
    frame.extend([0x08, 0x00, 0, 0]);
    frame.extend([0xFF; 6]);
    frame.extend([0x00, 0x16, 0x56, 0x2D, 0x53, 0x01]);
    frame.extend([0x00, 0x16, 0x56, 0x2D, 0x53, 0x01]);
    frame.extend([0, 0, 0x34, 0x12, 0, 0]);
    for (index, pair) in frame.chunks_exact(2).enumerate() {
        let value = u16::from_le_bytes([pair[0], pair[1]]);
        sender.arm7_write_16(0x0480_4100 + 2 * index as u32, value);
    }
    wifi_write(sender, 0x0A0, 0x8080); // W_TXBUF_LOC1
    wifi_write(sender, 0x0AE, 0x0001); // W_TXREQ_SET
    linked.run_frame(&[], &mut Vec::new());
    let receiver = hw(&mut linked, 1);
    assert_eq!(
        wifi_read(receiver, 0x010) & 0x0041,
        0x0041,
        "RX start and done"
    );
    assert_ne!(
        receiver.arm7.interrupts.flags & crate::interrupt::irq::WIFI,
        0
    );
    assert_eq!(receiver.arm7_read_16(0x0480_4C00), 0x8018, "RX header");
    assert_eq!(receiver.arm7_read_16(0x0480_4C00 + 12 + 24), 0x1234);
    assert_eq!(
        wifi_read(receiver, 0x054),
        0x0600 + (12 + 28) / 2,
        "W_RXBUF_WRCSR"
    );
    let sender = hw(&mut linked, 0);
    assert_eq!(wifi_read(sender, 0x054), 0x0600, "nothing comes back");
    assert_eq!(wifi_read(sender, 0x0B8), 0x0001, "W_TXSTAT: sent");
}

#[test]
fn linked_states_round_trip_and_carry_on_the_same() {
    let mut linked = linked();
    linked.run_frame(&[], &mut Vec::new());
    power_up(hw(&mut linked, 0), [2; 6], [2; 6]);
    linked.run_frame(&[], &mut Vec::new());
    let mut state = Vec::new();
    linked.save_state(&mut state);
    // Loading marks the saves as changed, so both load it.
    let mut other = self::linked();
    other.load_state(&state).expect("loads");
    linked.load_state(&state).expect("loads");
    for core in [&mut linked, &mut other] {
        core.run_frame(&[], &mut Vec::new());
    }
    let (mut a, mut b) = (Vec::new(), Vec::new());
    linked.save_state(&mut a);
    other.save_state(&mut b);
    assert!(a == b, "the loaded pair ran differently");
    assert!(other.load_state(&state[..state.len() / 2]).is_err());
}

#[test]
fn a_breakpoint_on_the_second_console_stops_it_and_the_frame_resumes() {
    let (mut plain, mut debugged) = (linked(), linked());
    assert_eq!(debugged.cpus().len(), 4);
    assert_eq!(debugged.cpus()[3].player, 1);
    let stops = Stops {
        breakpoints: vec![Breakpoint {
            cpu: 3,
            address: 0x0238_0000,
        }],
        ..Stops::default()
    };
    let outcome = debugged.run_until(&[], &mut Vec::new(), &stops);
    assert_eq!(
        outcome.stop.map(|stop| (stop.cpu, stop.reason)),
        Some((3, StopReason::Breakpoint))
    );
    debugged.run_frame(&[], &mut Vec::new());
    plain.run_frame(&[], &mut Vec::new());
    let (mut a, mut b) = (Vec::new(), Vec::new());
    plain.save_state(&mut a);
    debugged.save_state(&mut b);
    assert!(a == b, "the stop changed the emulation");
}
