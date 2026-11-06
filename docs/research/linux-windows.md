# Linux and Windows

Researched 2026-11-06. This is the map for porting ds-emulator to Linux
(x86-64 and arm64) and Windows (x86-64). It covers what GPUI 0.2.2 does on
each, every place the repository assumes macOS, the outside programs the app
downloads or drives, packaging, and where to test Windows. It ends with a
split of the port into work streams that don't share files. No code was
changed.

PRD section 4 lists Windows and Linux releases as a non-goal for version 1,
and decision 0001 calls them "possible later but untested". This research
is the first step in changing that.

The evidence:
- GPUI 0.2.2 and blade-graphics 0.7.1, read in `~/.cargo/registry`.
- This repository at `origin/main` `d14a61e`, which includes PR #54's
  Linux and Windows CI.
- `cargo check` runs on this Mac with Rust 1.96.1.
- Azahar's, mkxp-z's and FVX's releases, read through GitHub's API. The
  Linux and Windows downloads were fetched and inspected (never run).
- Web pages, fetched on the same day (see Sources).

## Summary

- **GPUI draws no menu bar on Linux or Windows.** There, `set_menus` only
  stores the menus. Anything that is only in the menu bar becomes
  unreachable: Fast-Forward Speed, Original Game Boy Games, Use
  Microphone, Connect Second Console, Insert GBA Cartridge and Edit
  Cheats. The app needs a menu of its own in the window, built from the
  same `app::menus()`. GPUI hands the menus back through `cx.get_menus()`.
- **Open ROM… and Add Games to Library… can't pick a file off macOS.**
  Both ask for files and folders together, because an RPG Maker game is a
  folder. GPUI's Linux portal dialog and its Windows `IFileOpenDialog`
  both turn `directories: true` into a picker that only takes folders.
  Neither can mix the two (`can_select_mixed_files_and_dirs()` is
  false). The dialogs need splitting.
- **Hide Others crashes the app on Windows.** GPUI's Windows
  `hide_other_apps` and `unhide_other_apps` are `unimplemented!()`.
  On Linux they only log.
- **Windows has no title bar, and neither does GNOME's Wayland
  session.** On Windows, `appears_transparent: true` removes the
  caption. GNOME doesn't offer server-side decorations to Wayland
  windows. In both cases the app must draw close, minimise and maximise
  buttons and a drag area, plus resize edges on Wayland. GPUI 0.2.2 has
  the hooks for this.
- **Every shortcut is written with `cmd-`, which off macOS is the Windows
  or Super key.** The shell takes most of those keys. There are 44 in
  `app.rs` and 14 in Guide's composer, plus 16 strings that show ⌘ or ⌥.
  GPUI's `secondary-` prefix means ⌘ on macOS and Ctrl elsewhere.
- **Linux doesn't compile until `libc` is pinned down.** GPUI pulls in
  `xattr 0.2.3`, which can't find `libc::ENOATTR` in the locked
  `libc 0.2.190` (checked today). `cargo update -p libc --precise 0.2.189`
  resolves cleanly in a dry run.
- **Windows release builds must be built on Windows.** GPUI's build
  script compiles its HLSL shaders with `fxc.exe` from the Windows SDK,
  and embeds its DPI-awareness manifest, only when the build host is
  Windows. `cargo check --release -p gpui --target x86_64-pc-windows-gnu`
  from this Mac fails: `couldn't read …/shaders_bytes.rs`. A debug build
  cross-compiles, but it reads the HLSL from the build machine's cargo
  registry at run time.
- **Renderers.** On Linux GPUI draws through Vulkan 1.1 with four
  extensions (blade). It accepts a software device such as Mesa's
  lavapipe. With no Vulkan at all, it panics before any window opens. On
  Windows it uses Direct3D 11 at feature level 10.1 or above, through
  DirectComposition.
- **The upscaler stays on the CPU off macOS.** `gpu_upscale.rs` is Metal
  and MetalFX, and its stub already falls back. Auto and Sharp Pixels
  become sharp-bilinear, and Smooth and Rounded Pixels become Scale2x or
  Scale3x. Porting the shaders, for example to WGSL, is separate work.
- **3DS:** Azahar 2126.2 ships libretro cores for Linux x86-64 (14.7 MB)
  and Windows x64 (12.1 MB). Both contain Vulkan and OpenGL renderers and
  get Vulkan through the frontend, so emu-3ds swaps MoltenVK for the
  system's Vulkan loader. **No Linux arm64 core exists**, in Azahar's
  release or on libretro's buildbot.
- **RPG Maker:** mkxp-z has no GitHub releases. Its Linux and Windows
  builds exist only as CI artifacts, which expire after 90 days and need
  a GitHub login. Its Ruby is 3.1, which has no `UNIXSocket` on Windows,
  so the bridge needs TCP there. How to hide the player's window off
  macOS is the biggest open question.
- **Randomizer:** FVX 1.6.2 ships Linux x86, Linux ARM and Windows zips,
  each with its own Java. The layout matches the Mac zips.
- **Paths:** `dirs::data_dir()` is the roaming `%APPDATA%` on Windows,
  which would roam the 3DS core, its NAND and every unpacked game.
  `dirs::data_local_dir()` is the same folder on macOS and Linux and the
  local AppData on Windows. Setting `HOME` doesn't isolate a test run on
  Windows.
- **Packaging:** an AppImage and a `.deb` for Linux, and a signed NSIS
  installer plus a zip for Windows. cargo-packager builds all four from
  one config. Flatpak comes later.
- **Windows CI:** Namespace's Windows runners need its Team plan
  ($100 a month). GitHub's `windows-latest` costs $0.010 a minute once
  the budget is raised. A public repository runs GitHub's standard
  runners for free.

## 1. GPUI 0.2.2 off macOS

### Renderer and GPU

**Linux.** GPUI draws through blade-graphics 0.7.1 on Vulkan
(`blade-graphics/src/lib.rs:50-69` picks the backend; `gles` is only a
`--cfg` opt-in). `vulkan/init.rs` rejects a device below Vulkan 1.1 or
without `VK_EXT_inline_uniform_block`, `VK_KHR_timeline_semaphore`,
`VK_KHR_descriptor_update_template` and `VK_KHR_dynamic_rendering`
(lines 20-25, 111-131). It doesn't reject CPU devices, it only notes them
(`is_software_emulated`). So Mesa's lavapipe works without a GPU, and the
merged `scripts/linux/Dockerfile` already installs it with Xvfb.

- `ZED_DEVICE_ID=0x…` (a PCI device ID) picks a GPU
  (`platform/blade/blade_context.rs:13`).
- If no device qualifies, the app panics before it has a window.
  Wayland does `BladeContext::new().expect("Unable to init GPU context")`
  (`linux/wayland/client.rs:501`). X11 returns the error, and
  `current_platform` unwraps it (`platform.rs:112-116`).
- Frames are presented with MAILBOX, else FIFO_RELAXED, else IMMEDIATE,
  which can tear (`blade-graphics/src/vulkan/surface.rs:209-219`).
- The build checks the WGSL shaders with naga and needs no outside tools.
- The system packages are the ones in `scripts/linux/install-build-deps.sh`
  (xkbcommon, Wayland, X11/xcb, fontconfig, Vulkan, ALSA, udev).

**Windows.** GPUI uses Direct3D 11 at feature level 11.1, 11.0 or 10.1,
with a DXGI 1.6 factory, a DirectComposition swap chain and DirectWrite
for text (`windows/directx_devices.rs`, `directx_renderer.rs`). It takes
the first adapter that makes a D3D11 device. DXGI also lists the
"Microsoft Basic Render Driver" (WARP) as an adapter, so a VM without a
GPU should still start, though this wasn't tried. Zed's docs say the same
renderer "requires a DirectX 11 compatible GPU", and that inside a VM "it
will use the emulated adapter".

