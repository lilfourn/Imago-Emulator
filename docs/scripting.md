# Lua scripting

Scripts are Lua 5.4 files that run alongside a game, in the spirit of
BizHawk's, mGBA's and DeSmuME's scripts. They read and write memory,
press buttons, save and load states, and draw text and boxes over the
screens. The `emu-script` crate runs them (PRD UI-14).

```lua
-- hud.lua: the frame count, and a byte of Game Boy work RAM.
emu.on_frame(function()
    gui.text(2, 2, "frame " .. emu.frame())
    gui.text(2, 10, ("C000: %02X"):format(memory.read_u8(0xC000)), "yellow")
end)
```

Run one from File > Run Script… (⌘⇧L), and stop it with File > Stop
Script (⌘⌥L). From the command line:

```sh
ds-emulator --script hud.lua game.gb               # with the first game to start
emu-headless --frames 600 --script hud.lua game.gb # without a window
```

[`scripts/lua/`](../scripts/lua/) has two examples: `frame-counter.lua`
shows the frame count and a byte of RAM, and `input-macro.lua` plays a
macro of button presses.

## How a script runs

A script runs on the emulation thread, between frames, so the machine
stands still while it looks. Its main body runs once when it starts.
After that, every frame it:

1. carries on from wherever its main body waits in `emu.frameadvance()`;
2. calls each `emu.on_frame` callback, in the order they were added.

So a script is either a set of callbacks or a loop:

```lua
-- Callbacks:
emu.on_frame(function() gui.text(0, 0, emu.frame()) end)

-- A loop, as BizHawk scripts are written:
while true do
    gui.text(0, 0, emu.frame())
    emu.frameadvance()
end
```

The frame callbacks run after the cheats, so a script sees what the
cheats wrote and can override it. A script whose main body has ended and
which left no callbacks is finished, and stops.

Players, screens and CPUs are numbered from 1, as Lua numbers things,
and each defaults to 1: the first player, the top screen of a DS, the
DS's ARM9.

A script stops with its game: opening another game, or linking a second
console, stops it. The frontend's rewind and state loading don't reach
the script: it keeps its variables and its `emu.frame()` count.

## Functions

### `emu`

| Function | What it does |
|---|---|
| `emu.frame()` | Frames finished since the script started: 0 in the main body before any frame, 1 in the first frame's callbacks. |
| `emu.system()` | `"gb"` (Game Boy or Game Boy Color), `"gba"` or `"nds"`. |
| `emu.frameadvance()` | Waits for the next frame. Only in the main body; a callback that calls it fails. |
| `emu.on_frame(fn)` | Calls `fn()` after every frame. |
| `emu.on_exec(address, fn [, cpu])` | Calls `fn(address, cpu)` each time the CPU is about to run the instruction at `address`, mid-frame. |
| `emu.registers([cpu])` | The CPU's registers as a table of names to values, as the debugger names them: `R0` to `R12`, `SP`, `LR`, `PC`, `CPSR` and `SPSR` on ARM CPUs, `AF`, `BC`, `DE`, `HL`, `SP` and `PC` on the Game Boy. |
| `emu.pause()` | Pauses after this frame (in emu-headless, ends the run). |
| `emu.save_state(slot)` | Saves a state to slot 1–9: the same `game.ss1` to `game.ss9` files as ⌘S, in memory in emu-headless. |
| `emu.load_state(slot)` | Loads one. |

Saving, loading and pausing happen once the script's call returns,
before the next frame: memory read after `emu.load_state` in the same
callback is still the old.

`emu.on_exec` stops at the instruction's address, without the Thumb bit,
as a debugger breakpoint does, and runs the frame instruction by
instruction while it waits, as the debugger does. An instruction that
starts a frame is not caught.

### `memory`

| Function | What it does |
|---|---|
| `memory.read_u8(address [, cpu])` | The byte at `address`, or `nil` where it can't be read. |
| `memory.read_u16(address [, cpu])` | Two bytes, little-endian. |
| `memory.read_u32(address [, cpu])` | Four bytes, little-endian. |
| `memory.write_u8(address, value [, cpu])` | Writes a byte; returns whether it took. |
| `memory.write_u16(address, value [, cpu])` | Writes two bytes. |
| `memory.write_u32(address, value [, cpu])` | Writes four bytes. |

Memory is as the CPU sees it, read and written through the debugger's
side-effect-free access: reads reach RAM, video memory, ROM and the I/O
registers whose reads change nothing; writes reach RAM, video memory,
palettes and OAM, never ROM or I/O registers. The `cpu` argument picks the
DS's ARM7 (2) over its ARM9 (1); in link play the second player's CPUs
come after the first's. Writes keep the low bits of `value`, so `-1` is all ones.

### `joypad`

| Function | What it does |
|---|---|
| `joypad.get([player])` | The buttons the last frame ran with, as a table: `{a = true, b = false, ...}`. |
| `joypad.set(buttons [, player])` | Sets buttons for the next frame only: `true` presses one, `false` lets it go, whatever the player holds; buttons not named stay as the player has them. |

The buttons are `a`, `b`, `x`, `y`, `l`, `r`, `up`, `down`, `left`,
`right`, `start` and `select`, in any case. Call `joypad.set` every frame
a button should stay held. When one call both presses and releases a
button, the release wins.

### `gui`

| Function | What it does |
|---|---|
| `gui.text(x, y, text [, color [, screen]])` | Writes `text`, eight screen pixels tall, its top-left corner at `x`, `y`. White by default. |
| `gui.box(x1, y1, x2, y2 [, fill [, outline [, screen]]])` | A box with corners `x1, y1` and `x2, y2`, both inside it. No fill by default; the outline is the fill's colour, or white without a fill. |

Positions are in the screen's own pixels (0–255 across a DS screen), and
the drawing is scaled with it. A script draws afresh every frame: what it
drew shows until the next frame's drawing replaces it, so drawing once
shows for one frame. Colours are `0xRRGGBB`, `0xAARRGGBB` (`0x80000000`
is half-transparent black), `"#RRGGBB"`, `"#RRGGBBAA"`, a name (`white`,
`black`, `red`, `green`, `blue`, `yellow`, `cyan`, `magenta`, `orange`,
`purple`, `gray`) or `"clear"`; `false` draws nothing.

Drawing is not part of screenshots or recordings, which are of the
console's own screens. On turned screens text stays upright.

### `console`

`console.log(...)` and `print(...)` write their arguments as Lua's
`print` does, separated by tabs. In the app, each line goes to stdout
after the script's name, and the latest shows as a notice. In
emu-headless, lines go to stdout as they are, before its own `key=value`
lines, so a test script can print `key=value` lines of its own.

## Sandbox

Scripts get Lua's base library, `coroutine`, `table`, `string`, `utf8`
and `math`, but no `io`, `os`, `require`/`package` or `debug`, so they
cannot read or write files, start programs or read the clock. `dofile`,
`loadfile` and `string.dump` are removed, and `load` reads source text
only, never compiled chunks. `math.random` starts from the same seed
every run.

So the same script with the same game and input runs the same way every
time, frame for frame. One exception is Lua's own: the order `pairs`
visits a table's string keys can differ between runs, so sort the keys
when the order matters (as `input-macro.lua` does).

A script may use 64 MiB of memory and run 100 million Lua instructions
each time it is called, well under a second; a script stuck in a loop is
stopped, even inside `pcall`. Any error stops the script, and the app
shows it as a notice naming the script and line, such as
`hud.lua:3: attempt to index a nil value (global 'player') · script
stopped`; the game runs on. emu-headless prints it and exits with status 1.
