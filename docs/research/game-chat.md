# Research — a game guide chat beside the screens

Date: 2026-11-04. Plan: [PRD-game-chat.md](../PRD-game-chat.md).

The owner asked for a chat in the empty margins beside a running game,
where players ask about the game they are playing. It should talk to
models through OpenRouter, copy Imago's chat style, and know where good
answers about games come from: the big fan sites and wikis. This file
records what was found. The PRD records what to build.

## 1. What the app has today

- **The margins.** Windowed play has a dark stage (`theme::STAGE`,
  `#0B0B0D`) around the screens. With Backdrop set to Ambient (the
  default), the stage shows a blurred, dimmed copy of the game's colours
  (`ambient.rs`). Stacked DS and 3DS screens are tall and narrow, so a
  wide window leaves two large margins. In the owner's screenshot
  (Omega Ruby at about 1000 × 645 pt), each margin is about 230 pt wide.
  On the 1728 × 1117 pt built-in display in full screen, stacked DS
  screens are 745 pt wide and leave about 490 pt on each side. 3DS
  screens leave about 400 pt. GB, GBC and GBA screens are landscape, so
  they leave margins only in a tall window.
- **What already sits in the margins.** The notice plate (bottom left)
  and the speed readout (bottom right) are drawn over the window
  (`view.rs`, "The plate under words over the game").
- **The game view is one big render.** `GameView::render` (`view.rs`)
  lays out the whole window and is notified at the game's frame rate,
  60–120 times a second. A chat drawn in that tree would be laid out
  again on every game frame. GPUI 0.2.2 has `AnyView::cached`
  (`gpui-0.2.2/src/view.rs:103`), which reuses a child view's layout and
  paint until that view is notified. The chat must be a separate entity
  drawn through it.
- **Keys.** `key_routes.rs` remembers where each held key went (Library,
  Menu or Game). The default binds use most letters: WASD, U I J K, Q E,
  Z C, R, B, Tab, Space, Enter and `` ` ``. Typing in a chat therefore
  needs its own route, and it must release held game keys when it takes
  the keyboard.
- **Text entry.** The library's search field is hand-rolled from
  `key_char` (`library_view.rs:611`). It has no IME, selection or paste.
  GPUI 0.2.2 provides `EntityInputHandler` (`src/input.rs:10`) and an
  `examples/input.rs` field with IME, selection and clipboard support.
  A chat composer should be built on those.
- **Other GPUI 0.2.2 pieces a chat needs.** All of these exist:
  - `list` + `ListState` with `ListAlignment::Bottom`, for a virtualized
    transcript pinned to the bottom.
  - `with_content_mask`, for the shimmer.
  - `shape_line`, `linear_gradient`, clipboard and `open_url`.
  - Missing: backdrop blur and `reduce_motion`. Imago gets these from
    its `zui` fork.
- **Networking.** The app has no HTTP crate. Box art and the store run
  `curl` (`boxart.rs:489`, `store.rs`), which macOS ships. `curl -N`
  streams server-sent events line by line. Headers can come from a
  0600 `--config` file and the body from stdin, so neither the key nor
  the body appears in `ps` (§4.8).
- **Knowing the game.** `game_db.rs` maps CRC, serial or title to a
  No-Intro name, such as "Pokemon Omega Ruby (USA) (En,Ja,…) (Rev 2)".
  `rom_info.rs` gives the platform and game code. `library.rs` knows:
  - ROM hacks, made from a patch (`made_from_patch`).
  - RPG Maker fan games and their titles (`Game::rpg_maker`).
  - Randomized copies (decision 0012).
  - Play time.
- **Grabbing the screen.** Screenshots copy the core's own frames
  between frames on the emulation thread (`Command::SaveScreenshot`). A
  variant that returns PNG bytes, instead of writing a file, would give
  the chat what the player sees.

## 2. Imago's chat style

Imago (`~/Desktop/Imago`) is a GPUI app on the `zui` fork. Paths below
are under `crates/`. `U/` is `ui/src/` and `M/` is `ui/src/markdown/`.

### 2.1 Layout and type

- **Turns.** Both sides are drawn as "wash" plates.
  - **User turn:** right-aligned, at most 80% of the column wide,
    padding 16 × 10, corner radius 16. Text is 14 px with a 22 px line
    height (`U/transcript/bubbles.rs`, `row.rs:414-430`).
  - **Assistant turn:** on the left with the same padding, radius and
    fill. A one-paragraph answer shrinks to fit its text. A multi-block
    answer fills the column.
  - **Fill:** a 4% black wash in light, 8% white in dark
    (`U/theme.rs:1587-1592, 1651`).
  - **Join:** the corner where two plates from the same side meet
    tightens to 4.
- **Gaps.** A new turn starts 16 below the last. Blocks in an answer are
  12 apart. Joined plates are 4 apart.
- **Text styles.**
  - Body: 14/22.
  - Headings: H1 19/27, H2 16/24, H3 15/22, all semibold
    (`M/render.rs:805-812`).
  - Code: 12.5/18 in a monospace font.
  - Meta: 12 px, muted.
- **`imago_light` colours** (`theme/src/builtins.rs:288-319`):
  - Text, muted and faint: `#232327`, `#55555e`, `#76767f`.
  - Accent: `#0070b5`.
  - Danger: `#c83314`.
  - ds-emulator's `theme.rs` already uses the same text, accent and
    danger colours.