**Shaders and manifest on Windows.** In a release build, the build script
compiles `shaders.hlsl` with `fxc.exe` (`build.rs:138-237`). It looks for
`fxc.exe` in `GPUI_FXC_PATH`, then on `PATH` through `where.exe`, then at
`C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64\fxc.exe`, and
panics if it finds none. A debug build compiles the HLSL at run time with
`D3DCompileFromFile`, from the gpui crate's source path on the machine
that built it (`directx_renderer.rs:1491-1555`). That exe only works on
the build machine.

The build script is compiled for the host, and both of those steps sit
behind `#[cfg(target_os = "windows")]`. So on a Mac or Linux host, a
release build has no shader blob (the failure above), and no build gets
the manifest. The manifest, `resources/windows/gpui.manifest.xml` (default
feature `windows-manifest`), sets PerMonitorV2 DPI awareness and Common
Controls 6. GPUI makes no DPI-awareness call at run time, so an exe built
elsewhere would be blurry on high-DPI displays.

**msvc or gnu.** Both targets compile; the gnu one was checked from this
Mac, in debug. Build releases with `x86_64-pc-windows-msvc` on a Windows
runner:
- It is Rust's tier-1 Windows target.
- GitHub's and Namespace's Windows images carry Visual Studio and the
  Windows SDK, which is where `fxc.exe` comes from anyway.
- With `-C target-feature=+crt-static` the exe needs no Visual C++
  redistributable.

**Window icon.** GPUI loads the exe's icon resource number 1
(`windows/platform.rs:1000-1013`). emu-app has no build script, so the
window and taskbar would show a blank icon. `embed-resource` is already in
the tree through GPUI. Don't embed a second manifest: GPUI's takes
resource ID 1.

**`runtime_shaders`** only changes the macOS Metal path. Off macOS it does
nothing.

### Display servers on Linux

GPUI picks Wayland when `WAYLAND_DISPLAY` is set and not empty, then X11
when `DISPLAY` is, then a headless client with no windows
(`platform.rs:98-151`). Both backends are default features. Clearing
`WAYLAND_DISPLAY` forces X11 (XWayland) under a Wayland session.

- **Wayland decorations** come from `zxdg_decoration_manager_v1`.
  - KDE and the wlroots compositors offer server-side decorations.
  - GNOME's Mutter has no such manager, so it draws no frame.
  - GPUI asks for server-side decorations unless told otherwise
    (`window.rs:997`). On GNOME it then believes it has them, and the
    window has no title bar, borders or buttons.
  - The app should ask for client-side decorations
    (`WindowOptions::window_decorations`, `Window::request_decorations`),
    check `Window::window_decorations()`, and draw its own using
    `start_window_move`, `start_window_resize(ResizeEdge)`,
    `show_window_menu` and `window_controls()`.
- **Window position.** Wayland doesn't let a client place its window, so
  the saved `x`/`y` and display in `settings.toml` are ignored there.
- **Focus and hiding.** `activate`, `hide`, `hide_other_apps` and
  `unhide_other_apps` only log on Linux (`linux/platform.rs:231-245`).
- **X11** gets its decorations from the window manager. GPUI drives
  redraws from a calloop timer at the RandR mode's refresh rate
  (`linux/x11/client.rs:1783-1810`), not from vsync.

### Feature by feature

Where the app uses each feature is given as `crates/emu-app/src` paths.

| Feature | Where the app uses it | Linux | Windows | What the port needs |
|---|---|---|---|---|
| Menu bar, `set_menus` | `app.rs:208`, `view.rs:2498` | Stored, never shown (`linux/platform.rs:474-482`) | Stored, never shown (`windows/platform.rs:516-522`) | An in-window menu built from `app::menus()` or `cx.get_menus()`. `SystemMenuType::Services` means nothing there. |
| Hide, Hide Others, Show All | `app.rs:131-133`, bound to `cmd-h` and `cmd-alt-h` | Logged, no-op | `hide` is a no-op. `hide_other_apps` and `unhide_other_apps` are `unimplemented!()` and panic (`windows/platform.rs:370-380`) | Leave the actions and keys out off macOS. |
| `activate(true)` | `app.rs:254` | No-op | No-op | Nothing. |
| About panel, Dock icon | `window_chrome.rs:33,49` (stubs off macOS) | No-op | No-op | An About page in the app with the licences (`assets/NOTICE.md`, and the GPL cores it downloads). The icon comes from packaging. |
| Open dialog, `prompt_for_paths` | `view.rs:1693,1724` (files and folders); `guide/mod.rs:514` (files) | XDG portal FileChooser over D-Bus (ashpd). `directories: true` gives a folder chooser. With no portal it returns `FILE_PICKER_PORTAL_MISSING`, which the app ignores without a word | `IFileOpenDialog`. `directories` sets `FOS_PICKFOLDERS`, so only folders (`windows/platform.rs:902-945`) | Open… takes files only. Add Open Game Folder…, or accept the game's `Game.ini` or player and use its folder. Show the dialog's error. No OS has file-type filters in 0.2.2. |
| Drag and drop of files and folders | `view.rs:2185,3175` | XDND on X11, data device on Wayland | OLE `IDropTarget` | Works. |
| Title bar, `TitlebarOptions { appears_transparent, traffic_light_position }` | `app.rs:260-266`, `debugger_window.rs:107` | Ignored. The WM's bar on X11 and KDE; nothing on GNOME Wayland | `appears_transparent` hides the caption (`windows/window.rs:380-383`). The app marks areas with `window_control_area(Drag/Close/Max/Min)`, which GPUI hit-tests (`windows/events.rs:855-890`) | Draw the controls in `app::TOP_BAR`, following the show/hide controls setting (`view.rs:2990`, a no-op stub off macOS). |
| Full screen | `view.rs:2987,3162`, `WindowBounds::Fullscreen` at launch | `xdg_toplevel.set_fullscreen`, or `_NET_WM_STATE_FULLSCREEN` on X11 | A borderless window over the monitor, with its style restored after (`windows/window.rs:238-300`) | Bind F11 off macOS (`ctrl-cmd-f` is ⌃⌘F). |
| Key modifiers | `app.rs:155-205`, `guide/composer.rs:138-171` | `cmd` = `super` = `win` = the platform modifier (`keystroke.rs:152-158`). GNOME takes Super | Windows key. The shell takes Win+E, R, L, D, M, G, 1–4 and more | Use `secondary-`, which is ⌘ on macOS and Ctrl elsewhere (`keystroke.rs:143-149`), and `Modifiers::secondary()` in code. |
| Display refresh rate | `view.rs:3253` through `window_chrome::refresh_interval`, which reads AppKit | No GPUI API. The stub returns `None`, so frame pacing keeps the system clock (decision 0003) | The same | Windows: `DwmGetCompositionTimingInfo`, or `EnumDisplaySettingsW` on the window's monitor. X11: RandR through `x11rb`, already in the tree. Wayland: `wl_output` isn't reachable through GPUI, so measure the interval between frames drawn. |
| Frame callbacks, `request_animation_frame` | `view.rs:3286`, `guide/render.rs:38,46`, `guide/pictures.rs:220` | Wayland frame callbacks. X11's timer above | A vsync thread waits on `DwmFlush` at the compositor's period (`windows/vsync.rs`) | Works. |
| `open_url` | `openrouter.rs:229`, `store_view.rs:1109`, `guide/render.rs`, `settings_window.rs:433` | Portal OpenURI, then the `open` crate (`xdg-open` and others) | `ShellExecute` | Works. |
| `reveal_path` | `library_view.rs:1842`, labelled "Show in Finder" at `:1835` | Portal OpenDirectory: opens the folder without selecting the file | Explorer, with the file selected | Rename the item for each OS. |
| Clipboard text | `guide/composer.rs:454,467`, `guide/mod.rs:876`, `randomize_window.rs:318` | X11 and Wayland | Win32 | Works. ⌘V detection checks `modifiers.platform` (`randomize_window.rs:455`). |
| Images | `RenderImage`, `img()`, `window.drop_image` | The `image` crate decodes PNG, JPEG, WebP, GIF, BMP and TIFF on every OS, and resvg draws SVG (`platform.rs:1629-1780`). Blade's atlas is BGRA and frees a texture once nothing uses it, as Metal's does | DirectX atlas, same rules | Nothing: frames upload as BGRA without conversion. |
| Fonts | `".SystemUIFont"` (`view.rs:3695`), `"Menlo"` (`view.rs:3538`, `debugger_window.rs:461`, `guide/answer.rs:256,354`) | `.SystemUIFont` means IBM Plex Sans (`linux/text_system.rs:216`), rarely installed, then GPUI's fallback list (Ubuntu, Adwaita Sans, Cantarell, Noto Sans, DejaVu Sans…). Menlo is missing, so the debugger's columns fall back to a proportional font | `.SystemUIFont` is the system font. Menlo is missing | Bundle an OFL monospace font, or pick per OS (Consolas; DejaVu Sans Mono). |
| Display UUIDs, for where the window was left | `app.rs:228` | X11: an ID GPUI makes. Wayland: a v5 UUID of the output's name | From the device | Works, except that Wayland can't place windows. |
| `app_id: "ds-emulator"` | `app.rs:246` and each window | The Wayland app_id or X11 `WM_CLASS`. It must match the `.desktop` file's name for the icon and taskbar grouping | Unused | Packaging. |

