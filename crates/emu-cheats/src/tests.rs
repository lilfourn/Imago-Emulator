//! Whole cheat lists, read and applied.

use emu_core::{RomPatch, System};

use crate::tea::Cipher;
use crate::test_memory::TestMemory;
use crate::{Device, Memory, apply, parse, rom_patches};

/// Reads `text` for `system` and applies it once to `memory`.
fn run(system: System, text: &str, memory: &mut TestMemory) {
    let cheats = parse(system, text).unwrap_or_else(|error| panic!("{error}"));
    apply(&cheats, memory);
}

/// The devices `text` reads as on `system`.
fn devices(system: System, text: &str) -> Vec<Device> {
    let cheats = parse(system, text).unwrap_or_else(|error| panic!("{error}"));
    cheats.iter().map(|cheat| cheat.device).collect()
}

fn line(code: u64) -> String {
    format!("{:08X} {:08X}\n", code >> 32, code as u32)
}

#[test]
fn reads_names_flags_and_devices() {
    let text = "# money\n[Money]\n82025E30 270F\n\n[Moon jump] off\n02000F04:01\n";
    let cheats = parse(System::Gba, text).expect("valid list");
    let summary: Vec<_> = cheats
        .iter()
        .map(|cheat| (cheat.name.as_str(), cheat.enabled, cheat.device))
        .collect();
    assert_eq!(
        summary,
        [
            ("Money", true, Device::CodeBreaker),
            ("Moon jump", false, Device::Raw)
        ]
    );
    let mut memory = TestMemory::default();
    apply(&cheats, &mut memory);
    assert_eq!(memory.half(0x0202_5E30), 0x270F);
    assert_eq!(memory.peek(0x0200_0F04), Some(0));
}

#[test]
fn errors_name_the_line_and_the_device() {
    let error = parse(System::Gba, "[Money]\nnot a code\n").expect_err("bad code");
    assert_eq!(error.line, 2);
    let error = parse(System::Nds, "[A]\n02000000 00000001\nC4000000 00000000\n").expect_err("C4");
    assert_eq!(
        (error.line, error.device),
        (3, Some(Device::ActionReplayDs))
    );
    assert_eq!(
        error.to_string(),
        "line 3 (Action Replay DS): code type C4 is not supported yet"
    );
    let error = parse(System::Gba, "[A] ards\n02000000 00000001\n").expect_err("wrong system");
    assert!(error.to_string().contains("Nintendo DS"), "{error}");
    let error = parse(System::Gb, "[A]\n00A-17B-C49\n00A-177\n").expect_err("not ROM");
    assert_eq!((error.line, error.device), (3, Some(Device::GameGenie)));
}

#[test]
fn applies_enabled_cheats_and_conditions() {
    let text = "[On]\n02000000:1234\n[Off] off\n02000010:FF\n\
                [Guarded]\n72000000 0000\n32000020 0055\n72000000 1234\n32000021 0066\n";
    let mut memory = TestMemory::default();
    run(System::Gba, text, &mut memory);
    assert_eq!(memory.half(0x0200_0000), 0x1234);
    assert_eq!(memory.peek(0x0200_0010), Some(0));
    // The first condition fails, so its write is skipped; the second passes.
    assert_eq!(memory.peek(0x0200_0020), Some(0));
    assert_eq!(memory.peek(0x0200_0021), Some(0x66));
}

#[test]
fn code_breaker_modifies_in_place_and_mixes_with_patches() {
    let mut memory = TestMemory::default();
    memory.set_word(0x0200_0000, 0xFFFF_000F);
    run(
        System::Gba,
        "22000000 00F0\n02000004:AB\nE2000002 0002\n",
        &mut memory,
    );
    assert_eq!(memory.half(0x0200_0000), 0x00FF);
    assert_eq!(memory.half(0x0200_0002), 0x0001);
    assert_eq!(memory.peek(0x0200_0004), Some(0xAB));
}

#[test]
fn code_breaker_encryption_carries_to_later_cheats() {
    let mut memory = TestMemory::default();
    run(
        System::Gba,
        "[Master code]\n9123ABCD 0001\n[Write]\n5A538F9F 1B53\n",
        &mut memory,
    );
    assert_eq!(memory.half(0x0200_38D0), 0xA805);
}

#[test]
fn code_breaker_joypad_codes_read_keyinput() {
    // If A and B are held, write.
    let text = "D0000020 0003\n82000000 0001\n";
    let mut memory = TestMemory::default();
    run(System::Gba, text, &mut memory);
    assert_eq!(memory.half(0x0200_0000), 0);
    memory.keys = 0x03FC;
    run(System::Gba, text, &mut memory);
    assert_eq!(memory.half(0x0200_0000), 1);
}

