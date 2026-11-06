# ds-emulator — Guide: a game chat beside the screens

| | |
|---|---|
| Status | Phases 0–2 built (2026-11-04): the panel, streaming answers, screenshots, the source map and look-ups. Phase 3 partly (key usage, Thinking and Look Things Up settings). See 9. |
| Date | 2026-11-04 |
| Owner | Luke Fournier |
| Research | [research/game-chat.md](research/game-chat.md) |
| Related | [design-system.md](design-system.md) · [decision 0008](decisions/0008-rpg-maker-games.md) (fan games) · [decision 0012](decisions/0012-randomizer.md) (randomized copies) · Imago's transcript (`~/Desktop/Imago/crates/ui/src/transcript`) |

## 1. Summary

**Guide** is a chat that lives in the empty margins beside a running game.
The player asks things like "where do I get Surf?", "what does this item
do?" or "what do I do here?", and gets a short, well-formatted answer while
the game keeps running beside it.

- **Models through OpenRouter.** The player connects their own OpenRouter
  account once, with "Connect OpenRouter" and a browser sign-in, as in
  Imago. Requests go straight from the Mac to OpenRouter. No server of
  ours is involved.
- **Knows the game.** Every question carries:
  - the exact game: No-Intro name, platform, region and revision;
  - whether it is a ROM hack, randomized copy or fan game;
  - any screenshots the player took with the **Screenshot** button or
    ⌥⌘S: both screens exactly as they were, sent only when attached
    (5.4).
- **Knows where to look.** A curated source map tells the model which fan
  sites and wikis players trust for this game, such as Bulbapedia,
  Serebii, StrategyWiki or Zelda Wiki. The model searches and reads
  those sites through OpenRouter's web search and fetch, limited to the
  map's domains. Each answer shows the pages it came from.
- **Looks like ChatGPT, in the game's colours.** The owner asked for
  ChatGPT's cleanness (2026-11-04), taken from its web and iOS apps on
  Mobbin:
  - answers sit on the panel with no bubble, and questions sit in a grey
    pill;
  - thinking is one grey "Thought for 6s ›" line;
  - sources are small chips and a Sources sheet;
  - a quiet action row sits under each answer;
  - the composer is rounded, with tool chips and a white send / stop
    circle.

  Imago supplies what makes streaming feel good: mended partial
  Markdown, the fade-in veil, block-by-block parsing and the shimmer.
  Both are ported to stock GPUI 0.2.2. The mockup is
  `/tmp/guide-mock/guide-mock.html`; it is described in 6.7.

  The panel has no fixed theme. Its glass, accent and spinner take
  their colours from the game on screen, the way the ambient backdrop
  does, so it looks like part of whatever game is being played (6.7).
- **One model: GPT-6 Luna** (`openai/gpt-6-luna`), chosen by the owner.

## 2. Background

What exists, and what was found, is in the
[research](research/game-chat.md). In short:

- **The space is there.** Stacked DS and 3DS screens leave two dark
  margins in a wide window: about 230 pt each in the owner's screenshot,
  and 400–490 pt in full screen on the built-in display. The ambient
  backdrop fills them with the game's blurred colours.
- **Drawing it in the game view would cost frames.** The game view
  renders at the game's frame rate. The chat must be its own cached view
  (`AnyView::cached`), so streaming text never lays out the game, and
  game frames never lay out the chat.
- **Fetching uses `curl`.** The app has no HTTP crate and fetches through
  `curl`. `curl -N` streams OpenRouter's server-sent events.
- **Imago's look is mostly in the renderer, not the prompt.** Imago
  never calls OpenRouter's chat API itself; opencode does. Only its
  sign-in and key storage carry over. The streaming client is new work.
- **Some fan sites can't be fetched.** Bulbapedia, GameFAQs,
  PokéCommunity and Relic Castle sit behind Cloudflare bot checks.
  StrategyWiki, the independent series wikis, Fandom, Serebii, PokémonDB
  and PokeAPI answer `curl` directly.

## 3. Goals and non-goals

| ID | Goal |
|---|---|
| GG-1 | **Right answers about this game.** Answers name the exact game and version, ground facts in trusted sources, and say so when nothing was found instead of guessing. |
| GG-2 | **Free in play.** Opening, typing in and streaming into the chat never drops a game frame or adds input latency, including at 3× fast-forward on the 120 Hz and 240 Hz displays. |
| GG-3 | **Never in the way.** The chat uses the margins the screens leave. Closed, it leaves the window exactly as today. It never covers the screens, and never changes their arrangement on its own (stacked stays stacked). |
| GG-4 | **Imago quality.** It looks and moves like Imago's chat: streaming that never shows broken Markdown, thinking shown and folded away, a calm working indicator. |
| GG-7 | **Part of the game.** The panel takes its colours from the game being played, so it complements Omega Ruby's greens, a Game Boy's greys or a fan game's palette alike, and always stays readable. |
| GG-5 | **The player's own account and data.** The player brings their own OpenRouter account. The key never leaves the Mac except to OpenRouter. A screenshot is sent only when the player attaches one, and each shows on the message it went with. Nothing is sent before the player connects. |
| GG-6 | **Keyboard first.** One shortcut opens the chat and puts the caret in it. Escape gives the keys back to the game. Held game keys never leak into the chat, and chat keys never reach the game. |

Non-goals:

- Our own backend, accounts or billing. OpenRouter is the only service.
- Voice input or spoken answers.
- The model pressing buttons or playing the game.
- Reading game memory (party, location) in phase 1. See phase 4.
- Getting past sites' bot checks, or scraping against a site's
  `robots.txt`.
- Helping find ROMs of commercial games. The system prompt declines,
  matching the store's stance.
- Windows and Linux.

## 4. Scenarios

1. **Omega Ruby, full screen on the MacBook.** The player presses ⌘J and
   types "where can I catch a Ralts?".
   - The answer streams in within a couple of seconds: the route, the
     encounter rate in Omega Ruby, and a one-line tip.
   - A Sources row underneath links Serebii's ORAS page and Bulbapedia.
   - Escape returns the keys to the game, which never stopped.
2. **Platinum, stuck in a cave.** "What do I do here?" with the screen
   on. The model sees both screens, recognises Mt. Coronet from the map,
   and gives a hint first: "Head up the stairs on the left; you'll need
   Rock Climb." It offers the full route if the player asks.
3. **A randomized Emerald.** "Where is Abra?" The answer says this copy is
   randomized, so wild Pokémon differ from the wikis. It suggests the
   in-game Pokédex area view instead of quoting Route 116.
4. **An RPG Maker fan game.** "How do I get past the guard in Chapter 2?"
   The source map routes to the game's own wiki and to PokéCommunity
   through search. With nothing found, the answer says so and suggests
   what to try, without inventing a solution.
5. **Not connected yet.** The panel shows one card:
   - what Guide does;
   - what gets sent where;
   - "Connect OpenRouter".

   The browser opens, the player approves, and the card turns into the
   composer.
6. **A narrow window.** The margins are too narrow for the panel's
   minimum width. Opening Guide shrinks the screens just enough to make
   room, still stacked. Closing it restores them.

## 5. Experience

### 5.1 Where it lives

