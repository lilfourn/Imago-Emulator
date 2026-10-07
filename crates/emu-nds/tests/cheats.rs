//! Action Replay DS cheats against the DS core: button activators read
//! the keys the frame ran with, and writes land in main RAM.

use emu_core::{Button, Core, Input, Media};
use emu_nds::Nds;

/// The core as cheats see it.
struct Memory<'a>(&'a mut Nds);

impl emu_cheats::Memory for Memory<'_> {
    fn peek(&self, address: u32) -> Option<u8> {
        self.0.peek(0, address)
    }

    fn poke(&mut self, address: u32, value: u8) -> bool {
        self.0.poke(0, address, value)
    }
}

/// A cartridge whose ARM9 and ARM7 programs just loop.
fn rom() -> Vec<u8> {
    let mut rom = vec![0; 0x400];
    let mut put = |offset: usize, value: u32| {
        rom[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    };
    for (header, address, program) in [(0x20, 0x0200_4000, 0x200), (0x30, 0x0238_0000, 0x300)] {
        put(header, program);
        put(header + 4, address);
        put(header + 8, address);
        put(header + 12, 4);
        // B .
        put(program as usize, 0xEAFF_FFFE);
    }
    rom
}

fn word(nds: &Nds, address: u32) -> u32 {
    u32::from_le_bytes([0, 1, 2, 3].map(|byte| nds.peek(0, address + byte).unwrap_or(0)))
}

fn holding(buttons: &[Button]) -> Input {
    Input {
        buttons: buttons.iter().copied().collect(),
        touch: None,
    }
}

#[test]
fn a_button_activated_pointer_code_writes_main_ram() {
    // The shape of a Pokémon money code: if L+R, and the pointer is set,
    // write past it. The pointer is planted by the code's first line, in
    // the area the Action Replay keeps for itself.
    let text = "[Marker while L+R] ards\n\
                94000130 FCFF0000\n\
                023FE004 023FE100\n\
                623FE004 00000000\n\
                B23FE004 00000000\n\
                20000010 000000FF\n\
                D2000000 00000000\n";
    let cheats = emu_cheats::parse(emu_core::System::Nds, text).expect("valid");
    let rom = rom();
    let mut nds = Nds::new(&Media::new(&rom)).expect("valid header");

    nds.run_frame(&[holding(&[Button::L])], &mut Vec::new());
    emu_cheats::apply(&cheats, &mut Memory(&mut nds));
    assert_eq!(word(&nds, 0x023F_E004), 0, "L alone is not enough");

    nds.run_frame(&[holding(&[Button::L, Button::R])], &mut Vec::new());
    emu_cheats::apply(&cheats, &mut Memory(&mut nds));
    assert_eq!(word(&nds, 0x023F_E004), 0x023F_E100);
    assert_eq!(word(&nds, 0x023F_E110), 0xFF);
}

#[test]
fn x_and_y_activators_read_extkeyin() {
    // If Y is held (EXTKEYIN bit 1, active low), write.
    let text = "94000136 FFFD0000\n023FE200 00000001\nD2000000 00000000\n";
    let cheats = emu_cheats::parse(emu_core::System::Nds, text).expect("valid");
    let rom = rom();
    let mut nds = Nds::new(&Media::new(&rom)).expect("valid header");
    nds.run_frame(&[holding(&[Button::X])], &mut Vec::new());
    emu_cheats::apply(&cheats, &mut Memory(&mut nds));
    assert_eq!(word(&nds, 0x023F_E200), 0);
    nds.run_frame(&[holding(&[Button::Y])], &mut Vec::new());
    emu_cheats::apply(&cheats, &mut Memory(&mut nds));
    assert_eq!(word(&nds, 0x023F_E200), 1);
}