### Timers, threads, sound and gamepads

- **Sleeps.** Since Rust 1.75, `thread::sleep` on Windows uses
  high-resolution waitable timers (rust-lang/rust#116461), good to about
  0.5 ms, so `speed::sleep_until`'s naps should hold. Measure it. Linux
  sleeps are precise to the timer slack, 50 µs by default.
- **Thread priority.** `qos.rs` is a no-op off macOS.
  - On Windows, consider `SetThreadPriority` or MMCSS
    (`AvSetMmThreadCharacteristicsW("Games")`) for the emulation thread.
    Also call `SetProcessInformation(ProcessPowerThrottling)` to opt out
    of EcoQoS. Windows 11 can throttle a process like the 3DS helper,
    which has no window.
  - On Linux, a negative nice needs privilege. rtkit can grant real-time
    priority over D-Bus, and cpal's optional `realtime` feature brings
    `audio_thread_priority`, which does that. Measure before adding
    either.
- **Sound.** By default cpal 0.18 plays through ALSA on Linux
  (`libasound.so.2`). PipeWire and PulseAudio take that through their ALSA
  plugins, and cpal also has optional `pulseaudio`, `pipewire` and `jack`
  features. On Windows it uses WASAPI.
- **Gamepads.** gilrs 0.11 reads evdev through libudev on Linux. The user
  needs read access to `/dev/input/event*`, which logind's `uaccess`
  normally gives. On Windows it uses Windows.Gaming.Input, which "requires
  an in focus window to be associated with the process to receive
  events" (gilrs README). A benchmark run behind another app gets no
  gamepad input there.

## 2. macOS assumptions in this repository

Paths are from the repository root. "Error" means it doesn't compile on
that OS. Lines inside `#[cfg(test)]` modules are marked as tests.

### Build and dependencies

| Where | What it does | Linux | Windows | Replacement |
|---|---|---|---|---|
| `Cargo.lock`: `libc 0.2.190` | `xattr 0.2.3` (GPUI → `gpui_http_client` → `zed-async-tar`) uses `libc::ENOATTR` | Error E0425 (checked) | Fine | `cargo update -p libc --precise 0.2.189`, and check it after every `cargo update` |
| `crates/emu-app` has no `build.rs` | — | Fine | No icon resource, so a blank window and taskbar icon | A `build.rs` that embeds `assets/app-icon.ico` as icon 1 with `embed-resource` |
| `crates/emu-app/src/main.rs` (no `windows_subsystem`) | — | Fine | A console window opens with the app | `#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]`, with `AttachConsole(ATTACH_PARENT_PROCESS)` so `--help` and the CLI still print |
| `Cargo.toml:34-37` | The comment on `runtime_shaders` speaks of the Xcode Metal Toolchain | — | — | Note that Windows release builds need `fxc.exe` (Windows SDK) |
| `README.md:76` "Needs … Xcode" | Build instructions | — | — | Per-OS instructions: the Linux packages, and Visual Studio Build Tools with the Windows SDK |

### Keys and shortcut labels

| Where | What it does | Linux | Windows | Replacement |
|---|---|---|---|---|
| `crates/emu-app/src/app.rs:155-205` | 44 bindings written `cmd-…`, including `ctrl-cmd-f` (full screen), `cmd-h` and `cmd-alt-h` | Super+key; GNOME takes Super | Win+key; the shell takes most of them | `secondary-…`. F11 for full screen. No Hide or Hide Others. Avoid `ctrl-alt-` chords: they are AltGr on many Windows layouts, GNOME switches workspaces on Ctrl+Alt+←/→, and some Intel drivers rotate the display on Ctrl+Alt+arrow (rotate is `cmd-alt-left/right`) |
| `crates/emu-app/src/guide/composer.rs:138-171` | 14 `cmd-` text-editing keys: line start and end, select all, copy, cut, paste, delete to line start or end | Super | Win | `secondary-` for copy, cut, paste and select all. Home/End and Ctrl+Home/End for lines, and Ctrl+←/→ for words, as Linux and Windows text fields do |
| `crates/emu-app/src/view.rs:1313,1351-1358,1400-1420` | ⌘ chords never reach the game; every game key is released when ⌘ goes down, because macOS sends no key-up then | Works with Ctrl, which is already excluded at `:1358` | Same | Use `Modifiers::secondary()`. The ⌘-down release is macOS-only and harmless elsewhere |
| `crates/emu-app/src/library_view.rs:644-647`, `randomize_window.rs:455` | ⌘Z puts back a removed game; ⌘V pastes settings | `modifiers.platform` is Super | Win | `secondary()` |
| ⌘/⌥ in UI strings: `view.rs:874,876,1715,1758,1904`; `library_view.rs:757,1866,1911,2341`; `randomize_window.rs:825`; `debugger_window.rs:534-547`; `guide/render.rs:912` | 16 strings name shortcuts with Mac glyphs | Wrong keys shown | Same | One label function per OS ("Ctrl+Shift+S") |
| `README.md:136-162`, `crates/emu-app/src/settings.rs:20-36` (comments) | The controls table and settings docs use ⌘ | — | — | A column for each OS, or "⌘/Ctrl" |

### Window, menus and dialogs

| Where | What it does | Linux | Windows | Replacement |
|---|---|---|---|---|
| `crates/emu-app/src/app.rs:208`, `view.rs:2498`, `app.rs:330-475` | The whole menu bar, with checkmarks rebuilt by the view | Not shown | Not shown | An in-window menu, perhaps behind a button in `TOP_BAR`, drawn from the same tree |
| `crates/emu-app/src/app.rs:131-133` | Hide, Hide Others, Show All | No-op | Panic on Hide Others and Show All | `#[cfg(target_os = "macos")]` |
| `crates/emu-app/src/app.rs:130,253`; `window_chrome.rs:32-70` | About panel and Dock icon | Stubs | Stubs | In-app About; icon from packaging and resources |
| `crates/emu-app/src/app.rs:260-266`; `window_chrome.rs:9-29`; `view.rs:2990` | Traffic lights placed in the 40 px top bar, hidden with the window controls | No bar on GNOME Wayland | No caption | App-drawn controls (§1) |
| `crates/emu-app/src/view.rs:1693,1724` | One dialog for files and folders | Folders only | Folders only | Split, as in §1 |
| `crates/emu-app/src/library_view.rs:1835` | "Show in Finder" | — | — | "Show in Files" or "Show in Explorer" |
| `crates/emu-app/src/app.rs:100-103` | `SYSTEM_BARS` guesses 100 px for the menu bar and Dock | Panels differ | The taskbar | Fine as a guess. Revisit if the first window opens too tall |

### Paths

The `dirs` crate already gives each OS its own folder. These are where its
choices or a hardcoded path matter.

| Where | What it does | Linux | Windows | Replacement |
|---|---|---|---|---|
| `dirs::data_dir()`: `crates/emu-app/src/archive.rs:38`, `romhack.rs:26`, `randomizer.rs:91`, `emulator.rs:317`, `openrouter.rs:88`, `guide/mod.rs:296`; `crates/emu-3ds/src/lib.rs:71` | Games, Hacks, Randomizer, RPG Maker players, OpenRouter key, Guide chats, the 3DS core and its NAND | `~/.local/share/ds-emulator` | `%APPDATA%` (Roaming), which roams tens to hundreds of MB | `dirs::data_local_dir()`: unchanged on macOS and Linux, `%LOCALAPPDATA%` on Windows |
| `dirs::config_dir()`: `crates/emu-app/src/settings.rs:609`, `bindings.rs:529`, `library.rs:353` | `settings.toml`, `bindings.toml`, `library.toml` | `~/.config/ds-emulator`, apart from the data | `%APPDATA%`, which is fine for small files | Keep, and document the split |
| `dirs::cache_dir()`: `crates/emu-app/src/boxart.rs:51`, `guide/pictures.rs:492` | Box art, store and Guide pictures | `~/.cache` | `%LOCALAPPDATA%` | Fine |
| `crates/emu-headless/src/main.rs:320-325` | `~/Library/Caches/emu-headless`, hardcoded | Makes `~/Library` | `%USERPROFILE%\Library` | `dirs::cache_dir()` |
| `crates/emu-guide/examples/ask.rs:76` | `~/Library/Application Support/ds-emulator/OpenRouter`, hardcoded | Wrong | Wrong | The library's own path |
| Test isolation (`scripts/macos/*.sh` set `HOME`) | A home of the test's own | `dirs` follows `HOME` and `XDG_*` | `dirs` asks for Known Folders and ignores `HOME` | An override such as `DS_EMULATOR_HOME` read by one shared function |
| Docs | `Library/Application Support` or `Library/Caches` is on 13 lines of `docs/ARCHITECTURE.md` and 10 of `README.md`, plus `docs/decisions/0008-0012,0015`, `docs/PRD-game-chat.md`, `docs/research/3ds.md`, `fan-games.md` | — | — | One table of folders per OS, linked from the rest |
| `scripts/store-compat.py:24,28,73,97` | Its cache and the app's 3DS core | Wrong | Wrong | Use the same rules (only needed if the sweep runs off macOS) |

### Outside programs

Off macOS, a GUI-subsystem Windows app that starts a console program
flashes a console window unless it passes `CREATE_NO_WINDOW`
(`std::os::windows::process::CommandExt::creation_flags(0x0800_0000)`).
That applies to every `Command` below that stays.

| Where | What it does | Linux | Windows | Replacement |
|---|---|---|---|---|
| `crates/emu-app/src/archive.rs:246,316` | `/usr/bin/tar` (macOS's bsdtar) unpacks `.7z` and `.rar` and lists their sizes | `/usr/bin/tar` is GNU tar, which reads neither format. bsdtar is in `libarchive-tools`, not installed by default | `C:\Windows\System32\tar.exe` is bsdtar (Windows 10 1803+), but at another path. Its 7z and RAR support wasn't checked | In process: `sevenz-rust2` (Apache-2.0) for 7z. For RAR, the `unrar` crate, which bundles RARLAB's UnRAR source under its own licence (check against PRD §9), or ask the player to unpack it |
| `crates/emu-app/src/archive.rs:533` (test) | Makes a 7z with tar | Fails | Fails | A checked-in fixture, or the crate's writer |
| `crates/emu-app/src/leftovers.rs:257-275` | `pkill -KILL -f '^/usr/bin/tar …'` stops a `tar` still unpacking into an abandoned folder | The pattern never matches | No `pkill` | Gone once unpacking is in process; otherwise keep the `Child` |
| `crates/emu-app/src/leftovers.rs:400` (test) | `/bin/sh` with `trap` | Works | Fails | Unix only |
| `crates/emu-app/src/cheats.rs:47`, `randomize_window.rs:444` | `open -t` opens `game.cht` or the spoiler log in the text editor | `xdg-open` picks by MIME type | Nothing is associated with `.cht`, so Windows asks which app to use | Linux `xdg-open`; Windows `notepad.exe`, or `ShellExecuteExW` with the `edit` verb |
| `crates/emu-app/src/guide/mod.rs:1052-1057` | `defaults read com.apple.universalaccess reduceMotion` | Missing, so false | Missing, so false | `gsettings get org.gnome.desktop.interface enable-animations`; `SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION)` |
| `crates/emu-guide/src/login.rs:83-90` | `scutil --get ComputerName` labels the OpenRouter key | Empty, so "DS Emulator" | Same | `gethostname`; `%COMPUTERNAME%` |
| `crates/emu-app/src/boxart.rs:489`; `crates/emu-guide/src/curl.rs:31`, `chat.rs:410` | `curl` from `PATH` for box art, the store, Guide and OpenRouter | Not on every desktop install | `curl.exe` ships in System32 since Windows 10 1803 | Declare it as a package dependency on Linux and pass `CREATE_NO_WINDOW` on Windows. Or move to an in-process client (`ureq` with `rustls`; `rustls` is already in the lock) |
| `crates/emu-app/src/randomizer.rs:230`; `crates/emu-3ds/src/install.rs:414` | `/usr/bin/curl` by absolute path | Usually works | Fails | `curl` from `PATH`, as above |
| `crates/emu-app/src/randomizer.rs:172`; `crates/emu-3ds/src/install.rs:318` | `/usr/bin/ditto -x -k` unzips the FVX and Azahar downloads | Missing | Missing | The `zip` crate (already an emu-app dependency; add it to emu-3ds) |
| `crates/emu-3ds/src/install.rs:326` | `/usr/bin/tar` unpacks MoltenVK | Not needed | Not needed | macOS only |
| `crates/emu-app/src/randomizer.rs:529` | Runs FVX's own Java | `java/bin/java` from the Linux zips | `java/bin/java.exe`, or `javaw.exe` to avoid a console | Pins per OS (§3) |
| `crates/emu-app/src/app_icon.rs:16,30`; used at `boxart.rs:357-358`, `library_view.rs:2357-2360` | `plutil` and `sips` turn an RPG Maker player app's icon into a PNG | Missing | Missing | Windows: the icon resource of `mkxp-z.exe` or `Game.exe`. Linux: an icon file in the game folder. Else the library's title plate, which already exists |
| `crates/emu-mkxp/src/player.rs:141-165` | `open -n -j -g --env …` starts the hidden player app | Missing | Missing | `std::process::Command` on the player binary, with the environment set directly |
| `crates/emu-mkxp/src/player.rs:225-256` | `kill`, `pkill` and `kill -KILL` stop the player | Work | Missing | `Child::kill`, or the pid with `TerminateProcess` |
| `crates/emu-mkxp/src/player.rs:277-300` | `cp -cR`, `plutil` and `codesign` copy the player app, mark it `LSUIElement` and sign it | Not needed | Not needed | macOS only: there is no bundle to patch elsewhere |
| `crates/emu-mkxp/src/audio.rs:83` | `mkfifo` makes the sound pipe | Works; `libc::mkfifo` avoids the shell-out | Missing | §3, mkxp-z |

### Unix-only APIs

| Where | What it does | Linux | Windows | Replacement |
|---|---|---|---|---|
| `crates/emu-app/src/randomizer.rs:24,190` (also `:679,818,926` in tests) | `PermissionsExt::from_mode(0o755)` on Java's programs | Works | Error | `#[cfg(unix)]` |
| `crates/emu-guide/src/key.rs:141-170` | 0700 folder and 0600 key file, already `#[cfg(unix)]` | Works | Skipped; the profile's ACL protects it | Fine |
| `crates/emu-3ds/src/install.rs:25,375` | File modes | Works | Error | `#[cfg(unix)]` |
| `crates/emu-3ds/src/jit_fix.rs:30,92` | `FileExt::read_at/write_at` and modes, to patch the Apple arm64 core | Works | Error | Only build it for macOS arm64 |
| `crates/emu-3ds/src/host.rs:12,286-291` | `dup(1)`, `dup2(2, 1)` and `File::from_raw_fd` keep the protocol on the real stdout and send the core's prints to the log | Works | Error | The Windows core links `msvcrt.dll` (checked), a different C runtime from Rust's, so a `dup2` in Rust's UCRT wouldn't redirect it. Duplicate the stdout handle for the protocol, then `SetStdHandle(STD_OUTPUT_HANDLE, stderr)` before loading the DLL |
| `crates/emu-3ds/src/host.rs:243` | `libc::_exit` skips the core's static destructors | Works | `libc` has `_exit` for Windows targets, but check it | `std::process::exit` won't do; keep `_exit` or use `ExitProcess` |
| `crates/emu-3ds/src/host.rs:304-311` | `pthread_set_qos_class_self_np` | Error (macOS only) | Error | §1 thread priority |
| `crates/emu-3ds/src/host.rs:401-431` | `PRIO_DARWIN_ROLE` with `getppid`, `getpriority` and `setpriority` copies the app's Game Mode role | Error: `setpriority`'s argument types differ | Error | macOS only |
| `crates/emu-3ds/src/cia.rs:415`, `link.rs:462-470` (tests) | `MetadataExt`, a 0755 test script | Works | Error | `#[cfg(unix)]` tests |
| `crates/emu-mkxp/src/link.rs:26,69-84` | `UnixListener`/`UnixStream` to the bridge | Works | No AF_UNIX in std (`uds_windows 1.2.1` is in the lock), and the player's Ruby can't connect anyway (§3) | TCP on 127.0.0.1 with a random port and a token, on every OS or on Windows only |
| `crates/emu-mkxp/src/audio.rs:14,125` | `OpenOptionsExt::custom_flags(O_NONBLOCK)` to wake a reader stuck on the FIFO | Works | Error | §3 |
| `crates/emu-mkxp/src/bridge.rb:36-67` | Fiddle `getpriority`/`setpriority` with `PRIO_DARWIN_ROLE` | Fails quietly (an invalid `which`) | No `setpriority` symbol | Only when `RUBY_PLATFORM` is darwin |
| `crates/emu-mkxp/src/bridge.rb:71` | `UNIXSocket.new` | Works | Ruby 3.1 has none | `TCPSocket` |

### Threads and the GPU

| Where | What it does | Linux | Windows | Replacement |
|---|---|---|---|---|
| `crates/emu-app/src/qos.rs:14-21`, called at `emulator.rs:643` | Gives the emulation thread the user-interactive class | No-op | No-op | §1 thread priority, measured |
| `crates/emu-app/src/window_chrome.rs:74-87` | The display's refresh interval from `NSScreen` | `None` | `None` | §1 refresh rate |
| `crates/emu-app/src/gpu_upscale.rs:75-665`, `crates/emu-app/src/shaders/*.metal` | Sharp Pixels, OmniScale and the beside-3D pass in Metal; Smooth in MetalFX | The stub at `:668` fails to start, so the CPU path draws | Same | wgpu with WGSL ports of `sharp.metal` and OmniScale covers Vulkan, D3D12 and Metal alike, and AMD FSR 1 (MIT) stands in for MetalFX. Or blade on Linux (in the tree) plus D3D11 on Windows: two ports. The message at `:56` says "this Mac has no Metal GPU" |
| `crates/emu-3ds/src/vulkan.rs:28-33,267-273` | Loads `libMoltenVK.dylib` by path with `ash::Entry::load_from` | — | — | `ash::Entry::load()` (`libvulkan.so.1`, `vulkan-1.dll`). `create_instance` (`:53-75`) already adds portability only when offered |
| `crates/emu-3ds/src/install.rs:88-139`; `link.rs:244-258` | Pins the core plus MoltenVK and passes MoltenVK's path to the helper; `MVK_CONFIG_LOG_LEVEL` | — | — | Pins per OS with no MoltenVK; `Parts::moltenvk` becomes optional |

### `scripts/macos`

`check-layout.sh` and `bench.sh` depend on:
- `swiftc` to build `ax.swift` and `layout.swift`;
- the Accessibility API, to press menu items and post keys to one process
  (`ax`);
- `osascript`, to put the user's front app back;
- `screencapture -l`, for window captures;
- `pgrep` and `ps -o time=`, for CPU time;
- `cp -c` clones of saves and the 3DS folder;
- `HOME` pointed at a scratch home.

The app's own hooks are portable: `DS_EMULATOR_PROBE` (`probe.rs`) writes
element frames, and `DS_EMULATOR_TRACE` (`trace.rs`) writes the timing
trace. `bench-report.py` uses only the standard library.

**A Linux version** (`scripts/linux/`) runs the release build under Xvfb.
The merged Dockerfile has Xvfb and lavapipe, and a real session also
works.
- Keys go in with `xdotool key --window <id>`. Under Xvfb the app is the
  only window, so focus isn't a problem.
- Captures come from `xwd` or ImageMagick's `import -window`.
- CPU time comes from `/proc/<pid>/stat` or `ps`.
- `HOME` and `XDG_CONFIG_HOME`/`XDG_DATA_HOME`/`XDG_CACHE_HOME` give the
  run a home of its own.
- There's no menu bar to press, so the script drives the in-window menu
  with keys, or the app takes a test-only command.
- A Wayland variant (headless weston or sway, `wtype`, `grim`) can come
  later.

**A Windows version** (`scripts/windows/`, PowerShell):
- Keys go in as `PostMessageW(WM_KEYDOWN/WM_KEYUP)` to the window. GPUI
  reads them in its window procedure, so this reaches a background window,
  which `SendInput` can't.
- Captures come from `PrintWindow` with `PW_RENDERFULLCONTENT`, which
  DirectComposition needs, or from a screen copy.
- CPU time comes from `(Get-Process).TotalProcessorTime`.
- Isolation needs the `DS_EMULATOR_HOME` override.
- macOS stops drawing a covered window (`bench.sh:12`). Whether Windows
  and DirectComposition do the same is unknown, so check it first.
- Gamepad input needs the window focused (§1).

## 3. Outside pieces

### Azahar 2126.2 (the version `crates/emu-3ds/src/install.rs` pins)

Published 2026-10-08:
<https://github.com/azahar-emu/azahar/releases/tag/2126.2>. The SHA-256
values are GitHub's asset digests, which match the downloads.

| Asset | Size | SHA-256 | Inside |
|---|---:|---|---|
| `azahar-libretro-linux-x86_64-2126.2.zip` | 14,742,826 | `2223444b74de2c922bae1efcfed60002f696b3c2aaf4fc73fc03e66a395dc26e` | `azahar_libretro.so`, 47,174,368 bytes |
| `azahar-libretro-windows-x86_64-2126.2.zip` | 12,097,452 | `d863711cff775ed697ba0cc5a963f2145b9672c4ad9a891367821d7f3cc1f888` | `azahar_libretro.dll`, 33,769,472 bytes |
| Linux arm64 | — | — | None. The release has libretro cores only for Android arm64-v8a, iOS, tvOS, macOS and the two above |

- **Linux x86-64.** The core needs `libstdc++.so.6`, `libm`, `libmvec`,
  `libgcc_s`, `libc` and `ld-linux-x86-64`, with symbols up to
  `GLIBC_2.34` and `GLIBCXX_3.4.30`: Ubuntu 22.04, Debian 12 or newer.
- **Windows x64.** The core is a MinGW build. It imports `msvcrt.dll`,
  `KERNEL32`, `ADVAPI32`, `bcrypt`, `CRYPT32`, `ole32`, `SHELL32`, `WINMM`
  and `WS2_32`, so it needs no Visual C++ runtime. It doesn't import
  `vulkan-1.dll` or `opengl32.dll`.
- **Renderers.** Both builds contain Azahar's `renderer_vulkan` and
  `renderer_opengl`. The macOS build has no OpenGL. The `citra_graphics_api`
  option chooses between them and software. Vulkan comes from the frontend
  through libretro's hardware-render interface, so emu-3ds keeps its
  Vulkan path and loads the system's Vulkan loader instead of MoltenVK.
  OpenGL would need the helper to make a GL context, which isn't worth it.
  The software renderer needs no GPU, which lets CI smoke-test 3DS games
  (`DS_EMULATOR_3DS_RENDERER=software`, `host.rs:469`).
- **What the player needs.** A Vulkan driver: Mesa or NVIDIA plus
  `libvulkan1` on Linux; on Windows the GPU vendor's driver installs
  `vulkan-1.dll`.
- **Download size.** Without MoltenVK the first download is about 15 MB
  on Linux and 12 MB on Windows, against 67 MB on a Mac.
- **The shader-JIT fix** (`jit_fix.rs`, `LOG2`) is for the Apple arm64
  build. The x86-64 builds have no such bug: the Intel Mac pin has
  `fix: None`.
- **Linux arm64 has no build anywhere.** libretro's buildbot has
  `nightly/linux/x86_64/latest/azahar_libretro.so.zip` (14,991,808 bytes)
  and `nightly/windows/x86_64/latest/azahar_libretro.dll.zip`, but
  `nightly/linux/aarch64/latest/azahar_libretro.so.zip` returns 404. Only
  the retired `citra_libretro` is built for aarch64, and nightlies aren't
  pinnable anyway. The options:
  - Build the core from `azahar-unified-source-2126.2.tar.xz` (43 MB) in
    CI and pin its digest. The project would then distribute a GPL-3.0
    binary and owe its source; decision 0011 avoided exactly that.
    Builds for arm64 would also need the LOG2 shader-JIT fix, since the
    bug is in the arm64 JIT.
  - Or leave 3DS games out on Linux arm64, with a clear message.

### mkxp-z

<https://github.com/mkxp-z/mkxp-z>, GPL-2.0-or-later, default branch
`dev`, tags up to `v2.4.2`. It has **no GitHub releases**: builds come only
from its "Automatic Build" workflow. The last successful run on `dev` was
<https://github.com/mkxp-z/mkxp-z/actions/runs/37110075912> (2026-10-03,
`37a04d1`), with 39 artifacts, among them:

| Artifact | Bytes |
|---|---:|
| `mkxp-z.linux.ubuntu.xenial.x86_64` | 20,093,482 |
| `mkxp-z.linux.debian.trixie.x86_64` | 23,069,033 |
| `mkxp-z.linux.ubuntu.xenial.arm64` | 18,850,554 |
| `mkxp-z.linux.debian.trixie.arm64` | 21,110,735 |
| `mkxp-z.windows.msys2.x86_64` | 20,800,667 |
| `mkxp-z.windows.cross.x86_64` | 20,497,967 |
| `mkxp-z.windows.cross.arm64` | 16,804,412 |
| `mkxp-z.windows.cross.i686` | 17,804,943 |

- **Artifacts can't be pinned.** They expire, these on 2027-01-01, and
  downloading one needs a GitHub login. The app can't fetch and pin
  mkxp-z as it does Azahar. As on the Mac today, the player comes with
  the game folder. The README says mkxp-z "supports Windows, Linux (x86,
  ARM, and POWER), and both Intel and Apple Silicon versions of macOS".
