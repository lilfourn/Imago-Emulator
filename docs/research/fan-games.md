# Fan-made Pokémon games

Researched 2026-11-02. This covers the famous fan-made Pokémon games, what
each one needs from the emulator, what was tested with the dumps on hand,
and what to build so that all of them run.

Fan games come in three kinds, and each reaches the app differently:

- **ROM hacks** are patches to a Nintendo game. The player applies one to
  their own dump. They run on the GB, GBC, GBA and DS cores, and the app
  already applies IPS, UPS, BPS and xdelta patches
  ([decision 0009](../decisions/0009-rom-hack-patches.md)).
- **Randomizers** are tools that rewrite the player's own dump: the Universal
  Pokémon Randomizer ZX and its successor FVX, and the randomizer modes built
  into some hacks.
- **PC fan games** are standalone games with engines of their own. Most are
  built with Pokémon Essentials on RPG Maker XP, which the emu-mkxp core runs
  through mkxp-z ([decision 0008](../decisions/0008-rpg-maker-games.md)). A
  few are web, Java or .NET games.

The PRD's legal policy (§9) still holds. The app may apply patches and run
randomizers on the player's dumps. It never fetches or links a ROM. That
includes hacks that ship as whole prebuilt ROMs (Polished Crystal, Orange),
and Pokémon Uranium, which was taken down by a DMCA notice. Online-only games
(PokeMMO, Pokémon Revolution Online) are out of scope.

## Summary

- **DS hacks should run as they are.** The test below made Renegade Platinum
  from the Platinum dump with the app's own patcher. It boots and plays its
  intro. Every famous DS hack is an xdelta patch on a game whose save the core
  already knows. None needs DSi mode, a real BIOS or extra RAM.
- **UPR ZX works today, outside the app.** Its command line randomized
  Platinum in under a second, and the result boots and plays the intro frame
  for frame the same as the clean game.
- **The biggest gap is the GBA clock for FireRed-based hacks.** The GBA core
  turns on the cartridge clock by game code, and FireRed has none. Radical
  Red, Unbound and other hacks built on the Complete FireRed Upgrade (CFRU)
  engine read the clock. Without it they show "Enable the Real Time Clock"
  and fall back to a fixed date. mGBA has a rule for exactly this case.
- **Most PC fan games use Pokémon Essentials v19 or later**, which runs on
  mkxp-z's Ruby 3 natively. They should need far less work than This Gym of
  Mine (Essentials v18, Ruby 1.8). Older Ruby 1.8 games (Insurgence,
  Zeta/Omicron, old Xenoverse) need a general fix rather than one preload
  per game.

## What was tested

Everything ran on emu-headless built from a clean export of `ee6d917` (the
`main` of 2026-11-02). Inputs came from Lua scripts that tap A and Start. The
working files are in `~/Library/Caches/ds-emulator-fan-research`, with the
scripts in its `scripts/` folder.

| Test | Result |
|---|---|
| UPR ZX 4.6.1 command line on OpenJDK 26, Platinum (USA) (Rev 1), with random starters, wild Pokémon and trainers | It wrote a 104.6 MB ROM and a spoiler log in 0.3 s. The ROM boots. At frame 24,000 of the intro, both screens match the clean game's exactly (CRC32 `7216ef5e`, `d877577b`). |
| Renegade Platinum v1.3.0, `RenegadePlatinum4997.xdelta`, applied by `emu_patch::apply` to the same dump | 104.9 MB in 0.13 s, SHA-1 `d4ee068b…`. Boots and plays the intro. |
| Renegade's `SpeedUpPatch.xdelta`, applied on top of that result | It applied in 0.04 s. Text visibly runs faster in the intro. |
| `RenegadePlatinum3541.xdelta`, which is for Rev 0, applied to the Rev 1 dump | Refused: "the patched game came out wrong, so the patch was made from a different ROM". That is correct, but the message doesn't say which patch to use instead. |

What these tests couldn't reach:

- **Anything past the intro's "touch a button" prompt.** Lua scripts can't
  touch the screen, so a script can't answer it.
- **Volt White and randomized White.** The library's Pokémon White entry
  points at `~/Downloads/…White Version….nds`, which is no longer on disk.
- **GB, GBC and GBA hacks.** There are no dumps of their base games.

UPR ZX's command line needs a settings file (`.rnqs`). Its GUI writes one.
For the test, a 10-line Java program wrote one through the jar's own
`Settings` class: `scripts/MakeSettings.java`. FVX can take a settings string
and a seed on its command line instead.

## Gaps, by how many games they block

### 1. A clock for FireRed-based GBA hacks (small)

