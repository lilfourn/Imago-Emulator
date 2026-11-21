//! Application setup: actions, menus, shortcuts and the main window.

use std::path::PathBuf;

use emu_core::DmgColors;
use gpui::{
    Action, App, Application, Bounds, KeyBinding, Menu, MenuItem, Pixels, SystemMenuType,
    TitlebarOptions, WindowBounds, WindowOptions, actions, point, prelude::*, px, size,
};

use crate::about_window;
use crate::bindings::Shortcut;
use crate::chrome;
use crate::fonts;
use crate::icons;
use crate::layout::{self, Arrangement};
use crate::picture::{self, Picture, Resolution3d};
use crate::settings::{self, Settings, WindowPlace};
use crate::speed::{self, Speed, Speeds};
use crate::view::EmulatorView;
use crate::window_chrome;

actions!(
    ds_emulator,
    [
        About,
        OpenSettings,
        OpenSupporter,
        HideApp,
        HideOtherApps,
        ShowAllApps,
        ToggleWindowControls,
        OpenRom,
        OpenGameFolder,
        AddToLibrary,
        AddGameFolders,
        OpenStore,
        GameInfo,
        RemoveFromLibrary,
        CloseGame,
        ConnectLinkCable,
        InsertGbaCartridge,
        EditCheats,
        Randomize,
        RunScript,
        StopScript,
        SaveScreenshot,
        ToggleRecording,
        SwitchPlayer,
        TogglePause,
        Reset,
        SaveState,
        LoadState,
        NextStateSlot,
        PreviousStateSlot,
        ToggleFastForward,
        ToggleSlowMotion,
        ToggleFps,
        SwapScreens,
        RotateClockwise,
        RotateAnticlockwise,
        NextScreenGap,
        ToggleColorCorrection,
        ShowDebugger,
        DebugStep,
        DebugStepOver,
        DebugNextFrame,
        ToggleFullScreen,
        ToggleMicrophone,
        ToggleGuide,
        ScreenshotForGuide,
        CloseWindow,
        Minimize,
        Zoom,
        Quit,
        ShowAppMenu,
    ]
);

// The actions that choose one of several: each carries what it chooses,
// so a menu lists its choices from the list the choices are kept in, and
// one handler in the window serves them all. A menu item's shortcut and
// its tick are found by the value it carries. None is built from JSON:
// the app's bindings are made here, in code.

/// Lays the screens out in this arrangement.
#[derive(Debug, Clone, PartialEq, Action)]
#[action(namespace = ds_emulator, no_json)]
pub struct Arrange(pub Arrangement);

/// Runs fast-forward at this speed from now on.
#[derive(Debug, Clone, PartialEq, Action)]
#[action(namespace = ds_emulator, no_json)]
pub struct SetFastForward(pub Speed);

/// Draws DS and 3DS 3D at this resolution.
#[derive(Debug, Clone, PartialEq, Action)]
#[action(namespace = ds_emulator, no_json)]
pub struct SetResolution3d(pub Resolution3d);

/// Shows the screens as this picture.
#[derive(Debug, Clone, PartialEq, Action)]
#[action(namespace = ds_emulator, no_json)]
pub struct SetPicture(pub Picture);

/// Shows games made for the original Game Boy in these colours, from the
/// next one opened.
#[derive(Debug, Clone, PartialEq, Action)]
#[action(namespace = ds_emulator, no_json)]
pub struct SetDmgColors(pub DmgColors);

/// The screen arrangements, in the View menu's order: each with its
/// item's title there and its shortcut. The menu's items and the key
/// bindings are both made from this.
const ARRANGEMENTS: [(Arrangement, &str, &str); 4] = [
    (Arrangement::Vertical, "Screens Stacked", "secondary-1"),
    (
        Arrangement::Horizontal,
        "Screens Side by Side",
        "secondary-2",
    ),
    (Arrangement::Single, "One Screen", "secondary-3"),
    (Arrangement::Hybrid, "One Large, Both Small", "secondary-4"),
];

/// How games made for the original Game Boy can be shown, as View >
/// Original Game Boy Games offers them. The button combinations a Game Boy
/// Color takes are the settings file's alone.
const DMG_COLORS: [(DmgColors, &str); 2] = [
    (DmgColors::Grey, "In Grey, as on a Game Boy"),
    (DmgColors::ByTitle, "In Colour, as on a Game Boy Color"),
];

/// The action a gamepad hotkey runs: the one its menu item and its
/// shortcut run.
pub fn hotkey_action(shortcut: Shortcut) -> Box<dyn Action> {
    match shortcut {
        Shortcut::SaveState => Box::new(SaveState),
        Shortcut::LoadState => Box::new(LoadState),
        Shortcut::NextSlot => Box::new(NextStateSlot),
        Shortcut::PreviousSlot => Box::new(PreviousStateSlot),
        Shortcut::Pause => Box::new(TogglePause),
        Shortcut::Reset => Box::new(Reset),
        Shortcut::ToggleFastForward => Box::new(ToggleFastForward),
        Shortcut::ToggleSlowMotion => Box::new(ToggleSlowMotion),
        Shortcut::SwapScreens => Box::new(SwapScreens),
        Shortcut::SwitchPlayer => Box::new(SwitchPlayer),
        Shortcut::FullScreen => Box::new(ToggleFullScreen),
    }
}

/// Two DS screens stacked, in emulated pixels: the shape the window opens
/// in.
const STACKED_DS: (f32, f32) = (256.0, 384.0);

/// How much of the display's free area the window opens to fill.
const DISPLAY_SHARE: f32 = 0.9;

/// Room on the display for the menu bar and the Dock, in logical pixels.
/// GPUI reports only the whole display, not the part they leave free, so
/// this is a generous guess.
const SYSTEM_BARS: f32 = 100.0;

/// The app's icon, made from `assets/app-icon.svg` with
/// `resvg -w 1024 -h 1024 assets/app-icon.svg assets/app-icon.png`.
pub const APP_ICON: &[u8] = include_bytes!("../../../assets/app-icon.png");

/// The bar across the top of the window that the close, minimise and
/// zoom buttons sit in, in logical pixels. The window draws under it, so
/// the game shows through it.
pub const TOP_BAR: f32 = 40.0;

/// The smallest the main window can be, in logical pixels.
const MIN_SIZE: (f32, f32) = (256.0, 192.0);

/// Where the close button's top-left corner sits in [`TOP_BAR`], centring
/// the buttons in it.
const TRAFFIC_LIGHTS: (f32, f32) = (16.0, 13.0);

