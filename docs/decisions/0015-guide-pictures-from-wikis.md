# 0015 — Guide's pictures come from the game's wiki, by page title

- Status: accepted
- Date: 2026-11-05
- Deciders: Claude, for Luke Fournier's review
- Related: [decision 0014](0014-guide-through-openrouter.md),
  [PRD-game-chat.md](../PRD-game-chat.md) §6.7

## Context

The owner asked for Guide's answers to show pictures: the Pokémon,
characters and map locations an answer is about. Until now, Markdown
images were never drawn (PRD §6.7, after Imago).

Asking the model for image addresses would not work. It seldom has one,
it invents plausible ones, and Bulbapedia, the series' best source, turns
`curl` away at its bot check (its pages and its image host,
`archives.bulbagarden.net`, both answer 403 "Just a moment…").

Checked by hand on 2026-11-05: MediaWiki's PageImages API answers `curl`
on Fandom's Pokémon Wiki and on every series wiki in `sources.toml`
except Metroid Wiki, which redirects (and is followed). It gives each
page's main picture, scaled to a width asked for: a Pokémon's artwork, a
character's, and for a route or town its map from the game (Pokémon Wiki's
Route 210 is `Route_210_Pt.png`).

## Decision

1. **The model asks for a page, not an image.** It writes
   `![Route 210](wiki:Route_210)` on a line of its own, with the page's
   title on the game's wiki. The system prompt (version 2) asks for this
   when an answer is about a Pokémon, a character or a place, at most four
   an answer.
2. **The game's picture wiki** is its series' `pictures` entry in
   `sources.toml` (the Pokémon Wiki on Fandom for Pokémon games, fan games
   and hacks), else the series' first MediaWiki site on a host of its own.
   A game in no series gets no pictures and no picture prompt.
3. **`emu_guide::pictures` finds and fetches them**: the exact title with
   redirects first, then a search of the wiki, the first result with a
   picture winning; then the picture, at most 800 px wide and 12 MB, over
   HTTPS only (redirects too). Files are kept in
   `~/Library/Caches/ds-emulator/Guide pictures`, the newest 400.
4. **The panel draws a paragraph of nothing but images as pictures**
   (`Block::Pictures`): one fills the column up to 240 pt tall; several
   share a row on one baseline, captioned. Artwork with transparent corners
   is drawn bare, maps and screens with rounded corners and a hairline.
   Sprites (320 px or less) are scaled up by whole pixels first, so pixel
   art stays crisp. A picture fades in over 220 ms; a click shows it large
   with a link to its page. A picture the wiki lacks leaves no gap. An
   image amid text still shows its alt text.
5. **Each picture is fetched on a thread of its own** the first time its
   row is drawn, and its row measured again when it comes. A chat or game
   change forgets them, and late arrivals are dropped.

## Consequences

- The app now contacts the game's wiki with page titles when an answer
  shows pictures, whatever Settings > Guide > Look Things Up says; it
  sends nothing else.
- Pictures follow the wiki's choice of main picture, which is sometimes a
  later game's artwork (Fantina's is from Brilliant Diamond).
- The wikis' pictures are under their licences (Fandom's text is CC BY-SA);
  the enlarged view links back to each picture's page.
- Bulbapedia stays a search-only source. If it ever answers `curl`, it
  could become the Pokémon picture wiki by changing `sources.toml`.
