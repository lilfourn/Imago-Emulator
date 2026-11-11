# 0009 — ROM hacks become games of their own, patched once

- Status: accepted
- Date: 2026-10-23
- Deciders: Claude, for Luke Fournier's review

## Context

Fan-made ROM hacks are the largest source of new games for these
systems, Pokémon most of all. A hack is shared as a patch, never as a
game: IPS, UPS or BPS for Game Boy and GBA hacks, xdelta (VCDIFF) for DS
hacks, whose ROMs are too big for IPS. Players apply the patch to their
own copy of the game it was made from.

Other emulators soft-patch: they apply `game.bps` beside `game.gba`
whenever the game loads. Then the hack and the original share one file
name, so they share saves and states, and the library would show one
game for two. A patch also often keeps the original's game code, which
the library looks box art and names up by, so the hack would be shown as
the original.

Of the Rust VCDIFF crates, the one with xdelta3's additions (oxidelta,
MIT) does not read the LZMA compression xdelta3 3.0.10 and later use by
default. It expects `.lzma` streams. xdelta3 writes `.xz` streams that it
flushes and never finishes, one for each kind of section, running from
one window to the next. Its decoder also allocates whatever sizes a
patch states.

## Decision

1. **A patch makes a new ROM, once.** Opening a patch writes the hacked
   game into `~/Library/Application Support/ds-emulator/Hacks`, named
   after the patch (`Radical Red.gba`). From then on it is an ordinary
   game, with its own saves, states, cheats and library tile. The
   original ROM is never changed. Making the same game again reuses the
   file; a different game with the same name is numbered, as unpacked
   downloads are (`Game (2).gba`), so no save is ever lost.
2. **The base is found by checksum, or chosen.** UPS and BPS record the
   CRC-32 of the ROM they were made from, which the library keeps for
   each game, so that game is used. IPS and xdelta record none, so the
   app asks which game the patch is for, unless it was opened with one
   ROM (on the command line, or in the same archive).
3. **The library trusts the folder.** A game in the hacks folder is
   titled with its file name and never looked up, so it cannot be named
   after the game it was made from.
4. **The formats live in their own crate, `emu-patch`**, as cheat formats
   do in `emu-cheats`: bytes in, bytes out, with every check the format
   has (CRC-32 for UPS and BPS, Adler-32 per window for xdelta) and a size
   limit enforced before anything is allocated. It decodes VCDIFF itself,
   written from RFC 3284 and xdelta3's source, including its LZMA
   streams. These are unpacked with `lzma-rs` (MIT), after checking the
   size each LZMA2 chunk header gives. xdelta3's older DJW and FGK
   compression, used only when asked for, is refused with a message
   saying so.
5. **Archives other than zip** (`.7z`, `.rar`) unpack through macOS's own
   `/usr/bin/tar`, which is libarchive's `bsdtar`, into a scratch folder.
   No archive code is added. Patches found in archives are unpacked into a
   temporary folder, since only the games they make are kept.

## Consequences

- A hacked game costs its ROM's size on disk again, up to 512 MiB for a
  DS hack.
- A new version of a hack, patched to the same name, gets a numbered file,
  and its save stays with the old one until the player copies it over.
- A hack moved out of the hacks folder is looked up like any game, and
  may take the original's name.
- IPS patches cannot be checked. Applied to the wrong ROM, they make a
  broken game.
- Patches are tested against ones made by Floating IPS and xdelta3 from
  made-up ROMs (`scripts/make-patch-fixtures.sh`). A 128 MiB DS-sized
  patch applies in well under a second.

## Update, 2026-11-11: Archipelago patches

The owner wants every kind of Pokémon randomizer playable. FVX shuffles
Pokémon; [Archipelago](https://archipelago.gg) shuffles where items and
key items are, and hands each player a patch for their own ROM. That is a
patch like any other here, so it is read where the others are.

1. **`emu-patch` reads the container**, as `worlds/Files.py` of
   Archipelago 0.6.8 writes it (format version 7): a zip with
   `archipelago.json` and the files its `procedure` names. Two steps are
   read, all that Pokémon Emerald's and Red and Blue's patches use:
   `apply_bsdiff4`, a bsdiff 4.0 patch, and `apply_tokens`, small edits
   made after it. A manifest with no procedure is one `delta.bsdiff4`. A
   step of a game's own, a newer format, or a "custom" procedure is
   refused by name.
2. **bzip2 is unpacked by `bzip2-rs`** (MIT or Apache-2.0, pure Rust,
   reading only): a bsdiff patch is three bzip2 streams. libbzip2's own
   licence is not on the list. Each stream is held to the size of the game
   it makes before it is unpacked further. `md-5` was in the lock file
   already; `zip` and `serde_json` are the ones the app uses.
3. **The game is named by MD5**, which the library does not keep. The app
   reads the library's games of the kind the patch makes (`.gba` for
   Emerald) until one matches, and otherwise asks, as for an IPS patch. A
   ROM that is not the one named is refused with both checksums: bsdiff
   checks nothing, and would make a broken game.
4. **It becomes a game of its own**, as a hack does, not the game's
   randomized run: its seed and settings are Archipelago's, made on its
   site, and a run's switch would hide that it needs a patch from there.
5. **No client.** The patched ROM holds what its own player finds, so a
   single-player seed plays through. Items sent between players go
   through Archipelago's server, which the app does not speak to.

Checked against Archipelago's own code, not a real seed: the fixtures are
written by its `Files.py` and applied again by it
(`scripts/make-archipelago-fixtures.py`), and the tests hold this crate
to the same bytes. No patch from archipelago.gg has been applied, and no
game made from one played: the owner's library has no Emerald, Red or
Blue to make one for.
