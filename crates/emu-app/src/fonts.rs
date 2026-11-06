//! The app's typefaces on each system. GPUI's `.SystemUIFont` is the
//! system's own on macOS (SF) and Windows (Segoe UI), and IBM Plex Sans on
//! Linux, where no one face is everywhere: the app brings Plex Sans and
//! Plex Mono for Linux (`assets/fonts`, SIL Open Font License 1.1). The
//! monospaced face lines up the debugger's columns and Guide's code.

/// The interface's typeface: the system's on every OS.
pub const UI: &str = ".SystemUIFont";

/// The monospaced typeface: Menlo on macOS, Consolas on Windows, the
/// bundled IBM Plex Mono on Linux.
pub const MONO: &str = if cfg!(target_os = "macos") {
    "Menlo"
} else if cfg!(windows) {
    "Consolas"
} else {
    "IBM Plex Mono"
};

/// The faces the app brings on Linux: GPUI draws `.SystemUIFont` there in
/// IBM Plex Sans, and [`MONO`] is Plex Mono.
#[cfg(target_os = "linux")]
const BUNDLED: [&[u8]; 7] = [
    include_bytes!("../../../assets/fonts/IBMPlexSans-Regular.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexSans-Italic.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexSans-Medium.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexSans-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexSans-Bold.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexMono-Regular.ttf"),
    include_bytes!("../../../assets/fonts/IBMPlexMono-SemiBold.ttf"),
];

/// Gives GPUI the faces the app brings for this system, if any, before
/// any window draws text.
pub fn register(cx: &gpui::App) {
    #[cfg(target_os = "linux")]
    {
        let faces = BUNDLED
            .iter()
            .map(|bytes| std::borrow::Cow::Borrowed(*bytes));
        if let Err(error) = cx.text_system().add_fonts(faces.collect()) {
            eprintln!("ds-emulator: the bundled fonts did not load: {error:#}");
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = cx;
}
