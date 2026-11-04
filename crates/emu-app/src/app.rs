//! Application setup: actions, menus, shortcuts and the main window.

use std::path::PathBuf;

use gpui::{
    App, Application, Bounds, KeyBinding, Menu, MenuItem, Pixels, SystemMenuType, TitlebarOptions,
    WindowBounds, WindowOptions, actions, point, prelude::*, px, size,
};

use crate::icons;
use crate::layout;
use crate::settings::{self, Settings, WindowPlace};
use crate::speed::{self, Speed, Speeds};
use crate::view::EmulatorView;
use crate::window_chrome;

actions!(
    ds_emulator,
    [
        About,
        OpenSettings,
        HideApp,
        HideOtherApps,
        ShowAllApps,
        ToggleWindowControls,
        OpenRom,
        AddToLibrary,
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
        FastForward2x,
        FastForward3x,
        FastForward4x,
        FastForwardUnlimited,
        ToggleFps,
        LayoutVertical,
        LayoutHorizontal,
        LayoutSingle,
        LayoutHybrid,
        SwapScreens,
        RotateClockwise,
        RotateAnticlockwise,
        NextScreenGap,
        ToggleColorCorrection,
        DmgColorsGrey,
        DmgColorsGameBoyColor,
        PictureAuto,
        PictureSharpPixels,
        PictureRoundedPixels,
        PictureSmooth,
        PictureLcdGrid,
        PictureScanlines,
        ShowDebugger,
        DebugStep,
        DebugStepOver,
        DebugNextFrame,
        ToggleFullScreen,
        Resolution3dMatchDisplay,
        Resolution3d1x,
        Resolution3d2x,
        Resolution3d3x,
        Resolution3d4x,
        Resolution3d6x,
        Resolution3d8x,
        ToggleMicrophone,
        CloseWindow,
        Minimize,
        Zoom,
        Quit,
    ]
);

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
const APP_ICON: &[u8] = include_bytes!("../../../assets/app-icon.png");

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
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.on_action(|_: &About, _| window_chrome::show_about_panel());
            cx.on_action(|_: &HideApp, cx| cx.hide());
            cx.on_action(|_: &HideOtherApps, cx| cx.hide_other_apps());
            cx.on_action(|_: &ShowAllApps, cx| cx.unhide_other_apps());
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
            cx.bind_keys([
                KeyBinding::new("cmd-,", OpenSettings, None),
                KeyBinding::new("cmd-h", HideApp, None),
                KeyBinding::new("cmd-alt-h", HideOtherApps, None),
                KeyBinding::new("cmd-o", OpenRom, None),
                KeyBinding::new("cmd-shift-a", AddToLibrary, None),
                KeyBinding::new("cmd-shift-g", OpenStore, None),
                KeyBinding::new("cmd-alt-r", Randomize, None),
                // The chosen game's menu, as Get Info in the Finder, and
                // taking it out of the library, as Move to Trash there.
                KeyBinding::new("cmd-i", GameInfo, Some("Emulator")),
                KeyBinding::new("cmd-backspace", RemoveFromLibrary, Some("Emulator")),
                KeyBinding::new("cmd-w", CloseGame, None),
                // In the settings and debugger windows ⌘W closes the window,
                // as in any Mac app; only the game's own window closes games.
                KeyBinding::new("cmd-w", CloseWindow, Some("Settings")),
                KeyBinding::new("cmd-w", CloseWindow, Some("Debugger")),
                KeyBinding::new("cmd-m", Minimize, None),
                KeyBinding::new("cmd-t", SwitchPlayer, None),
                KeyBinding::new("cmd-p", TogglePause, None),
                KeyBinding::new("cmd-r", Reset, None),
                KeyBinding::new("cmd-s", SaveState, None),
                KeyBinding::new("cmd-l", LoadState, None),
                KeyBinding::new("cmd-shift-s", SaveScreenshot, None),
                KeyBinding::new("cmd-shift-r", ToggleRecording, None),
                KeyBinding::new("cmd-shift-l", RunScript, None),
                KeyBinding::new("cmd-alt-l", StopScript, None),
                KeyBinding::new("cmd-]", NextStateSlot, None),
                KeyBinding::new("cmd-[", PreviousStateSlot, None),
                KeyBinding::new("cmd-f", ToggleFastForward, None),
                KeyBinding::new("cmd-shift-f", ToggleSlowMotion, None),
                KeyBinding::new("cmd-alt-f", ToggleFps, None),
                KeyBinding::new("cmd-1", LayoutVertical, None),
                KeyBinding::new("cmd-2", LayoutHorizontal, None),
                KeyBinding::new("cmd-3", LayoutSingle, None),
                KeyBinding::new("cmd-4", LayoutHybrid, None),
                KeyBinding::new("cmd-e", SwapScreens, None),
                KeyBinding::new("cmd-alt-right", RotateClockwise, None),
                KeyBinding::new("cmd-alt-left", RotateAnticlockwise, None),
                KeyBinding::new("cmd-g", NextScreenGap, None),
                KeyBinding::new("cmd-shift-c", ToggleColorCorrection, None),
                KeyBinding::new("cmd-shift-d", ShowDebugger, None),
                KeyBinding::new("cmd-shift-i", DebugStep, None),
                KeyBinding::new("cmd-shift-o", DebugStepOver, None),
                KeyBinding::new("cmd-shift-n", DebugNextFrame, None),
                // AppKit puts View > Enter Full Screen in the menu itself, on
                // Globe-F; this adds the classic shortcut.
                KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
                KeyBinding::new("cmd-q", Quit, None),
            ]);
            cx.set_menus(menus(MenuChecks::default()));
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
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
                titlebar: Some(top_bar("ds-emulator")),
                window_min_size: Some(size(px(MIN_SIZE.0), px(MIN_SIZE.1))),
                app_id: Some("ds-emulator".to_owned()),
                ..WindowOptions::default()
            };
            cx.open_window(options, |window, cx| {
                cx.new(|cx| EmulatorView::new(roms, script, window, cx))
            })
            .expect("opening the main window");
            window_chrome::set_dock_icon(APP_ICON);
            cx.activate(true);
        });
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