- **Per-turn strip.** Under a turn, shown on hover: a timestamp and a
  copy button that copies the turn's Markdown (`row.rs:560-600`).

### 2.2 Thinking

- **The chip.** Reasoning is a chip labelled **"Thought process"**
  (`U/transcript/thought.rs`).
  - It is open while it streams and closed once settled, unless the
    user has toggled it. The fold tween is 140 ms ease-out.
  - Its body is the reasoning's Markdown, flattened to 12 px lines,
    18 px high, in the faint colour. Bold, italic and code keep their
    style; lists use "• ".
  - At most 24 lines show, then "… N more lines". While live it keeps
    the newest lines; once settled, the first.
- **No duration.** Imago shows no "Thought for N s".
- **Before the first token**, a trailer sits under the last row
  (`U/transcript/trailer.rs:109-232`):
  - **Spinner:** a 3 × 3 grid of round cells tinted `#B6D3EF`,
    `#EDB185` and `#F888A0` by row. Each cell pulses opacity
    1.0 → 0.1 → 1.0 over a 750 ms period, offset by its distance from
    the bottom centre (`U/loaders.rs:27-53`,
    `U/loader_motion.rs:30-79`).
  - **Word:** one of 21 flavour words ("Thinking…", "Pondering…",
    "Brewing…", …), in 12 px muted. It changes every 7 s.
  - **Time:** the elapsed time, faint ("7s", "1m 3s").
- **Shimmer.** The live word and live titles have a highlight that
  sweeps across every 3.4 s (`U/transcript/tool_motion.rs:17-18,
  151-161`). It is drawn by repainting the same shaped text in 2 px
  strips under content masks, mixing muted to text colour.

### 2.3 Streaming

- **Commits.** Streamed text is committed every 120 ms
  (`doc/src/constants.rs:6`).
- **Parsing.** `IncrementalParser` (`M/parser.rs:920`) reparses only
  from the last top-level block. Each block is its own list row, so a
  long answer costs no more per update than a short one.
- **Mending** (`M/mend.rs`). A display-only copy of the last block
  gets these fixes:
  - Unclosed `**`, `*`, `_`, `~~` and backticks are closed.
  - `[text](partial` becomes a link that can't be clicked yet.
  - A last line of only `-` or `=` gets a zero-width space, so it is
    not read as a heading underline.
  - The canonical text stays untouched.
- **The veil** (`M/veil.rs`). New text fades in by opacity only.
  - Alpha is `1 − (1 − p)^1.6`.
  - Duration is `clamp(ema × 3, 120, 400)` ms, where `ema` tracks the
    gaps between chunks.
  - Text already on screen when the chat opens never fades.
  - There is no cursor.
- **Scroll.** A spring keeps the bottom in view
  (`U/transcript/scroll.rs`).
  - Only wheel input breaks the pin. Scrolling back within 70 px of the
    end, while moving down, re-pins it.
  - A jump-to-bottom button appears past 320 px from the end.
  - A 24 px edge fade sits at the top and bottom.

### 2.4 Markdown

- **Parser.** `pulldown-cmark` 0.12 with tables, strikethrough, task
  lists and math (`M/parser.rs:170-173`), plus Imago's own bare-URL
  autolinks.
- **Inline code.** Monospace in the accent colour, on a 10% accent
  wash, radius 4.5.
- **Links.** Body colour with a 1 px muted underline.
- **Lists.** Accent-coloured `N.` markers or a 5 px accent disc, with a
  4 px gap between items.
- **Blockquote.** A 2 px accent left border at 0.6 alpha, a 5% accent
  fill, right corners radius 6.
- **Tables.** Flat hairlines: no box or header fill, 1 px row rules at
  10%, cell padding 12, bold headers, each column at least 96 px wide.
  Tables matter here: encounter and stat tables are a staple of game
  answers.
- **Code blocks.** Radius 14, a 3.5% ink fill, a 28 px header with the
  language name and a copy button. Syntax highlighting is tree-sitter;
  game answers rarely need it.
- **Selection.** Imago adds its own text-selection registry, because
  GPUI has no text selection. That is heavy. Copy buttons cover most of
  the need.

### 2.5 Composer

- **Shape.** A pill, radius 26, with a hairline border.
  - The textarea is 76 px empty and grows to 260.
  - Placeholder: "Do anything…".
- **Send.** A 28 px filled circle with an up arrow, in text colour on
  the background. It is 35% opaque when blocked.