/// Runs the app until the user quits, opening `roms` at launch: one game,
/// a DS game with a GBA game in its GBA slot, or two linked Game Boy or
/// Game Boy Advance games. `script`, a Lua script, runs alongside the
/// first game to start.
pub fn run(roms: Vec<PathBuf>, script: Option<PathBuf>) {
    Application::new()
        .with_assets(icons::Icons)
        .run(move |cx: &mut App| {
            fonts::register(cx);
            cx.on_action(|_: &Quit, cx| cx.quit());
            // AppKit's About panel on macOS; the app's own window elsewhere.
            if cfg!(target_os = "macos") {
                cx.on_action(|_: &About, _| window_chrome::show_about_panel());
            } else {
                cx.on_action(|_: &About, cx| about_window::show(cx));
            }
            // Only macOS hides apps: elsewhere GPUI logs these, or panics
            // on Windows, so they are neither bound nor in the menus.
            if cfg!(target_os = "macos") {
                cx.on_action(|_: &HideApp, cx| cx.hide());
                cx.on_action(|_: &HideOtherApps, cx| cx.hide_other_apps());
                cx.on_action(|_: &ShowAllApps, cx| cx.unhide_other_apps());
            }
            // Window actions act on whichever window is in front.
            cx.on_action(|_: &CloseWindow, cx| {
                if let Some(window) = cx.active_window() {
                    window
                        .update(cx, |_, window, _| window.remove_window())
                        .ok();
                }
            });
            cx.on_action(|_: &Minimize, cx| {
                if let Some(window) = cx.active_window() {
                    window
                        .update(cx, |_, window, _| window.minimize_window())
                        .ok();
                }
            });
            cx.on_action(|_: &Zoom, cx| {
                if let Some(window) = cx.active_window() {
                    window.update(cx, |_, window, _| window.zoom_window()).ok();
                }
            });
            cx.bind_keys(bindings());
            cx.bind_keys(crate::guide::composer::bindings());
            // Before the window, which draws in the supporter's colours.
            crate::supporter::load(cx);
            cx.set_menus(menus(MenuChecks::default()));
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    quit_after_close(cx);
                }
            })
            .detach();

            let display = cx.primary_display().map(|display| {
                let size = display.bounds().size;
                (f32::from(size.width), f32::from(size.height))
            });
            let (width, height) = initial_size(display);
            let fresh = Bounds::centered(None, size(px(width), px(height)), cx);
            // Where the window was left, if that display is still there.
            let left = settings::default_path()
                .and_then(|path| Settings::load(&path).ok())
                .and_then(|settings| settings.window_place);
            let display = left.and_then(|place| match place.display {
                Some(uuid) => cx
                    .displays()
                    .into_iter()
                    .find(|display| display.uuid().is_ok_and(|id| id.as_u128() == uuid)),
                None => cx.primary_display(),
            });
            let window_bounds = opening_bounds(
                left,
                display.as_ref().map(|display| display.bounds()),
                fresh,
            );
            let display_id = display
                .filter(|_| window_bounds != WindowBounds::Windowed(fresh))
                .map(|display| display.id());
            let options = WindowOptions {
                window_bounds: Some(window_bounds),
                display_id,
                titlebar: Some(top_bar("Imago Emulator")),
                window_min_size: Some(size(px(MIN_SIZE.0), px(MIN_SIZE.1))),
                app_id: Some("ds-emulator".to_owned()),
                window_decorations: chrome::window_decorations(),
                ..WindowOptions::default()
            };
            let main = cx
                .open_window(options, |window, cx| {
                    cx.new(|cx| EmulatorView::new(roms, script, window, cx))
                })
                .expect("opening the main window");
            // The Dock icon, which is a supporter's to choose.
            main.update(cx, |view, _, cx| view.apply_look(cx)).ok();
            cx.activate(true);
        });
}

/// Quits once the last window has closed. On macOS at once. Elsewhere the
/// window closes from inside the system's event handling, which in GPUI
/// 0.2.2 still holds its X11 client's state (`client.rs`, `ClientMessage`),
/// so quitting there panics ("RefCell already borrowed"); and deferring
/// within the update would still run inside it. So off macOS the quit
/// waits for the event loop's next turn.
fn quit_after_close(cx: &mut App) {
    if cfg!(target_os = "macos") {
        cx.quit();
    } else {
        cx.spawn(async |cx| cx.update(|cx| cx.quit()).ok()).detach();
    }
}

/// The shortcuts the app's words name, in GPUI's syntax (see
/// [`crate::keys::shortcut`]): one binding for every system where it can
/// be, `secondary` being ⌘ on macOS and Control elsewhere. Where a
/// Control chord is the system's own off macOS, the app takes another.
pub mod shortcut {
    /// Opens a game.
    pub const OPEN_ROM: &str = "secondary-o";
    /// Adds games to the library.
    pub const ADD_TO_LIBRARY: &str = "secondary-shift-a";
    /// Takes the chosen game out of the library: ⌘⌫ as Move to Trash in
    /// the Finder, Delete as in File Explorer and Linux's file managers.
    pub const REMOVE_FROM_LIBRARY: &str = if cfg!(target_os = "macos") {
        "secondary-backspace"
    } else {
        "delete"
    };
    /// Puts back the game last taken out of the library.
    pub const PUT_BACK: &str = "secondary-z";
    /// Closes the game.
    pub const CLOSE_GAME: &str = "secondary-w";
    /// Pauses or resumes.
    pub const PAUSE: &str = "secondary-p";
    /// Fast-forward and slow motion, on or off.
    pub const FAST_FORWARD: &str = "secondary-f";
    pub const SLOW_MOTION: &str = "secondary-shift-f";
    /// Opens or closes Guide, and screenshots the game for it.
    pub const GUIDE: &str = "secondary-j";
    pub const SCREENSHOT_FOR_GUIDE: &str = "secondary-alt-s";
    /// Stops a running script: ⌥⌘L on macOS. Control-Alt-L locks the
    /// screen on KDE, Xfce and Cinnamon.
    pub const STOP_SCRIPT: &str = if cfg!(target_os = "macos") {
        "secondary-alt-l"
    } else {
        "secondary-shift-k"
    };
    /// Turns the screens: ⌥⌘→ and ⌥⌘← on macOS. Control-Alt-arrows
    /// switch workspaces on GNOME, Xfce and Cinnamon, and turn the display
    /// with some Intel drivers on Windows.
    pub const ROTATE_CLOCKWISE: &str = if cfg!(target_os = "macos") {
        "secondary-alt-right"
    } else {
        "secondary-shift-right"
    };
    pub const ROTATE_ANTICLOCKWISE: &str = if cfg!(target_os = "macos") {
        "secondary-alt-left"
    } else {
        "secondary-shift-left"
    };
    /// The debugger's steps.
    pub const DEBUG_STEP: &str = "secondary-shift-i";
    pub const DEBUG_STEP_OVER: &str = "secondary-shift-o";
    pub const DEBUG_NEXT_FRAME: &str = "secondary-shift-n";
    /// Pastes a randomizer's settings.
    pub const PASTE: &str = "secondary-v";
    /// Full screen: ⌃⌘F on macOS, F11 elsewhere.
    pub const FULL_SCREEN: &str = if cfg!(target_os = "macos") {
        "ctrl-cmd-f"
    } else {
        "f11"
    };
    /// Opens the menu in the window, where the system draws no menu bar.
    pub const APP_MENU: &str = "f10";
}

