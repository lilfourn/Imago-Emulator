# 0019 — Updates: a feed on imagoemulator.com, installed in place on the Mac

- Status: accepted
- Date: 2026-11-21
- Deciders: Luke Fournier (that the app updates, as T3 Code does); Claude
  (the rest)
- Related: [decision 0016](0016-public-release.md), `crates/emu-app/src/update.rs`

## Context

The owner asked for the app to find out when a new version is released and
let its users update to it, as T3 Code does. Until now the app never asked
for a newer version, and the privacy page said so.

How others do it:

- **T3 Code** (electron-updater): a pill in the sidebar's footer goes from
  "Update available" to "Downloading (N%)" to "Restart to update"; the
  offer can be dismissed until the next launch; Settings can check by hand;
  nothing installs on quit; `T3CODE_DISABLE_AUTO_UPDATE=1` turns it off.
- **Sparkle**, the Mac's usual updater: a feed (the appcast) on the
  developer's site; the download is checked before it is used; the new app
  must be signed by the same developer as the running one; it replaces the
  app after it quits, and is opened again.

## Decision

1. **The feed is `https://imagoemulator.com/update.json`**: the version,
   the download page, and each package's address, SHA-256 and size.
   `site/publish.sh` writes it with `site/update-feed.mjs` from what the
   page already prints, so publishing a version publishes its feed, and the
   site's tests check it against the page. Not GitHub's releases: the site
   is where the downloads are, and a draft release there gets its Mac
   image by hand.
2. **The app asks 8 seconds after launch and every 6 hours**, and when the
   player chooses Check for Updates… (the app menu on macOS, Help
   elsewhere), which answers in a dialog. Settings > Updates turns the
   automatic checks off (`[updates] check = false`), as does
   `DS_EMULATOR_DISABLE_AUTO_UPDATE`. Debug builds and test homes
   (`DS_EMULATOR_HOME`) only ask when `DS_EMULATOR_UPDATE_FEED` names a
   feed, which may then be a stand-in server on `http://127.0.0.1`.
3. **T3 Code's pill**, in the library's header: Update Available
   (dismissable until the next launch), Downloading N%, Restart to Update.
   Nothing is downloaded until it is pressed, and nothing installs on quit.
4. **On the Mac the app installs the update itself**, as Sparkle does: it
   downloads the disk image, checks its size and SHA-256 against the feed,
   copies the app off it, and checks that app with `codesign` (the running
   app's identifier and Developer ID team), `spctl` (notarized) and its
   `CFBundleShortVersionString` (the feed's version, so an older release
   can't be put back). Restart to Update moves the running app aside,
   moves the new one into its place, quits, and a shell started for it
   opens the new app once this process has ended and removes the old one.
5. **It doesn't, and the pill opens the download page, when** the app
   isn't a Developer ID build (ad hoc builds, such as `install-local.sh`'s),
   macOS runs it translocated, or its folder can't be written to; and on
   Windows and Linux, whose packages aren't signed yet, and where a `.deb`
   needs the system's installer.

## Consequences

- A release is published with `site/publish.sh`, which now also writes the
  feed; a version published without it isn't offered.
- The privacy page says what the check sends: the IP address, and the
  app's name and version in its user agent.
- Installing on Windows (the NSIS installer's `/S`) and the AppImage can
  follow once their packages are signed; the feed already lists their
  packages, under the names of their `DOWNLOAD_` variables.
- An app installed for all users in a folder this user can't write to is
  sent to the download page; the app never asks for an administrator's
  password.
