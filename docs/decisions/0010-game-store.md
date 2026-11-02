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
| [Universal-DB](https://db.universal-team.net) | DS, 3DS | 455 entries; 29 DS games | One JSON file of the whole catalog, used by its own Universal-Updater app; downloads are the authors' own GitHub releases | Yes, DS games only |
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
   and Universal-DB's DS games, keeps both in
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
   over one without.
3. **Checked to start.** `scripts/store-compat.py` downloads every game
   the store would offer, runs each headless for 900 frames with no input,
   and records whether any screen showed more than one colour at frame
   300, 600 or 900. Its verdicts are compiled into the app
   (`crates/emu-app/src/store_compat.toml`). Games that crash, stay blank
   or have nothing to play are not offered; games that started are marked
   "Starts in ds-emulator" on their page; games added since the last run
   are offered unchecked. On 2026-11-02, 1,602 of 1,647 started. The
   rest, with what stops each and a lead for the cores, are in
   [games-we-cant-run.md](../research/games-we-cant-run.md), written by
   the same script (`--backlog`), along with the 3DS and NES homebrew
   the app has no core for.
4. **Withheld by hand.** Seventeen catalog entries are crack groups'
   intros and trainers named after, shipped with or holding a commercial
   game (a 512 KiB `TRUE LIES +1`, a 1 MiB `NUTZ ZORN`), and one port of a
   commercial game. Their ROM headers, sizes and archives gave them away.
   `WITHHELD` in `store.rs` lists them, so the store never offers them,
   whatever the catalog or the checks say; the script reads the same
   list.
5. **One click adds a game.** Get downloads the game into its own folder,
   `~/Library/Application Support/ds-emulator/Games/Store/<catalog>/<slug>/`,
   where its saves go beside it, unpacking an archive as an added
   download is unpacked. Some Homebrew Hub files are ZIPs named like the
   ROM inside (`game.gb`); they are told by their first bytes and
   unpacked too. It adds the game to the library under the
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

- An empty library offers the store, and the store offers 1,586 games,
  demos, music carts and tools, all free and shareable.
- The store goes online: for the two catalogs (about 5 MB, once a day),
  covers as they are seen, and games when asked for. Nothing is sent but
  the requests themselves, with the user agent `ds-emulator/<version>`.
- A game's licence and its catalog's credit show on its page. The
  catalogs' takedown routes apply: a game they remove leaves the store at
  the next fetch, though copies already downloaded stay in the library.
- The checks prove a game starts, not that it plays to the end. Games
  that wait for a button on a blank screen would be marked blank. Rerun
  `python3 -I scripts/store-compat.py` after core changes; it caches
  downloads and results in `~/Library/Caches/ds-emulator-store-compat`
  and reruns only what changed.
- What the checks found is a list of leads for the cores, kept in
  [games-we-cant-run.md](../research/games-we-cant-run.md): ten
  dual-mode Game Boy games that stay white in colour mode, demos that
  turn the screen off and wait, a 64-byte intro that needs its ROM
  mirrored, and a GBA header check. Commercial games are never fetched,
  so Pokémon and other retail games play only from the user's own
  dumps; the 3DS has no core (PRD section 4).