[`rtc::present`](../../crates/emu-gba-cart/src/rtc.rs#L31) gives the clock
only to Ruby, Sapphire, Emerald and Boktai, by header game code. Hacks keep
their base game's code, so every FireRed (BPRE) or LeafGreen (BPGE) hack gets
no clock. The save type is already right, because BPR and BPG are in the
save table.

Hacks this breaks:

- **CFRU hacks: Radical Red, Unbound and Inflamed Red.** CFRU carries
  Emerald's real clock driver and probes it at boot. When the probe fails, it
  shows "Enable the Real Time Clock" and uses 2000-01-01, so day/night and
  daily events stop working. Unbound also checks for a clock that runs
  backwards. (CFRU source: `src/rtc.c`, `strings/overworld_strings.string`.)
- **Old hacks with clock routines of their own: Ash Gray, Liquid Crystal
  and Adventure Red Chapter.** These write the GPIO data register without
  setting pin directions. On a real cartridge that wouldn't work, but VBA
  allowed it ([mGBA #971](https://github.com/mgba-emu/mgba/issues/971)).

mGBA's rule (`GBAOverrideApplyDefaults` in
[overrides.c](https://github.com/mgba-emu/mgba/blob/master/src/gba/overrides.c),
since 0.6.1):

- **When it fires:** the Game Freak header at 0x108 reads
  `pokemon red version` (FireRed) or `pokemon emerald version`, or the game
  code is AXVE, and the ROM's CRC32 is not one of 17 clean dumps (FireRed
  1.0 is `DD88761C`, 1.1 is `84EE4776`).
- **What it turns on:** Flash 128K, the clock, and "VBA bug compat", where a
  write to the GPIO data register drives every pin whatever the direction
  register says.

The fix is to copy that rule, add LeafGreen (`pokemon green version`), and
add the VBA behaviour as a flag on the clock. A clock nobody uses is harmless:
until a game turns on the GPIO port's read enable, its addresses read as ROM.

mGBA's author on why there is no smarter detection: "basically impossible to
detect which ROM hacks will use the RTC ahead of time" (#971). Mesen2 scans
the ROM for the `SIIRTC_V001` string instead, which misses CFRU: there the
string appears only in a comment.

### 2. Name the right patch when the base is wrong (small)

- **The patch's own description.** Delta Patcher stores a description in
  the xdelta application header, as base64. Renegade's says "Use this patch
  to convert a 4997/4998 Platinum ROM (or a later release cart dump) into
  Renegade Platinum." emu-patch skips the application header
  ([vcdiff.rs](../../crates/emu-patch/src/vcdiff.rs#L14)); decoding it and
  showing it with a mismatch error would point the player straight at the
  right patch.
- **A table of known bases.** A short table of SHA-1s would let the app say
  which dump the player has: "Platinum (USA) Rev 1". The bases worth listing
  are FireRed 1.0/1.1, Emerald, Ruby 1.0/1.1, Crystal 1.0/1.1, Red, Yellow,
  Platinum Rev 0/1, HG/SS, B/W and B2/W2.
- **Stacking patches already works.** Add-on patches go on top of a hack
  (Renegade's Classic, SpeedUp and shiny-rate patches). The base picker
  takes any file, including a game in the Hacks folder.

### 3. Randomize from the app (done)

Done on 2026-11-03: File > Randomize Game… runs FVX 1.6.2 with a
preset or a pasted settings string
([decision 0012](../decisions/0012-randomizer.md)). What was found on the
way:

- **FVX misreads ZX 4.6.1's settings strings.** It gives version id 322 to
  ZX 4.6.1 and to the closer-to-vanilla fork, reads 322 as the fork's and
  throws. ZX's format didn't change from 4.6.0 (321), so the app
  relabels a ZX-shaped 322 string as 321 before passing it on.
- **FVX writes Gen 1 games as `.gbc`.** The app keeps the base game's own
  extension, so Red stays a Game Boy game.
- **The same seed and settings give the same ROM, byte for byte**, with
  `-z`.
- **3DS games are left out.** FVX writes a decrypted `.cxi` that the 3DS
  core opens, but Azahar keeps saves by title ID in one SD card, so a
  randomized copy would share the clean game's save.

#### Randomizers surveyed, 2026-11-11

Asked whether randomizing could be a switch on a running game:

- **Nothing maintained randomizes an untouched ROM as it runs.** The
  closest are [GBAXG](https://kittypboxx.github.io/GBAXG/), which rewrites
  memory in its own browser emulator from hard-coded addresses (2023), and
  a Red/Blue proof of concept through an edited save
  ([RAMdomizer](https://github.com/NPO-197/PokemonRedBlue-RAMdomizer),
  2021). Every randomizer in use writes a ROM or a patch first.
- **No emulator has one built in.** mGBA and RetroArch soft-patch a
  `game.bps` beside the ROM and keep the ROM's save name, so the patched
  and plain game share a save. The IronMON trackers call UPR's command
  line from BizHawk and load numbered ROMs.
- So the app keeps making a ROM, and makes it the game's run, with a
  switch ([decision 0012](../decisions/0012-randomizer.md)).

- **The 3DS games** (built the same day, decision 0012's second update).
  FVX's command line refuses them (`EXTRA_MEMORY_NOT_AVAILABLE`), in 1.6.2
  and 1.6.3 alike: only its window calls
  `RomOpener.setExtraMemoryAvailable(true)`. With that flag set by a
  ten-line launcher class, `-d` wrote Omega Ruby's randomized files as a
  33 MB folder, `<title ID>/romfs` and `code.bin`, in 5 s. Azahar's
  libretro core loads that from `Azahar/load/mods/<title ID>/` in the
  folder it is given, and keeps its saves there too. Checked by running,
  on a copy of Omega Ruby and its save: with an "Everything" mod there
  the party's Gyarados was Ground, as the spoiler log said, and Water and
  Flying without; with the mod's `code.bin` blanked the game did not
  start.

- **PC fan games** (built the same day, decision 0012's third update):
  a script of the app's swaps species as the game makes Pokémon. Checked
  in a copy of Pokémon This Gym of Mine (Essentials v18.1, 912 species)
  by calling the game's own functions with the script loaded: wild
  Bulbasaur became Rowlet and Mewtwo Lugia, each within a few points of
  the same base stat total; a trainer's six became six others at the same
  levels with their own moves; a Pikachu given by name was swapped and
  one the game made was not; the same seed gave the same swaps and
  another seed others; with trainers and gifts off those were unchanged.

What else there is, for later:

- **Key items and progression**: [Archipelago](https://archipelago.gg)
  patches (`.apemerald`, `.apred`, and community worlds for Crystal,
  FireRed, Platinum and HeartGold). A single-player seed plays in any
  emulator without its client. The patch is a zip of a bsdiff and a list
  of byte edits, which `emu-patch` reads since 2026-11-11 (decision
  0009's update), checked against Archipelago's own code but not yet on
  a patch from its site.
- **Hacks**: FVX runs on a hack that keeps its game's code, without
  saying so, and may break it. Drayano's hacks and the CFRU ones (Radical
  Red, Unbound) are known to break; those two have a randomizer of their
  own at New Game.
- **RPG Maker games**: Essentials v20 and later raise
  `:on_wild_pokemon_created` and `:on_trainer_load`, v19
  `Events.onWildPokemonCreate` and `onTrainerPartyLoad`, which a script
  loaded with the game could shuffle from.
- **FVX 1.6.3** (7 November 2026) fixes random move names failing, alt
  formes' types and abilities, and a Gen 1 crash.

What follows is the research that led to it.


The Universal Pokémon Randomizer ZX
([repo](https://github.com/Ajarmar/universal-pokemon-randomizer-zx), GPL-3.0)
is at v4.6.1 (23 Nov 2024). It needs 64-bit Java 8 or later, and about 4 GB
of heap. It supports:

| Generation | Games |
|---|---|
| 1 | Red, Blue, Yellow (US and others) |
| 2 | Gold, Silver, Crystal 1.0 and 1.1 |
| 3 | Ruby, Sapphire, Emerald, FireRed, LeafGreen (including 1.1 revisions) |
| 4 | Diamond, Pearl, Platinum (US Rev 0 and Rev 1), HeartGold, SoulSilver |
| 5 | Black, White, Black 2, White 2 |
| 3DS | X/Y onward (out of scope here) |

Its command line, from `CliRandomizer.java`:

```
java -Xmx4608M -jar PokeRandoZX.jar cli -s <settings.rnqs> -i <source ROM> -o <output ROM> [-l]
```

- `-l` writes `<output>.log`, the spoiler log.
- It has no seed flag, so every run makes a new seed.
- It writes a whole ROM, never a patch.
- For DS games it rebuilds the ROM: trimmed, with the header CRC rewritten,
  and the game code kept.

[UPR FVX](https://github.com/upr-fvx/universal-pokemon-randomizer-fvx) is
the active successor (v1.6.2, 3 Oct 2026). It ships Apple Silicon builds
with Java bundled and adds type-chart, palette and player-graphics
randomization. Its command line also takes a settings string and a seed:

```
java -jar UPR-FVX.jar cli -i <src> -o <out> {-s <file> | -S <string>} [-z <seed>] [-l]
```

How it would fit the app:

- **A "Randomize…" command on a library game.** It runs FVX (or ZX) as a
  separate process with a preset (Nuzlocke-friendly, wilds only, everything)
  or the player's own settings string. The result is a new game in the Hacks
  folder, named with the seed, as decision 0009 does for patches. The
  spoiler log goes beside it.
- **Licensing.** Running a GPL tool as a separate program keeps it apart from
  the app's code. Download it on first use, or ask for its path; don't build
  it into the app.
- **Hacks can't be randomized this way.** The randomizer doesn't support
  ROM hacks. It warns that the ROM isn't clean, then randomizes anyway,
  which can crash the hack. Renegade's own
  `RandomiseInstructions.txt` randomizes the clean game and moves single
  files across, by hand. Radical Red, Unbound and Emerald Rogue have
  randomizer modes built in.

[Archipelago](https://archipelago.gg) seeds for Emerald, Red/Blue, Crystal,
FireRed/LeafGreen, Platinum, HG/SS and B/W also make a patched ROM. A
single-player seed plays offline in any emulator. Only its item tracking and
hints need a client, which talks to BizHawk's Lua.

### 4. BSP patches, for Prism (medium)

[Pokémon Prism](https://rainbowdevs.com/prism-setup/), one of the most played
GBC hacks, ships as a BSP patch. BSP is a scripted patch format: the patch
is a small program that checks the base and writes the result. emu-patch
doesn't read it.

### 5. Essentials v19+ games without per-game work (medium)

From v19 (April 2021), Pokémon Essentials ships mkxp-z itself as `Game.exe`,
on Ruby 3.0 (v19) or 3.1 (v20 and later), with every Win32API call removed
([v19 changelog](https://essentialsdocs.fandom.com/wiki/Change_log/v19)).
These games need none of tgom_compat.rb's Ruby 1.8 fixes. They need only:

- the bridge's input and save-state hooks;
- the updaters and Discord status turned off.

One generic preload should cover them all. Spotting them: modern games
still say `Library=RGSS104E.dll` in Game.ini, so look for a bundled
`x64-msvcrt-ruby3*0.dll` instead.

First games to try:

- Infinite Fusion: an Essentials v18 fork that ships Ruby 3.0/3.1 DLLs.
- Tectonic (v19.1).
- Vanguard (v19.1).
- Empyrean (v20.1).

### 6. Web games: PokéRogue (medium)

[PokéRogue](https://github.com/pagefaultgames/pokerogue) is AGPL-3.0,
TypeScript on Phaser and Vite.

- **Offline.** With `VITE_BYPASS_LOGIN=1` it makes no server calls and keeps
  its saves, encrypted, in the browser's localStorage. Imports and exports
  are `.prsv` files.
- **An existing offline app.**
  [pokerogue-offline](https://github.com/VdustR/pokerogue-offline) is an
  Electron build for Apple Silicon that blocks the network.
- **In the app**, it would be a web-view "core" over a local static build,
  or a launcher entry for the Electron app.

### 7. Ruby 1.8 Essentials games in general (medium to high)

Essentials v18 and older run on RGSS1, which embeds Ruby 1.8. Under mkxp-z's
Ruby 3.1, the same things break as in This Gym of Mine.

Three ways to handle them, from least to most work:

1. Keep writing one preload per game, as now.
2. Build emu-mkxp's player with mkxp-z
   [PR #304](https://github.com/mkxp-z/mkxp-z/pull/304) ("syntax transform",
   still open), which makes Ruby 3.1 accept 1.8 grammar and restores the
   removed methods. [Empo](https://github.com/mateo-m/empo-app) (GPLv3)
   carries the same work as 36 patches and also runs on Apple Silicon Macs.
3. Ship a second player built on Ruby 1.8.7.

All three still need stand-ins for Win32API and custom DLLs. The hard one is
Insurgence's MGC_Hmode7 DLL, which draws its 3D maps. JoiPlay turns it off,
so maps go flat.

### 8. Scripts: touch, and a BizHawk-style API (medium)

These aren't needed to play, but:

- **Touch input in Lua** would let headless tests and the store sweep get
  past DS touch prompts.
- **A BizHawk-compatible layer** would run the community's Lua tools. That
  means memory domains, `memory.read_bytes_as_array`, `emu.getsystemid`,
  `event.onframeend`, and LuaSocket for the trackers. The tools are the
  Ironmon trackers (the
  [NDS tracker](https://github.com/Brian0255/NDS-Ironmon-Tracker) covers
  D/P/Pt/HGSS/BW/B2W2) and Archipelago's auto-tracking.
- **mGBA-style Lua.** Emerald Rogue's and Run & Bun's optional multiplayer,
  and Quetzal's online add-on, use mGBA's Lua API. That is online play,
  which isn't wanted.

### 9. hg-engine's Hall of Fame crash (to check with a dump)

Hacks built on [hg-engine](https://github.com/BluRosie/hg-engine) (HeartGold
Generations, Origin HeartGold) crash entering the Hall of Fame on melonDS
and on hardware, but not on DeSmuME. Its author: "desmume just doesn't
crash on reading from invalid pointers"
([thread](https://www.pokecommunity.com/threads/pok%C3%A9mon-heartgold-generations-v2-0.537551/page-10)).

Players beat the Champion in DeSmuME and switch back. The DS core takes
data aborts as hardware does, so it will probably crash too. A per-game
"lenient memory" option could match DeSmuME there.

hg-engine needs nothing else unusual: its code loads into normal main RAM
(0x023C8000), and its save still fits the 512 KiB flash.

### 10. Smaller items

- **Setting the clock.** The app always starts the clock at the local time
  (`local_clock_seconds` in
  [emulator.rs](../../crates/emu-app/src/emulator.rs)).
  [Platinum Kaizo](https://emi.dev/platinum-kaizo)'s timed events, and
  testing day/night, want a date the player picks.
- **Fast-forward and fake clocks.** Emerald Imperium uses a clock kept in
  game (`OW_USE_FAKE_RTC`), and Emerald Rogue counts its own minutes. Both
  run faster under fast-forward. That is expected.
- **GBA save type for unknown codes.** When neither the game-code table nor
  the library string matches, mGBA detects the save type from the game's
  first access. All the decomp hacks checked keep BPEE or BPRE, so this is
  rare.
- **Saves from mGBA.** mGBA writes 128 KiB + 16 bytes, with the clock at the
  end. The core loads the save and ignores the extra 16 bytes, so the
  in-game clock offset is lost. DeSmuME's `.dsv` footer is already handled.
- **32 MiB GBA ROMs and AGBPrint.** Radical Red, Unbound, Glazed and Elite
  Redux are 32 MiB (or likely so). mGBA puts a debug-print buffer over
  0x09FE0000 once a game writes its protect register. That once hid an
  Unbound bug that crashed on hardware
  ([mGBA #1884](https://github.com/mgba-emu/mgba/issues/1884)). Matching
  hardware (no buffer) is right, but it may show such bugs.
- **MBC30 is already there.** Pokémon Coral is a 4 MiB GBC hack whose header
  says plain MBC3. The core gives ROMs over 2 MiB 8-bit bank numbers
  ([cartridge.rs:292](../../crates/emu-gb/src/cartridge.rs#L292)), as mGBA
  does.

## The games

"?" marks a fact that couldn't be confirmed. The mGBA, CFRU,
pokeemerald-expansion, UPR and hg-engine facts come from their source.
The rest come from each hack's own docs or thread. PokéCommunity and
romhacking.net pages were read through archives or search results, so
treat those details with more care.

Base ROMs used below:

| Short name | Dump | CRC32 |
|---|---|---|
| FR | FireRed (USA) 1.0 | `DD88761C` |
| EM | Emerald (USA, Europe) | `1F1C08FB` |
| C1.0 | Crystal 1.0 | `EE6F5188` |
| C1.1 | Crystal 1.1 | `3358E30A` |

### Game Boy Advance hacks

| Hack | Base | Patch | Latest | Clock | Notes |
|---|---|---|---|---|---|
| Radical Red | FR (not 1.1) | UPS, web patcher | 4.1, Mar 2024 | Yes (CFRU) | 32 MiB?; must be Flash 128K. Blocked by gap 1 ([thread](https://www.pokecommunity.com/threads/pok%C3%A9mon-radical-red-version-4-1-released-gen-9-dlc-pokemon-character-customization-now-available.437688/)). |
| Unbound | FR | UPS | 2.1.1.1, Nov 2022 | Yes (CFRU), and checks for tampering | Shows "WRONG SAVE TYPE" if not Flash 128K. Blocked by gap 1 ([FAQ](https://pokemonunbound.miraheze.org/wiki/Pok%C3%A9mon_Unbound_Wiki:FAQ_Page)). |
| Inflamed Red | FR | UPS | 1.0.5, 2022 | Probably (CFRU) | |
| Gaia | FR | UPS | Beta 3.2, Nov 2018 | No | |
| Clover | FR | BPS, web patcher | 1.3.3, Oct 2023 | No | [FAQ](https://poclo.net/faq) |
| Fire Red Omega | FR | IPS | 2009 | No | |
| FireRed Rocket Edition | FR (not 1.1) | IPS | 1.0.2, 2021 | No | [site](https://rocket-edition.com/download/) |
| Ash Gray | FR | IPS | Beta 4.5.3, ~2015 | Yes, own routine | Needs VBA-style clock pins (gap 1). |
| Liquid Crystal | FR | IPS | 3.3, Sep 2016 | Yes, own routine | Needs VBA-style clock pins. |
| Adventure Red Chapter | FR | UPS | Beta 15, Oct 2020 | Yes, own routine | Ignores pin direction ([mGBA #971](https://github.com/mgba-emu/mgba/issues/971)). |
| Dark Rising 1/2 | FireRed (revision ?) | ? | 2013 | DR2 has day/night | |
| Vega | FR | ? | Aug 2020 | No | |
| Ultra Shiny Gold Sigma | FR | BPS | 1.5.1, 2024? | ? | |
| Light Platinum | Ruby 1.0 or 1.1 (two patches) | IPS | Final+, 2017 | Ruby's own | |
| Glazed | EM | UPS | 8.6.3 | Emerald's | 32 MiB. |
| Quetzal | EM | BPS, web patcher | 0.9.1, Oct 2026 | ? | 1–4 player link cable ([site](https://pokemonquetzal.app/en/downloads)). |
| Emerald Rogue | EM (decomp) | xdelta | 2.2.1, Sep 2026? | Own counter | Multiplayer via mGBA Lua (online). |
| Inclement Emerald | EM | xdelta | 1.13, Nov 2021 | Yes | [releases](https://github.com/BuffelSaft/pokeemerald/releases) |
| Run & Bun | EM | BPS | 1.07, Apr 2023 | ? | |
| Elite Redux | EM | BPS | 2.65 beta 2, Mar 2026 | Yes | 32 MiB? ([site](https://elite-redux.com)) |
| Emerald Kaizo | EM | IPS | 2.1, Mar 2020 | ? | |
| Emerald Imperium | EM (expansion) | BPS, web patcher | 1.3.1, Sep 2025 | Fake clock | [patching](https://emeraldimperium.info/patch.html) |
| Emerald Seaglass | EM (expansion) | IPS | 3.0, 2024 | ? | |
| Theta Emerald EX | EM | patch + patcher | Feb 2017 | Probably | 32 MiB? |
| Emerald Crest | EM (expansion) | UPS | 1.0.F, Jun 2025 | Yes | ~31 MiB, not a power of two. |
| Emerald Enhanced | EM 1.0 | xdelta (Discord) | Sep 2026 | Required | Soft-resets without one ([GitHub](https://github.com/Enhanced-Projects/Emerald-Enhanced)). |
| Heart & Soul v2 | EM (expansion) | UPS | 2.0.6, Sep 2026 | Yes | ~32 MiB ([FAQ](https://pokemonhns-development.github.io/pokehns-expansion-documentation/faq.html)). |
| Emerald Legacy | EM | BPS | 1.1.4, Feb 2025 | Yes | Own save-type error screen ([repo](https://github.com/cRz-Shadows/Pokemon_Emerald_Legacy)). |

Emerald and Ruby hacks keep game codes BPEE and AXVE, which already get
Flash 128K and the clock. Modern Emerald hacks are mostly built on
[pokeemerald-expansion](https://github.com/rh-hideout/pokeemerald-expansion)
(1.17.2, 31 Oct 2026). Its builds pad to a power of two, up to 32 MiB, and
keep `FLASH1M_V103`. No case was found of unaligned access, prefetch, open
bus or timing breaking a CFRU or expansion hack. The one known crash was a
divide by zero in some move animations. It broke MyBoy and No$GBA, not mGBA
or hardware, and was fixed in expansion 1.7.4.

### Game Boy and Game Boy Color hacks

| Hack | Base / ships as | Latest | Cartridge | Notes |
|---|---|---|---|---|
| Polished Crystal | Whole .gbc builds (not fetched by the app) | 3.2.3, Dec 2025 | MBC3 + clock, 2 MiB, 32 KiB RAM | [FAQ](https://github.com/Rangi42/polishedcrystal/blob/master/FAQ.md) |
| Crystal Clear | Crystal (revision ?), BPS | 2.6.0, Mar 2026 | MBC3 + clock | Optional Super Game Boy border ([FAQ](https://github.com/ShockSlayer/ccdocs/blob/master/docs/FAQ.md)) |
| Prism | C1.0 or C1.1, **BSP** | 0.95 b0254, Aug 2023 | MBC3 (2 MiB) | Blocked by gap 4 ([setup](https://rainbowdevs.com/prism-setup/)) |
| Crystal Legacy | C1.0 or C1.1, IPS/BPS | 1.3.1, Aug 2024 | MBC3 + clock, CGB only | [repo](https://github.com/cRz-Shadows/Pokemon_Crystal_Legacy) |
| Crystal Kaizo | C1.0, IPS (BPS for 2026) | 2026 | MBC3 + clock | |
| Sour Crystal | Crystal 1.1, IPS | 7.0a, Sep 2026 | MBC3 + clock | |
| Gold/Silver 97 Reforged | Crystal, IPS | 6.1f, Apr 2025 | MBC3 + clock | |
| Speedchoice Crystal / Red | C1.1 / Red, IPS | 7.2 / 1.1 | MBC3 | Speedrun builds; UPR-Speedchoice randomizes them. |
| Coral | C1.0, IPS | Demo 1.2, 2022 | 4 MiB, MBC3 header | Needs MBC30, which the core has (gap 10). |
| Orange | Whole .gbc builds (not fetched by the app) | Jan 2026 | MBC3 + clock | |
| Red++ v3 | Red, IPS | Dec 2018 | MBC3 + RAM, CGB only | |
| Shin Pokémon Red/Blue | Red / Blue, BPS | 1.25.0, Oct 2025 | MBC3 + RAM | Game Boy Printer support ([repo](https://github.com/jojobear13/shinpokered)) |
| Brown | Red, UPS | 6.1.2, Dec 2024 | ? | |
| Yellow Legacy | Yellow, IPS | 1.0.10, Jul 2024 | MBC5, 1 MiB | |

Nearly all of these are built from pret's disassemblies (pokered,
pokecrystal, pokeyellow), which carry no license. Prism, Crystal Clear and
Brown are closed source. Patches for Crystal often fit only one of 1.0 or
1.1, and IPS records neither, so the known-base table in gap 2 matters
most here.

### Nintendo DS hacks

| Hack | Base | Latest | Size | Notes |
|---|---|---|---|---|
| Renegade Platinum | Platinum (USA): `RenegadePlatinum3541.xdelta` for Rev 0, `…4997.xdelta` for Rev 1 | 1.3.0, Apr 2019 | 128 MiB | **Tested: boots** (see above). Add-ons: Classic, SpeedUp, shiny rates; [Renegade QoL](https://github.com/Ratchet83/renegade-platinum-qol/releases/tag/v1.0) ([wiki](https://fredericdlugi.github.io/platinum-renegade-wiki/)). |
| Platinum Kaizo | Platinum Rev 1 only | Final, Apr 2026 | 128 MiB | Timed events use the DS clock ([guide](https://emi.dev/platinum-kaizo)). |
| Platinum Redux | Platinum Rev 1? | 4.0 | 128 MiB | Four type-chart builds. |
| Following Platinum | Platinum Rev 0? | 1.1.3, Sep 2026 | 128 MiB | Uses the ARM9 "synthetic overlay". |
| Sacred Gold / Storm Silver | HG/SS (USA) | 1.05, 2012 | 128 MiB | Anti-piracy bypass built in. |
| Blaze Black / Volt White | Black / White (USA) | 3.1, Dec 2011 | 256 MiB | Untested: the White dump is missing. |
| Blaze Black 2 / Volt White 2 Redux | Black 2 / White 2 (USA, Europe) | 1.4.1, Jun 2023 | 512 MiB | White-screens in DSi mode on TWiLight Menu++; fine in DS mode ([guide](https://smilingzero.github.io/BlazeBlack2ReduxWiki/patchingGuide/)). |
| Black 2 Kaizo | Black 2 | 1.6, Oct 2022? | 512 MiB | |
| Moon Black 2 | Black 2 | Beta 4.2.3, Jun 2023 | 512 MiB | |
| hg-engine hacks (HeartGold Generations, Origin HeartGold) | HeartGold (USA) | Ongoing | ~256 MB | Hall of Fame crash (gap 9). melonDS users turn its JIT off ([hg-engine #417](https://github.com/BluRosie/hg-engine/issues/417)). |

All are xdelta patches. A 512 MiB B2/W2 hack is exactly the patcher's
limit. Every one keeps a Gen 4 or 5 game code, so its save is the 512 KiB
flash the core already gives those games. HG/SS anti-piracy relies on the
infrared cart and the secure-area redirect, both of which the core has.

### PC fan games

| Game | Engine | Mac build | Latest | Approach |
|---|---|---|---|---|
| Reborn | Custom mkxp-z fork, Essentials-based | Yes, macOS 12+; Apple Silicon? | 19.5.0, Sep 2024 | Launch the Mac app, or run its data in emu-mkxp (it needs its bundled Ruby stdlib, gems and OpenSSL) ([download](https://www.rebornevo.com/pr/download/)) |
| Rejuvenation | Essentials fork on mkxp | Yes | V14, Jul 2026 | Same as Reborn ([download](https://rejuvenationgame.com/download)) |
| Desolation | Essentials fork on mkxp | Yes, with an Apple Silicon updater | 6.0.13, Jun 2025 | Same as Reborn |
| Infinite Fusion | Essentials v18 fork, mkxp-z, Ruby 3.x | No | 6.8.2, Jul 2026 | emu-mkxp, gap 5. Offline with the preloaded sprite pack (over 3 GB). |
| Tectonic | Essentials v19.1, mkxp-z | Linux only | 3.4.2 | emu-mkxp, gap 5 ([repo](https://github.com/Pokemon-Tectonic-Team/Pokemon-Tectonic)) |
| Vanguard | Essentials v19.1 | No | 4.0.3 | emu-mkxp, gap 5 |
| Empyrean | Essentials v20.1 | No | 1.1, Feb 2023 | emu-mkxp, gap 5 |
| Añil (v4) | Newer Essentials (21.1?) | No | 4.1.x (Spanish), 4.0.2 (English) | emu-mkxp, gap 5 |
| World Frontier | Essentials 21.1 | Yes, at launch | 23 Oct 2026 | emu-mkxp or its own app |
| Xenoverse: Ordem et Chaos | Essentials 21.1 | Yes | — | emu-mkxp or its own app |
| Insurgence | Essentials, RGSS1 (Ruby 1.8), MGC_Hmode7 DLL | Only an old 32-bit Wine wrapper | 1.2.7 | emu-mkxp, gap 7, plus a Hmode7 stand-in |
| Zeta / Omicron | Essentials, RGSS1 | Old Mac zips, probably Wine | 1.5.5.2, Mar 2017 | gap 7 |
| Xenoverse (original) | Essentials v16/17, RGSS + `Rpg.NET.dll` | No | ~1.5.5 | gap 7 |
| Phoenix Rising, Ethereal Gates, Sage | Essentials, RGSS1 | No | Demos, dormant | gap 7 |
| Realidea System | Essentials (version ?) | No | 4.1, Dec 2023 | Check its Ruby first |
| This Gym of Mine | Essentials v18.1, RGSS1 | No | 4.2.2 | **Runs today**, with tgom_compat.rb |
| Uranium | Essentials, RGSS1 | No | 1.2.4 | Taken down by DMCA in 2016: not fetched or linked. |
| PokéRogue | TypeScript, Phaser (web) | Runs in a browser | Ongoing | gap 6 |
| PSDK / Pokémon Studio games | Ruby + LiteRGSS2 (SFML), not RGSS | Studio exports for Apple Silicon | — | Launch their own Mac builds. Older exports each need their own Ruby (2.5.9 to 3.3.0). |
| PokeWilds | Java, libGDX | A jar | 0.8 alpha | Run the jar with Java |
| Pokémon 3D | VB.NET, MonoGame 3.7, DirectX 9 | No | 0.61.2 | Needs a port; not worth it |
| PokeMMO, Revolution Online | Online-only | — | — | Out of scope |

For Essentials games, the save and update details that matter:

- **Reborn, Rejuvenation and Desolation** keep saves outside the game
  folder.
- **Infinite Fusion** can download custom sprites (200,000+ files) and has an
  auto-updater. Both are optional.
- **Tectonic** checks a version server at start; the error is harmless
  offline.

## Plan

In the order that unlocks the most games for the least work:

1. **FireRed-hack clock (gap 1):** mGBA's rule, plus the VBA pin behaviour.
   Unlocks Radical Red, Unbound and the old clock hacks.
2. **Better wrong-base messages (gap 2):** the xdelta description, and a
   known-base table.
3. **Randomize in the app (gap 3):** done, with FVX on its command line,
   writing into the Hacks folder ([decision 0012](../decisions/0012-randomizer.md)).
4. **A generic preload for Essentials v19+ (gap 5).** Try it on Infinite
   Fusion and Tectonic.
5. **BSP patches (gap 4)**, for Prism.
6. **PokéRogue (gap 6)**, in a web view.
7. **Ruby 1.8 games (gap 7):** the syntax transform.
8. **Touch in Lua, then a BizHawk layer (gap 8).**
9. **A clock picker (gap 10).** Then check hg-engine's crash (gap 9) once
   there is a HeartGold dump.

Each step needs the player's dump of the base game to be tested in game. At
present only Platinum Rev 1 is on hand.
