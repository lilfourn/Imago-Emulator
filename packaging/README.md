# Packages for Linux and Windows

Imago Emulator's Linux and Windows packages, as research
[§4](../docs/research/linux-windows.md#4-packaging-and-distribution)
recommends. The Mac's disk image is made separately, by
`scripts/macos/release.sh` ([decision 0016](../docs/decisions/0016-public-release.md));
nothing here changes it. Every package ships the same license and notices
as the Mac app.

| File | What it is |
|---|---|
| `Imago-Emulator-VERSION-linux-x86_64.deb`, `…-linux-aarch64.deb` | For Ubuntu 22.04 and later (the workflow tests 22.04 and 24.04), and Debian 12 and later. `apt install ./FILE` installs what it needs; `apt remove imago-emulator` removes it |
| `Imago-Emulator-VERSION-linux-x86_64.AppImage`, `…-linux-aarch64.AppImage` | One file for most distributions with glibc 2.35 or later: make it executable and open it |
| `Imago-Emulator-VERSION-windows-x86_64.exe` | The installer, for Windows 10 and 11 on x86-64 |
| `Imago-Emulator-VERSION-windows-x86_64-portable.zip` | The same files, to run from any folder |

The version is the workspace's, in `Cargo.toml`. The binary, the Linux
desktop ID and the data folders keep the name `ds-emulator`, as on the Mac.

## Building them

`.github/workflows/release-linux-windows.yml` builds, packages and checks
every one. Run it from Actions > Run workflow, or
`gh workflow run release-linux-windows.yml --ref BRANCH`; the packages are
the run's artifacts. It doesn't run on pull requests, because GitHub's
Windows minutes are paid.

To release a version: raise `version` in `Cargo.toml`, merge, and push the
tag `vVERSION`. The workflow checks that the tag names that version, and
puts the packages and `SHA256SUMS-linux-windows-VERSION.txt` on a draft
GitHub release. Add the Mac's disk image to the draft, then publish it.

By hand:

- **Linux**, on Ubuntu 22.04 (the oldest system the packages are for) of
  the architecture to package:
  ```sh
  scripts/linux/install-build-deps.sh
  sudo apt-get install dpkg-dev binutils patchelf desktop-file-utils appstream file
  cargo build --release --locked -p emu-app
  packaging/notices.sh x86_64-unknown-linux-gnu target/notices   # needs cargo-about
  packaging/linux/package.sh --notices target/notices target/release/ds-emulator
  ```
  From a Mac, build with `scripts/linux/cross.sh build --release --locked -p emu-app`
  (`DS_LINUX_ARCH=x86_64` for x86-64), write the notices on the Mac, and
  run `package.sh` in a container of that architecture with the tools
  above. Those packages need glibc 2.39, cross.sh's Ubuntu 24.04.
- **Windows**, with Visual Studio's C++ tools, the Windows SDK and NSIS 3:
  ```powershell
  cargo build --release --locked -p emu-app   # GPUI_FXC_PATH if fxc.exe isn't SDK 10.0.26100.0's
  bash packaging/notices.sh x86_64-pc-windows-msvc target/notices
  pwsh packaging/windows/package.ps1 -Exe target/release/ds-emulator.exe -Notices target/notices
  ```

Packages go in `target/dist`.

`packaging/linux/smoke.sh` installs a Linux package on a clean system and
plays a test ROM under Xvfb; `packaging/windows/smoke.ps1` installs,
starts and uninstalls the Windows ones. The workflow runs them on Ubuntu
22.04, Ubuntu 24.04 and Fedora 44 containers, and on its Windows runner.

`packaging/icons.sh` remakes the Linux icons and `assets/app-icon.ico`
from `assets/icon.svg`.

## Linux

Both packages install the same files:

| Where (under `/usr`, or the AppImage's `usr`) | What |
|---|---|
| `bin/ds-emulator` | The app, without its line tables (symbols are kept for crash reports) |
| `share/applications/ds-emulator.desktop` | Named after GPUI's app ID, so the desktop matches the window to it |
| `share/metainfo/com.lilfourn.ds-emulator.metainfo.xml` | AppStream's description, for software centres |
| `share/icons/hicolor/*/apps/ds-emulator.png`, `scalable/…/ds-emulator.svg` | 16 to 512 px |
| `share/imago-emulator/` | `License.txt`, `Acknowledgements.html` and `Source/seed_tables.rs` |
| `share/doc/imago-emulator/copyright` | The license again, where Debian keeps it; minimal systems drop the rest of `/usr/share/doc` |

**What the app needs from the system.** The `.deb` asks apt for each one:

- glibc 2.35 or later; ALSA (`libasound2`) for sound; udev for gamepads;
  xcb and xkbcommon for X11, and `libwayland-client` on Wayland;
- Vulkan (`libvulkan1`) and a driver: Mesa's, or NVIDIA's. GPUI draws
  with it, and the app can't start without one;
- `curl`, for box art, the store, Guide, the randomizer and the 3DS core.

It recommends `xdg-desktop-portal` (the open dialog is the desktop
portal's), `libarchive-tools` (bsdtar unpacks `.rar` games) and
`xdg-utils` (opening the cheat file and spoiler logs in a text editor),
and suggests 7-Zip and unrar, which the app also tries for `.rar`. The
randomizer brings its own Java.

**What the AppImage carries.** Only xkbcommon, xkbcommon-x11 and
xcb-xkb, which not every Wayland desktop installs, with their licenses
in `usr/share/doc`. It takes the rest from the system, as it must: Vulkan
and its driver, glibc, ALSA, udev, xcb and Wayland have to be the
system's own to work with its GPU, sound server and display. The
AppImage uses the static type 2 runtime, so it needs no `libfuse2`; where
FUSE can't mount it (some containers), run it with
`--appimage-extract-and-run`.

3DS games need an x86-64 PC: Azahar makes no core for ARM Linux, and the
app says so.

## Windows

The installer installs for the current user, with no administrator
rights:

- the app in `%LOCALAPPDATA%\Programs\Imago Emulator`, with `License.txt`,
  `Acknowledgements.html` and `Source\seed_tables.rs` beside it;
- a Start menu shortcut, and an uninstaller in Settings > Apps.

`/S` installs without asking, and `/D=FOLDER` picks the folder. The
uninstaller (`Uninstall.exe /S` for no questions) leaves the player's
saves, settings and library, which are elsewhere
(`%LOCALAPPDATA%\ds-emulator`, `%APPDATA%\ds-emulator`, and saves beside
each game).

`ds-emulator.exe` has the app's icon and version information
(`crates/emu-app/build.rs`) and GPUI's manifest. It links the C runtime
in, so it needs no Visual C++ Redistributable. It draws with Direct3D 11;
3DS games need Vulkan, which GPU drivers install.

A release build is a GUI program, so no console window opens with it.
Started from a command prompt, a `--script`'s lines and command-line
errors print in that prompt (`crates/emu-app/src/console.rs`). The prompt
doesn't wait for the app, so use `start /wait ds-emulator.exe …` to keep
them in order. A pipe or a file (`ds-emulator.exe … > log.txt`) gets
them too. Debug builds stay console programs.

**Not signed yet.** There is no code-signing certificate, so:

- Edge or Chrome may say the download "isn't commonly downloaded". Choose
  Keep (in Edge, under ⋯).
- Opening the installer or the portable exe shows SmartScreen's "Windows
  protected your PC" with "Unknown publisher". Choose More info, then Run
  anyway.

Signing needs Azure Artifact Signing (individuals in the US and Canada)
or an OV certificate (research §4). Even signed, SmartScreen's warning
fades only as a signed release builds reputation.

## What's left

- Code signing on Windows, and AppImage signing and update information.
- Flatpak, once the app has settled how it reaches saves beside the ROM.
- Windows file associations and an MSI; Windows on Arm.
