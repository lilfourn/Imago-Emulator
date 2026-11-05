# 0014 — Guide asks OpenRouter through `curl`, and draws as a cached view

- Status: accepted
- Date: 2026-11-04
- Deciders: Claude, for Luke Fournier's review
- Related: [PRD-game-chat.md](../PRD-game-chat.md),
  [research](../research/game-chat.md)

## Context

The owner asked for a chat beside the running game, through OpenRouter,
with Imago's look in the game's colours, and chose GPT-6 Luna and
"Connect OpenRouter" as in Imago. The app has no HTTP crate: box art and
the store already fetch through the `curl` macOS ships. The game view
renders at the game's frame rate, up to 240 times a second at 3×
fast-forward.

Phase 0 asked OpenRouter real questions with the owner's key on
2026-11-04 (research §6):

- GPT-6 Luna streams answers in about 0.8 s without search and 2.5–4.5 s
  with it. Exa's search reached Bulbapedia, which `curl` cannot fetch.
- Luna's reasoning comes back encrypted only: no readable summaries, so
  there is no thinking text to show.
- Citations come as `delta.annotations` in the chunk before the text;
  `reasoning.server_tool_call` entries did not stream.
- With `provider.sort = "latency"`, OpenRouter chose Amazon Bedrock, which
  goes on billing after a stop.

## Decision

1. **One `curl` child per answer.** Its standard input carries a config
   file with the `Authorization` header and the request body
   (`data-binary`), so neither the key nor the question is in any
   process's arguments, and no file holds the key but the 0600 one
   sign-in wrote. `--speed-limit 1 --speed-time 60` fails a stream that
   sends nothing, keep-alives included, for a minute. Stop kills the
   child. One retry before anything streamed, on a dropped connection, 502
   or 503.
2. **GPT-6 Luna pinned, with GPT-5.6 Luna as OpenRouter's fallback**, and
   `provider.order = ["openai", "azure"]`, which stop billing on
   disconnect.
3. **Look-ups are OpenRouter's server tools** (`openrouter:web_search`
   and `openrouter:web_fetch`, engine Exa) limited to the game's sources
   from `sources.toml`. The model cites a fact with a Markdown link named
   after its site; the panel draws such links as source chips, and the
   annotations fill the Sources sheet.
4. **The panel is its own entity, drawn as `AnyView::cached`.** Game
   frames reuse its last layout and paint; streaming notifies only it, at
   most every 120 ms plus the fade-in's frames. Network, parsing of the
   stream, PNG encoding and file writes run on worker threads.
5. **The thinking line shows what is known:** "Thinking", or "Searching
   Bulbapedia, Serebii" while OpenRouter works with search on, then
   "Thought for Ns", which opens onto the searches and pages read, and
   readable thinking if a model ever sends it.
6. **Screenshots come from the frames the window last took**, which the
   emulation thread copied between frames, so no new command is needed.
   They carry nothing drawn over the screens.

## Consequences

- An HTTP crate is not needed. If `curl`'s streaming ever stalls behind a
  proxy, `ureq` with rustls is the fallback.
- The model picker, the app's own wiki and PokeAPI tools, the gamepad
  chord and the side setting are phase 3 and not built.
- A key on another provider than OpenAI or Azure only answers when both
  fail; that is OpenRouter's fallback, not the app's.