- **Stop.** The same circle with an 11 px rounded square, shown while a
  run is live and the draft is empty.
- **Keys.** Enter sends and Shift-Enter adds a line. Enter never stops
  a run.
- **Model picker.** Sits in the actions row, with reasoning levels.

### 2.6 Errors

An error chip with a radius of 10:

- a 1 px border at danger 16% and a danger 5% fill;
- the label "Error" and a copyable message;
- 12 px text (`U/notice.rs:61-110`).

### 2.7 What makes Imago's answers look good

It is the renderer, not the prompts. Imago sends no "use Markdown" or
"be concise" style guide. Its only output rules are:

- Write maths as LaTeX.
- Never draw charts as ASCII art.
- Never put Markdown images in the chat.

The polish comes from mending partial Markdown, the veil, block-level
reparsing and the spacing above. The game chat should copy those, and
add its own system prompt for game answers (§6).

### 2.8 What ports to stock GPUI 0.2.2

- **Ports directly.**
  - The layout constants.
  - The pure maths of the veil, shimmer phase, spring and spinner.
  - `mend.rs` and the block-level parser, which are pure Rust.
  - `StyledText` with `TextRun`s.
  - `list` with bottom alignment.
- **Needs a replacement.**
  - `EdgeFade` becomes a gradient overlay.
  - Frosted vibrancy becomes a translucent plate over the ambient
    backdrop, which is already blurred.
  - `reduce_motion` becomes a read of macOS's Reduce Motion setting
    through AppKit.
- **Crates.** `pulldown-cmark` (Imago pins 0.12) and
  `unicode-segmentation`. Skip tree-sitter and RaTeX.

### 2.9 ChatGPT's look, from Mobbin

The owner asked to copy ChatGPT's cleanness (2026-11-04). The references,
all on Mobbin:

- web: an answer with "Thought for 42s" and inline sources;
- web: the Activity and Sources side panel;
- web: the Sources list;
- web: the empty chat with its composer;
- iOS: a narrow answer with its action row and Sources;
- iOS: the Sources sheet.

What to take:

- **Answers.** Answers sit on the page with no bubble. Questions sit in
  a grey pill on the right.
- **Thinking.** Reasoning is one grey line, "Thought for Ns ›". Opened,
  it lists what was searched and read, with each site's icon.
- **Sources.**
  - Small grey chips ("Mobbin +2") follow the sentence they back.
  - A Sources button with stacked site icons sits in the action row,
    and opens a list: site, title, a line from the page.
- **The action row.** Copy, good, bad, retry, share, as small grey
  icons under each answer.