- **Ruby 3.1** (`meson_options.txt`: `mri_version` `'3.1'`). Ruby added
  `UNIXSocket` on Windows in 3.2 (Feature #19135), so the bridge's socket
  must be TCP there. Its Fiddle priority code must check the platform
  first (§2).
- **Finding the player.** `emu_mkxp::player_app` looks for a `.app`
  folder (`crates/emu-mkxp/src/lib.rs:103-122`). Off macOS it should find
  the mkxp-z executable in the game folder, under whatever name the game
  ships it (`mkxp-z.exe`, `Game.exe` replaced by mkxp-z, or a Linux
  binary or AppImage).
- **Hiding the window is the main unknown.** On macOS it rests on
  `LSUIElement`, `open -j -g` and `SDL_MAC_BACKGROUND_APP` (decision 0008).
  Linux and Windows have no equivalent. Candidates, none tried:
  - SDL's offscreen video driver (`SDL_VIDEODRIVER=offscreen`, which
    needs EGL);
  - a window SDL creates hidden;
  - on Linux, a private Xvfb display for the player.

  Decision 0008 found the player must keep drawing while unseen, so test
  each candidate for frames.
- **Sound.** OpenAL Soft's `wave` backend writes to a path. The Linux
  FIFO works as on macOS. On Windows the app could create a named pipe
  (`\\.\pipe\…`) for the backend to open as a client, or read a growing
  file. Neither has been tried.