/// Every shortcut of the app's, as the menus show them. Game keys never
/// take a Control or ⌘ chord, so these never reach the game.
pub fn bindings() -> Vec<KeyBinding> {
    let mut bindings = vec![
        KeyBinding::new("secondary-,", OpenSettings, None),
        KeyBinding::new(shortcut::OPEN_ROM, OpenRom, None),
        KeyBinding::new(shortcut::ADD_TO_LIBRARY, AddToLibrary, None),
        KeyBinding::new("secondary-shift-g", OpenStore, None),
        KeyBinding::new("secondary-alt-r", Randomize, None),
        // The chosen game's menu, as Get Info in the Finder, and
        // taking it out of the library, as Move to Trash there.
        KeyBinding::new("secondary-i", GameInfo, Some("Emulator")),
        KeyBinding::new(
            shortcut::REMOVE_FROM_LIBRARY,
            RemoveFromLibrary,
            Some("Emulator"),
        ),
        KeyBinding::new(shortcut::CLOSE_GAME, CloseGame, None),
        // In the settings and debugger windows ⌘W closes the window,
        // as in any Mac app; only the game's own window closes games.
        KeyBinding::new("secondary-w", CloseWindow, Some("Settings")),
        KeyBinding::new("secondary-w", CloseWindow, Some("Debugger")),
        KeyBinding::new("secondary-m", Minimize, None),
        KeyBinding::new("secondary-t", SwitchPlayer, None),
        KeyBinding::new(shortcut::PAUSE, TogglePause, None),
        KeyBinding::new("secondary-r", Reset, None),
        KeyBinding::new("secondary-s", SaveState, None),
        KeyBinding::new("secondary-l", LoadState, None),
        KeyBinding::new("secondary-shift-s", SaveScreenshot, None),
        // Guide, the chat beside the screens, and a screenshot for it.
        KeyBinding::new(shortcut::GUIDE, ToggleGuide, None),
        KeyBinding::new(shortcut::SCREENSHOT_FOR_GUIDE, ScreenshotForGuide, None),
        KeyBinding::new("secondary-shift-r", ToggleRecording, None),
        KeyBinding::new("secondary-shift-l", RunScript, None),
        KeyBinding::new(shortcut::STOP_SCRIPT, StopScript, None),
        KeyBinding::new("secondary-]", NextStateSlot, None),
        KeyBinding::new("secondary-[", PreviousStateSlot, None),
        KeyBinding::new(shortcut::FAST_FORWARD, ToggleFastForward, None),
        KeyBinding::new(shortcut::SLOW_MOTION, ToggleSlowMotion, None),
        KeyBinding::new("secondary-alt-f", ToggleFps, None),
    ];
    bindings.extend(
        ARRANGEMENTS
            .iter()
            .map(|&(arrangement, _, keys)| KeyBinding::new(keys, Arrange(arrangement), None)),
    );
    bindings.extend([
        KeyBinding::new("secondary-e", SwapScreens, None),
        KeyBinding::new(shortcut::ROTATE_CLOCKWISE, RotateClockwise, None),
        KeyBinding::new(shortcut::ROTATE_ANTICLOCKWISE, RotateAnticlockwise, None),
        KeyBinding::new("secondary-g", NextScreenGap, None),
        KeyBinding::new("secondary-shift-c", ToggleColorCorrection, None),
        KeyBinding::new("secondary-shift-d", ShowDebugger, None),
        KeyBinding::new(shortcut::DEBUG_STEP, DebugStep, None),
        KeyBinding::new(shortcut::DEBUG_STEP_OVER, DebugStepOver, None),
        KeyBinding::new(shortcut::DEBUG_NEXT_FRAME, DebugNextFrame, None),
        // On macOS AppKit puts View > Enter Full Screen in the menu
        // itself, on Globe-F; this adds the classic shortcut. Elsewhere
        // F11, as in every Linux and Windows app.
        KeyBinding::new(shortcut::FULL_SCREEN, ToggleFullScreen, None),
        KeyBinding::new("secondary-q", Quit, None),
    ]);
    if cfg!(target_os = "macos") {
        bindings.extend([
            KeyBinding::new("cmd-h", HideApp, None),
            KeyBinding::new("cmd-alt-h", HideOtherApps, None),
        ]);
    } else {
        bindings.push(KeyBinding::new(shortcut::APP_MENU, ShowAppMenu, None));
    }
    bindings
}

/// A title bar drawn by the window itself as its [`TOP_BAR`], with the
/// close, minimise and zoom buttons centred in it and `title` hidden.
pub fn top_bar(title: &'static str) -> TitlebarOptions {
    TitlebarOptions {
        title: Some(title.into()),
        appears_transparent: true,
        traffic_light_position: Some(point(px(TRAFFIC_LIGHTS.0), px(TRAFFIC_LIGHTS.1))),
    }
}

/// Where to open the main window: at `left`, where it was left, if its
/// top bar is still on the `display` it was on, to drag it by, or else at
/// `fresh`. Both are measured across that display.
fn opening_bounds(
    left: Option<WindowPlace>,
    display: Option<Bounds<Pixels>>,
    fresh: Bounds<Pixels>,
) -> WindowBounds {
    let Some(place) = left else {
        return WindowBounds::Windowed(fresh);
    };
    let bounds = Bounds::new(
        point(px(place.x), px(place.y)),
        size(
            px(place.width.max(MIN_SIZE.0)),
            px(place.height.max(MIN_SIZE.1)),
        ),
    );
    let bar = point(px(place.x + place.width / 2.0), px(place.y + TOP_BAR / 2.0));
    if !display.is_some_and(|display| display.contains(&bar)) {
        return WindowBounds::Windowed(fresh);
    }
    if place.fullscreen {
        WindowBounds::Fullscreen(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    }
}

/// The window's size at launch, in logical pixels: stacked DS screens as
/// large as fit most of a `display` of that size, or at 2x if there is
/// none. The top bar lies over the screens, so takes no room.
fn initial_size(display: Option<(f32, f32)>) -> (f32, f32) {
    let Some((width, height)) = display else {
        return (STACKED_DS.0 * 2.0, STACKED_DS.1 * 2.0);
    };
    let free = (width, height - SYSTEM_BARS);
    let area = (free.0 * DISPLAY_SHARE, free.1 * DISPLAY_SHARE);
    let (width, height) = layout::fit(STACKED_DS, area);
    (width.floor(), height.floor())
}

/// What the menus show checked. GPUI menus have no check marks of their
/// own, so the view rebuilds them when one of these changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuChecks {
    /// The speed fast-forward runs at.
    pub fast_forward: Speed,
    /// Whether the speed readout shows.
    pub show_fps: bool,
}