#[test]
fn encrypted_game_shark_advance_codes_are_detected() {
    let cipher = Cipher::GAMESHARK_ADVANCE;
    let text = format!(
        "[Master code]\n{}{}[Money]\n{}",
        line(cipher.encrypt(0x4241_5850_001D_C0DE)),
        line(cipher.encrypt(0xF800_0100_0000_0001)),
        line(cipher.encrypt(0x1200_0010_0000_270F)),
    );
    assert_eq!(
        devices(System::Gba, &text),
        [Device::GameSharkAdvance, Device::GameSharkAdvance]
    );
    let mut memory = TestMemory::default();
    run(System::Gba, &text, &mut memory);
    assert_eq!(memory.half(0x0200_0010), 0x270F);
}

#[test]
fn encrypted_pro_action_replay_codes_are_detected() {
    let cipher = Cipher::PRO_ACTION_REPLAY;
    let text = format!(
        "[Money]\n{}{}",
        line(cipher.encrypt(0x0420_0010_0000_270F)),
        line(cipher.encrypt(0x0220_0020_0001_0063)),
    );
    assert_eq!(devices(System::Gba, &text), [Device::ProActionReplay]);
    let mut memory = TestMemory::default();
    run(System::Gba, &text, &mut memory);
    assert_eq!(memory.word(0x0200_0010), 0x270F);
    assert_eq!(memory.word(0x0200_0020), 0x0063_0063);
}

#[test]
fn gba_rom_patches_count_as_codes_and_patch_halfwords() {
    let patch = [
        RomPatch {
            address: 0x0800_2468,
            value: 0x78,
            compare: None,
        },
        RomPatch {
            address: 0x0800_2469,
            value: 0x56,
            compare: None,
        },
    ];
    // GameShark Advance: if [02000000] = 1, run the next code (the ROM
    // patch); then write.
    let cipher = Cipher::GAMESHARK_ADVANCE;
    let text = format!(
        "[Patch]\n{}{}{}",
        line(cipher.encrypt(0xD200_0000_0000_0001)),
        line(cipher.encrypt(0x6000_1234_0000_5678)),
        line(cipher.encrypt(0x1200_0010_0000_270F)),
    );
    assert_eq!(devices(System::Gba, &text), [Device::GameSharkAdvance]);
    let cheats = parse(System::Gba, &text).expect("valid");
    let mut memory = TestMemory::default();
    apply(&cheats, &mut memory);
    // The failed condition skipped the patch's code, not the write.
    assert_eq!(memory.half(0x0200_0010), 0x270F);
    // Conditions don't gate ROM patches.
    assert_eq!(rom_patches(&cheats), patch);

    // Pro Action Replay v3: the value is on the second line.
    let text = "[Patch]\n00000000 18001234\n00005678 00000000\n";
    assert_eq!(devices(System::Gba, text), [Device::ProActionReplayRaw]);
    let cheats = parse(System::Gba, text).expect("valid");
    assert_eq!(rom_patches(&cheats), patch);
    let off = parse(System::Gba, &text.replace("[Patch]", "[Patch] off")).expect("valid");
    assert!(rom_patches(&off).is_empty());
}

#[test]
fn pro_action_replay_io_writes_store_aligned_halfwords() {
    let cipher = Cipher::PRO_ACTION_REPLAY;
    let text = format!(
        "[Brighten]\n{}{}",
        line(cipher.encrypt(0xC600_0051_0000_00FF)),
        line(cipher.encrypt(0xC700_00D6_1234_5678)),
    );
    assert_eq!(devices(System::Gba, &text), [Device::ProActionReplay]);
    let mut memory = TestMemory::default();
    run(System::Gba, &text, &mut memory);
    // As the CPU aligns stores: the halfword to 0x50, the word to 0xD4.
    assert_eq!(
        memory.io,
        [
            (0x0400_0050, 0x00FF),
            (0x0400_00D4, 0x5678),
            (0x0400_00D6, 0x1234)
        ]
    );
    // I/O writes are memory operations, not ROM patches.
    let cheats = parse(System::Gba, &text).expect("valid");
    assert!(rom_patches(&cheats).is_empty());
}

#[test]
fn unencrypted_codes_are_detected_and_tags_override() {
    assert_eq!(
        devices(
            System::Gba,
            "[GSA]\n12000010 0000270F\n[PAR]\n04200010 0000270F\n"
        ),
        [Device::GameSharkAdvanceRaw, Device::ProActionReplayRaw]
    );
    assert_eq!(
        devices(System::Gba, "[Forced] gsa-raw\n04200010 0000270F\n"),
        [Device::GameSharkAdvanceRaw]
    );
    let error = parse(System::Gba, "[Junk]\n0F123456 12345678\n").expect_err("implausible");
    assert!(error.message.contains("device tag"), "{error}");
}

