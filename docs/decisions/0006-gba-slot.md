# 0006 — The DS GBA slot shares the GBA cartridge through its own crate

- Status: accepted
- Date: 2026-10-07
- Deciders: Luke Fournier

## Context

Pal Park in Diamond, Pearl and Platinum (and the Gen 3 checks in
HeartGold and SoulSilver) read a GBA game and its save from the DS's GBA
slot (PRD NDS-8, M15). The slot holds an ordinary GBA cartridge: ROM on a
16-bit bus, SRAM or Flash on an 8-bit bus, and the Pokémon clock on GPIO
(GBATEK, "DS Memory Control - Cartridges and Main RAM", "DS Cartridge GBA
Slot"). `emu-gba` already emulates that cartridge, but cores never depend
on each other.

Everything else around the slot was shaped for one cartridge per player:
`Media` carried one ROM and save, `Core::backup_generation(player)`
counted one save per player, and the app refused two ROMs for different
systems.

## Decision

1. **Sharing:** the cartridge unit (`cartridge.rs`, `backup.rs`, `rtc.rs`,
   which depend only on std and serde) moves to a new crate,
   `emu-gba-cart`, as `emu-psg` did for the sound generator. Its API is the
   `Cartridge` type (ROM, save memory, clock, write counter, ROM and save
   region accessors) and `CLOCK_HZ`, the 16.78 MHz clock its cycle counts
   use. `emu-gba` and `emu-nds` both depend on it; neither depends on the
   other. The GBA state layout and test ROM results do not change.
2. **Insertion is at session start**, not hot-plug. `Media` gains
   `second_slot: Option<Cartridge>` (ROM and save bytes): a cartridge in a
   second slot, the DS GBA slot. GB and GBA cores reject it;
   `emu_cores::second_slot` says which system's games fit.
3. **Backups are indexed by cartridge, not player.** For linked pairs
   cartridge `i` is still player `i`'s. On a DS, cartridge 0 is the game
   card and cartridge 1 the GBA slot's. `CoreInfo` gains nothing: the app
   already knows how many ROMs it loaded.
4. **Frontend:** `ds-emulator game.nds game.gba` puts the GBA game in the
   slot; File > Insert GBA Cartridge… (enabled only while a DS runs)
   restarts the session with one. Each cartridge keeps its own `.sav`, and
   loading refuses two games that would share one (`pokemon.nds` and
   `pokemon.gba` in one folder). Reopening a running session's game stops
   it first, so its saves are flushed before they are read again. Cheats,
   inputs and state names stay per player; the ROM CRC that identifies
   states covers the slot's ROM.

## DS behaviour

- EXMEMCNT bit 7 gives the slot to the ARM9 (0) or the ARM7 (1). The other
  CPU reads zeros and its writes are dropped, even with the slot empty, as
  GBATEK describes.
- ROM shows once at 0x08000000-0x09FFFFFF; past its end the bus returns
  the halfword address. 32-bit reads are two 16-bit reads.
- SRAM and Flash (commands, IDs, both 64 KiB banks of a 128 KiB chip) sit
  at 0x0A000000, repeating every 64 KiB. Wide accesses behave as in GBA
  mode: a read repeats the byte at the address and a write stores the byte
  lane at it. GBATEK's DS timing table charges a 32-bit access the same as
  an 8-bit one, which fits a single byte transfer. melonDS instead
  assembles consecutive bytes; Nintendo's library reads saves a byte at a
  time, so both agree on real games.
- The EEPROM is not wired up; the games Pal Park reads all save to Flash.
- An empty slot reads all ones. GBATEK lists address-dependent values for
  fast ROM timings, but Nintendo's library sets the 18-cycle timing, which
  reads high-Z, before probing the slot.
- Wait states come from the owning CPU's own EXMEM bits 0-4 (SRAM 10, 8,
  6 or 18 bus cycles; ROM first access the same, second 6 or 4), doubled
  for the ARM9.
- The cartridge clock counts DS bus cycles converted to 16.78 MHz.
- Direct boot copies the header fields the firmware leaves at 0x027FFC30
  (module ID, expansion IDs, maker and game code), as NitroSDK's
  `CTRDGModuleInfo` expects; an empty slot leaves module ID 0xFFFF.
- Nintendo's library tells a GBA game from an expansion pak by comparing
  the cartridge's Nintendo logo with the ARM9 BIOS's copy at 0xFFFF0020.
  The HLE BIOS ships no logo, so it holds the inserted cartridge's own,
  with the two debug-bit fields a retail header leaves clear cleared. A
  cartridge with those bits set fails, as on hardware.
- States carry the slot without its ROM; a state from a session with a
  different slot occupancy is refused. Reset keeps the cartridge.

## Consequences

- One cartridge implementation for two systems: a fix to Flash or the
  clock reaches both.
- `emu-gba-cart` is the second shared chip crate; the rule in
  ARCHITECTURE.md still holds (sharing stops at self-contained chips).
- Changing cartridges restarts the DS, losing unsaved progress.
- Pal Park itself is unverified: there are no GBA dumps in this project.
  Synthetic ROMs cover the bus, ownership, Flash and SRAM behaviour.

## Update, 2026-11-10: states are named after both games

Decision 4 said state names stay per player. For the slot they no longer
do: a DS game played with a cartridge names its states after both,
`game.emerald.ss1`, so the DS game played alone keeps its own states,
and so does each cartridge it is played with. A state left under the old
shared name still loads. The session's left-off picture is written at
`game.ss0.png` too, where the library looks for the DS game's.