### FVX 1.6.2 (the randomizer)

<https://github.com/upr-fvx/universal-pokemon-randomizer-fvx/releases/tag/vFVX1.6.2>,
published 2026-10-03. The three zips were downloaded and listed.

| Asset | Bytes | SHA-256 | Layout |
|---|---:|---|---|
| `UPR_FVX-v1_6_2-Linux_x86.zip` | 51,098,007 | `a4a8a87b0fea78ff44ea8b3a1406fe8e503ceab291c482d78001122c5ec5783b` | `UPR-FVX.jar`, `java/bin/java`, `java/lib/jspawnhelper`, `launcher.sh` |
| `UPR_FVX-v1_6_2-Linux_ARM.zip` | 49,632,260 | `cb91e8dd3cd066bf239b485a6301ec420ae88640a658b1a4c37779b18d94ad66` | The same |
| `UPR_FVX-v1_6_2-Windows.zip` | 41,474,594 | `1cee4080967075e5babcc06e93d7c382ec2f00f05d99e17ce31fc0a99d581acb` | `UPR-FVX.jar`, `java/bin/java.exe`, `java/bin/javaw.exe`, `launcher.bat` |

The jar sits at the same path in every zip. `JAVA` and `EXECUTABLES` in
`randomizer.rs:76-80` need per-OS values, and Windows needs no
permissions set.