/// The codes are encrypted with an independent transcription of GBATEK's
/// rounds and DEADFACE reseeding, over mGBA's tables: the master code,
/// `DEADFACE 00001234`, then `12000010 0000270F` under the new seeds.
#[test]
fn deadface_reseeds_the_codes_after_it() {
    let text = "[Seeds]\nD41A4D50 EF6B965D\n70BDB80D 69F37FCB\n[Money]\n42F38FDA B792A396\n";
    let mut memory = TestMemory::default();
    run(System::Gba, text, &mut memory);
    assert_eq!(memory.half(0x0200_0010), 0x270F);
}

#[test]
fn ambiguous_lengths_ask_for_a_tag() {
    let text = "[Both]\n82025E30 270F 82025E32 0001 82025E34 0002 82025E36 0003\n";
    let error = parse(System::Gba, text).expect_err("ambiguous");
    assert!(error.message.contains("[name] cb"), "{error}");
    assert_eq!(
        devices(System::Gba, &text.replace("[Both]", "[Both] cb")),
        [Device::CodeBreaker]
    );
}

#[test]
fn game_genie_codes_patch_rom_reads_while_on() {
    let text = "[Lives]\n00A-17B-C49\n[Jump] off\n3E1-23F\n[Both] gg\n01A-17B-C49\n02000000:05\n";
    let cheats = parse(System::Gb, text).expect("valid");
    assert_eq!(
        devices(System::Gb, text),
        [Device::GameGenie, Device::GameGenie, Device::GameGenie]
    );
    let patches = rom_patches(&cheats);
    assert_eq!(
        patches,
        [
            RomPatch {
                address: 0x4A17,
                value: 0x00,
                compare: Some(0xC8)
            },
            RomPatch {
                address: 0x4A17,
                value: 0x01,
                compare: Some(0xC8)
            },
        ]
    );
    // ROM patches don't run as memory writes; raw patches beside them do.
    let mut memory = TestMemory::default();
    apply(&cheats, &mut memory);
    assert_eq!(memory.peek(0x0200_0000), Some(5));

    // Switching cheats off or replacing the list drops their patches.
    let off = parse(System::Gb, &text.replace("[Lives]", "[Lives] off")).expect("valid");
    assert_eq!(rom_patches(&off).len(), 1);
    let replaced = parse(System::Gb, "[Jump]\n3E1-23F\n").expect("valid");
    assert_eq!(
        rom_patches(&replaced),
        [RomPatch {
            address: 0x0123,
            value: 0x3E,
            compare: None
        }]
    );
}

#[test]
fn game_boy_bank_codes_write_the_named_bank() {
    let mut memory = TestMemory::default();
    run(System::Gb, "[Bank]\n916310D0\n830200A0\n", &mut memory);
    assert_eq!(memory.banks.get(&(1, 0xD010)), Some(&0x63));
    assert_eq!(memory.banks.get(&(3, 0xA000)), Some(&0x02));
}

#[test]
fn ards_writes_add_the_offset() {
    let text = "[Writes]\n02000000 12345678\n12000010 0000ABCD\n22000020 000000EF\n\
                D3000000 00000100\n02000000 11111111\nD2000000 00000000\n";
    let mut memory = TestMemory::default();
    run(System::Nds, text, &mut memory);
    assert_eq!(memory.word(0x0200_0000), 0x1234_5678);
    assert_eq!(memory.half(0x0200_0010), 0xABCD);
    assert_eq!(memory.peek(0x0200_0020), Some(0xEF));
    assert_eq!(memory.word(0x0200_0100), 0x1111_1111);
}

#[test]
fn ards_button_activators_read_keyinput() {
    // L+R: KEYINPUT bits 8 and 9 clear, whatever the others.
    let text = "[Money] ards\n94000130 FCFF0000\n02000000 0001869F\nD2000000 00000000\n";
    let mut memory = TestMemory::default();
    for (keys, expected) in [(0x03FF, 0), (0x02FF, 0), (0x00FF, 99_999), (0x00F0, 99_999)] {
        memory.set_word(0x0200_0000, 0);
        memory.keys = keys;
        run(System::Nds, text, &mut memory);
        assert_eq!(memory.word(0x0200_0000), expected, "keys {keys:03X}");
    }
}