impl Default for MenuChecks {
    fn default() -> Self {
        Self {
            fast_forward: Speeds::default().fast_forward,
            show_fps: false,
        }
    }
}

/// The menu item that opens Settings at the supporter's key: the one
/// place outside Settings where the app says it can be supported
/// ([decision 0017](../../../docs/decisions/0017-supporter-keys.md)).
const SUPPORT: &str = "Support Imago Emulator…";

/// The menu bar, with `checks` checked: macOS's, and off macOS the menu
/// the window draws (`app_menu.rs`) from the same list, laid out as Linux
/// and Windows apps lay theirs out.
pub fn menus(checks: MenuChecks) -> Vec<Menu> {
    let fps = if checks.show_fps {
        "Hide FPS"
    } else {
        "Show FPS"
    };
    let mac = cfg!(target_os = "macos");
    let mut menus = Vec::new();
    if mac {
        menus.push(Menu {
            name: "Imago Emulator".into(),
            items: vec![
                MenuItem::action("About Imago Emulator", About),
                MenuItem::separator(),
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::action(SUPPORT, OpenSupporter),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Hide Imago Emulator", HideApp),
                MenuItem::action("Hide Others", HideOtherApps),
                MenuItem::action("Show All", ShowAllApps),
                MenuItem::separator(),
                MenuItem::action("Quit Imago Emulator", Quit),
            ],
        });
    }
    // An RPG Maker game is a folder, and no file dialog off macOS takes
    // files and folders at once, so folders get their own items there.
    let mut file = vec![MenuItem::action("Open ROM…", OpenRom)];
    if !mac {
        file.push(MenuItem::action("Open Game Folder…", OpenGameFolder));
    }
    file.push(MenuItem::action("Add Games to Library…", AddToLibrary));
    if !mac {
        file.push(MenuItem::action(
            "Add Game Folders to Library…",
            AddGameFolders,
        ));
    }
    file.extend([
        MenuItem::action("Game Store", OpenStore),
        MenuItem::action("Game Info", GameInfo),
        MenuItem::action("Remove from Library", RemoveFromLibrary),
        MenuItem::action("Close Game", CloseGame),
        MenuItem::action("Connect Second Console…", ConnectLinkCable),
        MenuItem::action("Insert GBA Cartridge…", InsertGbaCartridge),
        MenuItem::separator(),
        MenuItem::action("Randomize Game…", Randomize),
        MenuItem::separator(),
        MenuItem::action("Save Screenshot", SaveScreenshot),
        MenuItem::action("Screenshot for Guide", ScreenshotForGuide),
        MenuItem::action("Start / Stop Recording", ToggleRecording),
        MenuItem::separator(),
        MenuItem::action("Edit Cheats…", EditCheats),
        MenuItem::separator(),
        MenuItem::action("Run Script…", RunScript),
        MenuItem::action("Stop Script", StopScript),
    ]);
    if !mac {
        // Windows names it Exit, as its apps do.
        let quit = if cfg!(windows) { "Exit" } else { "Quit" };
        file.extend([
            MenuItem::separator(),
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::action(quit, Quit),
        ]);
    }
    menus.push(Menu {
        name: "File".into(),
        items: file,
    });
    menus.push(Menu {
        name: "Emulation".into(),
        items: vec![
            MenuItem::action("Pause / Resume", TogglePause),
            MenuItem::action("Reset", Reset),
            MenuItem::action("Fast-Forward", ToggleFastForward),
            MenuItem::action("Slow Motion", ToggleSlowMotion),
            MenuItem::submenu(fast_forward_menu(checks.fast_forward)),
            MenuItem::action("Switch Player", SwitchPlayer),
            MenuItem::action("Use Microphone", ToggleMicrophone),
            MenuItem::separator(),
            MenuItem::action("Step Instruction", DebugStep),
            MenuItem::action("Step Over", DebugStepOver),
            MenuItem::action("Next Frame", DebugNextFrame),
            MenuItem::separator(),
            MenuItem::action("Save State", SaveState),
            MenuItem::action("Load State", LoadState),
            MenuItem::action("Next State Slot", NextStateSlot),
            MenuItem::action("Previous State Slot", PreviousStateSlot),
        ],
    });
    let mut view: Vec<MenuItem> = ARRANGEMENTS
        .iter()
        .map(|&(arrangement, title, _)| MenuItem::action(title, Arrange(arrangement)))
        .collect();
    view.extend([
        MenuItem::separator(),
        MenuItem::action("Swap Screens", SwapScreens),
        MenuItem::action("Rotate Clockwise", RotateClockwise),
        MenuItem::action("Rotate Anticlockwise", RotateAnticlockwise),
        MenuItem::action("Change Screen Gap", NextScreenGap),
        MenuItem::separator(),
        MenuItem::submenu(Menu {
            name: "3D Resolution".into(),
            items: picture::RESOLUTIONS_3D
                .iter()
                .map(|&resolution| {
                    MenuItem::action(resolution.label(), SetResolution3d(resolution))
                })
                .collect(),
        }),
        MenuItem::submenu(Menu {
            name: "Picture".into(),
            items: Picture::ALL
                .iter()
                .map(|&picture| MenuItem::action(picture.label(), SetPicture(picture)))
                .collect(),
        }),
        MenuItem::action("Colour Correction", ToggleColorCorrection),
        MenuItem::action(fps, ToggleFps),
        MenuItem::submenu(Menu {
            name: "Original Game Boy Games".into(),
            items: DMG_COLORS
                .iter()
                .map(|&(colors, title)| MenuItem::action(title, SetDmgColors(colors)))
                .collect(),
        }),
        MenuItem::separator(),
        MenuItem::action("Guide", ToggleGuide),
        MenuItem::action("Show / Hide Window Controls", ToggleWindowControls),
        MenuItem::action("Debugger", ShowDebugger),
    ]);
    if !mac {
        // AppKit adds Enter Full Screen to macOS's View menu itself.
        view.push(MenuItem::separator());
        view.push(MenuItem::action("Full Screen", ToggleFullScreen));
    }
    menus.push(Menu {
        name: "View".into(),
        items: view,
    });
    menus.push(Menu {
        name: "Window".into(),
        items: vec![
            MenuItem::action("Minimize", Minimize),
            // Maximise elsewhere, which zooming is.
            MenuItem::action(if mac { "Zoom" } else { "Maximize" }, Zoom),
        ],
    });
    if !mac {
        menus.push(Menu {
            name: "Help".into(),
            items: vec![
                MenuItem::action("About Imago Emulator", About),
                MenuItem::action(SUPPORT, OpenSupporter),
            ],
        });
    }
    menus
}

