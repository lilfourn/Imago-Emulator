# 0018 — Open source under the Mozilla Public License 2.0

- Status: accepted
- Date: 2026-11-19
- Deciders: Luke Fournier (to open the source, and the license); Claude
  (the rest)
- Related: [decision 0016](0016-public-release.md) item 1, which this
  replaces; [PRD](../PRD.md) §9 and Q1

## Context

On 2026-11-19 the owner made the repository public, as
`github.com/lilfourn/Imago-Emulator`, and asked for the site to link to it
and say the app is open source. [LICENSE](../../LICENSE) still kept all
rights and said the source was not published, so the code could be read
but not reused, and calling it open source would have been untrue.

## Decision

1. **The code is under the Mozilla Public License 2.0.** LICENSE is the
   license's text, unchanged. The owner chose it over:
   - GPL-3.0-or-later (melonDS's license): forks would have to stay open,
     and melonDS's code could be ported in, but once GPL code is in, the
     license can never be loosened again;
   - MIT or Apache-2.0: anyone could ship a closed fork of the app.

   MPL-2.0 is mGBA's license. Changes to this project's files stay open,
   while the files may sit in larger works under other licenses, and
   every dependency the app builds with already allows it.
2. **The name and icon are not licensed.** MPL-2.0 §2.3 grants no
   trademark rights. A changed build that is given out needs its own name
   and icon (README, Legal).
3. **GPL code is still only read, never copied** (PRD §9 rule 4).
   `assets/randomizer/Launch3ds.java` stays a separate GPL-3.0-or-later
   program, as its header says.
4. **The site says it**: a GitHub link in the header and footer, "free and
   open source" where it says the app is free, and the terms describe the
   MPL-2.0 instead of a personal license.

## Consequences

- The whole history is public. Before writing this, `gitleaks` scanned all
  936 commits: its 24 hits were the placeholder Paddle tokens in
  `site/site.test.mjs` (`live_0123456789…`, `…_of-the-tests`). The
  supporter signing key was never committed, and no commit holds a file
  that `scripts/check-no-nintendo-files.sh` refuses (games, BIOS, saves,
  keys).
- Anyone may build the app with the supporter check taken out. That is
  accepted: a key only changes the accent colour and the Dock icon
  ([decision 0017](0017-supporter-keys.md)), and anything that plays a game
  is already free.
- Outside contributions can now be taken, under the MPL-2.0 (MPL §2.1 has
  each contributor license their changes on the same terms). The same
  rules apply to them as to the project's own code: no GPL code copied in,
  and nothing from PRD §9's list.
- The packages ship LICENSE as `License.txt`, as before, so the Windows
  installer's license page and the Debian package's `copyright` now show
  the MPL-2.0.