#[test]
fn ards_nested_ifs_keep_an_outer_false_block_skipped() {
    let text = "[Nested]\n\
                52000000 00000001\n\
                52000004 00000000\n02000008 000000AA\nD0000000 00000000\n\
                0200000C 000000BB\nD0000000 00000000\n\
                02000010 000000CC\n";
    let mut memory = TestMemory::default();
    run(System::Nds, text, &mut memory);
    assert_eq!(
        [0x08, 0x0C, 0x10].map(|offset| memory.word(0x0200_0000 + offset)),
        [0, 0, 0xCC]
    );
    memory.set_word(0x0200_0000, 1);
    run(System::Nds, text, &mut memory);
    assert_eq!(
        [0x08, 0x0C, 0x10].map(|offset| memory.word(0x0200_0000 + offset)),
        [0xAA, 0xBB, 0xCC]
    );
}

#[test]
fn ards_ifs_at_address_zero_read_the_offset() {
    let text = "D3000000 02000020\n50000000 0000002A\n00000004 00000001\nD2000000 00000000\n";
    let mut memory = TestMemory::default();
    run(System::Nds, text, &mut memory);
    assert_eq!(memory.word(0x0200_0024), 0);
    memory.set_word(0x0200_0020, 0x2A);
    run(System::Nds, text, &mut memory);
    assert_eq!(memory.word(0x0200_0024), 1);
}

#[test]
fn ards_loops_step_the_offset_and_data_registers() {
    let text = "[Fill]\nD5000000 00000010\nC0000000 00000003\n\
                D6000000 02000000\nD4000000 00000001\nD2000000 00000000\n";
    let mut memory = TestMemory::default();
    run(System::Nds, text, &mut memory);
    assert_eq!(
        [0, 4, 8, 12, 16].map(|offset| memory.word(0x0200_0000 + offset)),
        [0x10, 0x11, 0x12, 0x13, 0]
    );
}

#[test]
fn ards_next_restores_the_statuses_saved_by_for() {
    let text = "C0000000 00000001\n52000040 00000005\n02000044 00000001\n\
                D1000000 00000000\n02000048 00000007\nD2000000 00000000\n";
    let mut memory = TestMemory::default();
    run(System::Nds, text, &mut memory);
    assert_eq!(memory.word(0x0200_0044), 0);
    assert_eq!(memory.word(0x0200_0048), 7);
}

#[test]
fn ards_d2_resets_registers_and_each_cheat_starts_fresh() {
    let text = "[Registers]\nD3000000 02000100\nC0000000 00000000\nD1000000 00000000\n\
                00000000 00000001\nD2000000 00000000\nD5000000 00000009\n\
                D6000000 02000200\n\
                [Leaves an offset]\nD3000000 00000100\n\
                [Fresh]\n02000300 0000000A\n";
    let mut memory = TestMemory::default();
    run(System::Nds, text, &mut memory);
    // D1 kept the offset; D2 cleared it.
    assert_eq!(memory.word(0x0200_0100), 1);
    assert_eq!(memory.word(0x0200_0200), 9);
    // The previous cheat's offset did not carry over.
    assert_eq!(memory.word(0x0200_0300), 0xA);
    assert_eq!(memory.word(0x0200_0400), 0);
}

#[test]
fn ards_pointer_loads_and_null_checks() {
    // HG/SS-style: if the pointer is set, write a byte past it.
    let text = "62000300 00000000\nB2000300 00000000\n22000600 00000001\nD2000000 00000000\n";
    let mut memory = TestMemory::default();
    run(System::Nds, text, &mut memory);
    assert_eq!(
        memory.peek(0x0200_0600),
        Some(0),
        "a null pointer skips the write"
    );
    memory.set_word(0x0200_0300, 0x10);
    run(System::Nds, text, &mut memory);
    assert_eq!(memory.peek(0x0200_0610), Some(1));
}

#[test]
fn ards_patch_and_copy_codes() {
    let text = "[Patch]\nE2000700 0000000A\n44332211 88776655\n0000AA99 00000000\n\
                D3000000 02000700\nF2000800 00000004\nD2000000 00000000\n";
    let mut memory = TestMemory::default();
    run(System::Nds, text, &mut memory);
    let patched: Vec<u8> = (0x0200_0700..0x0200_070B)
        .map(|address| memory.peek(address).unwrap_or(0))
        .collect();
    assert_eq!(
        patched,
        [
            0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0
        ]
    );
    assert_eq!(memory.word(0x0200_0800), 0x4433_2211);
}