/// Emulation > Fast-Forward Speed, with `current` checked.
fn fast_forward_menu(current: Speed) -> Menu {
    Menu {
        name: "Fast-Forward Speed".into(),
        items: speed::FAST_FORWARD_CHOICES
            .iter()
            .map(|&speed| {
                let name = checked(&speed.label(), speed == current);
                MenuItem::action(name, SetFastForward(speed))
            })
            .collect(),
    }
}

/// `label` with a check mark before it if `on`, or an em space, about as
/// wide, to keep the labels lined up.
fn checked(label: &str, on: bool) -> String {
    if on {
        format!("✓ {label}")
    } else {
        format!("\u{2003}{label}")
    }
}

#[cfg(test)]
mod tests {
    use gpui::Action as _;

    use super::*;

    fn bounds(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
    }

    #[test]
    fn the_window_opens_where_it_was_left() {
        let display = Some(bounds(0.0, 0.0, 1512.0, 982.0));
        let fresh = bounds(500.0, 100.0, 529.0, 793.0);
        let place = WindowPlace {
            x: 40.0,
            y: 60.0,
            width: 600.0,
            height: 900.0,
            fullscreen: false,
            display: None,
        };
        assert_eq!(
            opening_bounds(Some(place), display, fresh),
            WindowBounds::Windowed(bounds(40.0, 60.0, 600.0, 900.0))
        );
        let full = WindowPlace {
            fullscreen: true,
            ..place
        };
        assert_eq!(
            opening_bounds(Some(full), display, fresh),
            WindowBounds::Fullscreen(bounds(40.0, 60.0, 600.0, 900.0))
        );
        assert_eq!(
            opening_bounds(None, display, fresh),
            WindowBounds::Windowed(fresh)
        );
    }

    #[test]
    fn a_window_off_its_display_or_on_one_now_gone_opens_afresh() {
        let display = Some(bounds(0.0, 0.0, 1512.0, 982.0));
        let fresh = bounds(500.0, 100.0, 529.0, 793.0);
        let place = WindowPlace {
            x: 40.0,
            y: 60.0,
            width: 600.0,
            height: 900.0,
            fullscreen: false,
            display: Some(7),
        };
        let off = WindowPlace { x: 2000.0, ..place };
        assert_eq!(
            opening_bounds(Some(off), display, fresh),
            WindowBounds::Windowed(fresh)
        );
        assert_eq!(
            opening_bounds(Some(place), None, fresh),
            WindowBounds::Windowed(fresh)
        );
    }

    #[test]
    fn window_opens_large_on_a_laptop_display() {
        // A 14-inch MacBook Pro: 793 px is 2.07x, leaving room for the
        // menu bar and Dock.
        assert_eq!(initial_size(Some((1512.0, 982.0))), (529.0, 793.0));
        assert_eq!(initial_size(Some((2560.0, 1440.0))), (804.0, 1206.0));
    }

    #[test]
    fn window_opens_at_2x_without_a_display() {
        assert_eq!(initial_size(None), (512.0, 768.0));
    }