## 4. Packaging and distribution

**What every Linux package needs**
- A `.desktop` file named after the app ID, `ds-emulator.desktop`, with
  `StartupWMClass=ds-emulator`, `Exec=ds-emulator %F`,
  `Categories=Game;Emulator;` and `MimeType=` for the ROM types.
- hicolor PNG icons from 16 to 512 px, plus scalable SVG from
  `assets/app-icon.svg`.
- An AppStream metainfo file, for software centres and Flathub.
- At run time:
  - `libvulkan.so.1` and a driver; the app panics without them, so check
    first or document it;
  - `libasound.so.2`;
  - `libxkbcommon` and `libxkbcommon-x11`, `libwayland-client`, the X11
    and xcb libraries, `libfontconfig` and `libudev.so.1`;
  - a running `xdg-desktop-portal` with a FileChooser backend, for the
    open dialog;
  - `curl`, unless replaced.
- glibc as old as the build machine's.

**What every Windows package needs**
- The exe with its icon resource and GPUI's manifest. Build it on
  Windows.
- `+crt-static`, so no VC++ redistributable.
- Direct3D 11, which every supported Windows has.
- Vulkan only for 3DS games, from the GPU driver.

### Linux

| Format | Needs | For | Against |
|---|---|---|---|
| Tarball | The binary, `.desktop` file, icons and an install script | Simplest | No integration, no updates; the user installs the libraries |
| AppImage | An AppDir with `AppRun`, the `.desktop` file and icon. Build with appimagetool and the static type2 runtime, so the user needs no `libfuse2`. Leave glibc, `libvulkan`, `libGL` and `libasound` to the host. Build on the oldest target (Ubuntu 22.04) | One file that runs on most distributions; separate x86-64 and arm64 files | No sandbox; updates only through zsync tooling |
| `.deb` | cargo-deb or cargo-packager's `deb`. `$auto` dependencies through `dpkg-shlibdeps` cope with Ubuntu 24.04 renaming `libasound2` to `libasound2t64`; add `curl` and `xdg-desktop-portal` | apt integration and clean removal | The Debian family only |
| Flatpak (Flathub) | `org.freedesktop.Platform`, which has the Vulkan loader, Mesa through its GL extension, and ALSA/Pulse. `finish-args`: `--device=dri`, `--device=input` with `--device-if=all:!has-input-device` for Flatpak before 1.15.6, `--socket=wayland`, `--socket=fallback-x11`, `--socket=pulseaudio`, `--share=network`, and `--filesystem=home` | Sandbox, updates, discoverability | The most work. Saves sit beside the ROM, so the document portal's per-file access isn't enough and Flathub must accept a home-folder permission. The downloaded 3DS core, FVX's Java and the game folders' mkxp-z players all run inside the sandbox's runtime |