#[test]
fn ards_skipped_patches_skip_their_data_lines() {
    // The data line looks like D2, which would end the false block.
    let text = "52000000 00000001\nE2000900 00000008\nD2000000 00000000\n\
                02000B00 00000006\nD0000000 00000000\n02000A00 00000005\n";
    let mut memory = TestMemory::default();
    run(System::Nds, text, &mut memory);
    assert_eq!(memory.word(0x0200_0900), 0);
    assert_eq!(memory.word(0x0200_0B00), 0);
    assert_eq!(memory.word(0x0200_0A00), 5);
}

#[test]
fn ards_parse_errors() {
    for (text, line, needle) in [
        ("[A]\nC5000000 00010001\n", 2, "C5"),
        (
            "[A]\nE2000000 00000010\n00000000 00000000\n",
            2,
            "2 data lines",
        ),
        ("[A]\n02000000 00000001\n02000000:01\n", 3, "raw"),
        ("[A]\nD4000001 00000002\n", 2, "melonDS"),
        ("[A]\nDF000000 00000000\n", 2, "unknown"),
        ("[A]\n02000000 0000000\n", 2, "16 hex digits"),
    ] {
        let error = parse(System::Nds, text).expect_err(text);
        assert_eq!(error.line, line, "{error}");
        assert!(error.message.contains(needle), "{error}");
    }
}

/// Memory all of which reads as zero and takes writes, counting them.
#[derive(Default)]
struct Counting {
    pokes: u64,
    /// The last address written.
    last: u32,
}

impl Memory for Counting {
    fn peek(&self, _: u32) -> Option<u8> {
        Some(0)
    }

    fn poke(&mut self, address: u32, _: u8) -> bool {
        self.pokes += 1;
        self.last = address;
        true
    }
}

/// How many bytes applying `text` once writes.
fn pokes(system: System, text: &str) -> u64 {
    let cheats = parse(system, text).unwrap_or_else(|error| panic!("{error}"));
    let mut memory = Counting::default();
    apply(&cheats, &mut memory);
    memory.pokes
}

/// Four lines that copied all of main RAM on each of four billion turns:
/// the step limit let half a million of them through, forty minutes of
/// one frame.
#[test]
fn ards_copies_in_a_loop_stop_at_the_write_budget() {
    let budget = u64::from(crate::ops::WRITE_BUDGET);
    let copy = "D3000000 02000000\nC0000000 FFFFFFFF\nF2000000 00400000\n\
                D1000000 00000000\n";
    assert_eq!(pokes(System::Nds, copy), budget);
    // The same with an E code and its two lines of data.
    let patch = "C0000000 FFFFFFFF\nE2000000 00000010\n11111111 22222222\n\
                 33333333 44444444\nD1000000 00000000\n";
    assert_eq!(pokes(System::Nds, patch), budget);
    // Each cheat has a budget of its own.
    let two = format!("[A]\n{copy}[B]\n{copy}");
    assert_eq!(pokes(System::Nds, &two), budget * 2);
}

/// A copy of all of main RAM, the most one F code moves, is still whole,
/// and the codes after it still run.
#[test]
fn ards_one_copy_of_all_of_main_ram_fits_the_write_budget() {
    let text = "F2000000 00400000\n02000000 00000001\nF2000000 00000004\n\
                0200F000 00000001\n";
    let cheats = parse(System::Nds, text).expect("valid");
    let mut memory = Counting::default();
    apply(&cheats, &mut memory);
    // The copy and the write after it; the second copy has no budget
    // left, and ends the cheat before its last write.
    assert_eq!(memory.pokes, u64::from(crate::ops::WRITE_BUDGET) + 4);
    assert_eq!(memory.last, 0x0200_0003);
}

/// A cheat of many fills, each within the limit one fill has, used to
/// write them all, 128 KiB each.
#[test]
fn fills_and_slides_stop_at_the_write_budget() {
    // CodeBreaker type 4: 0xFFFF halfwords from 0x02000000, 64 times.
    let slides = "42000000 0000\n0000FFFF 0002\n".repeat(64);
    let cheats = parse(System::Gba, &format!("[Fill] cb\n{slides}")).expect("valid");
    let mut memory = Counting::default();
    apply(&cheats, &mut memory);
    assert_eq!(memory.pokes, u64::from(crate::ops::WRITE_BUDGET));
    // 32 fills fit whole. The 33rd has the 64 bytes left, and the cheat
    // ends with it.
    assert_eq!(memory.last, 0x0200_003F);
}

#[test]
fn ards_runaway_loops_stop() {
    let mut memory = TestMemory::default();
    run(
        System::Nds,
        "C0000000 FFFFFFFF\nD4000000 00000001\nD1000000 00000000\n",
        &mut memory,
    );
}