```
┌───────────────────────────────────────────────────────────────┐
│ ● ● ●                                                         │
│  (ambient glow)  ┌──────────────────┐  ┌──────────────────┐   │
│                  │                  │  │ Omega Ruby    ＋ ✕│   │
│                  │    top screen    │  │──────────────────│   │
│                  │                  │  │   ┌────────────┐ │   │
│                  ├──────────────────┤  │   │ where's…   │ │   │
│                  │                  │  │   └────────────┘ │   │
│                  │  bottom screen   │  │ ▸ Thought process│   │
│                  │                  │  │ Route 102, in the│   │
│                  │                  │  │ tall grass …     │   │
│  notice          │                  │  │ Sources: Serebii │   │
│                  └──────────────────┘  │ ╭──────────────╮ │   │
│                                        │ │ Ask…    ▢ ⬆  │ │   │
│                                        └─╰──────────────╯─┘   │
└───────────────────────────────────────────────────────────────┘
```

The sketch shows where things go. The mockup (§1) shows how they look,
flush to the window's side as below.

- **One panel, in one margin.** The right one by default, with View >
  Guide Side > Left or Right.
- **Size.** Flush with the window's side, top and bottom, as ChatGPT's
  side panel is, with a hairline on its inner edge.
  - Its header is the top bar's 40 pt: the game's title and the buttons
    sit on the traffic lights' midline.
  - Width: the margin less one inset (16 pt) from the screens, clamped
    to 300–440 pt.
- **Not enough room.** If the margin is below 300 + 2 × 16 pt, opening
  Guide lays the screens out in the window less the panel's column. They
  shrink, stay stacked and stay centred in what's left. Closing restores
  them.
  - This is a deliberate action by the player, so it doesn't break the
    "no layout heuristics" rule (memory: screen and key preferences).
- **Landscape systems.** GB, GBC and GBA games in a landscape window have
  no side margins, so Guide always takes a column there, as above.
- **Notices and the speed readout move to the other margin** while the
  panel is open. Nothing is drawn over the screens.
- **Closed.** No panel, and no leftover chrome in the margins. Moving
  the mouse into the right margin fades in "Ask about Omega Ruby ⌘J" in
  quiet grey words in the window's top right corner, on the traffic
  lights' midline; under the pointer they light up on a faint pill. It
  goes after 2 s without movement. This is the visible way in (memory:
  every action needs one).

### 5.2 Ways in and out

| How | What |
|---|---|
| ⌘J, View > Guide | Open the panel and put the caret in the composer; if it is open with the caret, close it. ⌘J is free today (the shortcut list in `app.rs`). |
| Escape menu tile "Guide" | Same as ⌘J. A tile beside the others in `game_menu.rs`. |
| Margin pill | Click to open. |
| Escape while the composer has the caret | The caret leaves; keys go back to the game. The panel stays open. A second Escape opens the game menu as today. |
| Click on the screens | The caret leaves, as Escape does. |
| ✕ in the panel header | Close. |
| ⌥⌘S, Emulation > Screenshot for Guide | Take a screenshot into Guide's composer, as the Screenshot button does, at any time while playing. The panel may be closed: the notice "Screenshot added to Guide (2)" says so, and the shots wait in the composer for the next question. ⌥⌘S is free today. |
| Gamepad | Select + R3 opens and closes it (a `chords.rs` shortcut), if R3 is free. Typing still needs the keyboard. |

**Keys.** `key_routes.rs` gains `Route::Guide`.

- **Taking the keyboard.** When the composer takes the caret, every held
  game key is released at once (the same path as focus loss), so a
  held W stops walking.
- **Keys go where they were pressed.** A key pressed while typing goes to
  the composer, including its repeats and its key-up. A key held from
  the game finishes in the game.
- **Typing doesn't pause.** The game keeps running while the player
  types (the setting "Pause while typing" is off by default; §5.6).

### 5.3 States of the panel

1. **Not connected.**
   - Copy: "Guide answers questions about the game you're playing,
     with sources from the wikis players trust." Under the button:
     "Your questions, and any screenshots you add, go to OpenRouter."
   - Button: **Connect OpenRouter**.
   - While connecting: "Finish connecting in your browser." with a
     spinner and Cancel.
   - On failure: the reason and "Try again".
2. **Empty chat.**
   - ChatGPT's centred question, "What are you stuck on?", with the
     composer under it.
   - Three starter chips, chosen by game kind:
     - Pokémon: "Where am I?", "What should I do next?", "What's good
       against this gym?".
     - Other games: "Where am I?", "What should I do next?", "How do I
       beat this?".

     A starter takes a screenshot itself, as it needs one.
3. **Asked, before the first token.** The question's pill, with its
   screenshots above it, then one shimmering grey line: "Thinking".
4. **Thinking.** The line reads "Thinking" while reasoning streams. It
   opens into the reasoning summaries in faint 13 px type.
5. **Looking it up.** While searches and fetches run, the line names
   them: "Searching Bulbapedia for Roxanne's team", "Reading Route 104 ·
   Serebii".
6. **Answering.** The Markdown answer streams under the line, which
   becomes "Thought for 9s ›". Clicked, it opens ChatGPT's activity list:
   - what was searched;
   - each page read, with its site's icon;
   - the reasoning summaries.

   Partial Markdown is mended and new text fades in.
7. **Done.** Under the answer, an action row in grey icons: copy, ask
   again, then **Sources** with the sites' icons stacked. (Good and bad
   ratings were dropped at the owner's request on 2026-11-04.)
   - Sources opens a sheet over the panel listing each page: the site,
     the page title and a line from it. A page opens in the browser.
   - Small grey chips after a sentence ("Serebii", "Bulbapedia +1")
     name the page that sentence came from.
   - On hover, the row also shows the time and the cost ("$0.003").
8. **Error.** Imago's error chip:
   - 401: "OpenRouter no longer accepts this key — connect again".
   - 402 or 403, branching on `metadata.limit_source`:
     - "Your OpenRouter account is out of credits";
     - "This key has reached its spending limit".
   - 403 moderation: "The model's provider declined this question".
   - 429: "Too many requests — try again in a moment", using
     `Retry-After`.
   - Other errors show OpenRouter's message.

   **Retry** re-asks.
9. **Stopped.** The partial answer stays, marked "Stopped".

### 5.4 Composer

ChatGPT's composer, scaled to the panel:

- **Shape.** Radius 24, on a raised fill with a hairline edge. The text
  is on top and the actions row is under it. 76 pt tall when empty,
  growing to 200 pt.
- **Placeholder.** "Ask about {short title}"
- **Actions row.**
  - **＋**: add a picture from a file, such as a photo of a paper map or
    a guide's page.
  - **Screenshot**: a camera icon and the word, in the game's accent.
    The tooltip reads "Screenshot the game ⌥⌘S".
  - The **send / stop** circle: white with an arrow, and a square while
    an answer streams.
- **Screenshots.** The owner asked for this button (2026-11-04), so the
  player can capture the game as it is for reference.
  - **Pressing it.** The console's frames are copied between frames
    (`Command::CaptureScreens`), as View > Screenshot does. Both screens
    are stacked as shown, with nothing drawn over them: no notices,
    readouts or script overlays.
  - **The thumbnail.** It lands in a row above the text. For a moment it
    has a 1.5 pt edge in the game's accent, the only flash.
  - **Up to four a message.** The button greys out at four.
  - **Removing and enlarging.** × on a thumbnail removes it. A click
    opens it large over the panel.
  - **Sending.** The shots go with the question and show above its pill
    in the thread, so it is always visible what was sent.
  - **Saving.** They are saved with the chat (5.5), so they stay as
    references after the game moves on.
  - **Nothing else is sent.** Only attached shots go; there is no
    automatic screen. A model without vision greys the button out, with
    a tooltip saying why.
