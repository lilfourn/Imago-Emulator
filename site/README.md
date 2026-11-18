# The site

imagoemulator.com: one static page (`public/index.html`) and a Worker with a
few redirects (`src/worker.js`). It is published, with the Mac's disk image
beside it; see [Publishing](#publishing) and [Still to come](#still-to-come).

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
- **The app's windows** (`public/assets/app-library.webp`, `app-game.webp`
  and `app-menu.webp`) are captures of the released build, taken as
  `scripts/macos/check-layout.sh` takes its own: behind the front window,
  in a home of its own. The library is the games the page credits, each
  with its Homebrew Hub screenshot as its picture, as the store leaves one
  it adds. The game is uCity Advance, played into a city by a Lua macro
  (`ds-emulator --script`), since keys posted to a covered window don't
  reach the game.
- **The link preview** (`public/assets/share.png`, 1200 by 630) is the
  headline over five of the shelf's games.
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
| `/download/mac`, `/download/windows`, `/download/windows-zip`, `/download/linux`, `/download/linux-deb`, `/download/linux-arm64`, `/download/linux-arm64-deb` | Redirects to the address in the variable of that name: `DOWNLOAD_MAC`, `DOWNLOAD_WINDOWS_ZIP` and so on |
| `/buy/supporter`, `/buy/patron`, `/buy/founder` | Redirects to the checkout in `BUY_SUPPORTER`, `BUY_PATRON` or `BUY_FOUNDER` |
| `/supporter` | Redirects to `/#supporters`. The app's Become a Supporter button opens this address (`crates/emu-app/src/supporter.rs`) |

The variables are in `wrangler.jsonc`. One that is empty, or is not an https
address, answers 404 with "Not available yet.", and the page links only to
routes whose variable is filled (the tests check).

## Preview and check

```sh
python3 -m http.server 27811 --bind 127.0.0.1 --directory site/public
node --test site/site.test.mjs
wrangler deploy --dry-run --config site/wrangler.jsonc
```

The tests check that every link, picture and font on the page exists, that
the page loads nothing from another site, that every game shown is credited,
that the accents are the app's, that the download is this version's disk
image, and what the Worker answers.

## Publishing

```sh
scripts/macos/release.sh     # the notarized disk image, in target/dist
site/publish.sh PACKAGES     # the page, the Worker and the downloads
```

`publish.sh PACKAGES` copies `Imago-Emulator-VERSION.dmg`, and the Windows
and Linux packages found in the folder `PACKAGES` (the release workflow's
artifacts, from `gh run download RUN -D PACKAGES`), into `public/downloads/`
(ignored by git) and deploys. It refuses a disk image Gatekeeper does not
accept as notarized, a file whose SHA-256 is not the one the page prints,
and a `DOWNLOAD_` variable naming a file that is not there. For a new
version, change the version, the sizes and the SHA-256s in the page's
download section and the `DOWNLOAD_` variables in `wrangler.jsonc`.

The disk image is a static file of the Worker's, which may be 25 MiB at
most (it is 9.3 MB). imagoemulator.com's zone is in the Cloudflare account
named by `account_id` in `wrangler.jsonc`.

## Still to come

1. Linux. The page says "Not out yet". Its packages are built by
   `.github/workflows/release-linux-windows.yml`
   ([packaging](../packaging/README.md)), and on Ubuntu 22.04 their window
   stayed black in the workflow's start check. Once they are released,
   fill the four `DOWNLOAD_LINUX` variables and link the row. Windows is
   out, unsigned: its row says what Windows asks.
2. Supporter keys. The page shows the looks and says keys are not on sale
   yet; the prices are not settled and there is no seller
   ([decision 0017](../docs/decisions/0017-supporter-keys.md)). With a
   seller, fill the three `BUY_` variables and add the prices and buttons.