### Windows

| Format | Needs | For | Against |
|---|---|---|---|
| Zip | The exe and licences | Simplest, portable | No Start menu entry or file types; SmartScreen warns on unsigned files |
| NSIS installer | cargo-packager's `nsis`: a per-user install to `%LOCALAPPDATA%\Programs`, Start menu, file types, uninstaller | No admin rights needed | Still needs signing |
| MSI | cargo-packager's `wix` (WiX v3) or cargo-wix | What IT departments expect | More configuration; usually per-machine |
| MSIX / Microsoft Store | A signed package. The Store signs it, and registration has been free for individuals since 2025-09-10 | Clean installs, updates, the Store | Packaged apps have their `%LOCALAPPDATA%` writes virtualized and their own identity. Test the 3DS helper and FVX's Java under that before choosing it |

**Signing.** Azure Artifact Signing (formerly Trusted Signing) costs $9.99
a month (Basic, 5,000 signatures), but individuals can use it only in the
US and Canada. Elsewhere an OV certificate costs about $150–300 a year.
Neither gives instant SmartScreen trust; reputation builds as signed
releases ship.

### Recommendation

1. **Linux:** an AppImage for x86-64 and for arm64, plus a `.deb`, both
   from one cargo-packager config, built on Ubuntu 22.04 runners.
   Flathub once the app has settled on its file access.
2. **Windows:** a signed NSIS installer and a zip, from the same config,
   built on a Windows runner with the Windows SDK. An MSI only if asked
   for; the Store or MSIX later.
3. Keep the packaging files in a `packaging/` folder and a release
   workflow of their own, so they never touch CI or the app's code.

## 5. Windows test compute

GitHub refuses the Windows and macOS jobs today because the account's
Actions budget is $0 (PR #54). The Linux jobs run on Namespace's Developer
plan.

| Option | Cost | Notes |
|---|---|---|
| Raise the GitHub Actions budget | `windows-latest` (2 cores) $0.010 a minute; Linux $0.006, macOS $0.062. Free accounts include $12 of compute a month, Pro $18 | Nothing to change in `ci.yml`. 100 runs of 30 minutes a month is about $30 |
| Namespace Team plan | $100 a month for 100,000 unit minutes. Windows counts ×2: a 4 vCPU runner is $0.008 a minute prepaid, $0.012 over | Windows runners went generally available on 2026-08-05, "included in current standard Team, Business, and Business+ plans", so not Developer. Keeps CI on one provider, raises Linux concurrency, has Windows cache volumes and RDP for debugging. The same 3,000 minutes at 4 vCPU uses 24,000 unit minutes |
| WarpBuild | Windows 4 vCPU $0.016 a minute (no 2 vCPU size since 2026-06-08), per minute, no base fee; $10 credit at signup | Windows Server 2022 and 2025 |
| Depot | Windows 2 vCPU about $0.008 a minute (a third-party table); plans from $20 for 2,000 minutes | Not confirmed on Depot's own pages |
| Azure Pipelines | One free Microsoft-hosted job for private projects: 1,800 minutes a month, 60 minutes a run, with the organization linked to an Azure subscription | A second CI system with its own YAML |
| Make the repository public | Standard GitHub-hosted runners, Windows and macOS included, are free for public repositories | The owner's call, tied to the planned public launch |
| Self-hosted runner | Free on GitHub's side today (a 2026 per-minute fee for self-hosted runners was announced, then postponed) | A Windows PC with a GPU, or Windows 11 on Arm in Parallels on this Mac (DirectX 11 in the VM; x86-64 builds run under emulation, so timings don't count). Another machine to keep running |

None of the hosted runners has a GPU. GPUI on them gets WARP, and Vulkan
isn't there, so 3DS games can only be smoke-tested with Azahar's software
renderer. Frame pacing, the upscaler and 3DS under Vulkan need a real
Windows machine.

**Recommendation.** For pull requests, Namespace's Team plan if the
$100 a month is acceptable, since Linux already runs there. Otherwise
raise the GitHub budget to about $30–40 for the Windows job alone. Either
way, the people doing the port need one real Windows PC with a GPU, or a
Parallels VM as a stopgap, for interactive testing.

## Work streams

Each stream owns the files listed, and no other stream edits them. A
stream that needs a change in another's file asks its owner, or waits
until that stream merges. Streams 4 and 5 keep the public APIs the app
calls unchanged (`emu_3ds::ThreeDs::open`, `data_dir`, `install::*`,
`is_game`; `emu_mkxp::Mkxp::open`, `player_app`, `SaveFolders`), so
`emulator.rs` needn't change for them.

| # | Stream | Owns | Depends on |
|---|---|---|---|
| 0 | **Foundation.** Pin `libc`. Add `+crt-static` for the msvc target. A Windows icon and `windows_subsystem`. A small shared crate for OS folders (`data_local_dir`, config, cache, and a `DS_EMULATOR_HOME` override), with every `dirs::` call site switched to it in one mechanical change | `Cargo.toml`, `Cargo.lock`, `.cargo/config.toml` (new), `crates/emu-os/` (new; name open), `crates/emu-app/Cargo.toml`, `crates/emu-app/build.rs` (new), `crates/emu-app/src/main.rs`, `assets/app-icon.ico` (new). For the `dirs` switch only: the call sites in §2 Paths, `crates/emu-headless/src/main.rs` and `crates/emu-guide/examples/ask.rs` | — (merge first) |
| 1 | **CI.** Already merged in PR #54. Next: the Windows job's `fxc.exe` (or `GPUI_FXC_PATH`), a Windows runner that works (§5), and an Xvfb smoke start of the app on Linux | `.github/workflows/ci.yml`, `.gitattributes`, `scripts/linux/install-build-deps.sh`, `scripts/linux/cross.sh`, `scripts/linux/in-container.sh`, `scripts/linux/Dockerfile` | 0 |
| 2 | **App shell.** `secondary-` keys and per-OS labels. The in-window menu. App-drawn window controls (Windows; client-side decorations on Wayland). Split open dialogs and show portal errors. Hide and About off macOS. Monospace and UI fonts. Editor and reveal wording | `crates/emu-app/src/{app.rs, view.rs, library_view.rs, store_view.rs, settings_window.rs, debugger_window.rs, randomize_window.rs, memory_viewer.rs, game_menu.rs, key_guide.rs, ui.rs, theme.rs, icons.rs, probe.rs, cheats.rs}`, `crates/emu-app/src/guide/**`, new modules for the menu and labels, `assets/icons/**` | 0 |
| 3 | **Files, archives and downloads.** 7z and RAR in process, and no `pkill`. `write_atomic` and file-name sanitising on Windows. `curl` from `PATH` with `CREATE_NO_WINDOW`, or an HTTP crate. FVX pins per OS, unzipping with `zip`, `java.exe` or `javaw.exe`. Guide's `curl`, device name and key file | `crates/emu-app/src/{archive.rs, leftovers.rs, persist.rs, boxart.rs, store.rs, romhack.rs, randomizer.rs, library.rs, openrouter.rs, rom_info.rs, game_db.rs}`, `crates/emu-guide/**`, `scripts/store-compat.py` | 0 |
| 4 | **3DS on Linux and Windows.** Pins per OS with no MoltenVK. The system Vulkan loader. The helper's stdout on Windows. Priority and role code only on macOS. `jit_fix` only for macOS arm64. A clear message on Linux arm64. Updates decision 0011 and `docs/research/3ds.md` | `crates/emu-3ds/**`, `docs/decisions/0011-3ds-games.md`, `docs/research/3ds.md` | 0 |
| 5 | **RPG Maker on Linux and Windows.** Finding the mkxp-z binary. Starting and stopping it without `open`. Hiding its window. TCP link on Windows. Sound on Windows. Platform checks in `bridge.rb`. The player icon. Updates decision 0008 | `crates/emu-mkxp/**`, `crates/emu-cores/src/lib.rs` (its `rpg_maker` re-export, only if needed), `crates/emu-app/src/app_icon.rs`, `docs/decisions/0008-rpg-maker-games.md` | 0 |
| 6 | **Timing, threads, sound and input.** Refresh rate for Windows, X11 and Wayland. Thread priority and power throttling. Checking `sleep_until` and the refresh lock on each OS. Audio and gamepad checks | `crates/emu-app/src/{window_chrome.rs, qos.rs, speed.rs, refresh.rs, emulator.rs, audio.rs, resample.rs, microphone.rs, gamepad.rs, meter.rs, trace.rs}` | 0 |
| 7 | **Picture on the GPU off macOS** (after 2 and 6 land). WGSL ports of Sharp Pixels and OmniScale, and FSR 1 for Smooth, through wgpu (or blade plus D3D11) | `crates/emu-app/src/{gpu_upscale.rs, picture.rs, filter.rs, video.rs}`, `crates/emu-app/src/shaders/**` | 0, 6 |
| 8 | **Packaging and releases.** `.desktop`, metainfo, icons, AppImage, `.deb`, NSIS and zip, signing, and a release workflow | `packaging/**` (new), `Packager.toml` (new), `.github/workflows/release.yml` (new) | 0, 2 |
| 9 | **GUI test tools.** Linux and Windows versions of `check-layout.sh` and `bench.sh` | `scripts/linux/{check-layout.sh, bench.sh, …}` (new), `scripts/windows/**` (new) | 0 (the home override), 2 (menu parts in `probe.rs`) |
| 10 | **Docs.** One table of folders per OS. Build instructions. PRD section 4. A new decision record for Linux and Windows. Shortcut tables. Lands last, from the notes in each stream's PR | `README.md`, `docs/ARCHITECTURE.md`, `docs/PRD.md`, `docs/ROADMAP.md`, `docs/design-system.md`, `docs/decisions/0001-rust-and-gpui.md`, `docs/decisions/0016-…` (new), `docs/scripting.md` | All |