/// The menu bar, with `checks` checked.
pub fn menus(checks: MenuChecks) -> Vec<Menu> {
    let fps = if checks.show_fps {
        "Hide FPS"
    } else {
        "Show FPS"
    };
    vec![
        Menu {
            name: "ds-emulator".into(),
            items: vec![
                MenuItem::action("About ds-emulator", About),
                MenuItem::separator(),
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Hide ds-emulator", HideApp),
                MenuItem::action("Hide Others", HideOtherApps),
                MenuItem::action("Show All", ShowAllApps),
                MenuItem::separator(),
                MenuItem::action("Quit ds-emulator", Quit),
            ],
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("Open ROM…", OpenRom),
                MenuItem::action("Add Games to Library…", AddToLibrary),
                MenuItem::action("Game Store", OpenStore),
                MenuItem::action("Game Info", GameInfo),
                MenuItem::action("Remove from Library", RemoveFromLibrary),
                MenuItem::action("Close Game", CloseGame),
                MenuItem::action("Connect Second Console…", ConnectLinkCable),
                MenuItem::action("Insert GBA Cartridge…", InsertGbaCartridge),
                MenuItem::separator(),
                MenuItem::action("Randomize Pokémon Game…", Randomize),
                MenuItem::separator(),
                MenuItem::action("Save Screenshot", SaveScreenshot),
                MenuItem::action("Start / Stop Recording", ToggleRecording),
                MenuItem::separator(),
                MenuItem::action("Edit Cheats…", EditCheats),
                MenuItem::separator(),
                MenuItem::action("Run Script…", RunScript),
                MenuItem::action("Stop Script", StopScript),
            ],
        },
        Menu {
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
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Screens Stacked", LayoutVertical),
                MenuItem::action("Screens Side by Side", LayoutHorizontal),
                MenuItem::action("One Screen", LayoutSingle),
                MenuItem::action("One Large, Both Small", LayoutHybrid),
                MenuItem::separator(),
                MenuItem::action("Swap Screens", SwapScreens),
                MenuItem::action("Rotate Clockwise", RotateClockwise),
                MenuItem::action("Rotate Anticlockwise", RotateAnticlockwise),
                MenuItem::action("Change Screen Gap", NextScreenGap),
                MenuItem::separator(),
                MenuItem::submenu(Menu {
                    name: "3D Resolution".into(),
                    items: vec![
                        MenuItem::action("Match Display", Resolution3dMatchDisplay),
                        MenuItem::action("Native (1×)", Resolution3d1x),
                        MenuItem::action("2×", Resolution3d2x),
                        MenuItem::action("3×", Resolution3d3x),
                        MenuItem::action("4×", Resolution3d4x),
                        MenuItem::action("6×", Resolution3d6x),
                        MenuItem::action("8×", Resolution3d8x),
                    ],
                }),
                MenuItem::submenu(Menu {
                    name: "Picture".into(),
                    items: vec![
                        MenuItem::action("Auto", PictureAuto),
                        MenuItem::action("Sharp Pixels", PictureSharpPixels),
                        MenuItem::action("Smooth", PictureSmooth),
                        MenuItem::action("Rounded Pixels", PictureRoundedPixels),
                        MenuItem::action("LCD Grid", PictureLcdGrid),
                        MenuItem::action("Scanlines", PictureScanlines),
                    ],
                }),
                MenuItem::action("Colour Correction", ToggleColorCorrection),
                MenuItem::action(fps, ToggleFps),
                MenuItem::submenu(Menu {
                    name: "Original Game Boy Games".into(),
                    items: vec![
                        MenuItem::action("In Grey, as on a Game Boy", DmgColorsGrey),
                        MenuItem::action(
                            "In Colour, as on a Game Boy Color",
                            DmgColorsGameBoyColor,
                        ),
                    ],
                }),
                MenuItem::separator(),
                MenuItem::action("Show / Hide Window Controls", ToggleWindowControls),
                MenuItem::action("Debugger", ShowDebugger),
            ],
        },
        Menu {
            name: "Window".into(),
            items: vec![
                MenuItem::action("Minimize", Minimize),
                MenuItem::action("Zoom", Zoom),
            ],
        },
    ]
}

/// Emulation > Fast-Forward Speed, with `current` checked.
fn fast_forward_menu(current: Speed) -> Menu {
    let [twice, three_times, four_times, unlimited] = speed::FAST_FORWARD_CHOICES;
    let item = |speed: Speed| checked(&speed.label(), speed == current);
    Menu {
        name: "Fast-Forward Speed".into(),
        items: vec![
            MenuItem::action(item(twice), FastForward2x),
            MenuItem::action(item(three_times), FastForward3x),
            MenuItem::action(item(four_times), FastForward4x),
            MenuItem::action(item(unlimited), FastForwardUnlimited),
        ],
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
    fn the_fast_forward_menu_checks_the_current_speed() {
        let menu = fast_forward_menu(speed::FAST_FORWARD_CHOICES[2]);
        let names: Vec<String> = menu
            .items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { name, .. } => Some(name.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(
            names,
            ["\u{2003}2×", "\u{2003}3×", "✓ 4×", "\u{2003}Unlimited"]
        );
    }

    #[test]
    fn window_opens_at_2x_without_a_display() {
        assert_eq!(initial_size(None), (512.0, 768.0));
    }
}
