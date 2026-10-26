//! The app's colours, Graphite & Signal: near-black graphite surfaces,
//! white type and one warm signal-orange accent, so the colour on screen
//! comes from the games. Each is `0xRRGGBB`, for `gpui::rgb`.

/// Behind everything: the window, around the screens.
pub const BACKGROUND: u32 = 0x000E_0F11;
/// Raised surfaces: tiles, fields, segmented controls.
pub const PANEL: u32 = 0x0017_191C;
/// Hairlines and outlines.
pub const LINE: u32 = 0x0026_292E;
/// Text.
pub const TEXT: u32 = 0x00F3_F4F6;
/// Secondary text: hints, captions, placeholders.
pub const MUTED: u32 = 0x008A_9099;
/// The one accent: what is chosen, what to press.
pub const ACCENT: u32 = 0x00FF_6A3D;
/// Text and marks on the accent.
pub const ON_ACCENT: u32 = 0x00FF_FFFF;
/// `colour` with `alpha`, for `gpui::rgba`.
pub const fn with_alpha(colour: u32, alpha: u8) -> u32 {
    colour << 8 | alpha as u32
}

/// Room between the window's sides and what is in it, in logical pixels:
/// as far in as the close button.
pub const INSET: f32 = 16.0;
/// Behind a control under the pointer.
pub const HOVER: u32 = with_alpha(TEXT, 0x14);