Order: stream 0 first, as one small PR. Then 2, 3, 4, 5 and 6 in
parallel; stream 1 continues on its own. Stream 2 is the critical path:
until it lands, the app on Linux and Windows has no menu, no usable Open
dialog, and on Windows no window controls. Streams 7, 8 and 9 follow once
the app runs. Stream 10 lands last.

Smaller files nobody above lists, such as `layout.rs`, `controls.rs`,
`bindings.rs` and `settings.rs`, should need no OS changes. Whoever finds
otherwise coordinates through stream 0's owner.

## Open questions

- **RAR.** The `unrar` crate's bundled UnRAR code is under RARLAB's own
  licence, not one PRD §9 lists. Use it, shell out to an installed
  `unrar` or `7z`, or refuse `.rar` off macOS?
- **3DS on Linux arm64.** Build and host a GPL-3.0 core, or leave it out?
- **Hiding mkxp-z off macOS.** Which approach keeps frames coming
  (stream 5's first spike)?
- **Flatpak.** Will Flathub accept `--filesystem=home` for saves beside
  the ROM?
- **Windows CI.** Namespace Team, a raised GitHub budget, or a public
  repository?

## Sources

All fetched or read on 2026-11-06.

1. GPUI 0.2.2 source, `~/.cargo/registry/src/index.crates.io-*/gpui-0.2.2`:
   - `Cargo.toml.orig` and `build.rs`;
   - `src/platform.rs`, `src/platform/keystroke.rs`, `src/text_system.rs`
     and `src/window.rs`;
   - `src/platform/linux/{platform.rs, wayland/*, x11/*, text_system.rs}`;
   - `src/platform/windows/{platform.rs, window.rs, events.rs, directx_devices.rs, directx_renderer.rs, vsync.rs}`;
   - `src/platform/blade/*` and `resources/windows/*`.
2. blade-graphics 0.7.1 source: `src/lib.rs`, `src/vulkan/init.rs`,
   `src/vulkan/surface.rs`.
3. cpal 0.18.2 `Cargo.toml`; gilrs 0.11.2 `README.md`; gilrs-core 0.6.8
   `Cargo.toml`.
4. This repository at `d14a61e`: the files cited above.
5. Checks run on this Mac (Rust 1.96.1):
   - `cargo check -p xattr --target x86_64-unknown-linux-gnu` fails with
     E0425 `ENOATTR`;
   - `cargo update -p libc --precise 0.2.189 --dry-run` resolves cleanly;
   - `cargo check --release -p gpui --target x86_64-pc-windows-gnu` fails
     with a missing `shaders_bytes.rs`.
6. Azahar 2126.2 release, through GitHub's API, with the two cores
   downloaded and read with `llvm-objdump -p/-T` and `strings`:
   <https://github.com/azahar-emu/azahar/releases/tag/2126.2>
7. libretro buildbot, HEAD requests:
   <https://buildbot.libretro.com/nightly/linux/x86_64/latest/azahar_libretro.so.zip>,
   <https://buildbot.libretro.com/nightly/linux/aarch64/latest/azahar_libretro.so.zip> (404),
   <https://buildbot.libretro.com/nightly/windows/x86_64/latest/azahar_libretro.dll.zip>
8. mkxp-z: <https://github.com/mkxp-z/mkxp-z> (README,
   `meson_options.txt`, `.github/workflows/autobuild.yml`, tags) and the
   artifacts of
   <https://github.com/mkxp-z/mkxp-z/actions/runs/37110075912>
9. Ruby Feature #19135, "Support `UNIXSocket` on Windows":
   <https://bugs.ruby-lang.org/issues/19135>, listed under Ruby 3.2.0 in
   <https://github.com/ruby/ruby/blob/v3_2_0/NEWS.md> and absent from
   3.1.0's NEWS
10. FVX 1.6.2, with three zips downloaded and listed:
    <https://github.com/upr-fvx/universal-pokemon-randomizer-fvx/releases/tag/vFVX1.6.2>
11. Zed docs: <https://zed.dev/docs/linux>, <https://zed.dev/docs/windows>
12. rust-lang/rust#116461, "Windows: Support sub-millisecond sleep" (Rust
    1.75): <https://github.com/rust-lang/rust/pull/116461>
13. Namespace pricing and changelog: <https://namespace.so/pricing>,
    <https://namespace.so/changelog> (2026-08-05, "Windows runners are
    generally available"),
    <https://namespace.so/docs/solutions/github-actions/migration>
14. GitHub Actions runner pricing and billing:
    <https://docs.github.com/en/billing/reference/actions-runner-pricing>,
    <https://docs.github.com/en/billing/concepts/product-billing/github-actions>
15. WarpBuild Windows runners: <https://www.warpbuild.com/runners/windows>,
    <https://www.warpbuild.com/pricing>
16. Depot's Windows price (third party): <https://craci.com/compare/github-actions-runner-providers>,
    <https://cicdcost.com/github-actions-pricing-changes-2026> (also the
    postponed self-hosted fee)
17. Azure Pipelines parallel jobs and free tier:
    <https://learn.microsoft.com/en-us/azure/devops/pipelines/licensing/concurrent-jobs?view=azure-devops>
18. cargo-packager formats: <https://docs.rs/cargo-packager/latest/cargo_packager/enum.PackageFormat.html>,
    <https://docs.crabnebula.dev/packager/>
19. AppImage's static runtime: <https://github.com/AppImage/type2-runtime>
20. Flatpak permissions: <https://docs.flatpak.org/en/latest/sandbox-permissions.html>,
    <https://github.com/flatpak/flatpak/issues/5681>
21. Windows code signing:
    <https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options>,
    <https://techcommunity.microsoft.com/blog/microsoft-security-blog/trusted-signing-is-now-open-for-individual-developers-to-sign-up-in-public-previ/4273554>
22. Free Microsoft Store registration for individuals:
    <https://blogs.windows.com/windowsdeveloper/2025/09/10/free-developer-registration-for-individual-developers-on-microsoft-store/>
