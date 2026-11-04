//! The app's colours, taken from Imago Light so the two apps read as one
//! family: white paper, ink type, cool grey hairlines, Imago's blue for
//! what to press, and the four colour blocks of the app icon (blue, coral,
//! amber, green) for the systems' plates. Each is `0xRRGGBB`, for
//! `gpui::rgb`.

/// Behind everything: the window, the library, the store and settings.
pub const BACKGROUND: u32 = 0x00FF_FFFF;
/// Raised surfaces: tiles, fields, segmented controls.
pub const PANEL: u32 = 0x00F4_F4F5;
/// Hairlines and outlines.
pub const LINE: u32 = 0x00E4_E4E7;
/// Text.
pub const TEXT: u32 = 0x0023_2327;
/// Secondary text: hints, captions, placeholders.
pub const MUTED: u32 = 0x006E_6E78;
/// The one accent: what to press, such as Play, and the caret. Imago's
/// blue, darkened as Imago Light darkens it so white type on it reads.
/// What is chosen is ink instead (see `ui.rs`).
pub const ACCENT: u32 = 0x0000_70B5;
/// Text and marks on the accent.
pub const ON_ACCENT: u32 = 0x00FF_FFFF;
/// The only red: the action that destroys something, such as taking a
/// game out of the library.
pub const DANGER: u32 = 0x00C8_3314;
/// Around the screens while a game runs: Imago Dark's ink, so the games'
/// own colours and the ambient glow show as they would on a dark screen.
pub const STAGE: u32 = 0x000B_0B0D;

/// `colour` with `alpha`, for `gpui::rgba`.
pub const fn with_alpha(colour: u32, alpha: u8) -> u32 {
    colour << 8 | alpha as u32
}

/// Room between the window's sides and what is in it, in logical pixels:
/// as far in as the close button.
pub const INSET: f32 = 16.0;
/// Behind a control under the pointer.
pub const HOVER: u32 = with_alpha(TEXT, 0x0F);
/// Behind the chosen game, in the library and the store.
pub const CHOSEN: u32 = with_alpha(TEXT, 0x14);

/// The plate of a game without a picture, from the top left to the
/// bottom right, in its system's colour block: a Game Boy's green, a Game
/// Boy Color's amber, a Game Boy Advance's blue, a DS's coral, and the
/// deeper coral and amber for the 3DS and RPG Maker games. Each starts a
/// little darker than its block and ends darker still, enough for white
/// type.
pub fn tint(platform: crate::rom_info::Platform) -> (u32, u32) {
    match platform.tag() {
        "gb" => (0x004F_AE4C, 0x002A_6B24),
        "gbc" => (0x00E0_8506, 0x008F_5100),
        "gba" => (0x001F_9FE8, 0x0000_5D9A),
        "nds" => (0x00EC_4A2E, 0x00A8_290F),
        "3ds" => (0x00C8_3314, 0x007A_1D09),
        "rpgmaker" => (0x00A4_5E00, 0x005C_3400),
        _ => (MUTED, TEXT),
    }
}
