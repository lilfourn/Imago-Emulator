# 0012 — Randomized Pokémon games, made by FVX in its own process

- Status: accepted
- Date: 2026-11-03
- Deciders: Claude, for Luke Fournier's review

## Context

Randomizers are one of the most played kinds of fan-made Pokémon game:
the same game, with its wild Pokémon, trainers, starters and more
shuffled. The famous one is the Universal Pokémon Randomizer ZX
(GPL-3.0, Java). Its last release is 4.6.1, of November 2024. Its active
successor is FVX (1.6.2, October 2026), which reads ZX's settings and
adds more. Both write a whole new ROM from the player's own copy of a
game, never a patch. The emulator already plays what they write: a ZX
and an FVX Platinum boot and play as the clean game does
([research](../research/fan-games.md#3-randomize-from-the-app-done)).

What it took to play one was running a Java program by hand. On top of
that, a randomized ROM keeps its game's code, so the library took it for
the clean game.

Facts that shaped the choice:

- ZX needs Java installed, which macOS does not have. FVX ships a build
  for each Mac architecture with its own Java 25 (41 and 43 MB).
- ZX's command line has no seed: every run is a new game. FVX's takes
  `-z <seed>`, and a settings string with `-S`, so no settings file is
  needed. The same seed and settings make the same game, byte for byte.
- FVX gives ZX 4.6.1's settings version id, 322, to another fork too.
  It reads every 4.6.1 settings string as that fork's and fails. ZX's
  settings format did not change between 4.6.0 (321) and 4.6.1.
- Both also randomize the 3DS games (X/Y to Ultra Sun/Moon) into a
  decrypted `.cxi`, which the 3DS core opens. But Azahar keeps every 3DS
  game's save under its title ID in one SD card, and a randomized copy
  keeps the title ID. It would share, and overwrite, the save of the game
  it was made from.

## Decision

1. **FVX, pinned, downloaded at first use.** FVX 1.6.2's Mac build for
   the Mac's architecture is downloaded with curl, checked against a
   pinned SHA-256, and unpacked with ditto into
   `~/Library/Application Support/ds-emulator/Randomizer/upr-fvx-1.6.2`.
   The zip keeps no permissions, so its Java is made executable. Nothing
   needs installing first, and no GPL code is built into the app. Since
   2026-11-06, Linux (x86-64 and arm64) and Windows (x86-64) get FVX
   1.6.2's own builds for them, each pinned the same way, laid out as the
   Mac's and unpacked in the app with the `zip` crate. On Windows it runs
   `java\bin\javaw.exe`, with no console window.
2. **It runs as a separate program**, on its command line, headless:
   `java -Xmx4608M -Djava.awt.headless=true -jar UPR-FVX.jar cli -i <game>
   -o <out> -S <settings> -z <seed> -l`, in its own folder, where it reads
   its name lists.
3. **The result is a hack.** It goes into the hacks folder
   ([decision 0009](0009-rom-hack-patches.md)) as
   `<title> (Random <seed>).<extension>`, with the spoiler log beside it as
   `….log`. So it has its own saves and tile, and is titled by its file
   name, not looked up as the game it came from. It keeps its game's own
   extension: FVX writes Red as `.gbc`, which would play it as a Color
   game.
4. **Presets, or the player's settings.** The window offers three
   presets: Wild Pokémon, Classic and Everything. They are FVX settings
   strings made with FVX's own `Settings` class
   (`scripts/randomizer/MakePresets.java`). Each also turns evolutions
   by trade into evolutions by level. ⌘V pastes any ZX or FVX settings
   string, checked by its CRC-32 first, with a ZX 4.6.1 string
   relabelled as 4.6.0's for FVX.
5. **Seeds are six digits**, new for each game, short enough to read and
   to tell apart games made from the same settings. The seed is in the
   game's name and its log.
6. **Where it starts:** File > Randomize Pokémon Game…, or Randomize… in
   the menu of a library game the randomizer knows. The window lists the
   library's games again each time it comes forward. One game is made at a
   time, whichever window asked; closing the window doesn't stop a run,
   and the main window then says how it went.
7. **Generations 1 to 5 only.** A game is offered if its header names one
   of the games FVX knows: Red to Crystal by header title, Ruby to White 2
   by game code. Games in the hacks folder are not offered, since hacks
   can break when randomized. The 3DS games wait for a save of their own
   per copy.

## Consequences

- Each randomized game costs its ROM's size on disk again, up to 512 MiB
  for Black 2 and White 2.
- A new FVX release needs a new pin and new presets, as their settings
  version moves (`MakePresets.java` writes them). FVX reads older
  settings, so presets left behind still work; newer ones are refused
  with a message.
- The first randomizing needs the network, about 8 s here; after that it
  is offline. Randomizing Platinum takes 0.4–1.7 s.
- The FVX folder takes 112 MB unpacked.
- FVX itself refuses a release of a game it doesn't know, and the window
  says so. It prints its errors on its output and some exceptions on its
  errors, so both are read for the reason.
- Quitting the app during a run leaves FVX to finish on its own; its work
  folder is cleared by the next run once it is an hour old.

## Update, 2026-11-10: one installer

FVX's download is `emu_os::install` over `emu_os::fetch`, shared with the
3DS core's and the RPG Maker player's (see decision 0011's update of the
same day).

## Update, 2026-11-11: a switch on the game, not a second game

Decision 3 made the randomized game a game of its own in the library, as
a hack is. The owner asked for a switch on the game instead. No
randomizer changes a game while it runs, and none that is maintained
works on an untouched ROM: every one in use rewrites the ROM first
([research](../research/fan-games.md#randomizers-surveyed-2026-11-11)).
So the randomized ROM is still
made once, by FVX, into the hacks folder. What changed is whose it is:

1. **The run belongs to its game.** The library keeps it in the game's
   entry (`[game.randomized]`: the image, the seed, the settings and
   whether it is on), and lists no game for it. The tile plays the run
   while it is on. Its title, box art and play time are the game's.
2. **Each of the two keeps its own files**, as before: the run's battery
   save, states and checkpoint sit beside its image. So switching goes
   from where one was left to where the other was, and nothing is lost
   either way. A save state made in one never loads in the other, as its
   header names the ROM.
3. **The switch is where the game is.** A game the randomizer knows has a
   Randomizer tile in its game menu, whose page has the switch, New Seed…
   and Spoiler Log; the library's menu for the game has the same switch.
   The randomize window makes the run. Whichever of the two is played, by
   any way of opening it, is what the tile plays from then on.
4. **A run starts a new game, or from a copy of the game's save.** FVX
   changes only the game's data, and renumbers no species, move or item,
   so a save loads in either. The save is copied, never shared: the game
   it came from keeps its own. Pokémon already caught keep their species
   and take the randomized game's stats, types and moves. The game's
   cheats and bindings are copied to a new run too.
5. **A new seed takes the run's place.** The run before stays in the hacks
   folder, with its progress, if it has a save or a state; one never
   played is removed with its log, so trying seeds does not fill the disk.
6. **Randomized games made before this** were listed on their own. On
   first launch each becomes the run of the one game for its system with
   its game code (or, for a Game Boy game, its title), and is on if it was
   played after that game. One that could be from two games, or whose game
   is not in the library, stays a game of its own.

A library file with a run in it is not read by a build from before this:
it says so and leaves the file alone.