- **The composer.**
  - A rounded box: the text on top, then ＋, tool chips in colour
    ("Think", "Research"), and a filled send circle that becomes a
    square to stop.
  - A new chat centres a short question over it ("What are you working
    on?").
- **Colour.** Almost none: greys, with colour only on tool chips.

## 3. Imago's OpenRouter approach

- **Imago never calls OpenRouter's chat API from Rust.** It signs the
  user in, stores the key, and hands it to an opencode server, which
  does the inference. The game chat needs its own client: SSE parsing,
  reasoning deltas and tool calls.
- **Sign-in is OAuth PKCE over a loopback callback**
  (`engine/src/agent_accounts/openrouter_login.rs`):
  1. Bind `127.0.0.1:0`, and the same port on `::1`, before opening
     the browser.
  2. The verifier is 96 hex characters (three v4 UUIDs). The challenge
     is base64url(SHA-256(verifier)), method `S256`.
  3. Open `https://openrouter.ai/auth?callback_url=http://localhost:{port}/callback&code_challenge=…&code_challenge_method=S256&key_label=Imago · {device}`.
  4. Serve requests until `GET /callback`. Favicons and anything else
     get a 404. No `code`, or an empty one, means the user declined.
  5. `POST https://openrouter.ai/api/v1/auth/keys` with `{"code",
     "code_verifier", "code_challenge_method": "S256"}`, which returns
     `{"key": "sk-or-…"}`.
  6. Hold the browser's request open until the key is saved, then
     answer with a small page saying what happened. The page runs
     `history.replaceState` to drop the one-time code from the address
     bar.
- **UI copy.**
  - Button: "Connect OpenRouter".
  - While waiting: "Finish connecting in your browser." and then
    "Finishing OpenRouter's setup…", with a spinner and Cancel.
  - On failure: the reason and "Try again".
  - There is no form for pasting a key.
- **Key storage.** `{data_dir}/openrouter/auth.json`, mode 0600,
  written atomically. `OPENROUTER_API_KEY` in the environment wins.
  Imago does not use the Keychain for this key.
  - That suits an ad hoc–signed app. A Keychain item's access list
    names the binary's code hash, which changes with every local build,
    so macOS would ask for permission after each rebuild.
- **Key check.** `GET https://openrouter.ai/api/v1/key`:
  - 200: fine.
  - 401 or 403: "Key no longer works — connect again".
  - `data.usage` and `data.limit` give a credit meter; `is_free_tier`
    marks a free account.
- **Request headers.** `HTTP-Referer` and `X-Title` / `X-OpenRouter-Title`
  (`scripts/release/draft_notes.py`, `edge/src/connections/auto-check.ts`).
- **Lessons Imago recorded.**
  - Thinking added time and found nothing more on small models, so its
    quick checks turn reasoning off.
  - OpenRouter outages happen, so it retries once on infrastructure
    errors.
  - `~vendor/model-latest` ids are OpenRouter's stable aliases.

## 4. OpenRouter's API

Checked 2026-11-04 against openrouter.ai/docs, the OpenAPI spec
(`openrouter.ai/openapi.json`) and live `GET /api/v1/models`. Adding `.md`
to a docs URL returns the page as Markdown. No paid calls were made.
Items marked *unverified* need the phase 0 probe.

**Recent changes.**

- The `web` plugin and the `:online` model suffix are deprecated, in
  favour of the `openrouter:web_search` server tool (beta).
- `X-Title` is now `X-OpenRouter-Title`; the old name still works.
- `transforms: ["middle-out"]` became the `context-compression` plugin.
- Usage is always sent at the end of a stream. `stream_options` is no
  longer needed.
- `GET /credits` now needs a management key.

### 4.1 Chat completions and streaming

- **Request.**
  - `POST https://openrouter.ai/api/v1/chat/completions`.
  - Headers: `Authorization: Bearer`, `Content-Type`, and for
    attribution `HTTP-Referer`, `X-OpenRouter-Title` and optionally
    `X-OpenRouter-Categories`.
  - The Responses API (`/api/v1/responses`) also exists and is now shown
    first in the docs. Chat Completions is fully supported, and its SSE
    is simpler to parse by hand.
- **The stream.**
  - `data: {json}` lines, ending with `data: [DONE]`.
  - Comment lines such as `: OPENROUTER PROCESSING` keep the connection
    alive; skip any line starting with `:`.
  - The generation id is in the `X-Generation-Id` response header.
- **Errors.**
  - **Before the stream:** a real HTTP status with
    `{"error":{"code","message","metadata"}}`.
  - **Mid-stream:** HTTP stays 200. The last chunk has a top-level
    `error` and `finish_reason: "error"`, and it may be the only chunk.
- **Usage.** One extra chunk before `[DONE]` carries `usage`:
  - `prompt_tokens`, `completion_tokens` and
    `completion_tokens_details.reasoning_tokens`;
  - `cost` in dollars;
  - `server_tool_use_details.web_search_requests`.

  It repeats `finish_reason`, so `finish_reason` arrives twice.
- **Stopping.** Closing the connection stops billing for OpenAI,
  Anthropic, xAI, DeepSeek and others. It does not for Google, Bedrock,
  Groq or Mistral.
- **Stickiness.** `session_id` (or `x-session-id`) keeps a chat on one
  provider for prompt-cache hits, for 10 minutes of inactivity.
- **Caching.** Anthropic models also take a top-level
  `cache_control: {type: "ephemeral"}`.

### 4.2 Reasoning

- **Request.** `reasoning: {effort | max_tokens | enabled | exclude}`.
  - `effort` is one of `none`, `minimal`, `low`, `medium`, `high`,
    `xhigh` or `max`.
  - Send `effort` or `max_tokens`, never both.
  - `exclude: true` still bills the reasoning.
- **Per-model options.** Each model's `reasoning` object in `/models`
  lists `supported_efforts`, `default_effort` and `mandatory`. Never send
  `none` to a model where `mandatory` is true.
- **The stream.** `delta.reasoning` is plain text.
  `delta.reasoning_details[]` items carry `type`, `id`, `format` and
  `index`. The types are:
  - `reasoning.text`, with `text` and `signature`;
  - `reasoning.summary`;
  - `reasoning.encrypted`, whose `data` may read `[REDACTED]`;
  - `reasoning.server_tool_call`.
- **Passing it back.** Optional for plain chat, but needed across tool
  calls. Put `reasoning_details` on the assistant message exactly as
  received; the blocks must not be changed or reordered.
- **Out of room.** If reasoning uses the whole `max_tokens`, the answer
  is empty with `finish_reason: "length"`.

### 4.3 Images

- **Format.** `{"type":"image_url","image_url":{"url":"data:image/png;base64,…","detail":"auto"}}`.
  PNG, JPEG, WebP and GIF are accepted.
- **Order.** Put the text before the image, and keep the system message
  text only.
- **Limits.** No size limit is published (*unverified*). Oversized
  requests get a 413.
- **Which models.** Vision models list `"image"` in
  `architecture.input_modalities`, or filter with
  `GET /api/v1/models?input_modalities=image`.

### 4.4 Server tools: web search and fetch

Server tools are run by OpenRouter. The model calls them, and the app
never sends results back. They work on Chat Completions.

- **`openrouter:web_search`.** "Gives any model on OpenRouter access to
  real-time web information": OpenRouter runs the search when the model
  asks for it.
  - **GPT-6 Luna can use it two ways.** OpenAI lists native web search
    for "GPT-5 and later". Luna's OpenAI and Azure endpoints list
    `openrouter:web_search` in their `native_tools`; its Bedrock endpoint
    lists none. With `engine: "exa"`, the search runs on Exa whatever
    endpoint serves Luna.
  - Parameters: `engine` (`auto`, `native`, `exa`, `parallel`,
    `firecrawl` or `perplexity`), `max_results`, `max_uses`,
    `allowed_domains` and `excluded_domains`.
  - Domains take wildcards (`*.fandom.com`) and paths.
  - **Domain filtering by engine:** Exa honours both lists. Native
    OpenAI honours only the allow list. Native Google has no domain
    filter: with `auto` it falls back to Exa, and with `native` it
    returns 400. So the Guide uses `engine: "exa"`. It costs $0.007 a
    search against about $0.01 for OpenAI's native search, and it serves
    every endpoint the same.
  - Exa's `mode`: `instant` (~250 ms), `fast` (~450 ms) or `auto`
    (~1 s), all $0.007. Results come back as extracts from each page,
    about 2–4 k characters, unless `max_characters` or
    `search_context_size` sets the size.
- **`openrouter:web_fetch`.** Reads one page, with `allowed_domains`,
  `blocked_domains`, `max_uses`, `max_content_tokens` and `engine`.
- **Pricing**, on top of tokens:
  - Exa search: $0.007 a request, 10 results included.
  - Native search: the provider's price, about $0.01.
  - Firecrawl fetch: $0.0025 a page. Exa's fetch price is *unverified*.
  - Search is billed even on `:free` models.
- **Seeing what ran.** Calls show up in `reasoning_details` as
  `reasoning.server_tool_call {tool_name, arguments, result}`, and should
  be replayed in later turns.
- **Citations.**
  - The documented shape is
    `message.annotations[{type:"url_citation", url_citation:{url,title,content,start_index,end_index}}]`.
    The Responses schema has a flat version, so parse both.
  - Where annotations sit in a streamed Chat Completions response is
    *unverified*.
- **No "prefer" parameter.** To favour sites without limiting to them,
  name them in the system prompt. To limit to them, use
  `allowed_domains`.

### 4.5 Function calling

- **The stream.** Tool calls arrive as `delta.tool_calls[]`, each with
  `index`, `id`, `type: "function"` and
  `function: {name, arguments}`.
  - Join the `arguments` fragments by `index`.
  - The turn ends with `finish_reason: "tool_calls"`.
- **Sending results.**
  1. Append the assistant message: `content: null`, `tool_calls`, and
     `reasoning_details` unchanged.
  2. Append `{"role":"tool","tool_call_id","content"}` for each call.
  3. Send `tools` again.
- **Server and function tools together.** Both go in the same `tools`
  array.

### 4.6 Models

- **`GET /api/v1/models` is public.** Fields useful to a picker:
  - `id`, `name`, `context_length`;
  - `architecture.input_modalities`;
  - `pricing` (dollar strings per token, plus `web_search` per request);
  - `supported_parameters` (`tools`, `reasoning`);
  - `reasoning`;
  - `expiration_date`.
- **Server-side filters.** `input_modalities`, `supported_parameters`,
  `sort` (e.g. `pricing-low-to-high`) and `q`.
- **Aliases.** `~vendor/family-latest` ids follow a family's newest
  model.
- **Free models.**
  - `openrouter/free` picks a free model with the features a request
    needs.
  - `:free` models allow 20 requests a minute, and 50 a day until the
    account has bought $10 of credits (then 1000).

Candidates on 2026-11-04, live from `/models`. All take images, tools and
1M+ tokens of context. The owner chose GPT-6 Luna.

- **Luna's details.** Reasoning efforts `none` to `max`, default
  `medium`. It accepts `verbosity`. Cached input is $0.01 per million
  tokens. It is moderated.
- **Luna's endpoints.** OpenAI standard, `fast` (2× the price) and
  `flex` (half the price, slower), plus Azure and Amazon Bedrock.
- **Other ids.** `~openai/gpt-luna-latest` points at it today.
  `openai/gpt-5.6-luna` has the same features at $0.20 / $1.20.

| Model | $ per 1M in / out | Reasoning |
|---|---|---|
| `openai/gpt-6-luna` | 0.10 / 0.50 | optional, default medium; native search |
| `anthropic/claude-haiku-5.5` | 0.10 / 0.50 (more above 100k prompt tokens) | optional |
| `google/gemini-3.1-flash-lite` | 0.25 / 1.50 | optional, default minimal |
| `google/gemini-3.8-flash` | 0.825 / 4.125 | mandatory |
| `openrouter/free` | free | depends on the model picked |

A typical Guide question costs well under a cent:

- 2–3 k tokens in, with a DS screen;
- 300–600 tokens out;
- one Exa search.

### 4.7 OAuth PKCE, keys and errors

- **Authorize.**
  `https://openrouter.ai/auth?callback_url=http://localhost:<port>/callback&code_challenge=…&code_challenge_method=S256&state=…&key_label=…`.
  - Any localhost port is allowed.
  - The code is single-use and expires in 10 minutes.
  - Without a `callback_url`, the code is shown on the page for the
    user to paste.
- **Exchange.** `POST /api/v1/auth/keys` with
  `{code, code_verifier, code_challenge_method}` returns
  `{key, user_id}`.
  - 403 means the code is bad or has expired.
  - The key's own page is `https://openrouter.ai/keys/<sha256hex(key)>`,
    where the user can set a credit limit.
- **Key info.** `GET /api/v1/key` returns, under `data`:
  - `label`, `limit`, `limit_remaining`, `limit_reset`;
  - `usage`, `usage_daily`, `usage_weekly`, `usage_monthly`;
  - `is_free_tier`, `free_model_daily_requests`;
  - `budgets`, `expires_at`.

  `/credits` needs a management key, which an OAuth key is not, so the
  account balance is out of reach (*unverified*). The app shows the
  key's usage and limit instead.
- **Errors.** `{"error":{"code","message","metadata"}}`:

  | Code | Meaning |
  |---|---|
  | 400 | Bad parameters |
  | 401 | Bad or revoked key |
  | 402 | Credits. Branch on `metadata.limit_source`: `openrouter_credits`, `openrouter_key_limit` or `openrouter_in_flight_budget` |
  | 403 | Moderation, guardrail or budget |
  | 408 | Timeout |
  | 413 | Payload too large |
  | 429 | Rate limited, with `Retry-After` |
  | 502 | Provider failed |
  | 503 | No provider |

  A spent key limit is documented as 402 in one place and 403 in
  another; `limit_source` tells which.

### 4.8 `curl` for streaming

- `-sS -N` turns off buffering.
- `--data-binary @-` sends the body on stdin. Base64 screens can exceed
  the argument length limit, so the body must not go in argv.
- `-H @file` or `--config file` sends headers without putting the key
  in argv. A 0600 config file beside the key works without passing extra
  file descriptors. That would need `unsafe`, which the workspace denies.
- `-w '%{http_code}'` or `-D` separates a JSON error from a stream.
- Killing the child cancels the request.

Sources:

- https://openrouter.ai/docs/api_reference/streaming
- https://openrouter.ai/docs/guides/best-practices/reasoning-tokens
- https://openrouter.ai/docs/guides/overview/multimodal/image-understanding
- https://openrouter.ai/docs/guides/features/server-tools/web-search
- https://openrouter.ai/docs/guides/features/server-tools
- https://openrouter.ai/docs/guides/features/tool-calling
- https://openrouter.ai/docs/guides/overview/models
- https://openrouter.ai/docs/guides/overview/auth/oauth
- https://openrouter.ai/docs/api_reference/limits
- https://openrouter.ai/docs/api_reference/errors-and-debugging
- https://openrouter.ai/docs/guides/best-practices/prompt-caching
- https://openrouter.ai/docs/app-attribution

## 5. Where good game answers come from

Model memory alone is weak on game specifics: which route has which
encounter, what a version exclusive is, where an item is. The chat
should ground answers in the sites players trust, and say which page
each answer came from.

### 5.1 How the sites were chosen and checked

**Chosen.** For each series that the app's systems (GB to 3DS, plus
RPG Maker fan games) mostly play, the site its community treats as the
reference:

