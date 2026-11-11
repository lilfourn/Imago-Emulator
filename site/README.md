# The site

imagoemulator.com: one static page (`public/index.html`) and a Worker with a
few redirects (`src/worker.js`). It is not published yet; see
[Before publishing](#before-publishing).

The page is drawn in Imago Light, as the app and
[getimago.app](https://getimago.app) are: white paper, ink type, Geist, and
the four colour blocks of the app icon. What there is to press is a pill in
the app's accent. The hero's two rows of games are the icon's blocks made
into small handhelds, each showing a game's own screenshot. Copy follows
[decision 0016](../docs/decisions/0016-public-release.md): it names
systems, not commercial games, and shows only homebrew.

## What is real on it

- **The games** are in the app's store, from
  [Homebrew Hub](https://hh.gbdev.io). Each picture in `public/assets/games`
  is that game's own screenshot from its Homebrew Hub entry, saved under the
  entry's name. Only openly licensed games are shown, and the footer credits
  each maker with the licence its entry gives. A game added to the page is
  added to that list (`site.test.mjs` checks).
- **The app window** (`public/assets/app-game.jpg`) is a capture of the app
  running uCity Advance.
- **The accents and icons** in the supporters' section are the app's own:
  the colours are `Accent::colour` in `crates/emu-app/src/theme.rs` (checked
  by the test) and the icons are `assets/app-icon.png` and
  `assets/app-icons/`, scaled to 256 pixels with `sips -Z 256`. Choosing one
  on the page recolours the page's buttons or swaps its mark.
- **Geist** (`public/assets/geist-latin.woff2`) is the font getimago.app
  uses, under the SIL Open Font License 1.1.

## The Worker

Files in `public/` are served first. The Worker answers only:

| Path | What it does |
|---|---|
| `/download/mac`, `/download/windows`, `/download/linux` | Redirects to the address in `DOWNLOAD_MAC`, `DOWNLOAD_WINDOWS` or `DOWNLOAD_LINUX` |
| `/buy/supporter`, `/buy/patron`, `/buy/founder` | Redirects to the checkout in `BUY_SUPPORTER`, `BUY_PATRON` or `BUY_FOUNDER` |
| `/supporter` | Redirects to `/#supporters`. The app's Become a Supporter button opens this address (`crates/emu-app/src/supporter.rs`) |

The variables are in `wrangler.jsonc`. One that is empty, or is not an https
address, answers 404 with "Not available yet."

## Preview and check

```sh
python3 -m http.server 27811 --bind 127.0.0.1 --directory site/public
node --test site/site.test.mjs
wrangler deploy --dry-run --config site/wrangler.jsonc
```

The tests check that every link, picture and font on the page exists, that
the page loads nothing from another site, that every game shown is credited,
that the accents are the app's, and what the Worker answers.

## Before publishing

1. Fill the six variables in `wrangler.jsonc`: a public release for each
   system ([packaging](../packaging/README.md),
   `scripts/macos/release.sh`), and a seller's checkout for each key
   ([decision 0017](../docs/decisions/0017-supporter-keys.md)).
2. Settle the prices on the page ($10, $25 and $50 are the advice in
   decision 0017, not yet the owner's decision).
3. Replace `app-game.jpg` with a capture taken in the middle of a game. The
   current one is a title screen, as a covered window's game does not
   advance.
4. Point imagoemulator.com at Cloudflare, then
   `wrangler deploy --config site/wrangler.jsonc`.