    #[test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "checks Linux's and Windows's shortcuts (by design)"
    )]
    fn off_macos_no_shortcut_takes_the_logo_key_or_a_chord_the_system_keeps() {
        let bindings = bindings();
        let names: Vec<&str> = bindings.iter().map(|b| b.action().name()).collect();
        assert!(
            !names
                .iter()
                .any(|name| name.contains("Hide") || name.contains("ShowAll")),
            "only macOS hides apps; Windows panics on Hide Others"
        );
        for binding in &bindings {
            for keystroke in binding.keystrokes() {
                let keystroke = keystroke.inner();
                let modifiers = keystroke.modifiers;
                assert!(!modifiers.platform, "{keystroke}: the Windows or Super key");
                let arrow = matches!(keystroke.key.as_str(), "left" | "right" | "up" | "down");
                assert!(
                    !(modifiers.control && modifiers.alt && (arrow || keystroke.key == "l")),
                    "{keystroke}: switches workspaces, turns the display or locks the screen"
                );
            }
        }
        assert!(names.contains(&ShowAppMenu.name()), "F10 opens the menu");
    }

    #[test]
    fn every_shortcut_the_app_names_is_bound() {
        let bound = bindings();
        for shortcut in [
            shortcut::OPEN_ROM,
            shortcut::ADD_TO_LIBRARY,
            shortcut::REMOVE_FROM_LIBRARY,
            shortcut::CLOSE_GAME,
            shortcut::PAUSE,
            shortcut::FAST_FORWARD,
            shortcut::SLOW_MOTION,
            shortcut::GUIDE,
            shortcut::SCREENSHOT_FOR_GUIDE,
            shortcut::STOP_SCRIPT,
            shortcut::ROTATE_CLOCKWISE,
            shortcut::ROTATE_ANTICLOCKWISE,
            shortcut::DEBUG_STEP,
            shortcut::DEBUG_STEP_OVER,
            shortcut::DEBUG_NEXT_FRAME,
            shortcut::FULL_SCREEN,
        ] {
            let written = gpui::Keystroke::parse(shortcut)
                .expect("a keystroke")
                .unparse();
            let found = bound.iter().any(|binding| {
                binding
                    .keystrokes()
                    .iter()
                    .any(|keys| keys.inner().unparse() == written)
            });
            assert!(found, "{shortcut} is bound");
        }
    }

    /// The keys of `binding`, as GPUI writes them, with its context if it
    /// has one.
    fn keys_of(binding: &KeyBinding) -> String {
        let keys: Vec<String> = binding
            .keystrokes()
            .iter()
            .map(|keystroke| keystroke.inner().unparse())
            .collect();
        match binding.predicate() {
            Some(context) => format!("{} in {context}", keys.join(" ")),
            None => keys.join(" "),
        }
    }

    /// Every menu item with an action, by its path through the menus
    /// (`View > Picture > Smooth`), with the action.
    fn items_of(menus: &[Menu]) -> Vec<(String, Box<dyn gpui::Action>)> {
        fn walk(path: &str, items: &[MenuItem], out: &mut Vec<(String, Box<dyn gpui::Action>)>) {
            for item in items {
                match item {
                    MenuItem::Action { name, action, .. } => {
                        out.push((format!("{path} > {name}"), action.boxed_clone()));
                    }
                    MenuItem::Submenu(menu) => {
                        walk(&format!("{path} > {}", menu.name), &menu.items, out);
                    }
                    MenuItem::Separator | MenuItem::SystemMenu(_) => {}
                }
            }
        }
        let mut out = Vec::new();
        for menu in menus {
            walk(&menu.name, &menu.items, &mut out);
        }
        out
    }

    /// The menus as text: each menu's name, then its items in order, one
    /// to a line and indented under what they are in. A separator is `-`,
    /// a submenu ends in ` >`, a choice is `✓ name` if checked and `·name`
    /// if not, and an item is followed by the keys of every binding that
    /// runs its very action, in the order bound: those the system's menu
    /// bar and the window's menu pick its shortcut from.
    fn menus_written(menus: &[Menu], bindings: &[KeyBinding]) -> String {
        fn walk(depth: usize, items: &[MenuItem], bindings: &[KeyBinding], out: &mut String) {
            let indent = "  ".repeat(depth);
            for item in items {
                match item {
                    MenuItem::Separator => out.push_str(&format!("{indent}-\n")),
                    MenuItem::SystemMenu(menu) => {
                        out.push_str(&format!("{indent}{} (system)\n", menu.name));
                    }
                    MenuItem::Submenu(menu) => {
                        out.push_str(&format!("{indent}{} >\n", menu.name));
                        walk(depth + 1, &menu.items, bindings, out);
                    }
                    MenuItem::Action { name, action, .. } => {
                        // A choice not checked starts with an em space,
                        // which nobody can see in a listing.
                        let name = name.replace('\u{2003}', "·");
                        let keys: Vec<String> = bindings
                            .iter()
                            .filter(|binding| binding.action().partial_eq(action.as_ref()))
                            .map(keys_of)
                            .collect();
                        if keys.is_empty() {
                            out.push_str(&format!("{indent}{name}\n"));
                        } else {
                            out.push_str(&format!("{indent}{name}  [{}]\n", keys.join(", ")));
                        }
                    }
                }
            }
        }
        let mut out = String::new();
        for menu in menus {
            out.push_str(&format!("{}\n", menu.name));
            walk(1, &menu.items, bindings, &mut out);
        }
        out
    }

    /// The bindings as text, in the order bound, which decides between
    /// two on one key: each one's keys, then the menu item it runs, by its
    /// path, or the name of its action if no menu has it.
    fn bindings_written(bindings: &[KeyBinding], menus: &[Menu]) -> String {
        let items = items_of(menus);
        let mut out = String::new();
        for binding in bindings {
            let runs = items
                .iter()
                .find(|(_, action)| action.partial_eq(binding.action()))
                .map_or_else(
                    || format!("({})", binding.action().name()),
                    |(path, _)| path.clone(),
                );
            out.push_str(&format!("{}  ->  {runs}\n", keys_of(binding)));
        }
        out
    }

    /// macOS's menu bar with nothing changed from the defaults, as
    /// [`menus_written`] writes it: recorded from the build before the
    /// menus' choices became actions that carry their value.
    const MAC_MENUS: &str = r#"Imago Emulator
  About Imago Emulator
  -
  Settings…  [cmd-,]
  Support Imago Emulator…
  -
  Services (system)
  -
  Hide Imago Emulator  [cmd-h]
  Hide Others  [alt-cmd-h]
  Show All
  -
  Quit Imago Emulator  [cmd-q]
File
  Open ROM…  [cmd-o]
  Add Games to Library…  [cmd-shift-a]
  Game Store  [cmd-shift-g]
  Game Info  [cmd-i in Emulator]
  Remove from Library  [cmd-backspace in Emulator]
  Close Game  [cmd-w]
  Connect Second Console…
  Insert GBA Cartridge…
  -
  Randomize Game…  [alt-cmd-r]
  -
  Save Screenshot  [cmd-shift-s]
  Screenshot for Guide  [alt-cmd-s]
  Start / Stop Recording  [cmd-shift-r]
  -
  Edit Cheats…
  -
  Run Script…  [cmd-shift-l]
  Stop Script  [alt-cmd-l]