- **An independent wiki over a Fandom copy.** Many communities left
  Fandom, and Fandom keeps the old wiki up. Most Nintendo series have a
  member of NIWA, the Nintendo Independent Wiki Alliance. Indie Wiki
  Buddy's list (§5.3) names the independent wiki for each Fandom one,
  and has about twenty of these series.
- **A data site beside the wiki where one is the standard.** Serebii and
  PokémonDB for Pokémon, Serenes Forest for Fire Emblem.
- **For every game, a walkthrough site:** StrategyWiki, GameFAQs and
  IGN.
- **For fan games and hacks,** where their makers post: the game's own
  wiki, Eevee Expo, Relic Castle and PokéCommunity.

**Checked** on 2026-11-04 with `curl`, as the app would send it (user agent
`ds-emulator/0.1.0 (Guide; …)`). Each MediaWiki site was searched through
`api.php?action=query&list=search`, and each other site's page was
fetched. The verdicts:

- **API.** The wiki answers searches as JSON. The app can read it
  directly, for free (phase 3's own tools).
- **Page.** HTML, with no bot check.
- **Search.** A Cloudflare bot check (HTTP 403, "Just a moment…"), so it
  is reached only through OpenRouter's web search and fetch with Exa
  (§4.4). The app must never try to get past these checks.
  - Whether Exa's index covers these sites well is *unverified* until
    the first live search with a key. Phase 0 checks Bulbapedia and
    GameFAQs first.
- **Down.** No answer at the time.

**Robots and signals.**

- **Crawl delays:**
  - PokémonDB 2 s (30 s for some bots);
  - Mario Wiki 1 s;
  - StrategyWiki and Zelda Wiki 10 s.

  The app's own fetches respect them per site, and pages are cached for
  a day.
- **Content signals.** Relic Castle and Serenes Forest send Cloudflare's
  `Content-Signal: search=yes, ai-train=no, use=reference`.
  - Guide reads pages to answer a question, and cites them. That is
    reference use, never training.
  - No site that was checked forbids it.

### 5.2 The source map

The order within a series is the order of trust the system prompt gives.
The verdicts are those from §5.1.

**Pokémon, main series (GB to 3DS):**

| Site | Domain | Best for | Reach |
|---|---|---|---|
| Bulbapedia | bulbapedia.bulbagarden.net | Everything: walkthroughs (`Appendix:{game} walkthrough`), locations, mechanics, version differences | Search |
| Serebii | serebii.net | Per-game sections: each version's Pokédex and locations, items, trainers, events | Page |
| PokémonDB | pokemondb.net | Pokédex, moves, location lists by game (`/location/…`) | Page (2 s delay) |
| PokeAPI | pokeapi.co/api/v2 | Encounters by version, moves, evolutions, as data | JSON API |
| Smogon | smogon.com/dex | Competitive sets by generation | Page |
| IGN | ign.com/wikis | Walkthroughs for the bigger games | Page |
| Mystery Dungeon Wiki | mysterydungeonwiki.com/api.php | The Mystery Dungeon games | API |

Serebii's per-game paths, all returning 200:

| Games | Path |
|---|---|
| Red, Blue | `/rb/` |
| Yellow | `/yellow/` |
| Gold, Silver | `/gs/` |
| Crystal | `/crystal/` |
| Ruby, Sapphire | `/rubysapphire/` |
| Emerald | `/emerald/` |
| FireRed, LeafGreen | `/fireredleafgreen/` |
| Diamond, Pearl | `/diamondpearl/` |
| Platinum | `/platinum/` |
| HeartGold, SoulSilver | `/heartgoldsoulsilver/` |
| Black, White | `/blackwhite/` |
| Black 2, White 2 | `/black2white2/` |
| X, Y | `/xy/` |
| Omega Ruby, Alpha Sapphire | `/omegarubyalphasapphire/` |
| Sun, Moon | `/sunmoon/` |
| Ultra Sun, Ultra Moon | `/ultrasunultramoon/` |

**Pokémon ROM hacks and fan games.** The game's own wiki comes first.
Wiki locations for the original game do not apply to a randomized copy.

| Site | Domain | Best for | Reach |
|---|---|---|---|
| Infinite Fusion wiki | infinitefusion.fandom.com | Infinite Fusion | API |
| Unbound wiki | pokemonunbound.miraheze.org | Unbound (the independent wiki; a Fandom one also answers) | API |
| Reborn, Insurgence, Uranium wikis | reborn / pokemoninsurgence / pokemonuranium.fandom.com | Those games | API |
| Pokémon Infinity wiki | pokemoninfinity.miraheze.org | Infinity | API |
| Eevee Expo | eeveeexpo.com | Pokémon Essentials fan games: game pages, makers' threads | Page |
| Relic Castle | reliccastle.com | Older Essentials games' pages | Page |
| PokéCommunity | pokecommunity.com | ROM hack and fan game threads: makers' docs and FAQs | Search |
| romhack.ing | romhack.ing | Hack pages and readmes (Romhacking.net is behind a check) | Page |

**Nintendo series:**

| Series | Site (domain) | Reach | Also |
|---|---|---|---|
| Mario, Wario, Yoshi, Donkey Kong, Mario & Luigi, Paper Mario | Super Mario Wiki (mariowiki.com) | API | — |
| Zelda | Zelda Wiki (zeldawiki.wiki) | API | Zelda Dungeon, walkthroughs (Search) |
| Kirby | WiKirby (wikirby.com) | API | — |
| Metroid | Metroid Wiki (metroidwiki.org) | API | Wikitroid (metroid.fandom.com, API) |
| Fire Emblem | Fire Emblem Wiki (fireemblemwiki.org) | API | Serenes Forest: growths, data (Page) |
| Golden Sun | Golden Sun Universe (goldensunwiki.net) | API | — |
| Advance Wars | Wars Wiki (warswiki.org) | API | Wars World News (Page) |
| Animal Crossing | Nookipedia (nookipedia.com) | API | — |
| EarthBound, Mother 3 | WikiBound (wikibound.info) | API | Starmen.net (Search) |
| Pikmin | Pikipedia (pikminwiki.com) | API | — |
| Rhythm Heaven | Rhythm Heaven Wiki (rhwiki.net) | API | — |
| Smash Bros. (3DS) | SmashWiki (ssbwiki.com) | API | — |
| Professor Layton | Layton Wiki (layton.fandom.com) | API | — |
| Ace Attorney | Ace Attorney Wiki (aceattorney.fandom.com) | API | — |

**Other series common on these systems:**

| Series | Site (domain) | Reach |
|---|---|---|
| Final Fantasy, including Tactics Advance | Final Fantasy Wiki (finalfantasy.fandom.com) | API |
| Dragon Quest | Dragon Quest Wiki (dragon-quest.org) | API |
| Kingdom Hearts | Kingdom Hearts Wiki (khwiki.com) | API |
| Castlevania | Castlevania Wiki (castlevania.fandom.com) | API |
| Mega Man, including Battle Network, Zero, ZX and Star Force | Mega Man Knowledge Base (megaman.fandom.com) | API |
| Sonic | Sonic Retro (info.sonicretro.org) | API |
| Harvest Moon, Story of Seasons | FoGu (fogu.com, guides) | Page |
| Chrono Trigger (DS) | Chrono Wiki (chronowiki.org/w/api.php) | API |
| Yu-Gi-Oh! | Yugipedia (yugipedia.com) | API |
| Digimon | Wikimon (wikimon.net) | API |
| Shin Megami Tensei, Devil Survivor | Megami Tensei Wiki (megatenwiki.com) | Search |
| The World Ends with You | TWEWY Wiki (twewy.wiki) | Search |

Left out: Lylat Wiki (Star Fox), which gave no answer.

**Every game, after its series:**

| Site | Domain | Best for | Reach |
|---|---|---|---|
| StrategyWiki | strategywiki.org | Walkthroughs for most games | API (10 s delay) |
| GameFAQs | gamefaqs.gamespot.com | Text guides and FAQs for nearly every game, message boards | Search |
| IGN | ign.com/wikis | Guides for popular games | Page |
| The game's Fandom wiki | {name}.fandom.com | When no independent wiki exists | API |
| The Cutting Room Floor | tcrf.net | Unused content, version differences | Search |
| TASVideos | tasvideos.org | Glitches and their exact inputs | Page |
| Speedrun.com | speedrun.com | Routes and skips | Search |
| HowLongToBeat | howlongtobeat.com | How long a game takes | Page |
| GameHacking.org | gamehacking.org | Cheat codes, for the app's cheat lists | Search |

### 5.3 How the map is used

- **Picking the entry.** The game's identity selects it:
  - by No-Intro title, for example "Pokemon …" or "Legend of Zelda,
    The …";
  - by series;
  - by kind: ROM hack, randomized copy, RPG Maker fan game.
- **Phase 2.** The entry's domains become `web_search` and `web_fetch`'s
  `allowed_domains`, and the system prompt names the sites in order,
  with what each is best for. Every site in the map is reachable this
  way, including the Search ones.
- **Phase 3.** The app's own tools read the API and Page sites
  directly, for free and exactly, by title. The server tools stay for
  the Search sites.
- **Keeping it current.**
  - [Indie Wiki Buddy](https://github.com/KevinPayravi/indie-wiki-buddy)
    (MIT, updated 2026-10-30) publishes `data/data.json`: 863
    independent wikis, each with its Fandom origin, base URL, platform
    and search path. It seeds new series, and tells when a community
    moves.
  - `scripts/guide/check-sources.sh` re-runs §5.1's probe by hand.

## 6. Answer quality

Techniques that matter for this use case:

1. **Say exactly which game it is.**
   - The full No-Intro name and region, the platform, and the revision.
   - "ROM hack of …", "randomized" or "fan game" when it applies.
   - Version differences matter: Omega Ruby is not Alpha Sapphire, and
     a randomized copy has different wild Pokémon.
2. **Show the screen.** The current frames, attached as an image,
   answer "what do I do here" and "who is this" without the player
   typing context. DS screens are 256 × 192. Scaling them 2–3× with
   nearest-neighbour keeps pixel text legible to vision models.
3. **Look it up first, then cite.**
   - Search the source map's sites before answering a factual question.
   - Read the best page.
   - Answer from it, with a "Sources" row under the answer.
   - Say when nothing was found instead of guessing.
4. **Spoilers.** Answer only what was asked. Offer a hint first when the
   question is "how do I get past …". Never reveal later story beats
   unprompted.
5. **Shape for a narrow column.**
   - Lead with the answer in a sentence or two.
   - Use short lists.
   - Keep tables to three or four columns.
   - No headings for short answers.
   - Use the game's own names for places, items and moves.

## Sources

- Imago, `~/Desktop/Imago` at its working tree on 2026-11-04 (paths above).
- [Indie Wiki Buddy data](https://github.com/KevinPayravi/indie-wiki-buddy/blob/main/data/data.json), MIT.
- Reachability: `curl` probes from this machine on 2026-11-04 (§5.1).
- GPUI 0.2.2 sources in `~/.cargo/registry`.
