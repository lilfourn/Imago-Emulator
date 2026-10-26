# Design system

The app's look is Graphite & Signal (`crates/emu-app/src/theme.rs`):
graphite surfaces, white type and one signal-orange accent, so the colour
on screen comes from the games. `scripts/macos/check-layout.sh` checks the
rules below in every screen state; a new screen or control adds its checks
to `scripts/macos/layout.swift`.

## Rules

- **One inset.** Content sits `theme::INSET` (16pt) in from the window's
  sides, as far as the close button. Nothing but the screens comes closer.
- **The top bar mirrors the traffic lights.** It is 40pt tall. Its title
  and icons sit on the traffic lights' midline (20pt down). Its last
  icon's artwork ends as far from the right side as the close button starts
  from the left.
- **Icons are Solar (Linear)** (`crates/emu-app/src/icons.rs`): 24-unit
  artwork with 1.5-unit strokes, drawn at 16pt in 28pt buttons, 14pt in
  fields and 20pt on the game menu's tiles. Never a font glyph standing in
  for an icon. An icon is a `currentColor` SVG in `assets/icons`, credited in
  `assets/NOTICE.md`.
- **Padding is even.** What is inside a button, tile, cap, pill or field is
  centred in it: icons to within half a pixel, text to within a pixel
  (GPUI places text by its advance, not its ink). Text is centred down by its
  capitals, not its line, so descenders don't count.
- **A leading icon is inset as far from the side as from the top and bottom.**
- **Wrapped, centred text breaks its own lines** (`centred_lines` in
  `library_view.rs`). GPUI centres a line it wraps with the space it broke
  at, half a space off centre.
- **Text never runs out of its box.** A title too long for two lines ends in
  an ellipsis.
- **Nested corners share a centre.** An inner radius is the outer one less
  the gap between them.
- **Colour carries meaning.** The accent marks what is chosen or what to
  press; red marks only the action that destroys something.
- **A form is centred as one block.** Settings' names end on one line and
  their choices start on the next, and the two together are centred in the
  window.

## The checks

`scripts/macos/check-layout.sh` builds the release app and runs it behind
the frontmost app, with its own home folder holding a seeded library. It
drives the app through its menus and with keys posted to it alone, so it
never takes over the screen. Each state is captured at the default and the
minimum window size: the library, empty and searched; a running game with
the key guide and speed readout; the game menu and its Cheats and Restart
pages; and the settings window. Captures and reports are left in
`/tmp/ds-emulator-layout`; look at them as well, since the checks catch
geometry, not taste.

GPUI 0.2 gives the accessibility tree no elements, so the app reports its
own: with `DS_EMULATOR_PROBE` naming a folder, each window writes the frames
of its named parts there (`crates/emu-app/src/probe.rs`). A part's name
says what it is (`button:add`, `tile:Resume`, `field:search`), which picks
the rules the checks apply. Without the variable, the probe does nothing.
`layout.swift` reads those frames, the window's own buttons from the
accessibility tree and the pixels of a capture.
