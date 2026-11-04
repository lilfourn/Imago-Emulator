# 0010 — A store of free homebrew, from catalogs that share it

- Status: accepted
- Date: 2026-11-02
- Deciders: Claude, for Luke Fournier's review

## Context

The library only ever held games the user brought: their own dumps, ROM
hacks made from them, and RPG Maker games. A new user opened the app to
an empty window. The request was a store that adds games the app plays to
the user's library by itself, with as many games as can be found.

The legal policy (PRD section 9) rules out the obvious source: the app
never distributes or links to commercial games, Nintendo's or anyone
else's. Homebrew is different. Thousands of games, demos and tools have
been written for these systems since the 1990s, and much of it is
shared freely by its authors. The question was where to find it in a form
an app can read, with the authors' leave to share it.

Sources looked at, November 2026:

| Source | Systems | Size | For apps? | Used |
|---|---|---|---|---|
| [Homebrew Hub](https://hh.gbdev.io) (gbdev) | GB, GBC, GBA, NES | 1,642 entries; 1,632 with a ROM | A documented JSON API; hosts each ROM and its screenshots; files are shared with their authors' permission or under licences that allow it; takedowns by issue or email | Yes |
| [Universal-DB](https://db.universal-team.net) | DS, 3DS | 455 entries; 29 DS games, 129 3DS games | One JSON file of the whole catalog, used by its own Universal-Updater app; downloads are the authors' own GitHub releases | Yes: DS games, and 3DS games since the app plays them (decision 0011) |
| itch.io (GB Studio, GBA jams) | GB, GBC, GBA | thousands | Downloads need an account's API key and per-game pages; much of it is in Homebrew Hub already | No |
| PDRoms, GBAtemp downloads | all | hundreds | Web pages only, no API | No |
| Pokémon fan games (RPG Maker) | PC | many | Use Nintendo's characters without a licence | No |

Homebrew Hub's documented route for files (`/api/entry/<slug>/<file>`)
answers 404; its own web front end uses
`/static/<basepath>/entries/<slug>/<file>`, which works. Its search pages,
in their default order (newest first), move entries added at the same
moment between pages: paging 500 at a time gave 18 entries twice and
missed 18 others.

## Decision

1. **Two catalogs, read live.** The store reads Homebrew Hub's catalog
   (paged in slug order, then in one page if any entry is still missing)
   and Universal-DB's DS and 3DS games, keeps both in
   `~/Library/Caches/ds-emulator/store`, and fetches them again once a day
   or when asked (the refresh button). New homebrew shows up without an
   app update. Downloads come from where the catalog says, never from a
   copy of our own.
2. **Only what the app plays.** A Homebrew Hub entry needs a playable
   `.gb`, `.gbc`, `.cgb`, `.gba` or `.agb` file (its `default` one if it
   marks one); NES entries are left out. A Universal-DB entry needs to be a
   DS game with a `.nds`, `.zip` or `.7z` download. Of several downloads,
   the store takes a ROM over an archive, a DS build over a 3DS one, a
   build "for emulators" over a flash card's FAT build, and one with sound
   over one without. Since 2026-11-03 a 3DS game may have a `.3dsx`,
   `.cia`, `.zip` or `.7z` download, taken in that order; of archives, one
   named for a `.3dsx`, then for the 3DS, then for a `.cia`, any of them
   over one named for another system, and a ZIP over a 7-Zip file, which
   macOS's `tar` can't always unpack. An entry for both systems is offered
   once: as a DS game if it has a DS build (an archive named for the 3DS is
   not one), else as a 3DS game.
3. **Checked to start.** `scripts/store-compat.py` downloads every game
   the store would offer, runs each headless for 900 frames with no input,
   and records whether any screen showed more than one colour at frame
   300, 600 or 900. Its verdicts are compiled into the app
   (`crates/emu-app/src/store_compat.toml`). Games that crash, stay blank
   or have nothing to play are not offered; games that started are marked
   "Starts in ds-emulator" on their page; games added since the last run
   are offered unchecked. 3DS games run on the core the app downloads,
   cloned with `cp -c` from the app's install for each run, so each starts
   a console of its own; the six pixels the core inverts for its touch
   cursor, in the touch screen's corner, are not counted, or no 3DS game
   would be one colour. By 2026-11-03, 1,655 of 1,717 had started (the
   3DS games on that day, the rest the day before), 71 of the 89 3DS
   games among them. Five of those start on an error screen, as they
   need files their archives hold beside the game; the script marks
   them `error` and the store leaves them out. The rest, with what stops each and a lead for the
   cores, are in [games-we-cant-run.md](../research/games-we-cant-run.md),
   written by the same script (`--backlog`), along with the NES homebrew
   the app has no core for.
4. **Withheld by hand.** Seventeen Homebrew Hub entries are crack groups'
   intros and trainers named after, shipped with or holding a commercial
   game (a 512 KiB `TRUE LIES +1`, a 1 MiB `NUTZ ZORN`), and one port of a
   commercial game. Their ROM headers, sizes and archives gave them away.
   Thirty-nine Universal-DB entries, judged by their pages, READMEs and
   files when the store took 3DS games: 32 ports of commercial games and
   engines that need one's data (Mario Kart 64, Sonic Mania, Quake, Doom,
   and Raptor's DS and 3DS port, which carries its shareware data); fan
   ports that carry a commercial game's own art, sound or music (Balatro3DS
   says so; a Cookie Clicker clone ships Cookie Clicker's image files by
   name; Yet Another Mario Kart Clone ships Mario Kart's Yoshi textures and
   collision files; Super Photo Studio's cast and music come from Mario,
   Sonic, Pac-Man and Style Savvy); unofficial ports of two whole games
   that are not their porters' to share, OFF ("I own nothing", says its
   porter) and Doki Doki Literature Club (which Team Salvato also sells as
   DDLC Plus); and Level256 Installer, which patches the console's boot
   firmware. `WITHHELD` in `store.rs` lists all 56, so the store never
   offers them, whatever the catalog or the checks say; the script reads
   the same list, and never downloads or runs them.