Emulation
  Pause / Resume  [cmd-p]
  Reset  [cmd-r]
  Fast-Forward  [cmd-f]
  Slow Motion  [cmd-shift-f]
  Fast-Forward Speed >
    ✓ 2×
    ·3×
    ·4×
    ·Unlimited
  Switch Player  [cmd-t]
  Use Microphone
  -
  Step Instruction  [cmd-shift-i]
  Step Over  [cmd-shift-o]
  Next Frame  [cmd-shift-n]
  -
  Save State  [cmd-s]
  Load State  [cmd-l]
  Next State Slot  [cmd-]]
  Previous State Slot  [cmd-[]
View
  Screens Stacked  [cmd-1]
  Screens Side by Side  [cmd-2]
  One Screen  [cmd-3]
  One Large, Both Small  [cmd-4]
  -
  Swap Screens  [cmd-e]
  Rotate Clockwise  [alt-cmd-right]
  Rotate Anticlockwise  [alt-cmd-left]
  Change Screen Gap  [cmd-g]
  -
  3D Resolution >
    Match Display
    Native (1×)
    2×
    3×
    4×
    6×
    8×
  Picture >
    Auto
    Sharp Pixels
    Smooth
    Rounded Pixels
    LCD Grid
    Scanlines
  Colour Correction  [cmd-shift-c]
  Show FPS  [alt-cmd-f]
  Original Game Boy Games >
    In Grey, as on a Game Boy
    In Colour, as on a Game Boy Color
  -
  Guide  [cmd-j]
  Show / Hide Window Controls
  Debugger  [cmd-shift-d]
Window
  Minimize  [cmd-m]
  Zoom
"#;

    /// macOS's bindings, as [`bindings_written`] writes them, from the
    /// same build.
    const MAC_BINDINGS: &str = r#"cmd-,  ->  Imago Emulator > Settings…
cmd-o  ->  File > Open ROM…
cmd-shift-a  ->  File > Add Games to Library…
cmd-shift-g  ->  File > Game Store
alt-cmd-r  ->  File > Randomize Game…
cmd-i in Emulator  ->  File > Game Info
cmd-backspace in Emulator  ->  File > Remove from Library
cmd-w  ->  File > Close Game
cmd-w in Settings  ->  (ds_emulator::CloseWindow)
cmd-w in Debugger  ->  (ds_emulator::CloseWindow)
cmd-m  ->  Window > Minimize
cmd-t  ->  Emulation > Switch Player
cmd-p  ->  Emulation > Pause / Resume
cmd-r  ->  Emulation > Reset
cmd-s  ->  Emulation > Save State
cmd-l  ->  Emulation > Load State
cmd-shift-s  ->  File > Save Screenshot
cmd-j  ->  View > Guide
alt-cmd-s  ->  File > Screenshot for Guide
cmd-shift-r  ->  File > Start / Stop Recording
cmd-shift-l  ->  File > Run Script…
alt-cmd-l  ->  File > Stop Script
cmd-]  ->  Emulation > Next State Slot
cmd-[  ->  Emulation > Previous State Slot
cmd-f  ->  Emulation > Fast-Forward
cmd-shift-f  ->  Emulation > Slow Motion
alt-cmd-f  ->  View > Show FPS
cmd-1  ->  View > Screens Stacked
cmd-2  ->  View > Screens Side by Side
cmd-3  ->  View > One Screen
cmd-4  ->  View > One Large, Both Small
cmd-e  ->  View > Swap Screens
alt-cmd-right  ->  View > Rotate Clockwise
alt-cmd-left  ->  View > Rotate Anticlockwise
cmd-g  ->  View > Change Screen Gap
cmd-shift-c  ->  View > Colour Correction
cmd-shift-d  ->  View > Debugger
cmd-shift-i  ->  Emulation > Step Instruction
cmd-shift-o  ->  Emulation > Step Over
cmd-shift-n  ->  Emulation > Next Frame
ctrl-cmd-f  ->  (ds_emulator::ToggleFullScreen)
cmd-q  ->  Imago Emulator > Quit Imago Emulator
cmd-h  ->  Imago Emulator > Hide Imago Emulator
alt-cmd-h  ->  Imago Emulator > Hide Others
"#;

    /// The window's menu on Linux and Windows, from the same build, with
    /// `{quit}` where Windows says Exit and Linux Quit.
    const OTHER_MENUS: &str = r#"File
  Open ROM…  [ctrl-o]
  Open Game Folder…
  Add Games to Library…  [ctrl-shift-a]
  Add Game Folders to Library…
  Game Store  [ctrl-shift-g]
  Game Info  [ctrl-i in Emulator]
  Remove from Library  [delete in Emulator]
  Close Game  [ctrl-w]
  Connect Second Console…
  Insert GBA Cartridge…
  -
  Randomize Game…  [ctrl-alt-r]
  -
  Save Screenshot  [ctrl-shift-s]
  Screenshot for Guide  [ctrl-alt-s]
  Start / Stop Recording  [ctrl-shift-r]
  -
  Edit Cheats…
  -
  Run Script…  [ctrl-shift-l]
  Stop Script  [ctrl-shift-k]
  -
  Settings…  [ctrl-,]
  -
  {quit}  [ctrl-q]
Emulation
  Pause / Resume  [ctrl-p]
  Reset  [ctrl-r]
  Fast-Forward  [ctrl-f]
  Slow Motion  [ctrl-shift-f]
  Fast-Forward Speed >
    ✓ 2×
    ·3×
    ·4×
    ·Unlimited
  Switch Player  [ctrl-t]
  Use Microphone
  -
  Step Instruction  [ctrl-shift-i]
  Step Over  [ctrl-shift-o]
  Next Frame  [ctrl-shift-n]
  -
  Save State  [ctrl-s]
  Load State  [ctrl-l]
  Next State Slot  [ctrl-]]
  Previous State Slot  [ctrl-[]
View
  Screens Stacked  [ctrl-1]
  Screens Side by Side  [ctrl-2]
  One Screen  [ctrl-3]
  One Large, Both Small  [ctrl-4]
  -
  Swap Screens  [ctrl-e]
  Rotate Clockwise  [ctrl-shift-right]
  Rotate Anticlockwise  [ctrl-shift-left]
  Change Screen Gap  [ctrl-g]
  -
  3D Resolution >
    Match Display
    Native (1×)
    2×
    3×
    4×
    6×
    8×
  Picture >
    Auto
    Sharp Pixels
    Smooth
    Rounded Pixels
    LCD Grid
    Scanlines
  Colour Correction  [ctrl-shift-c]
  Show FPS  [ctrl-alt-f]
  Original Game Boy Games >
    In Grey, as on a Game Boy
    In Colour, as on a Game Boy Color
  -
  Guide  [ctrl-j]
  Show / Hide Window Controls
  Debugger  [ctrl-shift-d]
  -
  Full Screen  [f11]
Window
  Minimize  [ctrl-m]
  Maximize
Help
  About Imago Emulator
  Support Imago Emulator…
"#;

    /// Linux's and Windows's bindings, from the same build.
    const OTHER_BINDINGS: &str = r#"ctrl-,  ->  File > Settings…
ctrl-o  ->  File > Open ROM…
ctrl-shift-a  ->  File > Add Games to Library…
ctrl-shift-g  ->  File > Game Store
ctrl-alt-r  ->  File > Randomize Game…
ctrl-i in Emulator  ->  File > Game Info
delete in Emulator  ->  File > Remove from Library
ctrl-w  ->  File > Close Game
ctrl-w in Settings  ->  (ds_emulator::CloseWindow)
ctrl-w in Debugger  ->  (ds_emulator::CloseWindow)
ctrl-m  ->  Window > Minimize
ctrl-t  ->  Emulation > Switch Player
ctrl-p  ->  Emulation > Pause / Resume
ctrl-r  ->  Emulation > Reset
ctrl-s  ->  Emulation > Save State
ctrl-l  ->  Emulation > Load State
ctrl-shift-s  ->  File > Save Screenshot
ctrl-j  ->  View > Guide
ctrl-alt-s  ->  File > Screenshot for Guide
ctrl-shift-r  ->  File > Start / Stop Recording
ctrl-shift-l  ->  File > Run Script…
ctrl-shift-k  ->  File > Stop Script
ctrl-]  ->  Emulation > Next State Slot
ctrl-[  ->  Emulation > Previous State Slot
ctrl-f  ->  Emulation > Fast-Forward
ctrl-shift-f  ->  Emulation > Slow Motion
ctrl-alt-f  ->  View > Show FPS
ctrl-1  ->  View > Screens Stacked
ctrl-2  ->  View > Screens Side by Side
ctrl-3  ->  View > One Screen
ctrl-4  ->  View > One Large, Both Small
ctrl-e  ->  View > Swap Screens
ctrl-shift-right  ->  View > Rotate Clockwise
ctrl-shift-left  ->  View > Rotate Anticlockwise
ctrl-g  ->  View > Change Screen Gap
ctrl-shift-c  ->  View > Colour Correction
ctrl-shift-d  ->  View > Debugger
ctrl-shift-i  ->  Emulation > Step Instruction
ctrl-shift-o  ->  Emulation > Step Over
ctrl-shift-n  ->  Emulation > Next Frame
f11  ->  View > Full Screen
ctrl-q  ->  File > {quit}
f10  ->  (ds_emulator::ShowAppMenu)
"#;

    /// What [`OTHER_MENUS`] and [`OTHER_BINDINGS`] say on this system.
    fn off_macos(written: &str) -> String {
        written.replace("{quit}", if cfg!(windows) { "Exit" } else { "Quit" })
    }

    #[test]
    fn every_menu_item_its_title_its_place_and_its_shortcuts_are_what_they_were() {
        let written = menus_written(&menus(MenuChecks::default()), &bindings());
        if cfg!(target_os = "macos") {
            assert_eq!(written, MAC_MENUS);
        } else {
            assert_eq!(written, off_macos(OTHER_MENUS));
        }
    }

    #[test]
    fn every_binding_runs_what_it_ran_in_the_order_it_was_bound() {
        let written = bindings_written(&bindings(), &menus(MenuChecks::default()));
        if cfg!(target_os = "macos") {
            assert_eq!(written, MAC_BINDINGS);
        } else {
            assert_eq!(written, off_macos(OTHER_BINDINGS));
        }
    }

    #[test]
    fn each_choice_in_the_menus_carries_what_its_item_always_chose() {
        let items = items_of(&menus(MenuChecks::default()));
        let carries = |path: &str, chosen: &dyn Action| {
            let (_, action) = items
                .iter()
                .find(|(at, _)| at == path)
                .unwrap_or_else(|| panic!("no menu item {path}"));
            assert!(action.partial_eq(chosen), "{path}");
        };
        // As each item's own action, and its handler, had it before the
        // actions carried their choice.
        carries("View > Screens Stacked", &Arrange(Arrangement::Vertical));
        carries(
            "View > Screens Side by Side",
            &Arrange(Arrangement::Horizontal),
        );
        carries("View > One Screen", &Arrange(Arrangement::Single));
        carries(
            "View > One Large, Both Small",
            &Arrange(Arrangement::Hybrid),
        );
        let speed = |times| Speed::Times(speed::Rate::new(times).expect("a speed"));
        let speeds = "Emulation > Fast-Forward Speed";
        carries(&format!("{speeds} > ✓ 2×"), &SetFastForward(speed(2.0)));
        carries(
            &format!("{speeds} > \u{2003}3×"),
            &SetFastForward(speed(3.0)),
        );
        carries(
            &format!("{speeds} > \u{2003}4×"),
            &SetFastForward(speed(4.0)),
        );
        carries(
            &format!("{speeds} > \u{2003}Unlimited"),
            &SetFastForward(Speed::Unlimited),
        );
        let resolutions = "View > 3D Resolution";
        carries(
            &format!("{resolutions} > Match Display"),
            &SetResolution3d(Resolution3d::MatchDisplay),
        );
        carries(
            &format!("{resolutions} > Native (1×)"),
            &SetResolution3d(Resolution3d::Times(1)),
        );
        for times in [2, 3, 4, 6, 8] {
            carries(
                &format!("{resolutions} > {times}×"),
                &SetResolution3d(Resolution3d::Times(times)),
            );
        }
        for (title, picture) in [
            ("Auto", Picture::Auto),
            ("Sharp Pixels", Picture::SharpPixels),
            ("Smooth", Picture::Smooth),
            ("Rounded Pixels", Picture::RoundedPixels),
            ("LCD Grid", Picture::LcdGrid),
            ("Scanlines", Picture::Scanlines),
        ] {
            carries(&format!("View > Picture > {title}"), &SetPicture(picture));
        }
        let game_boy = "View > Original Game Boy Games";
        carries(
            &format!("{game_boy} > In Grey, as on a Game Boy"),
            &SetDmgColors(DmgColors::Grey),
        );
        carries(
            &format!("{game_boy} > In Colour, as on a Game Boy Color"),
            &SetDmgColors(DmgColors::ByTitle),
        );
        // Each is its own choice: no two items carry the same.
        for (at, (path, action)) in items.iter().enumerate() {
            for (other, same) in &items[at + 1..] {
                assert!(!action.partial_eq(same.as_ref()), "{path} and {other}");
            }
        }
    }

    #[test]
    fn every_gamepad_hotkey_runs_what_a_shortcut_and_a_menu_item_run() {
        let bound = bindings();
        let items = items_of(&menus(MenuChecks::default()));
        for (shortcut, name) in crate::bindings::SHORTCUTS {
            let action = hotkey_action(shortcut);
            assert!(
                bound
                    .iter()
                    .any(|binding| binding.action().partial_eq(action.as_ref())),
                "{name} has a shortcut"
            );
            // AppKit puts Enter Full Screen in macOS's View menu itself.
            let apples_own = cfg!(target_os = "macos") && shortcut == Shortcut::FullScreen;
            assert!(
                apples_own
                    || items
                        .iter()
                        .any(|(_, item)| item.partial_eq(action.as_ref())),
                "{name} has a menu item"
            );
        }
    }

    #[test]
    fn the_menus_checks_follow_what_is_chosen_and_change_nothing_else() {
        let default = menus_written(&menus(MenuChecks::default()), &bindings());
        let other = menus_written(
            &menus(MenuChecks {
                fast_forward: speed::FAST_FORWARD_CHOICES[3],
                show_fps: true,
            }),
            &bindings(),
        );
        let changed: Vec<(&str, &str)> = default
            .lines()
            .zip(other.lines())
            .filter(|(was, is)| was != is)
            .collect();
        let fps = if cfg!(target_os = "macos") {
            "alt-cmd-f"
        } else {
            "ctrl-alt-f"
        };
        assert_eq!(
            changed,
            [
                ("    ✓ 2×", "    ·2×"),
                ("    ·Unlimited", "    ✓ Unlimited"),
                (
                    format!("  Show FPS  [{fps}]").as_str(),
                    format!("  Hide FPS  [{fps}]").as_str()
                ),
            ]
        );
        assert_eq!(default.lines().count(), other.lines().count());
    }
}