- **Keys.** Enter sends; Shift-Enter adds a line; ⌘V pastes text. IME,
  selection and clipboard come from GPUI's `EntityInputHandler`, as in
  its `examples/input.rs` (§7.4).
- **History.** ↑ in an empty composer recalls the last question.

### 5.5 Conversations

- **One open chat per game**, kept across launches. Header ＋ starts a
  new one.
- The header's title menu lists earlier chats for this game, by first
  question and date. A chat can be deleted from there.
- Older chats are never sent to the model. Within a chat, every earlier
  turn's text is sent. Earlier screenshots are not: only the newest
  message's go.
- Screenshots are kept as PNGs beside the chat in
  `~/Library/Application Support/ds-emulator/Guide/` and are deleted
  with it.

### 5.6 Settings

A **Guide** section in the settings window (`settings_window.rs`), using
the existing row style:

| Row | Choices | Default |
|---|---|---|
| OpenRouter | ✅ Connect OpenRouter, or "Connected · key …1234 · $0.42 used" (from `/api/v1/key`) with Manage Keys and Disconnect (6.2) | not connected |
| Model | GPT-6 Luna; the picker (6.5) for others | GPT-6 Luna |
| Thinking | Off · Low · Medium · High | Low |
| Look things up | On · Off (tools and web search) | On |
| Pause while typing | On · Off | Off |
| Side | Left · Right | Right |

Settings file keys go under `[guide]` in `settings.toml`. The key itself
is never in the settings file (§6.2).

## 6. Design

### 6.1 Modules

```
crates/emu-guide/            pure Rust, no GPUI; unit-tested headless
  src/openrouter.rs          request body, SSE reader, event types, errors
  src/stream.rs              curl child: spawn, stream lines, cancel
  src/login.rs               OAuth PKCE: loopback listener, exchange
  src/key.rs                 0600 key file, env override, /key probe
  src/conversation.rs        turns, tool loop, persistence (JSON)
  src/game.rs                GameContext: identity, kind, screen PNG
  src/prompt.rs              the system prompt, built from GameContext
  src/sources.rs             the source map (sources.toml) and lookup
  src/tools.rs               server-tool config (phase 2); wiki_search,
                             wiki_page, pokeapi + extract (phase 3)
  src/markdown.rs            blocks, incremental parse, mend (from Imago)
  sources.toml               the curated source map, include_str!'d
crates/emu-app/src/guide/
  mod.rs                     GuidePanel entity, drawn via AnyView::cached
  composer.rs                the text field on EntityInputHandler
  transcript.rs              list(), rows, plates, scroll follow
  markdown.rs                blocks → GPUI elements, veil
  thought.rs                 the Thought process chip, tool rows
  motion.rs                  spinner, shimmer, veil maths (from Imago)
  palette.rs                 the game's palette from the ambient picture (6.7)
  connect.rs                 the not-connected card and its states
```

A new crate keeps the network, parsing and tool code testable without a
window, like `emu-cheats` and `emu-patch`. It also means a headless probe
(`cargo run -p emu-guide --example ask`) can try models from the command
line.

### 6.2 Connecting and the key

