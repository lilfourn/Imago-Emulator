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