5. **One click adds a game.** Get downloads the game into its own folder,
   `~/Library/Application Support/ds-emulator/Games/Store/<catalog>/<slug>/`,
   where its saves go beside it, unpacking an archive as an added
   download is unpacked. Some Homebrew Hub files are ZIPs named like the
   ROM inside (`game.gb`); they are told by their first bytes and
   unpacked too. A 3DS game's archive often holds it twice, as a `.3dsx`
   and a `.cia`; only one is added, the `.3dsx` first. It adds the game to
   the library under the
   catalog's title, with its first screenshot as box art. No game
   database is asked, since none lists most homebrew. A game the
   library has shows Play instead.
6. **Pictures as they come into view.** The grid is a virtual list: only
   the rows on screen are laid out, and only their covers (and the next
   row's) are downloaded, six at a time, latest asked for first, then kept
   as PNGs enlarged by whole pixels so pixel art stays sharp.
7. **Game Boy ROMs with a wrong header checksum start.** The Game Boy core
   refused them. A boot ROM locks up on one, so they never ran on
   hardware, but they run in other emulators without a boot ROM, as this
   core starts. Fifty-three of the catalog's games, mostly 1990s demos
   written on emulators, never set the checksum. The core now starts them
   unless the user supplies a boot ROM, which then locks up as on
   hardware.

## Consequences

- An empty library offers the store, and the store offers 1,650 games,
  demos, music carts and tools, all free and shareable, 66 of them for
  the 3DS. The first 3DS game played waits for the core's 67 MB download
  (decision 0011).
- The store goes online: for the two catalogs (about 5 MB, once a day),
  covers as they are seen, and games when asked for. Nothing is sent but
  the requests themselves, with the user agent `ds-emulator/<version>`.
- A game's licence and its catalog's credit show on its page. The
  catalogs' takedown routes apply: a game they remove leaves the store at
  the next fetch, though copies already downloaded stay in the library.
- The checks prove a game starts, not that it plays to the end. Games
  that wait for a button on a blank screen would be marked blank, and a
  game showing an error passes. Five 3DS games do: the store unpacks only
  the game from an archive, not the files it keeps beside it on the SD
  card, and Sonic Robo Blast 2's data is a download of its own. They are
  listed in the backlog. Rerun
  `python3 -I scripts/store-compat.py` after core changes; it caches
  downloads and results in `~/Library/Caches/ds-emulator-store-compat`
  and reruns only what changed.
- What the checks found is a list of leads for the cores, kept in
  [games-we-cant-run.md](../research/games-we-cant-run.md): ten
  dual-mode Game Boy games that stay white in colour mode, demos that
  turn the screen off and wait, a 64-byte intro that needs its ROM
  mirrored, a GBA header check, LÖVE Potion 3DS games that stay black,
  and a 3DS game the core stops on. Commercial games are never fetched,
  so Pokémon and other retail games play only from the user's own
  dumps.
- Fan games that borrow other companies' characters stay unless they are
  found to carry a commercial game's own files, on the 3DS as on the Game
  Boy (Mario Kart GB, Kirby XXL): Vincent Jouillat's Zelda games, Pong
  Pokemon, Orchestrina's Zelda songs, Sonic Robo Blast 2, Nazi Zombies:
  Portable. Their files were not checked one by one; withholding one is a
  line in `WITHHELD`.
