# 0002 — Game Boy timing: tick on every bus access

- Status: accepted
- Date: 2026-10-05
- Deciders: Luke Fournier

## Context

The research report recommends a cycle-timestamped event scheduler, and the
workspace has one (`emu-sched`). Game Boy accuracy tests (Mooneye, Blargg's
`mem_timing`) check what other components see at each M-cycle *inside* an
instruction: when a timer write lands, which cycle of a `CALL` pushes which
byte, when OAM DMA releases the bus.

## Decision

The Game Boy core does not use `emu-sched`. Every CPU memory access, and
every internal delay cycle, advances the timer, PPU, APU, serial port,
cartridge clock and OAM DMA by one M-cycle before the access completes
(`Bus::step_components`).

## Consequences

- M-cycle accuracy falls out of the structure: 107 of the first 139 test
  ROMs passed with no timing special cases beyond DMA bus conflicts.
- The cost is a function call per component per M-cycle, about 4 million a
  second at full speed. The release build runs the Game Boy many times
  faster than real time, so this does not matter.
- The GBA and DS cores, which run 16–67 MHz CPUs with wait states and long
  idle periods, will use `emu-sched` as planned.
