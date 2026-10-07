//! Application setup: actions, menus, shortcuts and the main window.

use std::path::PathBuf;

use gpui::{
    App, Application, Bounds, KeyBinding, Menu, MenuItem, TitlebarOptions, WindowBounds,
    WindowOptions, actions, prelude::*, px, size,
};

use crate::view::EmulatorView;

actions!(
    ds_emulator,
    [
        OpenRom,
        ConnectLinkCable,
        EditCheats,
        SwitchPlayer,
        TogglePause,
        Reset,
        SaveState,
        LoadState,
        NextStateSlot,
        PreviousStateSlot,
        ToggleFastForward,
        ToggleSlowMotion,
        LayoutVertical,
        LayoutHorizontal,
        LayoutSingle,
        LayoutHybrid,
        SwapScreens,
        RotateClockwise,
        RotateAnticlockwise,
        NextScreenGap,
        ToggleColorCorrection,
        ToggleMemoryViewer,
        Quit,
    ]
);

/// Runs the app until the user quits, opening `roms` at launch: one game,
/// or two linked Game Boy or Game Boy Advance games.
pub fn run(roms: Vec<PathBuf>) {
    Application::new().run(move |cx: &mut App| {
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([
            KeyBinding::new("cmd-o", OpenRom, None),
            KeyBinding::new("cmd-t", SwitchPlayer, None),
            KeyBinding::new("cmd-p", TogglePause, None),
            KeyBinding::new("cmd-r", Reset, None),
            KeyBinding::new("cmd-s", SaveState, None),
            KeyBinding::new("cmd-l", LoadState, None),
            KeyBinding::new("cmd-]", NextStateSlot, None),
            KeyBinding::new("cmd-[", PreviousStateSlot, None),
            KeyBinding::new("cmd-f", ToggleFastForward, None),
            KeyBinding::new("cmd-shift-f", ToggleSlowMotion, None),
            KeyBinding::new("cmd-1", LayoutVertical, None),
            KeyBinding::new("cmd-2", LayoutHorizontal, None),
            KeyBinding::new("cmd-3", LayoutSingle, None),
            KeyBinding::new("cmd-4", LayoutHybrid, None),
            KeyBinding::new("cmd-e", SwapScreens, None),
            KeyBinding::new("cmd-alt-right", RotateClockwise, None),
            KeyBinding::new("cmd-alt-left", RotateAnticlockwise, None),
            KeyBinding::new("cmd-g", NextScreenGap, None),
            KeyBinding::new("cmd-shift-c", ToggleColorCorrection, None),
            KeyBinding::new("cmd-shift-m", ToggleMemoryViewer, None),
            KeyBinding::new("cmd-q", Quit, None),
        ]);
        cx.set_menus(menus());
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        // Two DS screens stacked at 2x.
        let bounds = Bounds::centered(None, size(px(512.0), px(768.0)), cx);
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
            cx.new(|cx| EmulatorView::new(roms, window, cx))
        })
        .expect("opening the main window");
        cx.activate(true);
    });
}

fn menus() -> Vec<Menu> {
    vec![
        Menu {
            name: "ds-emulator".into(),
            items: vec![MenuItem::action("Quit ds-emulator", Quit)],
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("Open ROM…", OpenRom),
                MenuItem::action("Connect Link Cable…", ConnectLinkCable),
                MenuItem::separator(),
                MenuItem::action("Edit Cheats…", EditCheats),
            ],
        },
        Menu {
            name: "Emulation".into(),
            items: vec![
                MenuItem::action("Pause / Resume", TogglePause),
                MenuItem::action("Reset", Reset),
                MenuItem::action("Fast-Forward", ToggleFastForward),
                MenuItem::action("Slow Motion", ToggleSlowMotion),
                MenuItem::action("Switch Player", SwitchPlayer),
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
                MenuItem::action("Colour Correction", ToggleColorCorrection),
                MenuItem::action("Memory Viewer", ToggleMemoryViewer),
            ],
        },
    ]
}
