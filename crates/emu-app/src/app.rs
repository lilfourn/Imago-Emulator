//! Application setup: actions, menus, shortcuts and the main window.

use std::path::PathBuf;

use gpui::{
    App, Application, Bounds, KeyBinding, Menu, MenuItem, TitlebarOptions, WindowBounds,
    WindowOptions, actions, prelude::*, px, size,
};

use crate::layout;
use crate::speed::{self, Speed, Speeds};
use crate::view::EmulatorView;

actions!(
    ds_emulator,
    [
        OpenRom,
        ConnectLinkCable,
        InsertGbaCartridge,
        EditCheats,
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
        FilterNone,
        FilterLcdGrid,
        FilterScanlines,
        FilterScale2x,
        FilterScale3x,
        ShowDebugger,
        DebugStep,
        DebugStepOver,
        DebugNextFrame,
        ToggleFullScreen,
        Resolution3d1x,
        Resolution3d2x,
        Resolution3d3x,
        Resolution3d4x,
        ToggleMicrophone,
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

/// Room for the window's title bar, in logical pixels.
const TITLE_BAR: f32 = 33.0;

/// Runs the app until the user quits, opening `roms` at launch: one game,
/// a DS game with a GBA game in its GBA slot, or two linked Game Boy or
/// Game Boy Advance games. `script`, a Lua script, runs alongside the
/// first game to start.
pub fn run(roms: Vec<PathBuf>, script: Option<PathBuf>) {
    Application::new().run(move |cx: &mut App| {
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([
            KeyBinding::new("cmd-o", OpenRom, None),
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
        let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("ds-emulator".into()),
                ..TitlebarOptions::default()
            }),
            window_min_size: Some(size(px(256.0), px(192.0))),
            app_id: Some("ds-emulator".to_owned()),
            ..WindowOptions::default()
        };
        cx.open_window(options, |window, cx| {
            cx.new(|cx| EmulatorView::new(roms, script, window, cx))
        })
        .expect("opening the main window");
        cx.activate(true);
    });
}

/// The window's content size at launch, in logical pixels: stacked DS
/// screens as large as fit most of a `display` of that size, or at 2x if
/// there is none.
fn initial_size(display: Option<(f32, f32)>) -> (f32, f32) {
    let Some((width, height)) = display else {
        return (STACKED_DS.0 * 2.0, STACKED_DS.1 * 2.0);
    };
    let free = (width, height - SYSTEM_BARS);
    let area = (free.0 * DISPLAY_SHARE, free.1 * DISPLAY_SHARE - TITLE_BAR);
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
            items: vec![MenuItem::action("Quit ds-emulator", Quit)],
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("Open ROM…", OpenRom),
                MenuItem::action("Connect Second Console…", ConnectLinkCable),
                MenuItem::action("Insert GBA Cartridge…", InsertGbaCartridge),
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
                        MenuItem::action("Native (1×)", Resolution3d1x),
                        MenuItem::action("2×", Resolution3d2x),
                        MenuItem::action("3×", Resolution3d3x),
                        MenuItem::action("4×", Resolution3d4x),
                    ],
                }),
                MenuItem::submenu(Menu {
                    name: "Filter".into(),
                    items: vec![
                        MenuItem::action("None", FilterNone),
                        MenuItem::action("LCD Grid", FilterLcdGrid),
                        MenuItem::action("Scanlines", FilterScanlines),
                        MenuItem::action("Scale2x", FilterScale2x),
                        MenuItem::action("Scale3x", FilterScale3x),
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
                MenuItem::action("Debugger", ShowDebugger),
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

    #[test]
    fn window_opens_large_on_a_laptop_display() {
        // A 14-inch MacBook Pro: 760 px is 1.98x, leaving room for the
        // menu bar, Dock and title bar.
        assert_eq!(initial_size(Some((1512.0, 982.0))), (507.0, 760.0));
        assert_eq!(initial_size(Some((2560.0, 1440.0))), (782.0, 1173.0));
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
