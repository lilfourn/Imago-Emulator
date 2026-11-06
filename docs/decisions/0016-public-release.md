# 0016 — The public release: closed source, Imago Emulator, Developer ID

- Status: accepted
- Date: 2026-11-06
- Deciders: Luke Fournier (license, name, menu label); Claude (the rest)
- Related: [PRD](../PRD.md) §9 and open question Q1

## Context

The owner bought imagoemulator.com and asked for what must be fixed
before the app goes out to the public. On 2026-11-06:

- The repository had no license, and the app shipped no notices for the
  open-source code built into it (502 crates, by `cargo-about`, plus code
  copied into our own crates).
- The app was "DS Emulator", signed ad hoc, built by hand, with a pitch
  ("play the Pokémon games of generations 1–5") that names a game company's
  franchise as its purpose.
- Its `LSMinimumSystemVersion` said macOS 11, but the binary links MetalFX,
  which came with macOS 13, as a required framework: on 11 or 12 it would
  not launch.

## Decision

1. **Closed source for now** (Q1). The repository stays private and
   [LICENSE](../../LICENSE) keeps all rights, with terms that let anyone
   use the app for free. It can be opened later; an open license could not
   be taken back.
2. **The app is Imago Emulator**, at imagoemulator.com, in its menus,
   window title, About panel, Guide and the headers it sends. Its bundle
   identifier (`com.lilfourn.ds-emulator`), executable and data folders
   keep their old names, so settings, saves, the library and the
   microphone permission carry over. User agents name the site, as
   Wikimedia's policy asks.
3. **Public copy names systems, not games.** The README's pitch, and the
   site and release notes after it, describe the systems played and show
   homebrew, with a not-affiliated-with-Nintendo notice. The File menu says
   Randomize Game…; the randomizer's window still names what it supports.
4. **`scripts/macos/release.sh` makes every public build**: the app from
   `Cargo.lock`, signed with Developer ID under the hardened runtime,
   notarized and stapled, in a disk image that is signed, notarized and
   stapled too. Arm64 only, macOS 13 or later; the script fails if the
   binary asks for a newer macOS.
5. **The hardened runtime gets three exceptions**
   (`scripts/macos/entitlements.plist`), each shown to be needed by playing
   RollTheDice-3DS in a build without it:
   - `cs.allow-jit`: without it, the 3DS helper exits before its first
     frame, as Azahar's JITs can't map executable memory;
   - `cs.disable-library-validation`: without it, the helper can't load
     MoltenVK ("mapping process and mapped file (non-platform) have
     different Team IDs"), nor Azahar's core, which is re-signed ad hoc
     after its fix;
   - `device.audio-input`: the DS microphone.
   With all three, the signed app played it at 60 frames a second.
6. **Notices ship in the app.** `cargo-about` writes the crates' licenses
   for the shipping target, without dev- and build-dependencies, from
   `scripts/macos/about.toml` and `about.hbs`. The template adds by hand
   what it can't see: SameBoy (palettes, OmniScale), Lua (compiled in by
   the build-dependency `lua-src`), libretro's headers, zaydlang's
   multiplier, mGBA's cheat seed tables and Solar Icons. They are the About
   panel's credits (`Credits.rtf`) and `Acknowledgements.html`. mGBA's
   tables are MPL-2.0, so their file's source, `seed_tables.rs`, ships in
   the app and on the disk image.

## Consequences

- A release needs the Developer ID certificate and the `imago-emulator`
  notarytool profile in the login Keychain (see the script's header), and a
  higher `BUILD` for each build put out under one version.
- Code copied into our crates from elsewhere needs its notice added to
  `about.hbs` by hand; crates are picked up from `Cargo.lock`.
- Disabling library validation lets the app load libraries other teams
  signed. It only loads the core and MoltenVK it downloaded, checked
  against pinned digests, from its own data folder.
- Intel Macs and macOS 11–12 are not supported. Supporting them would take
  an x86-64 build with Azahar's x86-64 core, and weak-linking MetalFX.
- Signed builds keep one code signature, so Guide's key could now move to
  the Keychain (`emu-guide/src/key.rs` explains why it is a file).
- PRD §9's rules stand: no paid builds tied to games, nothing paywalled.
