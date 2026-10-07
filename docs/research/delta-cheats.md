# How Delta does Action Replay (and other) cheat codes

Research for adding Action Replay and related cheat formats to this emulator.
Evidence date: 2026-10-07. Source code was read at the pinned commits listed
in [Sources](#sources); numbers in brackets like [3] refer to that list.
Where something is my inference from reading code rather than something I
ran or a document states, the text says so.

## Summary

- Delta keeps cheats per game in Core Data (`Cheat`: name, code, type,
  isEnabled, icon, dates, UUID; `Game.cheats` cascades on delete). You open
  them from the pause menu's "Cheat Codes" item. Each cheat is a name, one
  device type picked from the core's list, and a hex code. Tapping a cheat
  toggles it [3][4][5][6].
- Formats offered: GBC has Game Genie `XXX-YYY-ZZZ` and GameShark `XXXXXXXX`.
  GBA has Action Replay and GameShark (both `XXXXXXXX YYYYYYYY`) plus Code
  Breaker `XXXXXXXX YYYY`. DS has Action Replay `XXXXXXXX YYYYYYYY` only. The
  text field accepts hex digits only, draws the separators itself, and
  reformats the code to the mask when you save [8][10][12][14][6].
- Validation is shallow. The editor checks that the name is set, that neither
  name nor code duplicates another cheat for the game, and that the code's
  length is a whole number of masks. If the core then rejects the code, Delta
  only prints to the debug console, so a bad code is saved, shows as enabled,
  and does nothing [5][7][9].
- CheatBase (Delta 1.4, February 2023) is a bundled SQLite database of 107,940
  cheats, all Action Replay DS. It matches a game by the SHA-1 of the whole ROM
  file, through OpenVGDB rom IDs. Pokémon titles make up 91,012 of those
  cheats, about 84%. The CheatBase repository has no license file [2][17][18].
- melonDSDeltaCore wraps each cheat as one melonDS `ARCode`. melonDS runs every
  enabled code when the ARM7 takes a VBlank IRQ, through ARM7 bus reads and
  writes. Each code starts with offset, data register and condition reset
  [12][13].
- GBADeltaCore hands both "Action Replay" and "GameShark" codes to VBA-M's
  `cheatsAddGSACode(..., v3=true)`, so it decrypts both with the Pro Action
  Replay v3 seeds. From reading the code, genuine GameShark Advance v1/v2
  codes should therefore decode wrongly. VBA-M accepts encrypted codes only
  (Delta issue #347) [10][11][21].
- GBCDeltaCore passes codes to Gambatte. Gambatte applies GameShark type `01`
  writes when the VBlank interrupt is taken and ignores every other type. It
  applies Game Genie by patching the ROM image in every bank where the address
  could map, as long as the compare byte matches [14][15].
- GBATEK documents the full Action Replay DS instruction set, which works like
  a small virtual machine. It also covers the GBA GameShark/Action Replay TEA
  encryption with both seed sets and the full CodeBreaker encryption. It does
  not include the T1/T2 tables needed for `DEADFACE` seed changes [22].
- The minimum to run Pokémon DS codes is an Action Replay DS interpreter for
  types 0–B, C0, D0–DC, E and F. It must read main RAM and the I/O registers
  KEYINPUT (0x04000130), EXTKEYIN (0x04000136) and, rarely, timer 0
  (0x04000100), and write main RAM only. That means widening the current
  `Memory::peek` contract (plain RAM only) to side-effect-free I/O reads.
- Implement from GBATEK, EnHacklopedia and Pan Docs. Do not copy from Delta
  (AGPL-3.0), melonDS (GPL-3.0), VBA-M or Gambatte (GPL-2.0). mGBA (MPL-2.0)
  and SameBoy (Expat) may be reused with attribution under PRD rule 4. Do not
  bundle CheatBase: its license is unknown.

## A. Delta's user-facing cheat feature

### A1. Where cheats live, the form, toggles, ordering, persistence

**Entry point.** The pause menu has a "Cheat Codes" item [4]. On iOS 26 it
pushes a SwiftUI `CheatsView`; on older systems it segues to the UIKit
`CheatsViewController` [4][3]. Delta removes the item while wireless DS
multiplayer is active and in RetroAchievements hardcore mode [3]. The FAQ
describes the same flow: open the pause menu, tap "+", pick the "cheat
device", enter name and code, and tap a cheat to disable it [1].

**List.** The UIKit list sorts by name, shows a check mark on enabled cheats,
toggles on tap, and offers Edit and Delete as swipe actions. When empty it
says "No Cheats — You can add a new cheat by pressing the + button in the top
right." [3] The iOS 26 list, added May 2026, shows each cheat as a capsule
button with an SF Symbol icon (default `star.fill`; an icon picker came in the
same month), the name, and the raw type string such as "ActionReplay". It adds
a sort menu (Name, Date Added, Date Modified, ascending or descending) saved
in settings [3]. For DS games the UIKit "+" button is a menu with "New Cheat
Code" and "Search CheatBase". The CheatBase item is disabled and reads "No
Cheats in CheatBase" when that game has none [3]. In the iOS 26 screen at
HEAD, "+" opens the editor directly, and I found no CheatBase entry point
there [3].

**Edit form.** The form has four sections: Name, Icon, Type and Code [5].

- Type is a segmented control holding the core's `supportedCheatFormats`,
  sorted by name. Its footer reads "Code format is XXXXXXXX YYYYYYYY."
- The Code footer reads "Spaces will be inserted automatically as you type."
  (or "dashes", or both).
- Save stays disabled until the name and code are both non-empty, and Return
  ends editing.
- Saving sets `isEnabled = true` and activates the cheat at once. If you
  changed the code, the old code is deactivated [5][3].

**Persistence.** The `Cheat` entity in the current model (Delta 12) has these
fields [6]:

- `code` (String)
- `creationDate`
- `identifier` (a unique UUID string)
- `isEnabled` (Bool, default NO)
- `modifiedDate`
- `name`
- `symbolName` (optional)
- `type` (transformable `CheatType`)
- `game`, a to-one relation whose inverse `Game.cheats` is to-many with
  Cascade delete

Cheats sync through Delta Sync (Harmony). The synced keys are code,
creationDate, modifiedDate, name and type, plus the game relation. `isEnabled`
and `symbolName` are not synced, and on conflict the newest copy wins [6].

**Activation lifecycle.**

- When a game starts, `updateCheats()` pauses the core ("Core MUST be paused
  when activating cheats, or else race conditions could crash the core"). It
  activates every enabled cheat for the game, deactivates the rest, and
  resumes [7][3].
- DeltaCore's `EmulatorCore` keeps a `[code: CheatType]` dictionary. Every
  activate or deactivate calls the bridge's `resetCheats()`, re-adds every
  stored code, then calls `updateCheats()`. Loading a save state re-runs this
  too [9].
- Because the store is a Swift dictionary keyed by code text, application
  order is unspecified, and two cheats with identical code text collapse into
  one. The validator also forbids duplicate codes per game [9][5].

### A2. Cheat types per system, masks and character sets

`CheatFormat` has four fields: `name`, `format` (the mask), `type` (an
extensible string enum) and `allowedCodeCharacters`. The character set
defaults to `0123456789ABCDEFabcdef` [8]. In a mask, alphanumeric characters
are placeholders and anything else is a separator [8].

| System | UI name | `CheatType` raw value | Mask | Characters | Core call |
|---|---|---|---|---|---|
| GBC | Game Genie | `GameGenie` | `XXX-YYY-ZZZ` | hex | Gambatte `setGameGenie` [14] |
| GBC | GameShark | `GameShark` | `XXXXXXXX` | hex | Gambatte `setGameShark` [14] |
| GBA | Action Replay | `ActionReplay` | `XXXXXXXX YYYYYYYY` | hex | VBA-M `cheatsAddGSACode(code, "code", true)` [10] |
| GBA | GameShark | `GameShark` | `XXXXXXXX YYYYYYYY` | hex | VBA-M `cheatsAddGSACode(code, "code", true)` [10] |
| GBA | Code Breaker | `CodeBreaker` | `XXXXXXXX YYYY` | hex | VBA-M `cheatsAddCBACode` [10] |
| DS | Action Replay | `ActionReplay` | `XXXXXXXX YYYYYYYY` | hex | melonDS `ARCode` via `ParseTextCode` [12] |

Delta has no separate "Pro Action Replay" entry. CheatBase's device table
maps "Action Replay MAX (v3/v4)" to GBA `ActionReplay`, and "GameShark
Advance / Action Replay (v1/v2)" to GBA `GameShark` [17][18]. The table also
lists Codebreaker DS, but no DS core supports that type, so importing one
would fail with "Delta does not support this cheat type". There are zero such
rows in the data [17][5].

**How auto-formatting works.**

- `CheatTextView` stores only allowed characters. Typing or pasting anything
  else is filtered out.
- It draws the separator characters as extra glyphs during layout, and sets
  its right inset so the text wraps at exactly one code's width.
- On save, `String.formatted(with:)` cuts the hex stream into the mask,
  repeats the mask with `\n` between codes, and uppercases the result [5][8].

So users paste codes with any spacing and get canonical
`XXXXXXXX YYYYYYYY\nXXXXXXXX YYYYYYYY` text. GitHub issue #39 (2019) reports
an early bug where line breaks were not inserted [21].

### A3. Validation and when errors appear

1. **Editor.** Save is disabled while the name or code is empty [5].
2. **`CheatValidator` on save.** It throws for an empty name, a missing game,
   another cheat for the same game with the same name or the same code, or a
   code whose length without newlines is not a multiple of the mask length
   (`sanitizedCode.count % format.format.count != 0`). Each case shows an
   alert: "Invalid Code — Please make sure you typed the cheat code in
   correctly and try again.", "Duplicate Name", "Duplicate Code" or "Invalid
   Name" [5].
3. **The core's `addCheatCode`.** It returns `NO` only for shape errors:
   - GBA: a character other than hex or space, Action Replay/GameShark codes
     not 16 digits, or a Code Breaker line not 13 characters.
   - GBC: wrong length (11 with dashes for Game Genie, 8 for GameShark) or
     bad characters.
   - DS: a line not exactly 17 characters, or bad characters [10][14][12].

   `EmulatorCore.activate` turns `NO` into `CheatError.invalid` [9]. Delta's
   `activateCheatWithErrorLogging` catches that and only `print`s it [7].
4. **Semantic errors never reach the user.**
   - VBA-M reports bad or wrong-game codes through its own `systemMessage`,
     and the bridge returns `YES` regardless [11][10].
   - melonDS prints `!! bad AR opcode` and stops that code at run time [13].

Two consequences come from reading the code; I have not run either:

- GBC Game Genie codes must have 9 digits. GBCDeltaCore rejects the 6-digit
  form, which Pan Docs says newer devices accept and which Gambatte's decoder
  handles [14][15][24].
- The melonDS fork's `ARCode` holds a fixed array of 128 words (64 code
  lines). The bridge tells `ParseTextCode` to stop at 128 words but sets
  `CodeLen` to the full hex length ÷ 8 without clamping. A cheat longer than
  64 lines therefore appears to make `RunCheat` read past the array.
  CheatBase has 336 DS cheats longer than 64 lines, 328 of them Pokémon; the
  longest is 343 lines [12][13][17].

### A4. CheatBase

- **What and when.** Added in Delta 1.4 (2023-02-28, "thanks Noah Keck"):
  "View and add all known cheats for supported games … Groups cheats by
  category … Limited to DS games for now" [2].
- **Data.** Delta bundles `Resources/cheatbase.zip`, a 3.1 MB zip holding a
  22 MB SQLite file dated 2023-02-03. It contains 107,940 rows in `CHEATS`,
  every one with `cheatDeviceID = 10` (Action Replay DS) [18][17].
  - Columns: `cheatName`, `cheatActivation` (e.g. "Press Select+Down to
    activate."), `cheatDescription`, `cheatSideEffect`, `cheatFolderName`,
    `cheatCategoryID`, `cheatCode` (newline-separated lines),
    `cheatDeviceID` and `cheatCredit` [17].
  - Delta copies the database into the app container on first launch, or
    when `CheatBase.cheatsVersion` changes [18].
- **Matching.**
  - `Game.identifier` is the SHA-1 of the ROM file [18].
  - `GamesDatabase.metadata(for:)` looks up that SHA-1 in Delta's trimmed
    OpenVGDB copy (`ROMs(romID, systemID, romHashSHA1)`) and gets a `romID`
    [18].
  - `CheatBase.cheats(for:)` selects `CHEATS` rows with that `romID`, joins
    categories and orders by name [18].
  - Trimmed or modified dumps therefore do not match. The source DB even
    lists Black/White entries as `[b]` dumps, while Delta's OpenVGDB copy
    maps the same rom ID to a different SHA-1 [17][18].
- **Import UX.**
  - The browser groups cheats into collapsible category sections, each with
    the category description as a footer, and searches name and description.
  - An info button shows "How to Activate" with the activation hint.
  - Tapping a cheat strips whitespace from the code and formats it with the
    device's mask. It then validates, activates and saves the cheat as
    enabled, and closes the browser. Errors show an "Unable to Add Cheat"
    alert [18][3].
- **Licensing and provenance.**
  - The CheatBase repo describes itself as "Based on OpenVGDB". It has no
    license file, and the GitHub API reports `license: null`.
  - Its Python scripts say "Copyright Noah Keck - All Rights Reserved".
  - `cheatCredit` is empty in 107,041 rows; the rest credit "McFizzle" (898)
    and "Kenobi" (1).
  - The upstream source of the codes is not stated.
  - OpenVGDB's repo also has no license file [17][19].
- **Requests.** Users have asked for importing cheat files and lists:
  `usrcheat.dat` in #160, gamehacking.org exports in #301, bulk JSON/CSV in
  #367, and raw address:value cheats in #416 [21].

## B. How Delta's cores apply cheats

### B1. melonDSDeltaCore → melonDS AREngine

**Conversion.**

- `addCheatCode` splits the code on `\n` and requires every line to be
  exactly 17 characters of hex or space.
- It strips everything that is not hex and runs the result through
  `ParseTextCode`, a copy of an old melonDS helper, into one
  `ARCode { Name, Enabled = YES, CodeLen = hexDigits/8, Code[128] }`.
- It wraps that code in its own `ARCodeCat` and appends it to a shared
  `ARCodeFile`.
- `resetCheats` clears the categories and calls `AREngine::Reset()`.
  `updateCheats` calls `AREngine::SetCodeFile` [12].
- Delta's melonDS fork is based on melonDS 0.9.5 [13].

**When it runs.** `ARM::TriggerIRQ` contains this comment: "ARDS cheat
support — normally, those work by hijacking the ARM7 VBlank handler". When
the ARM7 (`Num == 1`) takes an IRQ with VBlank both requested and enabled
(`IF & IE & VBlank`), it calls `AREngine::RunCheats()` [13].

- `RunCheats` runs every enabled code in category order.
- `Reset` binds the bus functions to `NDS::ARM7Read*/ARM7Write*`, or the DSi
  equivalents [13].
- So cheats run once per frame, on the ARM7 bus. That matches GBATEK, which
  says the real device hooks the ARM7 by default and so "there is no access
  to VRAM, TCM and Cache" [22].
- Upstream melonDS keeps the same trigger. Commit #2108 (2024-08) split
  `AREngine` from `ARCodeFile` and notes that cheats "are supposed to be
  executed each frame" [16].

**Interpreter behaviour** (fork, unchanged upstream except as noted) [13][16]:

- Each `ARCode` runs with fresh `offset = 0`, `datareg = 0`, `cond = 1`, an
  empty condition stack and no loop.
- Conditionals push `cond` onto a 32-bit stack. While `cond` is false, every
  opcode except C5 and D0–D2 is skipped, and a skipped E code also skips its
  data lines.
- Types 3–A read `[offset]` when the address field is 0.
- C4 is unsupported and aborts the code ("THE FUCKING C4000000 OPCODE").
- C5's counter is never reset ("TODO: does anything reset this??").
- An unknown opcode stops the code.
- Upstream added extended D4 operations on 2025-10-13 (commit 91ab680, "AR:
  add fun D4 opcodes"). The low byte of the first word selects the
  operation: 00 add, 01 OR, 02 AND, 03 XOR, 04 LSL, 05 LSR, 06 ROR, 07 ASR,
  08 MUL. GBATEK does not document these [16].

### B2. GBADeltaCore → VBA-M

**Routing.** `addCheatCode` splits on `\n` and rejects anything other than hex
or space [10].

- "Action Replay" and "GameShark" lines have their spaces removed, must be 16
  digits, and go to `cheatsAddGSACode(code, "code", true)`. The third
  argument is `v3`, and it is `true` for both types.
- "Code Breaker" lines must be 13 characters and go to `cheatsAddCBACode`.
- `resetCheats` calls `cheatsDeleteAll(true)`, which writes back the
  remembered old values of plain writes and ROM patches.
- `updateCheats` is empty.

**Decryption and detection in VBA-M** [11]:

- `cheatsAddGSACode` applies any `DEADFACE` seed change, then runs TEA
  decryption: 32 rounds, delta `0x9E3779B9`, starting sum `0xC6EF3720`. It
  uses the v1 seeds or the v3 seeds depending on the flag.
- A decrypted value of `0x001DC0DE` is the ID code. VBA-M compares it with
  the game code at ROM 0xAC and warns on mismatch.
- With `v3`, `C4xxxxxx` becomes the master (hook) code, and the opcode is
  decoded from the address bits.
- There is no automatic choice between v1/v2 and v3: the caller decides.
  Delta always says v3. From reading the code, GSA v1/v2 codes entered as
  "GameShark" are therefore decrypted with the wrong seeds (not tested).
- Issue #347 (2024) asks for unencrypted GSA/AR input because VBA-M accepts
  only encrypted codes [21].
- `cheatsAddCBACode` handles CodeBreaker, including the `9xxxxxxx`
  encryption seed code.

**When cheats run.**

- With no master code, VBA-M calls `cheatsCheckKeys` once per frame at the
  start of VBlank.
- With a master code, it runs the cheats whenever the CPU's next PC equals
  the hook address (`cpuMasterCodeCheck`) [11].

### B3. GBCDeltaCore → Gambatte

**GameShark.**

- Each line becomes a `GBCCheat` with length and character checks.
  `updateCheats` joins the codes with `;` and passes them to Gambatte's
  `setGameShark` [14].
- Gambatte parses each code as type, value and address, with the address
  stored low byte first [15].
- `Interrupter::interrupt` calls `applyVblankCheats` when it dispatches the
  VBlank vector 0x40. That writes `value` to `address` for type `0x01` only;
  other types (`8x`, `9x`) are accepted and silently ignored [15].
- Because of this, cheats run only when the game actually takes VBlank
  interrupts.

**Game Genie** (`setGameGenie`) [15]:

- Gambatte first undoes earlier patches from its undo list.
- It then decodes each code: new value from digits 0–1; address from digits
  2, 4, 5 and 6, with the top nibble XORed with `F`.
- If the code has 9 digits, it also decodes a compare byte from digits 8 and
  10: XOR `0xFF`, rotate right 2, XOR `0x45`.
- It writes the new value into the loaded ROM image in every bank where the
  address could be mapped and the original byte matches the compare value
  (or always, if there is no compare value).

## C. The code formats (authoritative docs)

### C1. Action Replay DS

The full table is in the [AR DS code-type reference](#ar-ds-code-type-reference)
below. It comes from GBATEK's "DS Cart Cheat Action Replay DS" section, which
credits Kenobi and Dualscreenman of Kodewerx, and from EnHacklopedia
[22][23]. Execution rules:

- A cheat runs as a program over four internal registers: `offset`,
  `datareg`, `loopcount` and a C5 `counter` [22].
- **Conditions.** Types 3–A are IFs, and "IF/ENDIF can be nested up to 32
  times". "The condition register is checked, for all code types but the D0,
  D1 and D2 code type". C5 is checked after it increments its counter, so
  the counter always increments [22].
  - EnHacklopedia adds: on a false IF, following codes are skipped "until a
    code type D0 or D2 is encountered, or until the end of the code list"
    [23].
- **Loops.** "FOR/NEXT cannot be nested, any FOR statement does forcefully
  terminate any prior loop. FOR does backup the current IF condition flags,
  and NEXT does restore these flags." [22] EnHacklopedia: "When repeat is
  executed, the AR reloads the 'next code to be executed' and the 'execution
  status'." [23]
  - melonDS restores the flags only when the loop finishes, not on each
    repeat (its comment says "checkme … GBAtek is not very clear") [13].
- **Flush.** D2 ends the loop, then resets `offset = 0` and `datareg = 0` and
  clears all condition flags, "so further ENDIF(s) aren't required". In
  practice nearly every cheat ends with `D2000000 00000000` [22].
  - In CheatBase's Pokémon codes, D2 is the most common opcode, used 90,719
    times [17].
- **The 0x0 and offset quirks.**
  - Types 0–2 and B always add `offset` to the 28-bit address.
  - From firmware v1.54, conditionals 3–A whose address field is 0 read
    `[offset]` instead of address 0 [22].
  - Older firmware could gain the same effect only through handler patches
    ("Hack #1"); EnHacklopedia lists other handler patches that turn D4 into
    ORR, AND or ADD-to-offset [23]. Those patches write the code handler at
    0x023FExxx, so they only make sense on real hardware.
  - `00000000 XXXXXXXX` lines are hook codes only inside a nine-line block
    marked (M). Anywhere else they are word writes to `[0 + offset]` [22].
    melonDS does not special-case them [13].
  - Before v1.54, DB wrongly added XXXXXXXX to `offset` after reading [22].
  - D3, DC, C6 and D6–DB take their address from the full 32-bit second
    word. Types 0–B, E and F take a 28-bit address from the first word
    [22][13].
- **Alignment.** Word and halfword accesses should be aligned. E and F copy
  with word loads and stores, then bytes for an odd tail [22].
- **Hook ("M") code.** Nine `00000000 XXXXXXXX` lines give the address used
  before launch, the hook address inside the ARM7 executable, the hook mode,
  the replaced opcode, and where to store the handler and the code list. By
  default the AR hooks the ARM7 automatically [22]. Emulators do not need
  hooks.
- **Game ID line.** `ABCD-NNNNNNNN` is the ASCII game code from header
  offset 0x00C plus the CRC32 of header bytes 0x000–0x1FF [22]. That is a
  better key for matching cheat lists than a whole-file SHA-1.

Where melonDS differs from GBATEK, as listed in B1:

- registers reset per cheat;
- condition flags not restored on each NEXT repeat;
- C4 unsupported;
- the C5 counter never reset;
- v1.54 behaviour throughout (offset-relative conditionals, fixed DB);
- extra D4 operations upstream [13][16].

### C2. GBA GameShark/Action Replay v1/v2, Pro Action Replay v3, CodeBreaker

**GameShark Advance / Action Replay v1/v2 raw codes** (GBATEK [22]):

| Code | Effect |
|---|---|
| `0aaaaaaa 000000xx` | byte write |
| `1aaaaaaa 0000xxxx` | halfword write |
| `2aaaaaaa xxxxxxxx` | word write |
| `3000cccc xxxxxxxx` + address lines | write xxxxxxxx to a list of addresses |
| `60aaaaaa y000xxxx` | ROM patch at 0x8000000 + a×2 |
| `8a1aaaaa`/`8a2aaaaa` | write only while the GS button is held |
| `Daaaaaaa 0000xxxx` | if halfword == xxxx, run the next code |
| `E0zzxxxx 0aaaaaaa` | if halfword == xxxx, run the next zz codes |
| `Faaaaaaa 00000x0y` | hook (master) code |
| `xxxxxxxx 001DC0DE` | ID code: xxxxxxxx is the game code at 0x080000AC |
| `DEADFACE 0000xxyy` | change the encryption seeds |

**Pro Action Replay v3 raw codes** [22]:

- `C4aaaaaa` is the hook. The ID code and `DEADFACE` work as in v1/v2.
- `00/02/04aaaaaa` are fill writes; `40/42/44` are indirect (pointer)
  writes; `80/82/84` add.
- `C6/C7aaaaaa` write the I/O area.
- Conditionals use `iiaaaaaa`, where ii combines a condition (= ≠ signed and
  unsigned < > AND), a width, and an action: run the next 1 or 2 codes, run
  until ELSE/ENDIF, or turn all codes off.
- `00000000` prefixes introduce slide/fill, button and ROM-patch variants
  that span two lines.
- Addresses are packed: `a0aaaaa` means bits 20–23 of the address come from
  the code's "aa" byte and the low 18 bits from the rest (VBA-M:
  `(address & 0x00F00000) << 4 | (address & 0x0003FFFF)`) [22][11].

**Encryption** (GBATEK, "Gameshark Encryption") [22]:

- A is the left word, V the right word. For I = 1 to 32:
  `A += (V*16+S0) XOR (V+I*9E3779B9h) XOR (V/32+S1)`, then
  `V += (A*16+S2) XOR (A+I*9E3779B9h) XOR (A/32+S3)`.
- This is TEA: 32 rounds, delta 0x9E3779B9. Decryption runs it backwards from
  sum 0xC6EF3720, which is what VBA-M does [11].
- GSA v1/v2 seeds: S0=09F4FBBD, S1=9681884A, S2=352027E9, S3=F3DEE5A7.
- PAR v3 seeds: S0=7AA9648F, S1=7FAE6994, S2=C0EFAAD5, S3=42712C57.
- `DEADFACE 0000xxyy` rebuilds the seeds from translation tables T1 and T2,
  which GBATEK says are "contained in the gameshark cartridge" and does not
  give. Both VBA-M and mGBA include the tables for v1 and v3 [22][11][20].
- The seeds come from Datel's device firmware. GBATEK publishes the initial
  seeds and the algorithm; the tables exist only in emulator source.

**CodeBreaker / Xploder** [22]:

- `0000xxxx 000y` is the game ID plus a CRC-like checksum of the first 64 KiB
  of ROM. `1aaaaaaa 000z` is the hook.
- Types: 2 OR, 3 byte write, 4 slide, 5 patch bytes, 6 AND, 7 if equal,
  8 halfword write, A if not equal, B/C signed > and <, D `D0000020 yyyy` if
  the joypad AND yyyy is 0, E add, F if AND.
- `9xyyxxxx xxxx` turns on encryption for every following code. GBATEK gives
  the whole algorithm: an LCG with constants 41C64E6D/3039, a 48-entry bit
  swap list, and a seed list. It is implementable from the spec alone.
- This repo already handles unencrypted types 0/1 (ignored), 2, 3, 6, 7, 8,
  A and E (`crates/emu-cheats/src/code.rs`).

**Telling the formats apart.** Nothing in a 16-digit code says whether it is
encrypted, or whether it is v1/v2 or v3.

- Delta and VBA-M make the user (or the caller) choose.
- mGBA decrypts the first code of a set four ways: with the GSA seeds, with
  the PAR seeds, and raw under each format. It scores each result for
  plausibility (known opcode, address inside EWRAM, IWRAM or I/O, value
  width fits) and treats `001DC0DE` and `DEADFACE` as certain. It then locks
  that version for the rest of the set, and `!GSAv1` / `!PARv3` / `… raw`
  directives override the guess [20].

**Master codes.** GBATEK says the hook "should point to an opcode which is
executed several times per second", and that GSA "can not write to RAM"
without it [22]. Emulators can ignore hooks and apply cheats once per frame:

- VBA-M does that when no hook is given, and otherwise runs cheats at the
  hook PC [11].
- mGBA installs a breakpoint at the hook when one is present [20].
- This repo already ignores CodeBreaker master codes.

### C3. Game Boy / Game Boy Color

**Game Genie (ROM patches).** Pan Docs [24]:

- `ABC-DEF-GHI` breaks down as AB = new data, FCDE = address XOR $F000,
  GI = old data XOR $BA rotated left by two, H = unknown.
- The address is in ROM ($0000–7FFF). The device compares address and old
  data on every read and substitutes the new data, which "more or less
  prohibits unwanted patching of wrong memory banks".
- Newer devices accept the 6-digit form, which has no compare byte. Three
  codes can be active at once.

**GameShark (RAM writes).**

- Pan Docs: 8 digits where byte 0 is an "SRAM bank", byte 1 the new value,
  and bytes 2–3 the address, low byte first. `010238CD` writes $02 to $CD38
  [24].
- EnHacklopedia lists three forms [23b]:
  - `01xxyyzz`: write xx to zzyy;
  - `8bxxyyzz`: switch the RAM bank to b, then write;
  - `9bxxyyzz`: switch the WRAM bank to b, then write (GBC only).
- The device hooks the VBlank handler and rewrites RAM every frame [24].
- SameBoy (Expat) treats byte 0 = 01 as "any bank" and any other value as
  bank `& 0xF`. It applies GameShark and Game Genie both as read-time
  substitutions [25].
- Gambatte applies `01` only [15].
- Search results suggest that Pokémon Crystal code lists use `91xxxxxx`
  codes; I did not verify the lists line by line. If so, this emulator's
  current GameShark parser, which accepts `01` only, rejects them.

## D. Recommendations for this emulator

Current design (read 2026-10-07):

- `emu-cheats` parses a plain-text `game.cht` (`[name]`, optional `off`,
  one code per line) into a flat `Vec<Op>` of Write, Modify and If-next-op
  operations.
- `emulator.rs` applies the enabled cheats before every `run_frame` through
  `Memory::peek`/`poke`.
- The core contract is "`None` where that is not plain RAM (I/O registers,
  ROM, unmapped space)". DS `peek`/`poke` cover main RAM 0x02xxxxxx for
  player 0 only. GBA covers EWRAM and IWRAM; GB covers cartridge RAM, WRAM
  and HRAM.

### D1. What to adopt from Delta

- **One list per game, each cheat a name, a device, a code and on/off.**
  `game.cht` already has the shape. Add an optional device token after the
  name, e.g. `[Infinite money] off gsa` with `ards`, `gsa`, `par3`, `cb`,
  `gs`, `gg` or `raw`. The default stays autodetect. This settles the
  16-digit GBA ambiguity the way Delta's type picker does.
- **Masks and whitespace tolerance.** Delta strips everything that is not
  hex and re-chunks to the mask, so pasted codes with odd spacing still
  work. Per device, strip whitespace and split into fixed groups: GS 8
  digits, GG 9 (dashes optional), CB 12, GSA/PAR/ARDS 16.
- **Activation hints.** CheatBase's `cheatActivation` ("Press Select+Down to
  activate") is the most useful field in the database for button-activated
  codes. In `.cht` this is just a `#` comment under the name; mention it in
  the template.
- **Checks on save.** Keep reporting errors with a line number, as
  `ParseError` already does, but go further than Delta. Report unknown
  opcodes, truncated E blocks and wrong lengths when parsing, because Delta
  saves such codes and they fail silently.
- **Optional, later:** an in-app list that toggles `on`/`off` by rewriting
  the file, as Delta's tap-to-toggle does. Also consider disabling cheats if
  online play or achievements ever arrive; Delta hides the menu in both
  cases.

### D2. What to skip

- Core Data, Delta Sync, SF Symbol icons and sort menus. Order in the file
  is the order of application.
- Bundling CheatBase. Its license is unknown, it is 22 MB and DS only, and it
  matches on whole-file SHA-1. Support importing user-supplied lists instead:
  melonDS's text `.mch` format (`CAT name`, `CODE 1 name`, then
  `XXXXXXXX YYYYYYYY` lines) is simple [13], and `usrcheat.dat` is the
  format users ask Delta for [21]. Match DS lists by game code + header
  CRC32, as in GBATEK's `ABCD-NNNNNNNN` line.
- Delta's core quirks: both GBA types forced to v3, fixed 128-word DS
  codes, dictionary order, errors that only reach the console.

### D3. Minimum feature set for Pokémon

**DS (HG/SS, B/W, B2/W2).** These need a real Action Replay DS interpreter:
nesting, loops, registers and inline E data do not fit the flat
`Vec<Op>`.

Opcode use across CheatBase's 91,012 Pokémon cheats, by number of code
lines [17]:

- D2 90,719; 0 69,959; 1 52,351; B 51,051; 9 45,335; 5 39,415; 6 24,238;
- C0 15,647; DC 13,654; 2 8,713; E 3,627; D6 3,243; D5 1,895; D9 1,408;
- D4 1,259; D7 1,226; DA 1,219; D3 1,214; D0 1,203; A 853; 3 420; D8 335;
- DB 170; 7 62; 4 44; D1 26; 8 24;
- C4, C5, C6 and F: 0. Across all DS cheats, C6 appears 9 times and F 299.

So the minimum is types 0–B, C0, D0–DC, E and F. C4, C5 and C6 can wait.
Patterns seen in the data [17]:

- **Button activators.** `94000130 FCFF0000` means "if L and R are held":
  the mask leaves bits 8–9, active low. `A4000130` is the negated form, and
  X/Y codes read `94000136` (EXTKEYIN) or `927FFFA8`.
- **Pointer chains.** HG/SS: `62111938 00000000` (if the pointer is not
  null), `B2111938 00000000` (offset = pointer), `20001DE4 000000FF` (byte at
  pointer + 0x1DE4), `D2000000 00000000`.
- **Code-in-RAM guards and patches.** B/W/B2/W2 check a Thumb instruction
  pair in an overlay with `5xxxxxxx` (e.g. `521A9444 D1032800`), then patch
  it with `1xxxxxxx`, e.g. `121AA440 000046C0` (a Thumb NOP). E blocks inject
  short routines.
- **Toggles latched in scratch RAM.** `DA000000 04000130` loads KEYINPUT into
  the data register; `D7000000 02000DFE` stores it.
- **Bulk edits.** C0 loops built from D9/D6/DC/D4 rewrite boxes and parties.
- **Length.** Up to 177 lines for Pokémon and 343 across all DS cheats, so
  store code words in a `Vec`.

**GBA (FireRed/LeafGreen, Ruby/Sapphire/Emerald).**

- Finish CodeBreaker: types 4, 5, B, C, D and F, plus `9` encryption from
  GBATEK.
- Add GSA v1/v2 and PAR v3: TEA decryption with both seed sets, the opcode
  tables above, per-cheat autodetection with a device override, and an
  option for raw codes (#347).
- Ignore hooks and ID codes, but use `001DC0DE` as a strong detection signal.
- `DEADFACE` is optional: it needs the T1/T2 tables, so either take mGBA's
  under MPL-2.0 or report it as unsupported.
- Most GBA codes in circulation are encrypted (per the reporter of #347).
  This work is the bulk of GBA value.

**GB/GBC (Red/Blue/Yellow, Gold/Silver/Crystal).**

- GameShark `01` already works. Add `8b` (SRAM bank) and `9b` (WRAM bank),
  needed if Crystal lists really use `91…`.
- Game Genie ROM patches (6 and 9 digits) are less common in Pokémon lists
  and can come after.

### D4. What each format needs from the cores

- **Widen `peek` to side-effect-free I/O reads.** For DS, expose KEYINPUT
  0x04000130 (2 bytes), EXTKEYIN 0x04000136 and ARM7 TM0CNT_L 0x04000100
  (computed counter, no side effects). In Pokémon codes, 43,475 absolute
  reads hit 0x04: 43,308 KEYINPUT, 105 EXTKEYIN, 62 timer 0 [17]. For GBA,
  expose 0x04000130 for CodeBreaker `D0000020` and PAR v3 button codes.
  Update the `Core::peek` doc comment; the current "not plain RAM" wording
  excludes these.
- **Writes can stay RAM-only for now.** Absolute writes in Pokémon codes go
  to main RAM (80,723), plus 38 to OAM and 4 junk addresses, and none to I/O
  [17]. PAR v3 `C6/C7` I/O writes can return "unsupported" until a bus write
  path for cheats exists.
- **Use the ARM7's view.** The real device and melonDS run on the ARM7, so
  ARM9 TCM, VRAM and ARM9-only I/O are out of reach [22][13]. Main RAM is
  shared, and the current mask (`MAIN_RAM_SIZE - 1`) already covers the
  mirrors up to 0x02FFFFFF, including 0x027FFFA8 and 0x023FFxxx. EXTKEYIN
  and the timers must come from the ARM7 side.
- **Input timing.** `emulator.rs` applies cheats before `run_frame`, and the
  DS `run_frame` calls `set_input` first. Cheats therefore see last frame's
  keys. Either set input before applying cheats, or apply cheats inside the
  core at VBlank, which is what the real devices hook. One frame of lag is
  tolerable, but hold-to-activate codes feel it.
- **Code patches in RAM.** These work today because ARM9 caches are not
  emulated (`emu-arm/src/cp15.rs`). If a decoded-instruction cache or a JIT
  is added, cheat writes must invalidate it.
- **Unreadable addresses.** Treat an unreadable address as a false condition
  and an unwritable one as a skipped write. Keep per-cheat fresh registers,
  as melonDS does: lists are written to end each cheat with D2, and
  toggling single cheats needs independent state.
- **Game Genie** needs a ROM-read hook in the GB cartridge, not `poke`. For
  example, a separate core method that installs
  `RomPatch { address, value, compare: Option<u8> }` and applies it at read
  time with the compare check (SameBoy's approach [25]). Gambatte patches the
  ROM image with an undo list instead [15]. GBA ROM patches (GSA `6`,
  PAR v3 `18/1A/1C/1E`) could use the same unit later.
- **GameShark `8b`/`9b`** need a bank-qualified write: either a GB-specific
  memory extension, or a bank number encoded above bit 16 of the address.

A suggested shape for `emu-cheats`, one unit per format:

- `ards.rs`: words → run against `Memory`.
- `gsa.rs`: TEA plus detection → ops.
- `codebreaker.rs`
- `gameshark_gb.rs`
- `game_genie_gb.rs`: text → ROM patches.

`Cheat` carries a program per format (flat ops, or Action Replay DS words)
and is parsed per cheat, not per line: Action Replay DS E data lines and
PAR v3 two-line codes are not standalone codes. Test with synthetic codes
against the existing test `Ram`, not with copied database rows.

### D5. Licensing: spec versus implementation

**Spec, safe to implement from:**

- GBATEK: Action Replay DS table and rules, the GBA CodeBreaker, GSA and
  PAR v3 tables, the TEA formula with both initial seed sets, and the full
  CodeBreaker encryption [22].
- EnHacklopedia: the same Action Replay DS semantics, plus GB GameShark
  `8b`/`9b` [23][23b].
- Pan Docs (CC0-1.0): the Game Genie and GameShark layouts [24].

**Implementation-specific, read but do not copy:**

- Delta (AGPL-3.0): UI, validator, formatter.
- melonDS (GPL-3.0-or-later): bitmask condition stack, per-cheat reset,
  extended D4 operations, the IRQ-time trigger.
- VBA-M (GPL-2.0): decode switch, master-code hook, restore on delete.
- Gambatte (GPL-2.0): VBlank-dispatch trigger, ROM-image patching.

**Reusable under PRD rule 4 with attribution:**

- mGBA (MPL-2.0, file-level copyleft): the autodetect heuristic and the
  GSA/PAR `DEADFACE` T1/T2 tables, the only source for those tables besides
  VBA-M [20].
- SameBoy (Expat, excluding its iOS directory): GB GameShark bank handling
  and Game Genie decoding [25].

**Data:** CheatBase and OpenVGDB have no license file [17][19]. Do not
bundle either.

## AR DS code-type reference

Semantics from GBATEK [22], cross-checked with EnHacklopedia [23]. "Gated"
means the opcode is skipped while the condition is false. The Pokémon count
is the number of code lines using that opcode in CheatBase's 91,012 Pokémon
cheats [17].

| Code | Operation | Gated | Notes | Pokémon count |
|---|---|---|---|---|
| `0XXXXXXX YYYYYYYY` | word[X+offset] = Y | yes | `00000000 XXXXXXXX` inside a nine-line (M) block is a hook code; ignore it | 69,959 |
| `1XXXXXXX 0000YYYY` | half[X+offset] = Y | yes | | 52,351 |
| `2XXXXXXX 000000YY` | byte[X+offset] = Y | yes | | 8,713 |
| `3XXXXXXX YYYYYYYY` | IF Y > word[X] (unsigned) | yes | pushes the condition; v1.54: X=0 reads [offset] | 420 |
| `4XXXXXXX YYYYYYYY` | IF Y < word[X] (unsigned) | yes | as above | 44 |
| `5XXXXXXX YYYYYYYY` | IF Y = word[X] | yes | as above; B/W guards on overlay code | 39,415 |
| `6XXXXXXX YYYYYYYY` | IF Y ≠ word[X] | yes | as above; pointer not-null checks | 24,238 |
| `7XXXXXXX ZZZZYYYY` | IF Y > (~Z AND half[X]) | yes | as above | 62 |
| `8XXXXXXX ZZZZYYYY` | IF Y < (~Z AND half[X]) | yes | as above | 24 |
| `9XXXXXXX ZZZZYYYY` | IF Y = (~Z AND half[X]) | yes | button activators `94000130 …`, `94000136 …` | 45,335 (+12 with X=0) |
| `AXXXXXXX ZZZZYYYY` | IF Y ≠ (~Z AND half[X]) | yes | as above | 841 (+12 with X=0) |
| `BXXXXXXX 00000000` | offset = word[X+offset] | yes | pointer load | 51,051 |
| `C0000000 YYYYYYYY` | FOR: run the block Y+1 times | yes | saves the condition flags; ends any earlier loop | 15,647 |
| `C4000000 00000000` | offset = address of this code (v1.54) | yes | melonDS does not support it | 0 |
| `C5000000 XXXXYYYY` | counter += 1; IF (counter AND Y) = X (v1.54) | counter always increments | melonDS never resets the counter | 0 |
| `C6000000 XXXXXXXX` | word[X] = offset (v1.54) | yes | | 0 (9 DS-wide) |
| `D0000000 00000000` | ENDIF (pop the condition) | no | | 1,203 |
| `D1000000 00000000` | NEXT | no | restores the FOR-time flags on repeat (GBATEK); melonDS restores only at the end | 26 |
| `D2000000 00000000` | NEXT, then flush: offset = 0, datareg = 0, flags cleared | no | ends nearly every cheat | 90,719 |
| `D3000000 XXXXXXXX` | offset = X | yes | 32-bit X | 1,214 |
| `D4000000 XXXXXXXX` | datareg += X | yes | upstream melonDS: `D40000nn` with nn 01–08 = OR/AND/XOR/LSL/LSR/ROR/ASR/MUL (not in GBATEK) | 1,259 |
| `D5000000 XXXXXXXX` | datareg = X | yes | | 1,895 |
| `D6000000 XXXXXXXX` | word[X+offset] = datareg; offset += 4 | yes | | 3,243 |
| `D7000000 XXXXXXXX` | half[X+offset] = datareg; offset += 2 | yes | | 1,226 |
| `D8000000 XXXXXXXX` | byte[X+offset] = datareg; offset += 1 | yes | | 335 |
| `D9000000 XXXXXXXX` | datareg = word[X+offset] | yes | | 1,408 |
| `DA000000 XXXXXXXX` | datareg = half[X+offset] | yes | `DA000000 04000130` reads KEYINPUT | 1,219 |
| `DB000000 XXXXXXXX` | datareg = byte[X+offset] | yes | before v1.54 also did offset += X (bug) | 170 |
| `DC000000 XXXXXXXX` | offset += X | yes | | 13,654 |
| `EXXXXXXX YYYYYYYY` | copy the Y parameter bytes that follow to [X+offset] | yes | data lines padded with zeros to 8 bytes; skip them when gated | 3,627 |
| `FXXXXXXX YYYYYYYY` | copy Y bytes from [offset] to [X] | yes | set offset first with D3, DC or B | 0 (299 DS-wide) |
| `ABCD-NNNNNNNN` | game ID: game code at 0x00C + CRC32 of 0x000–0x1FF | n/a | a list header, not a code | — |

## Sources

All fetched or read on 2026-10-07. Git sources were read at the commits
shown.

1. Delta FAQ, "Cheat Codes" — https://faq.deltaemulator.com/using-delta/cheat-codes (Markdown version: `.md` suffix)
2. Delta FAQ, "Release Notes" (1.4, 2023-02-28: CheatBase; 1.6, 2024-07-10: "Fixed preparing CheatBase every app launch") — https://faq.deltaemulator.com/release-notes
3. Delta app, commit `c1d3d068e019e6493eed45654569db3cc5beb86a` (2026-07-30, "2.0b5") — https://github.com/rileytestut/Delta/tree/c1d3d068e019e6493eed45654569db3cc5beb86a
   - `Delta/Pause Menu/Cheats/CheatsViewController.swift` (list, "+" menu L113–145, CheatBase import L190–229, toggle L264–285)
   - `Delta/Pause Menu/Cheats/CheatsView.swift`, `CheatsButton.swift`, `CheatIconPickerView.swift` (iOS 26 list; first commits 2026-05-30, "[DLTA-77] New liquid glass cheats screen (#25)", "[Feature] Cheat icon picker (#26)")
   - `Delta/Emulation/GameViewController.swift` (menu removed in multiplayer and hardcore, ~L630–650; `updateCheats()` on start, ~L766)
4. `Delta/Pause Menu/PauseViewController.swift` L265–280 and `Delta/Pause Menu/Menu/PauseView.swift` L180 at the same commit.
5. `Delta/Pause Menu/Cheats/EditCheatViewController.swift` (sections, save L265–330, footers L369–392, input filter L396–420), `CheatValidator.swift` (L43–75), `CheatTextView.swift`, `Delta/Database/Cheats/CheatDevice.swift`, at the same commit.
6. `Delta/Database/Model/Delta.xcdatamodeld/Delta 12.xcdatamodel/contents` (entity `Cheat`; `Game.cheats` Cascade) and `Delta/Database/Model/Human/Cheat.swift` (sync keys L40, default icon L57), at the same commit.
7. `Delta/Extensions/EmulatorCore+Delta.swift` L24–75 at the same commit.
8. DeltaCore, commit `633dfa86967816315fe19b482511dab1ce517f28` (2026-07-21): `DeltaCore/Model/CheatFormat.swift`, `DeltaCore/Extensions/CharacterSet+Hexadecimals.swift` — https://github.com/rileytestut/DeltaCore/tree/633dfa86967816315fe19b482511dab1ce517f28
9. DeltaCore `DeltaCore/Emulator Core/EmulatorCore.swift` L29–32, L305–345 and `DeltaCore/Cores/EmulatorBridging.swift` L52–55, at the same commit.
10. GBADeltaCore, commit `869c34aeca9dd2b3fbd329092bb2743b0ae80c98` (2026-07-21): `GBADeltaCore/GBA.swift` L52–56, `GBADeltaCore/Bridge/GBAEmulatorBridge.mm` L346–394, `Types/GBATypes.m` — https://github.com/rileytestut/GBADeltaCore/tree/869c34aeca9dd2b3fbd329092bb2743b0ae80c98
11. VBA-M at the submodule commit `453fa0decf179360926fb417725a794aa496d6e3` (2018-11-04): `src/gba/Cheats.cpp` (delete/restore L1326–1386, seeds and TEA L1500–1550, `cheatsAddGSACode` L1552–1800, `cheatsAddCBACode` L2422+), `src/gba/GBA.cpp` L3819–3822, `src/gba/GBAcpu.h` L225–235 — https://github.com/visualboyadvance-m/visualboyadvance-m/tree/453fa0decf179360926fb417725a794aa496d6e3 (license per GitHub API: GPL-2.0)
12. melonDSDeltaCore, commit `eb9b07ec17307cfd4986cb607f8ee5f6492d73cb` (2026-05-29): `MelonDSDeltaCore/MelonDS.swift` L83–86, `MelonDSDeltaCore/Bridge/MelonDSEmulatorBridge.mm` (`ParseTextCode` L54–100, cheats L605–653) — https://github.com/rileytestut/melonDSDeltaCore/tree/eb9b07ec17307cfd4986cb607f8ee5f6492d73cb
13. Delta's melonDS fork at the submodule commit `a53f9bab7b5c06efcbc6ecdf30ac0367555995aa` (2025-01-27, melonDS 0.9.5 base): `src/AREngine.cpp` (`RunCheat` L91–434, `RunCheats` L437), `src/ARM.cpp` L524–530, `src/ARCodeFile.h`, `src/ARCodeFile.cpp` (`.mch` loader) — https://github.com/rileytestut/melonDS/tree/a53f9bab7b5c06efcbc6ecdf30ac0367555995aa
14. GBCDeltaCore, commit `0871ccaad2bbd7cbd2de0ce06f9e26dc3d1bfdde` (2026-05-29): `GBCDeltaCore/GBC.swift` L44–48, `GBCDeltaCore/Bridge/GBCEmulatorBridge.mm` L311–362, `GBCDeltaCore/Model/GBCCheat.m` — https://github.com/rileytestut/GBCDeltaCore/tree/0871ccaad2bbd7cbd2de0ce06f9e26dc3d1bfdde
15. Gambatte at the submodule commit `f8a810b103c4549f66035dd2be4279c8f0d95e77` (2016-05-03): `libgambatte/src/interrupter.cpp` L30–75, `libgambatte/src/mem/cartridge.cpp` L720–760 — https://github.com/sinamas/gambatte/tree/f8a810b103c4549f66035dd2be4279c8f0d95e77 (COPYING: GPL v2)
16. melonDS upstream `master` (as of 2026-10-07): `src/AREngine.cpp` (D4 extensions), `src/ARM.cpp` L545–546, `src/ARCodeFile.h` (`std::vector<u32> Code`) — https://github.com/melonDS-emu/melonDS/blob/master/src/AREngine.cpp; commits https://github.com/melonDS-emu/melonDS/commit/91ab68090c0aa588aabaeaa5e2c62564fd661ccc (2025-10-13, "AR: add fun D4 opcodes") and c6bf5d5 (#2108, 2024-08-01). License per GitHub API: GPL-3.0; file headers say "version 3 … or (at your option) any later version".
17. CheatBase at the submodule commit `df52576e6fd1185747f41b022d9664ad0593608a` (2022-09-07): `README.md`, `CONTRIBUTING.md`, `cheatbase.sqlite`, `update_cheat_data.py` — https://github.com/rileytestut/CheatBase/tree/df52576e6fd1185747f41b022d9664ad0593608a. Repo metadata (GitHub API): description "Based on OpenVGDB…", `license: null`. Opcode and region counts in this report come from my own queries of `cheatbase.sqlite` (scripts kept outside the repo).
18. Delta's bundled data and lookup at commit `c1d3d06…`: `Resources/cheatbase.zip` (SQLite dated 2023-02-03, 107,940 rows, all device 10), `Resources/openvgdb.sqlite`, `Delta/Database/Cheats/CheatBase.swift` (L95–145), `CheatBaseView.swift`, `CheatMetadata.swift`, `Delta/Database/OpenVGDB/GamesDatabase.swift` (L152–188), `Delta/Database/DatabaseManager.swift` (L309–327, SHA-1 at L400).
19. OpenVGDB — https://github.com/OpenVGDB/OpenVGDB (GitHub API `license: null`; no LICENSE file at `master`).
20. mGBA `master` (as of 2026-10-07; MPL-2.0): `src/gba/cheats.c` (`GBACheatAddAutodetect`, directives), `src/gba/cheats/gameshark.c`, `src/gba/cheats/parv3.c` (seeds, T1/T2 tables, probability heuristics), `src/gb/cheats.c` — https://github.com/mgba-emu/mgba/tree/master/src/gba/cheats
21. Delta GitHub issues — #347 https://github.com/rileytestut/Delta/issues/347 (2024-04-23, unencrypted GBA GSA/AR); #39 https://github.com/rileytestut/Delta/issues/39 (2019); #160 https://github.com/rileytestut/Delta/issues/160 (usrcheat.dat); #301 https://github.com/rileytestut/Delta/issues/301; #367 https://github.com/rileytestut/Delta/issues/367; #416 https://github.com/rileytestut/Delta/issues/416; #214 https://github.com/rileytestut/Delta/issues/214. License of Delta per GitHub API and `COPYING`: AGPL-3.0.
22. GBATEK (extracted from no$gba v3.06, © 2025 Martin Korth): "DS Cart Cheat Action Replay DS" https://problemkaputt.de/gbatek-ds-cart-cheat-action-replay-ds.htm; "GBA Cheat Codes – General Info" https://problemkaputt.de/gbatek-gba-cheat-codes-general-info.htm; "Codebreaker/Xploder" https://problemkaputt.de/gbatek-gba-cheat-codes-codebreaker-xploder.htm; "Gameshark/Action Replay V1/V2" https://problemkaputt.de/gbatek-gba-cheat-codes-gameshark-action-replay-v1-v2.htm; "Pro Action Replay V3" https://problemkaputt.de/gbatek-gba-cheat-codes-pro-action-replay-v3.htm; full text https://problemkaputt.de/gbatek.txt
23. EnHacklopedia, "Hacking Nintendo DS" (Action Replay DS code types, handler hacks; page revisions to 2009) — https://doc.kodewerx.org/hacking_nds.html
    - 23b. EnHacklopedia, "Hacking Gameboy" (GameShark `01`/`8b`/`9b`) — https://doc.kodewerx.org/hacking_gb.html
24. Pan Docs, "Game Genie/Shark Cheats" (CC0-1.0; source last changed 2025-07-03) — https://gbdev.io/pandocs/Shark_Cheats.html, source https://github.com/gbdev/pandocs/blob/master/src/Shark_Cheats.md
25. SameBoy `master` `Core/cheats.c` (Expat license except the iOS directory) — https://github.com/LIJI32/SameBoy/blob/master/Core/cheats.c
26. This repository (read 2026-10-07): `crates/emu-cheats/src/lib.rs`, `crates/emu-cheats/src/code.rs`, `crates/emu-app/src/cheats.rs`, `crates/emu-app/src/emulator.rs` (`apply_cheats`, `PlayerMemory`), `crates/emu-core/src/core.rs` (`peek`/`poke` contract), `crates/emu-nds/src/lib.rs` (`peek`/`poke`, `run_frame`), `crates/emu-arm/src/cp15.rs`, `docs/PRD.md` (§9 rule 4, Q1).