✅ Built as in Imago, chosen by the owner (2026-11-04): the player
connects with a button and never pastes a key
([research §3](research/game-chat.md#3-imagos-openrouter-approach)).

**Where it lives.**

- `emu-guide` has `login.rs`, `key.rs` and `api.rs`.
- `emu-app`'s `openrouter.rs` holds one connection for the whole app, so
  connecting carries on after the settings window closes.
- The settings window's Guide section has the OpenRouter row.

**The flow.** It runs on a thread of its own, in plain blocking Rust:

1. Bind `127.0.0.1:0`, and the same port on `::1` if it is free, with
   `std::net::TcpListener`.
2. Make the PKCE verifier from 32 bytes of `/dev/urandom`, and its S256
   challenge.
3. Open the authorize URL in the browser:
   `https://openrouter.ai/auth?callback_url=http://localhost:{port}/callback&code_challenge=…&code_challenge_method=S256&key_label=Imago Emulator · {Mac name}`.
   - The Mac's name comes from `scutil --get ComputerName`.
   - There is no `state` parameter, as in Imago. A code caught on the
     way is useless without the verifier.
4. Wait up to 10 minutes for `GET /callback`, which is as long as
   OpenRouter's codes last. Cancel stops the wait within 50 ms.
   - Favicons and other paths get a 404.
   - No `code` means the player declined.
5. `curl` POSTs `{code, code_verifier, code_challenge_method}` to
   `/api/v1/auth/keys`, with the body on stdin, and gets `{key, …}`.
6. Write `~/Library/Application Support/ds-emulator/OpenRouter/auth.json`.
   The folder is 0700 and the file 0600. The file is created 0600 and
   then renamed into place.
7. Answer the browser's request, held open until now, with a small page:
   "OpenRouter is connected", or why not. The page runs
   `history.replaceState` to drop the code from the address bar.

`OPENROUTER_API_KEY` in the environment wins, for testing.

**The settings row**, named OpenRouter under the Guide heading:

| State | Buttons | Line under them |
|---|---|---|
| Not connected | **Connect OpenRouter** | Guide asks OpenRouter's models about the game you're playing. |
| Connecting | Cancel | Finish connecting in your browser. / Finishing connecting… |
| Connected | Manage Keys, Disconnect | Connected · key …1234 · $0.42 used |
| Connected through `OPENROUTER_API_KEY` | Manage Keys | Using OPENROUTER_API_KEY …1234 |
| Key refused | **Connect OpenRouter**, Disconnect | This key no longer works — connect again. (red) |
| Failed | **Try Again** | the reason, such as "Access was declined on OpenRouter." (red) |

- The accent marks the button to press.
- Return or Space presses the row's first button.
- Each time the window opens, `GET /api/v1/key` fills in what the key
  has spent, and whether OpenRouter still accepts it. The key goes in
  through `curl --config -` on stdin.
- **Manage Keys** opens `https://openrouter.ai/settings/keys`, where the
  player can set a credit limit or revoke the key.
- **Disconnect** deletes the file. It can't revoke the key on OpenRouter.

A file, not the Keychain:

- It is Imago's choice.
- The app is ad hoc signed, so a Keychain item's access list would name
  a code hash that changes with every build, and macOS would ask for
  permission after each rebuild.
- A 0600 file is readable by the user's own processes. So is the
  Keychain through `security` once allowed. The key is OpenRouter's,
  scoped by its own limits.

The key is never logged (its `Debug` shows only its last four
characters), never put in a process's arguments, and never in
`settings.toml`.

### 6.3 Requests and streaming

The client is one `curl` child per model call. It runs on a short-lived
thread, as recordings and screenshots use their own threads.

```
curl -sS --no-buffer --connect-timeout 10 --speed-limit 1 --speed-time 60
     -H 'Content-Type: application/json'
     -H 'HTTP-Referer: https://github.com/lilfourn/ds-emulator'
     -H 'X-OpenRouter-Title: Imago Emulator'
     --write-out '\n%{http_code}\n' --config -
     https://openrouter.ai/api/v1/chat/completions
  stdin, a curl config:  header = "Authorization: Bearer sk-or-…"
                         data-binary = "{…the JSON body, quoted…}"
```

- **Nothing secret in `ps`.** The key and the body go on stdin, so
  neither appears in the process list, and nothing is written to disk.
- **No `unsafe`.** Passing a second pipe would need it, and the
  workspace denies it.
- **Status.** The trailing `-w` status tells a JSON error (non-200) from
  a stream.

The reader parses lines:

- `data: {json}` becomes one or more events.
- Lines starting with `:` are keep-alive comments, ignored. This includes
  `: OPENROUTER PROCESSING`.
- `data: [DONE]` ends the stream.
- A top-level `error` with `finish_reason: "error"` is a mid-stream
  failure (HTTP stays 200), and may be the only chunk.
- The usage chunk repeats `finish_reason`; the second copy is ignored.

Events go through an `async_channel` (already a dependency) to the panel:

```rust
enum Event {
    Reasoning(String),
    ReasoningDetails(Vec<serde_json::Value>),
    Text(String),
    ToolCall { index: u32, id: Option<String>, name: Option<String>, arguments: String },
    ServerToolCall { tool: String, arguments: serde_json::Value },   // web_search, web_fetch
    Annotation(Citation),
    Usage { prompt: u32, completion: u32, reasoning: u32, cost: Option<f64> },
    Finished(FinishReason),
    Failed(GuideError),
}
```

- **Stop.** Stop kills the `curl` child (`Child::kill`). The partial
  answer stays.
  - Closing the connection stops billing on OpenAI's and Azure's
    endpoints for Luna, but not on Bedrock's (6.3, Provider).
- **Timeouts and retries.**
  - `--connect-timeout 10`.
  - No total timeout, but 60 s without a byte is a failure.
  - One automatic retry, only before the first token arrives and only on
    a connection error, 502 or 503 (Imago's lesson).
- **Mid-stream errors.** An `error` object inside a `data:` line is
  reported, along with what had streamed so far.

Request body
([research §4](research/game-chat.md#4-openrouters-api)):

```json
{
  "model": "openai/gpt-6-luna",
  "stream": true,
  "max_tokens": 4000,
  "session_id": "<chat id>",
  "reasoning": { "effort": "low", "summary": "auto" },
  "verbosity": "low",
  "provider": { "sort": "latency" },
  "messages": [
    { "role": "system", "content": "<6.4's prompt>" },
    "… earlier turns, assistant ones with reasoning_details as received …",
    { "role": "user", "content": [
      { "type": "text", "text": "where can I catch Ralts?" },
      { "type": "image_url", "image_url": { "url": "data:image/png;base64,…" } }
    ] }
  ],
  "tools": [ "… 6.6 …" ]
}
```

- **Thinking.** Off sends `effort: "none"`. Luna supports `none`, `low`,
  `medium`, `high`, `xhigh` and `max`, with `medium` as its default, so
  the setting maps one to one. For a model whose `reasoning.mandatory`
  is set, Off sends the lowest effort the model supports.
- **What thinking shows.** OpenAI models return reasoning as summaries
  (`reasoning.summary` items) plus encrypted blocks, not raw thought.
  `summary: "auto"` asks for the summaries, and the Thought process chip
  shows them. Phase 0 checks how they stream.
- **Verbosity.** `verbosity: "low"` is a parameter Luna accepts. It keeps
  answers short for the narrow panel, together with the prompt's rules.
- **Provider.** Luna is served by OpenAI's standard, fast and flex
  endpoints, plus Azure and Bedrock. Flex is half price but slower, and
  Bedrock keeps billing after a stop. `provider` steers to low latency;
  phase 0 confirms that this avoids flex, or names OpenAI's standard
  endpoint and Azure explicitly.
- **Room for the answer.** `max_tokens` covers reasoning and answer.
  An empty answer with `finish_reason: "length"` shows "The model ran
  out of room while thinking — try less Thinking".
- **`session_id`** keeps a chat on one provider, so the system prompt
  and earlier turns hit OpenAI's automatic prompt cache. Cached input
  costs $0.01 per million tokens instead of $0.10.
- **Echo reasoning.** Each assistant turn is stored with its
  `reasoning_details` exactly as received, and sent back unchanged. The
  API needs this across tool calls, including the
  `reasoning.server_tool_call` entries.
- **Cost.** It comes from the final usage chunk (`usage.cost`). No
  separate call is needed.

### 6.4 Game context and the system prompt

`GameContext` is built when the panel opens and refreshed per message:

- **Title.** The library title and the No-Intro name ("Pokemon Omega
  Ruby (USA) (Rev 2)"), the platform label and the game code.
- **Kind.** One of:
  - retail;
  - ROM hack (`made_from_patch`), with the base game;
  - randomized (decision 0012), with "wild Pokémon, trainers and items
    differ from the wikis";
  - RPG Maker fan game (title from Game.ini);
  - homebrew from the store.
- **Play time.** From the library.
- **The screens.** On request. The emulation thread copies the core's
  frames between frames, as `SaveScreenshot` does, through a new
  `Command::CaptureScreens(reply)`, and encodes a PNG on a short-lived
  thread.
  - DS screens are stacked in their shown order with a 4 px gap,
    scaled 2× nearest-neighbour (512 × 776).
  - 3DS screens are stacked at 1× (400 × 484).
  - GB/GBA screens are scaled 3×.
  - Only what the console drew is sent: no overlay, no app chrome.

The system prompt is built from that context and the source map. A
sketch, tuned in phase 0:

```
You are Guide, the in-game helper inside Imago Emulator. The player is
playing {title} ({platform}, {region}{, revision}). {kind line}
They are playing right now; answer quickly and to the point.

How to answer:
- Lead with the answer in one or two sentences. Then at most a few short
  bullets or a small table (≤ 4 columns). No headings for short answers.
  The panel is narrow.
- Use the game's own English names for places, items, moves and
  characters, as this version spells them.
- Facts about this game (locations, encounter rates, item spots,
  requirements, stats): search before answering, then read the best page.
  Trust these sources, in this order: {name — what it's good for, per
  source}. For this game, start at {per-game paths, e.g.
  serebii.net/omegarubyalphasapphire/}. Answer from what you read and
  cite it. If you can't find it, say so and suggest where to check; never
  invent locations or numbers.
- {randomized/hack line: wiki data may not apply …}
- Spoilers: answer only what was asked. For "how do I get past X", give a
  hint first and offer the full solution. Never reveal later story events
  unprompted.
- If a screen is attached, use it to tell where the player is and what is
  happening, and say what you see when it matters.
- Don't help find or download ROMs of commercial games.
```

### 6.5 Models

- **GPT-6 Luna, chosen by the owner (2026-11-04).** It ships pinned as
  `openai/gpt-6-luna`
  ([research §4.6](research/game-chat.md#46-models)):
  - **Price:** $0.10 in and $0.50 out per million tokens; cached input
    $0.01.
  - **Context:** 1.05M tokens, with up to 128k out.
  - **Input:** images and text.
  - **Features:** tools, optional reasoning and `verbosity`.
- **Not the alias.** `~openai/gpt-luna-latest` points at the same model
  today, but it would move to the next Luna without notice, and with it
  the answers and the price.
- **What a question costs.** A typical question with a DS screen and
  one search costs about $0.002–0.01.
- **Fallback.** `models: ["openai/gpt-6-luna", "openai/gpt-5.6-luna"]`
  lets OpenRouter fall back when every Luna 6 provider fails.
  `gpt-5.6-luna` has the same features and costs $0.20 / $1.20. The
  fallback shows as a note under the answer.
- **Free option.** `openrouter/free` is listed as "Free (slower, rate
  limited)" for players without credits. It allows 50 requests a day
  until the account has bought $10 of credits. Web search is still
  billed on it.
- **The picker** (phase 3) is for players who want something else. It
  lists `GET /api/v1/models`, filtered to models whose
  `supported_parameters` include `tools`. Each row shows:
  - the name;
  - the price per million input and output tokens;
  - an eye glyph when its `input_modalities` include `image`.
- **Cache.** The list is cached for a day in the app's cache folder.
- **No vision.** If the chosen model can't take images, the screen
  Screenshot button greys out with a tooltip saying why.

### 6.6 Looking things up

**The source map.** `crates/emu-guide/sources.toml`, hand-curated from
the verified list in [research §5.2](research/game-chat.md#52-the-source-map):

- about 30 series;
- Pokémon's per-game Serebii paths;
- fan games and hacks;
- a fallback for every game.

The sketch of its shape:

```toml
[[series]]
id = "pokemon"
titles = ["Pokemon ", "Pokémon "]          # No-Intro or library title prefix
sources = [
  { name = "Bulbapedia", domain = "bulbapedia.bulbagarden.net", reach = "search",
    good_for = "everything: walkthroughs, locations, mechanics, version differences" },
  { name = "Serebii", domain = "serebii.net", reach = "fetch",
    good_for = "per-game encounter tables, items, events",
    games = { "Omega Ruby" = "/omegarubyalphasapphire/", "Platinum" = "/platinum/" } },
  { name = "PokémonDB", domain = "pokemondb.net", reach = "fetch",
    good_for = "Pokédex entries, movesets, locations per game" },
  { name = "PokeAPI", api = "https://pokeapi.co/api/v2/", reach = "api",
    good_for = "structured encounters per version, moves, evolutions" },
]

[[series]]
id = "zelda"
titles = ["Legend of Zelda, The", "The Legend of Zelda"]
sources = [
  { name = "Zelda Wiki", domain = "zeldawiki.wiki", reach = "mediawiki", api = "/w/api.php" },
  { name = "Zelda Dungeon", domain = "zeldadungeon.net", reach = "search", good_for = "walkthroughs" },
]

[[fallback]]                                   # every game, after its series
sources = [
  { name = "StrategyWiki", domain = "strategywiki.org", reach = "mediawiki", api = "/w/api.php" },
  { name = "GameFAQs", domain = "gamefaqs.gamespot.com", reach = "search" },
  { name = "Fandom", domain = "fandom.com", reach = "mediawiki-search" },
]
```

Phase 2 ships about 25 series: Pokémon, Zelda, Mario, Kirby, Metroid,
Fire Emblem, Golden Sun, Advance Wars, Castlevania, Mega Man, Dragon
Quest, Final Fantasy, Animal Crossing, Professor Layton, Phoenix Wright,
and others. It also ships a fan-game entry: the game's own wiki on
Fandom, PokéCommunity and Relic Castle through search.
`scripts/guide/check-sources.sh` re-probes every entry's reach. It is
run by hand, not in CI.

**Looking things up happens in two steps.**

**Phase 2: OpenRouter's server tools, limited to the source map.**
OpenRouter runs these, so the app has no tool loop to run.

```json
"tools": [
  { "type": "openrouter:web_search",
    "parameters": { "engine": "exa", "max_results": 5, "max_uses": 2,
                    "allowed_domains": ["<this game's source domains>"] } },
  { "type": "openrouter:web_fetch",
    "parameters": { "engine": "exa", "max_uses": 3, "max_content_tokens": 6000,
                    "allowed_domains": ["<this game's source domains>"] } }
]
```

- **Why Exa.** GPT-6 Luna could also use OpenAI's own search (GPT-5 and
  later have it on OpenAI's and Azure's endpoints). Exa is chosen
  instead:
  - it honours domain lists on every model and every endpoint;
  - it costs $0.007 a search against about $0.01;
  - `mode: "fast"` answers in about 450 ms.
- **Sites `curl` can't read.** Exa's index should reach Bulbapedia and
  GameFAQs, which are behind bot checks. Phase 0 confirms it.
- **Cost.** About $0.007 a search.
- **Order of trust.** The system prompt lists the sources in the map's
  order, since the API has no way to rank them.
- **When nothing is found.** A question that finds nothing in the
  game's sources may search once more without the domain list. The
  answer then says it is not from the usual sources.
- **What the player sees.** Each search and fetch is a row in the
  thought group: "Searching Serebii, Bulbapedia for Ralts", "Reading
  serebii.net/omegarubyalphasapphire/…". These rows come from the
  `reasoning.server_tool_call` entries in the stream. Phase 0 checks how
  these, and citation annotations, arrive when streamed.
- **The Sources row** lists the cited and fetched URLs, with duplicates
  removed.

**Phase 3: the app's own tools, for cheaper and more exact lookups.**
These run on the worker thread with `curl`, through a function-calling
loop.

| Tool | What the app does |
|---|---|
| `wiki_search(query, site?)` | **MediaWiki sites in the map:** `api.php?action=query&list=search` in parallel, 5 hits each, with title, snippet and URL. Free, and exact about titles. |
| `wiki_page(title, site, section?)` | `action=parse&prop=wikitext`, by section when asked, trimmed to text with tables kept as rows. Capped at 24 000 characters. |
| `pokeapi(path)` (Pokémon only) | `GET pokeapi.co/api/v2/{path}`, trimmed to the version in play: encounters, moves, evolutions. |

- **Round limit.** At most 4 rounds. Then the model answers from what it
  has.
- **Echo reasoning.** `reasoning_details` is echoed every round (6.3).
- **Being a good guest.**
  - User agent `DS-Emulator-Guide/0.1 (+https://github.com/lilfourn/ds-emulator)`.
  - `robots.txt` crawl delays respected per domain.
  - Pages cached for a day in the app's cache folder.
  - Only domains in the map are fetched.
- **The server tools stay** for sites behind bot checks.

### 6.7 Rendering: ChatGPT's look, Imago's streaming

**Colours from the game.** The panel complements whatever game is
being played. A `guide::Palette` is taken from the game's own screens,
from the same small blurred picture the ambient backdrop makes
(`ambient::picture`, 48 px across). It is sampled even when the
backdrop is Solid or the window is full screen.

| Token | Where it comes from |
|---|---|
| Glass (the panel) | ChatGPT's dark (`#121212`) with the picture's average colour mixed in at 8%, at 94% opacity over the backdrop, with a 30 pt backdrop blur. A 1 px white hairline at 7% on its inner edge. |
| Accent (the Screenshot button, the "new" edge on a fresh thumbnail, inline code) | The picture's most prominent saturated colour: Omega Ruby's grass green, a Game Boy's palette green, a fan game's title colour. Lightened in OKLCH until it reaches 4.5 : 1 against the glass. ChatGPT keeps colour rare, so the accent marks only these. |
| Raised (question pill, composer, chips) | White at 7.5% over the glass. |
| Send / stop | `#F4F4F4` with black, as ChatGPT's dark mode. White at 18% when there's nothing to send. |
| Text / muted / faint | Near-white `#EDEDF0`, `#A1A1AA` and `#71717A`, nudged 4% towards the accent's hue. They stay neutral enough to keep 7 : 1, 4.5 : 1 and 3 : 1 against any glass. |
| Danger | Imago's coral `#FC6146` unless the accent is itself red. In that case it is the coral's lightness at the accent's hue plus 30°, so an error still stands out in a red game. |
| Answer | Text straight on the glass, with no plate, as ChatGPT draws it. |

How it follows the game:

- **Recomputed at most every 2 s, eased over 600 ms.** A new area,
  battle or menu shifts the panel's colours gently, never by the frame.
- **Dark, white or flat frames keep the last palette.** Fades,
  transitions and loading screens have a lightness below 8% or above
  92%, or no colour with enough chroma. Keeping the last palette means
  the panel doesn't flash grey between scenes.
- **Before the first frame, and for greyscale games,** the system's
  tint from `theme::tint(platform)` is used. An original Game Boy game
  in Grey gets the Game Boy's plate colour; in Game Boy Color colours,
  it gets those.
- **The ambient picture is not sampled again.** The palette comes from
  the picture the backdrop already made (or the same function at a 2 s
  rate when the backdrop is Solid), on the UI thread. That costs a few
  microseconds for 48 × 48 pixels.
- **Contrast is guaranteed by construction, and unit-tested.** The
  palette function is pure. Its tests feed it frames from test ROMs and
  synthetic extremes (pure red, pure white, black, a two-colour Game
  Boy frame) and check every pairing above.

Layout, from ChatGPT's iOS app, which is about the panel's width:

- **Margins and spacing.** 20 pt side padding, 14 pt between turns, 10
  pt between paragraphs.
- **The question.** Right-aligned, at most 80% wide. Radius 18, padding
  8 × 14. Its screenshots sit above it, both screens stacked, 76 pt
  wide.
- **The thinking line.** "Thought for 6s ›" in 13.5 pt muted type.
  Opened, the activity list has a 15 pt icon (search, the site's icon,
  the screen) and 13/19 text per step.
- **Source chips.** 18 pt high, radius 9, 11 pt muted type on raised
  fill, after the sentence they back.
- **The action row.** 16 pt icons in 28 pt buttons (copy, ask again),
  then "Sources" with up to three 16 pt site icons stacked.
- **The Sources sheet.** It slides up over the panel, which dims to
  35%, with radius 18 at the top. Each row has the site (12 pt) and the
  title (13.5 semibold) over one line from the page.
- **The composer.** Radius 24 on raised fill with a hairline edge and a
  soft shadow. ＋ and Screenshot on the left, send / stop (32 pt) on the
  right. Under it, in 11 pt faint type: "Guide can make mistakes.
  Answers cite fan wikis."
- **Screenshot thumbnails.** In the composer, 44 × 26 and 35 × 26 pt
  stacked on black, radius 10, with a × badge (18 pt) at the corner.

Type, in the system font, as the rest of the app:

- Body 14.5/23.
- H1 17/25, H2 15/23, H3 14.5/23, semibold.
- Code 12.5/18 Menlo, as the app already uses.
- Meta 13.5 and 12; fine print 11.

Behaviour, copied value for value from Imago (research §2):

- **Blocks.** The answer is split into top-level blocks; only the last
  block is reparsed per commit. Each block is a `list` row.
- **Mend.** Unclosed `**`, `*`, `_`, `~~` and backticks are closed for
  display. A partial link renders as unclickable link text. A trailing
  `-` or `=` line is guarded. The code is ported from Imago's `mend.rs`,
  with its tests.
- **Commit cadence.** Streamed text is committed every 120 ms, or at once
  when a block closes.
- **Veil.** New byte ranges fade in by opacity:
  - alpha `1 − (1 − p)^1.6`;
  - duration `clamp(3 × ema, 120, 400)` ms;
  - no fade for text already on screen when the panel opens;
  - off with macOS Reduce Motion.
- **The thinking line.** While live, it shimmers ("Thinking", then what
  is being searched or read). Settled, it reads "Thought for Ns ›" and is
  closed until clicked. It opens and folds over 140 ms, ease-out. The
  reasoning summaries in it are capped at 24 lines, then "… N more
  lines".
  - There's no spinner: ChatGPT's shimmering line is the only sign of
    work.
- **Shimmer.** A 3.4 s sweep, repainting the shaped line in 2 px strips
  under content masks.
- **Markdown elements:**
  - paragraphs;
  - bold, italic and strike;
  - inline code on a 12% accent wash, radius 4.5;
  - links: body colour, 1 px muted underline, open in the browser;
  - lists with accent markers;
  - blockquotes with a 2 px accent border;
  - flat hairline tables, scrolling sideways in the narrow panel;
  - fenced code with a copy button, unhighlighted.
  - A paragraph of nothing but `wiki:` images shows pictures from the
    game's wiki, captioned, side by side when several; an image amid text
    shows its alt text ([decision 0015](decisions/0015-guide-pictures-from-wikis.md)).
- **Scroll.** Pinned to the bottom while streaming. Only the wheel
  unpins it; scrolling back to within 70 pt re-pins it. A jump-to-bottom
  button shows past 320 pt. A 24 pt fade sits at the top edge.
- **Copy.** A copy button per answer (its Markdown) and per code block.
  Free text selection is not built, as Imago's took its own registry
  (GQ-4).
- **Probe.** Probe part names for `check-layout`: `panel:guide`,
  `field:guide`, `button:send`, `button:stop`, `button:screenshot`,
  `thumb:shot.N`, `line:thought`, `chip:source`, `button:sources`,
  `button:connect`, `sheet:enlarged`.

### 6.8 Staying free in play

- **A cached view.** `GuidePanel` is an `Entity` drawn as
  `AnyView::from(panel).cached(style)`. Game frames notify only the game
  view, so the panel's layout and paint are reused. Streaming notifies
  only the panel, at most once per 120 ms commit plus veil frames.
- **Off the UI thread.** Network, parsing, tool calls, PNG encoding and
  file writes all run on worker threads. The UI thread only appends text
  and lays out the last block.
- **Screen capture.** Copying frames on the emulation thread is
  what screenshots already do, one copy between frames.
- **Pause and fast-forward** behave as today. The panel never touches
  pacing (decision 0003).

## 7. Requirements

### 7.1 Panel and keys

| ID | Requirement | Priority · phase |
|---|---|---|
| GC-1 | A Guide panel in one margin, sized and placed as in 5.1, drawn as a cached view. Closed, the window renders exactly as today. | P0 · 1 |
| GC-2 | When the margin is narrower than the minimum, opening Guide lays the screens out in the window less the panel's column, still in the chosen arrangement; closing restores them. | P0 · 1 |
| GC-3 | ⌘J and View > Guide, the Escape menu's Guide tile and the margin pill open it. Escape and clicks on the screens give keys back (5.2). | P0 · 1 |
| GC-4 | `Route::Guide` in `key_routes.rs`. Held game keys are released when the composer takes the caret. No key typed in the composer reaches the game, and no held game key types into it. | P0 · 1 |
| GC-5 | A composer on `EntityInputHandler`: IME, selection, ⌘C/⌘V/⌘X/⌘A, Enter sends, Shift-Enter adds a line, grows to 160 pt. | P0 · 1 |
| GC-6 | Notices and the speed readout move to the other margin while the panel is open. | P1 · 1 |
| GC-7 | Side setting (Left or Right) and Pause while typing. | P1 · 3 |
| GC-8 | Gamepad chord to open and close it. | P2 · 3 |

### 7.2 OpenRouter

| ID | Requirement | Priority · phase |
|---|---|---|
| GC-10 | Connect OpenRouter by OAuth PKCE over a loopback callback, as 6.2, with the states in 5.3. Disconnect deletes the key file. ✅ in the settings window; the Guide panel's card (5.3) comes with the panel. | P0 · 1 |
| GC-11 | The key in a 0600 file, written atomically; `OPENROUTER_API_KEY` overrides it. Never logged, in argv or in settings. ✅ | P0 · 1 |
| GC-12 | Streaming chat completions through `curl` with the key and body on stdin or pipes; SSE parsing of text, reasoning, tool-call fragments, annotations, usage, keep-alive comments and mid-stream errors; stop kills the request. | P0 · 1 |
| GC-13 | Errors mapped to the copy in 5.3; one retry before the first token on connection errors, 502 and 503. | P0 · 1 |
| GC-14 | Reasoning effort from the Thinking setting; reasoning shown in the Thought process chip; reasoning details passed back across tool rounds as the API requires. | P0 · 1 |
| GC-15 | The model picker from `/api/v1/models` (6.5), cached a day; vision-less models grey out the Screenshot button. | P1 · 3 |
| GC-16 | Key usage (and limit, if set) in the settings row from `/api/v1/key`, with Manage key; cost per answer on hover, from the usage chunk. | P1 · 3 |

### 7.3 Answers

| ID | Requirement | Priority · phase |
|---|---|---|
| GC-20 | `GameContext` (6.4) in every request: exact title, platform, region, revision, kind. | P0 · 1 |
| GC-21 | The Screenshot button and ⌥⌘S (5.4): `Command::CaptureScreens` returns the console's frames between frames, without overlays. Up to four thumbnails in the composer, removable and enlargeable, sent as PNGs scaled as 6.4 and shown above the question. They are saved with the chat. Only attached shots are sent. | P0 · 1 |
| GC-22 | The system prompt of 6.4, versioned in `prompt.rs`, with snapshot tests per game kind. | P0 · 1 |
| GC-23 | The source map, about 25 series plus fan games and the fallback, with `check-sources.sh`. | P0 · 2 |
| GC-24 | `openrouter:web_search` and `openrouter:web_fetch` with the Exa engine, `allowed_domains` from the game's sources and use limits (6.6); one unrestricted search only when the sources found nothing, and the answer says so. | P0 · 2 |
| GC-25 | Search and fetch calls shown as rows in the thought group, from `reasoning.server_tool_call`. | P0 · 2 |
| GC-26 | A Sources row of chips under each answer that looked anything up; each opens its page. | P0 · 2 |
| GC-27 | Starter chips in an empty chat (5.3). | P1 · 2 |
| GC-28 | The app's own `wiki_search`, `wiki_page` and `pokeapi` tools with a function-calling loop (6.6), crawl delays, page cache and a 4-round limit. | P1 · 3 |

### 7.4 Look and feel

| ID | Requirement | Priority · phase |
|---|---|---|
| GC-30 | Markdown blocks with incremental reparse and mend, ported from Imago with its tests. | P0 · 1 |
| GC-31 | The veil, the thinking line with its activity list, and the shimmer, to 6.7's values; Reduce Motion turns them off. | P0 · 1 |
| GC-32 | Tables, blockquotes, lists, inline and fenced code styled as 6.7; tables scroll sideways. | P0 · 1 |
| GC-33 | Scroll follow with wheel unpin and jump-to-bottom. | P1 · 1 |
| GC-34 | One chat per game, persisted; ＋ for a new one; earlier chats in the title menu; delete. | P1 · 1 |
| GC-35 | Copy buttons per answer and code block. | P1 · 1 |
| GC-38 | ChatGPT's look (6.7, owner 2026-11-04): unboxed answers, the question pill, the thinking line, source chips and the Sources sheet, the action row, the rounded composer with ＋, Screenshot and send / stop, the panel flush to the window's side. Checked against the mockup and the Mobbin references in the research. | P0 · 1 |
| GC-36 | `check-layout.sh` states: not connected, empty, streaming, answered with sources, Sources sheet, screenshots in the composer, error, narrow window (GC-2), at default and minimum sizes. | P0 · 1–2 |
| GC-37 | The game's palette (6.7): the glass's tint, the accent, text and danger taken from the ambient picture. Recomputed at most every 2 s and eased; dark, white and flat frames keep the last palette; system tint before the first frame. Contrast pairs are unit-tested on test-ROM and synthetic frames. | P0 · 1 |

### 7.5 Untouched

| ID | Requirement |
|---|---|
| GC-N1 | Emulation, pacing, saves, rewind, frame CRCs and headless runs are identical with Guide open, closed or streaming. |
| GC-N2 | Nothing reaches the network until the player connects, and nothing but `openrouter.ai` and the source map's domains after. |
| GC-N3 | The library, store and settings windows are unchanged apart from the Guide settings section. |

## 8. Performance and acceptance

Budgets on this Mac, release build, Omega Ruby and Platinum full screen
on the 120 Hz built-in display and the 240 Hz external:

| What | Budget |
|---|---|
| Game frames with the panel streaming a long answer, 1× and 3× fast-forward | Same uneven-refresh count as with the panel closed (`scripts/macos/bench.sh` + `bench-report.py`), within run-to-run noise |
| UI thread per panel commit | ≤ 2 ms p99 with a 40-block transcript |
| UI thread on a game frame while the panel is open but idle | No more than with it closed (cached view) |
| Time to first token shown | Within 150 ms of the first SSE text byte |
| Screen capture to request sent | ≤ 50 ms |

Acceptance:

- **Unit tests** (`emu-guide`), all offline:
  - SSE parsing from recorded streams: text, reasoning, tool-call
    fragments across chunks, keep-alive comments, a mid-stream error and
    the final usage chunk.
  - The PKCE challenge, against RFC 7636's example.
  - The callback listener: granted, declined, favicon.
  - Mend, ported from Imago with its tests.
  - Source-map lookup by title and kind.
  - Prompt snapshots: retail, hack, randomized and fan game.
  - Tool trimming of MediaWiki and HTML fixtures.
- **A local fake OpenRouter.** A `TcpListener` serves recorded streams,
  so the client, the tool loop and the PKCE exchange are tested end to
  end without a network.
- **A live probe**, run by hand: `cargo run -p emu-guide --example ask --
  --game "Pokemon Omega Ruby (USA)" "where can I catch Ralts?"` with
  `OPENROUTER_API_KEY` set. It prints the stream, the tool rounds, the
  latency and the cost. It is never run in CI.
- **In the app**, with an isolated HOME and `cp -c` clones of saves
  (never the real ones):
  - `check-layout.sh` with GC-36's states;
  - a hand run of scenarios 1–6;
  - the pacing benchmarks with the panel streaming.

## 9. Phases

**Built on 2026-11-04** ([decision 0014](decisions/0014-guide-through-openrouter.md),
phase 0's findings in [research §7](research/game-chat.md#7-phase-0-what-gpt-6-luna-did-with-real-questions)):

- Phase 0: the streaming client (`emu_guide::chat`) and the `ask`
  example, tried on real questions.
- Phase 1: GC-1 to GC-6, GC-10 to GC-14, GC-20 to GC-22, GC-30 to GC-35,
  GC-37 and GC-38. The panel opens with ⌘J, View > Guide or the margin's
  pill; ⌥⌘S and File > Screenshot for Guide take screenshots.
- Phase 2: GC-23 to GC-27.
- Phase 3: GC-16, and the Thinking and Look Things Up settings
  (Settings > Guide).

Changed from the plan, after phase 0:

- Luna's reasoning is encrypted, so "Thought for Ns" opens onto the
  searches and pages read, not reasoning summaries. While a search runs
  the line reads "Searching Bulbapedia, Serebii".
- Citations arrive before the text with no position, so the inline source
  chips are the model's own `[Site](url)` links, which the prompt asks for.
- `provider.order = ["openai", "azure"]` replaces `sort = "latency"`,
  which chose Bedrock.
- The key and the body both go to `curl` on its standard input as a config
  file; nothing is written for a request.
- Screenshots come from the frames the window last took, which the
  emulation thread copied between frames; no `Command::CaptureScreens`.
- The Escape menu has no Guide tile: its grid is built for four tiles,
  and ⌘J, View > Guide and the margin's pill already open Guide.
- `check-layout.sh` has no Guide states yet (GC-36).

Not built: GC-7 (side, pause while typing), GC-8 (gamepad chord), GC-15
(model picker), GC-28 (the app's own wiki and PokeAPI tools), and the
pacing benchmarks with the panel streaming (section 8).


0. **Spike (1–2 days).**
   - The `emu-guide` crate with the `curl` streaming client and the
     `ask` example.
   - Settle the details the research marks *unverified*:
     - where streamed citation annotations and
       `reasoning.server_tool_call` entries arrive;
     - whether `id` and `name` come only in a tool call's first fragment;
     - the image size limit;
     - Exa's fetch price;
     - whether an OAuth key can read `/api/v1/key`;
     - how well Exa's index covers the sites behind bot checks
       (Bulbapedia, GameFAQs, PokéCommunity), with `allowed_domains`.
   - Tune GPT-6 Luna on about fifteen real questions, with and without
     the screen and search:
     - Pokémon Omega Ruby and Platinum;
     - a Zelda;
     - a randomized copy;
     - an RPG Maker fan game.

     Set the Thinking default (none, low or medium) and `verbosity` by
     correctness, time to first token and cost. Check that it reads
     pixel text on DS screens at 2×. Check how its reasoning summaries
     stream. Check which provider endpoint `provider` routing lands on.
   - Write [decision 0014](decisions/) (Guide through OpenRouter over
     `curl`, key in a file, a cached panel view).
1. **The panel.** GC-1 to GC-6, GC-10 to GC-14, GC-20 to GC-22, GC-30 to
   GC-36. Connect, ask, stream, think, stop, errors, screen, game context,
   persistence: a complete chat with no lookups yet.
2. **Grounded answers.** GC-23 to GC-27: the source map, OpenRouter's
   search and fetch limited to it, the Sources row and starters.
3. **Choice and polish.** GC-7, GC-8, GC-15, GC-16, GC-28: model picker,
   key usage, side setting, pause while typing, gamepad, and the app's
   own wiki and PokeAPI tools.
4. **Later, if wanted.**
   - Game-state context for Pokémon (party, location, badges) read from
     memory through the existing cheat and Lua peek paths, so "what
     should I use against this gym?" knows the team.
   - Prefer independent wikis over Fandom forks using Indie Wiki Buddy's
     full list.
   - Imago-style free text selection.

## 10. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| The chat's layout runs on game frames | Dropped frames at 3× fast-forward | Cached view (6.8); pacing benchmarks gate phase 1. |
| Models invent locations and numbers | Wrong answers, lost trust | Tools first, cite or say "not found" (6.4); test questions in phase 0 scored for correctness. |
| Fan sites block or rate-limit the app | Lookups fail | Map only sites that allow it; respect crawl delays; cache pages a day; fall back to web search; never get past bot checks. |
| A site's layout changes and HTML extraction breaks | Poor page text | Prefer MediaWiki and JSON APIs; HTML extraction is generic (text, tables), not per-site scraping. |
| Costs surprise the player | Distrust | Cheap default model, low default thinking, cost shown per answer, credits in settings, OpenRouter's own key limits. |
| Screens sent without the player meaning to | Privacy | Nothing is sent automatically. Screenshots are taken only by the button, ⌥⌘S or a starter, each is a thumbnail before sending and on the sent message, and the connect card says so. |
| `curl` streaming quirks (buffering, proxies) | Stalls | `-N --no-buffer`, idle timeout, phase 0 probe; an HTTP crate is the fallback (`ureq` with rustls), recorded in decision 0014 if needed. |
| A text field on `EntityInputHandler` is more work than it looks | Phase 1 slips | Start from GPUI's own `examples/input.rs`; multiline wrap is the main addition. |
| OpenRouter API changes (reasoning fields, web search) | Broken parsing | Unknown fields ignored; recorded fixtures updated with the live probe. |
| Server tools are beta (6.6) | Search breaks or changes shape | Phase 3's own tools cover the wikis without them; the answer still streams if a search fails. |
| Stopping doesn't stop billing on some providers (Google, Bedrock) | Small surprise costs | Luna's routing avoids Bedrock (6.3); OpenAI and Azure stop billing on disconnect. |
| A game's colours make the panel hard to read or garish | Unreadable answers, or a panel that clashes | Contrast enforced in the palette function and unit-tested; text stays near-white; accent only on small marks; changes eased and held through transitions (6.7). |
| Luna is retired or repriced | Guide stops or costs more | Pinned id with a fallback model (6.5); `expiration_date` from `/models` shown as a notice in settings when set. |

## 11. Open questions

| ID | Question | Needed by |
|---|---|---|
| GQ-1 | ~~Dark panel or an Imago Light white card?~~ Neither: "have it complement whatever game is being played" (owner, 2026-11-04). The panel takes its colours from the game (6.7, GC-37). | Phase 1 |
| GQ-2 | Keep playing while typing (the plan's default), or pause while the composer has the caret? Not answered; the default stands, with the setting in 5.6. | Phase 1 |
| GQ-3 | ~~System font or Imago's Geist?~~ The system font: ChatGPT's look, in the app's own type (6.7). | Phase 1 |
| GQ-4 | Is a copy button per answer enough, or is free text selection wanted? | Phase 3 |
| GQ-5 | Right margin by default, or left? | Phase 1 |
| GQ-6 | ~~Paste-a-key as well as Connect OpenRouter?~~ Connect OpenRouter only, as in Imago (owner, 2026-11-04). Built (6.2). | Phase 1 |
| GQ-7 | ~~Default model?~~ GPT-6 Luna, `openai/gpt-6-luna` (owner, 2026-11-04). | Phase 0 |
