# 0017 — Supporter keys: one free build, and thanks that are only looks

- Status: accepted
- Date: 2026-11-11
- Deciders: Luke Fournier (that the app may earn money this way); Claude
  (the rest)
- Related: [decision 0016](0016-public-release.md), [PRD](../PRD.md) §9

## Context

The owner asked how the app could bring in some income from its download
page without leaving a bad taste, and ruled out ads on 2026-11-06.

What other emulators do, looked up on 2026-11-11:

- melonDS takes donations (Patreon and PayPal) and gives nothing for
  them; its page showed about 45 paying members and $210 a month. That
  pays for a server.
- PPSSPP sells Gold for $4.99: the same emulator with a gold icon. One
  estimate put it near $10,000 a month in 2023, mostly on Android.
- Delta is free and its Patreon gives early builds; it pays two people.
- Yuzu sold early builds through Patreon, and settled with Nintendo for
  $2.4 million in March 2024. The complaint used the Patreon as evidence.
  No court ruled, so the line is untested.

So giving nothing earns little, and selling how soon or how well a game
plays is what drew a lawsuit. Decision 0016 kept PRD §9's rule that
nothing is paywalled; this decision says what that rule protects.

## Decision

1. **One build, and everything that plays a game is free in it.** Systems,
   speed, saves, states, the store, the randomizer, Guide, upscaling and
   every fix reach everyone at once. No early builds, no paid build.
2. **Supporters get looks, as thanks**: an accent colour (five) and, on
   macOS, a Dock icon (five). Nothing else is unlocked by a key, and
   nothing may be added to that list that a game needs, that any game
   company's games are named by, or that people already have. A dark
   theme, if the app gets one, is for everyone.
3. **The app asks once, quietly.** Settings has a Supporter section, and
   one menu item opens it (`Support Imago Emulator…`). No window at
   launch, no reminders, no marks on the free app, no countdowns.
4. **A key is checked on the computer, never by a server**
   (`emu-supporter`). It is an Ed25519 signature over the key's number,
   its date and the supporter's name, checked against public keys built
   into the app. The app makes no request about it, holds no account, and
   a key works offline for as long as the app does. Whoever sells keys
   only has to sign them, so the seller can be changed without changing
   the app.
5. **The looks are settings; the key says whether they show**
   (`supporter::look`). `settings.toml` gains a `[look]` section only once
   a supporter chooses one, so everyone else's file is as it was.
6. **Keys are taken back by number** (`REVOKED`), in the next build. That
   is slow on purpose: it is for a refund or a key posted in public, not
   for policing.
7. **The price is the owner's**; the advice is one payment, of the
   buyer's choosing from $10, through a merchant of record that handles
   tax. No subscription for looks.

## Consequences

- PRD §9 rule 5 now reads: no paid builds, and nothing a game needs
  behind payment. "Nothing paywalled" in decision 0016 meant this.
- The signing key is a file on the owner's Mac,
  `~/.config/imago-emulator/supporter-signing.key`, made by
  `supporter-key keygen`. It is not in the repository. If it is lost, no
  more keys can be made for the public key the app trusts; a new one is
  then added to `PUBLIC_KEYS` beside the old, and old keys keep working.
  If it leaks, anyone can make keys, and the fix is the same with the old
  public key removed, which ends the keys sold so far.
- A key can be shared, and the code that checks it can be patched out.
  Both are accepted: the looks are thanks, not a lock.
- The Dock icon shows while the app runs. The icon in the Finder and the
  Dock of an app that is not running is the bundle's, which cannot change
  without breaking its signature. Linux and Windows take a program's icon
  from its file or package, so the choice is not offered there.
- Selling is not built yet: the page at `imagoemulator.com/supporter`,
  the seller, and whatever signs a key for each sale. `supporter-key issue`
  makes keys by hand or in batches meanwhile.
- Public copy for supporting follows decision 0016: it names the app and
  its systems, never a game.
