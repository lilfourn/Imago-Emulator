# 0007 — DS local wireless: the MAC on a shared clock, frames on a perfect air

- Status: proposed
- Date: 2026-10-07
- Deciders: Claude, for Luke Fournier's review

## Context

Milestone M16 ends with a trade between two DS Pokémon games over local
wireless (PRD NDS-10). Nintendo's games talk through the WM library in
their ARM7 program, which drives the Wi-Fi chip on its own registers at
0x04808000 and 8 KiB of MAC RAM at 0x04804000 (GBATEK, "DS Wireless
Communications"). Local multiplayer uses the chip's multiplayer (MP)
mode: a host sends a CMD frame naming its clients, each client's hardware
answers in a time slot of its own with a reply the game queued in advance,
and the host's hardware sends an ACK listing who failed. The slots are
tens of microseconds wide, so the two consoles must agree on time to well
under that. Beacons, authentication and association come first, as
ordinary 802.11 frames.

[Decision 0004](0004-linked-cores.md) already runs linked Game Boys and
GBAs as one two-player core in lockstep on one thread. The DS core keeps
time in bus cycles (33.51 MHz) on a scheduler shared by both CPUs; the
Wi-Fi chip counts microseconds from its own 22 MHz crystal.

melonDS, the reference, ticks its Wi-Fi model every 8 µs and moves frames
between emulator processes through shared memory, with extra
synchronisation for MP clients. Its code is GPL-3; this project reads it
for understanding but does not copy it (PRD §9).

## Decision

1. **The MAC is a device in `emu-nds` (`wifi/`), driven by deadlines, not
   ticks.** Counters (W_US_COUNT, W_CMD_COUNT, W_CONTENTFREE) are worked
   out from the bus clock when read. Everything that happens at a time —
   each millisecond boundary of W_US_COUNT, the pre-beacon IRQ15, the end
   of a preamble or of a frame's bytes, a frame's arrival, the end of the
   2048 µs wake-up — is a deadline. The device asks the scheduler for one
   `Event::Wifi` at its earliest deadline, and catches up to the CPU's
   time before every register access, so reads always see the right
   moment. An idle or unpowered radio schedules nothing, which keeps
   single-console speed and behaviour unchanged where games leave Wi-Fi
   off. A microsecond is exactly 33.513982 bus cycles; conversions round
   so that a count reaches `n` at the first cycle on or after `n` µs.
2. **Frames travel as `Transmission`s**: the sender's 12-byte TX header and
   IEEE frame, the bus cycle its data started (IRQ7, after the preamble),
   and for MP replies the client slot it answers in. The sender's
   hardware fills in what the real chip does (sequence numbers, the
   beacon's timestamp, MP ACKs and empty replies). The receiver writes the
   frame into its RX ring a halfword at a time as the microseconds pass,
   runs GBATEK's RX filter at the end, and raises IRQ6/IRQ0. MP clients
   answer CMD frames in their slot without the game, and beacons from the
   receiver's network set its W_US_COUNT, as the hardware does.
3. **The air is perfect.** Every frame reaches every listener: no
   collisions, no channels, no lost frames and no ACK exchange — unicast
   frames always count as delivered (TXSTAT and TX header say so). A frame
   that arrives while the receiver is busy waits for it. This is what
   melonDS does too, and it keeps the protocol layer, which retries on its
   own, out of trouble that emulation would otherwise invent.
4. **`emu_nds::Linked` is two consoles on one clock.** Both power on
   together, so their bus clocks agree; the core runs them in alternating
   slices and, after each console's slice, hands its transmissions to the
   other, which receives each at its own start time. While either radio
   has power the slices are 512 bus cycles (about 15 µs), well under the
   96 µs preamble that precedes every frame's data, so a frame from the
   second console reaches the first at most a slice late and only its
   IRQ6 moves; where it lands and when it ends do not. While neither radio
   has power each console runs its whole frame at once. Frames end at
   VBlank, which both reach together.
5. **Each console has its own MAC address.** The synthesised firmware's
   wifi settings now hold a DS Lite's values (GBATEK's port values after
   initialisation, its baseband and radio tables); if both firmwares give
   the same address, the second console's last byte goes up by one and the
   settings' CRC is recomputed.
6. **The rest follows decision 0004.** CPUs 0-1 are the first console's
   ARM9 and ARM7 and 2-3 the second's; screens 0-1 and 2-3 likewise;
   cartridge `i` is player `i`'s game card; save states are the two
   machines in order. The app opens two DS games as a linked pair (File >
   Connect Second Console…, or two ROMs on the command line) and shows the
   consoles side by side, each laid out on its own (stacked by default);
   each has its own touchscreen.

## Consequences

- Deterministic: the same media and inputs give the same frames, sound and
  states for the pair, so rewind, save states and scripted runs work.
- Single-console behaviour changes only where a game touches Wi-Fi. Before
  this, ARM7 accesses to 0x04800000-0x0480FFFF fell through to the I/O page
  and aliased registers such as IF and the timers; Pokémon White tests and
  initialises the chip at boot, so its timing after boot shifts slightly.
- No internet, no DS Download Play server and no more than two consoles.
  A third console would need the carrying in `Linked` to become a loop
  over listeners; the device already queues frames by time.
- What a perfect air hides: games never see a lost frame or a busy channel,
  and the channel a game picks does not matter.
- The radio and baseband chips only remember their registers. A game that
  measured signal strength from them would see constant values.

## Verification

Two Pokémon White (USA, Europe) games, each with a save made at the
C-Gear and different start times on their clocks, ran through
`examples/link_probe.rs` without cheats. Both C-Gears light WIRELESS and
report the other console; in the Pokémon Center's Union Room each player
sees the other's avatar, they talk, and a Tepig and a Patrat trade both
ways, after which each party holds the other's Pokémon. The same linked
run twice gives byte-identical states. The pair runs at about 1.6× full
speed headless on an M5 Max, against 3.7× for one console, whose speed
is unchanged. Platinum, HeartGold/SoulSilver, battles and DS Download
Play are untried.
