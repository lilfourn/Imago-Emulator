//! An RPG Maker game's icon as a PNG, for the library to show the game by
//! its player's icon. Blocking; run it off the UI thread.
//!
//! On macOS the player is a Mac app, whose icon file its `Info.plist`
//! names. Elsewhere it is a program, and the game's icon is the one its
//! Windows `Game.exe` carries in its resources, as Explorer shows it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;

/// Saves the icon of the player at `app` as a PNG at `to`: a Mac app's,
/// or a program's, preferring the icon of the `Game.exe` beside it.
///
/// # Errors
///
/// Fails if the player has no icon or it cannot be converted.
pub fn save(app: &Path, to: &Path) -> io::Result<()> {
    if app.is_dir() {
        return save_mac_app(app, to);
    }
    let beside = app.parent().and_then(game_exe);
    let icon = beside
        .iter()
        .chain([&app.to_owned()])
        .find_map(|program| fs::read(program).ok().and_then(|bytes| exe_icon(&bytes)))
        .ok_or_else(|| io::Error::other(format!("{} has no icon", app.display())))?;
    let image = image::load_from_memory_with_format(&icon, image::ImageFormat::Ico)
        .map_err(io::Error::other)?;
    if let Some(folder) = to.parent() {
        fs::create_dir_all(folder)?;
    }
    image
        .save_with_format(to, image::ImageFormat::Png)
        .map_err(io::Error::other)
}

/// The `Game.exe` in `folder`, in any case.
fn game_exe(folder: &Path) -> Option<PathBuf> {
    fs::read_dir(folder)
        .ok()?
        .filter_map(|entry| Some(entry.ok()?.path()))
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("Game.exe"))
                && path.is_file()
        })
}

/// Saves the icon of the Mac app at `app` as a PNG at `to`.
///
/// # Errors
///
/// Fails if the app names no icon file, or `sips` cannot convert it.
#[cfg(target_os = "macos")]
fn save_mac_app(app: &Path, to: &Path) -> io::Result<()> {
    // `plutil` reads both the XML and the binary form of Info.plist.
    let name = run(Command::new("plutil")
        .args(["-extract", "CFBundleIconFile", "raw", "-o", "-"])
        .arg(app.join("Contents/Info.plist")))?;
    let mut icon = app.join("Contents/Resources").join(name.trim());
    // Info.plist may leave out the extension.
    if icon.extension().is_none() {
        icon.set_extension("icns");
    }
    if let Some(folder) = to.parent() {
        fs::create_dir_all(folder)?;
    }
    // `sips` exits as if it worked when it cannot read the icon, so what
    // it leaves at `to` says whether it did.
    fs::remove_file(to).ok();
    run(Command::new("sips")
        .args(["-s", "format", "png"])
        .arg(&icon)
        .arg("--out")
        .arg(to))?;
    if !to.is_file() {
        return Err(io::Error::other(format!(
            "cannot convert {} to a PNG",
            icon.display()
        )));
    }
    Ok(())
}

/// Fails: a Mac app's icon is read with macOS's own tools, and a Mac app
/// plays only on macOS.
#[cfg(not(target_os = "macos"))]
fn save_mac_app(app: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("{} is a Mac app", app.display()),
    ))
}

/// Runs `command` to the end: what it printed, or if it failed, what it
/// said went wrong.
#[cfg(target_os = "macos")]
fn run(command: &mut Command) -> io::Result<String> {
    let output = command.output()?;
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(said.trim().to_owned()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Resource types, as Windows numbers them.
const RT_ICON: u32 = 3;
const RT_GROUP_ICON: u32 = 14;

/// The icon of the Windows program `exe`, its bytes, as an `.ico` file
/// with one picture: the largest of the program's first icon group, as
/// Explorer shows the program, with the most colours.
fn exe_icon(exe: &[u8]) -> Option<Vec<u8>> {
    let resources = Resources::of(exe)?;
    let group = resources.first(&[RT_GROUP_ICON])?;
    let count = usize::from(u16_at(group, 4)?);
    // GRPICONDIRENTRY: width, height, colours, 0, planes, bits per pixel,
    // size, and the RT_ICON resource's ID; 0 is 256 pixels wide.
    let entry = (0..count)
        .filter_map(|index| group.get(6 + index * 14..6 + index * 14 + 14))
        .max_by_key(|entry| {
            let size = |byte: u8| if byte == 0 { 256 } else { u32::from(byte) };
            (size(entry[0]), u16::from_le_bytes([entry[6], entry[7]]))
        })?;
    let id = u32::from(u16::from_le_bytes([entry[12], entry[13]]));
    let picture = resources.first(&[RT_ICON, id])?;
    let size = u32::try_from(picture.len()).ok()?;
    // ICONDIR, then its one ICONDIRENTRY: GRPICONDIRENTRY's first 12 bytes,
    // with the picture's size, and where it starts, after these 22 bytes.
    let mut ico = vec![0, 0, 1, 0, 1, 0];
    ico.extend(&entry[..8]);
    ico.extend(size.to_le_bytes());
    ico.extend(22_u32.to_le_bytes());
    ico.extend(picture);
    Some(ico)
}

/// A Windows program's resources: its `.rsrc` section, a tree of type, ID
/// and language.
struct Resources<'a> {
    exe: &'a [u8],
    /// Where the tree starts in the file, and at which address the program
    /// loads it.
    start: usize,
    address: u32,
}

impl<'a> Resources<'a> {
    /// The resources of `exe`, a Windows program, if it has any.
    fn of(exe: &'a [u8]) -> Option<Self> {
        if !exe.starts_with(b"MZ") {
            return None;
        }
        let pe = usize::try_from(u32_at(exe, 0x3C)?).ok()?;
        if exe.get(pe..pe + 4)? != b"PE\0\0" {
            return None;
        }
        let sections = usize::from(u16_at(exe, pe + 6)?);
        let optional = pe + 24;
        let optional_size = usize::from(u16_at(exe, pe + 20)?);
        // The data directories, after PE32's or PE32+'s fields.
        let directories = match u16_at(exe, optional)? {
            0x10B => optional + 96,
            0x20B => optional + 112,
            _ => return None,
        };
        let address = u32_at(exe, directories + 2 * 8)?;
        (0..sections)
            .map(|index| optional + optional_size + index * 40)
            .find_map(|section| {
                let virtual_address = u32_at(exe, section + 12)?;
                let size = u32_at(exe, section + 16)?;
                let raw = u32_at(exe, section + 20)?;
                let offset = address.checked_sub(virtual_address)?;
                (address != 0 && offset < size).then(|| Self {
                    exe,
                    start: usize::try_from(raw + offset).unwrap_or(usize::MAX),
                    address,
                })
            })
    }

    /// The data of the first resource along `path`, a type and then IDs,
    /// taking the first entry at each level the path leaves out.
    fn first(&self, path: &[u32]) -> Option<&'a [u8]> {
        let mut directory = 0;
        for level in 0..3 {
            let at = self.start + directory;
            let entries =
                usize::from(u16_at(self.exe, at + 12)?) + usize::from(u16_at(self.exe, at + 14)?);
            let entry = (0..entries)
                .map(|index| at + 16 + index * 8)
                .find(|&entry| {
                    path.get(level)
                        .is_none_or(|&id| u32_at(self.exe, entry) == Some(id))
                })?;
            let next = u32_at(self.exe, entry + 4)?;
            if next & 0x8000_0000 == 0 {
                // IMAGE_RESOURCE_DATA_ENTRY: where its data loads, and its
                // size.
                let data = self.start + usize::try_from(next).ok()?;
                let offset = u32_at(self.exe, data)?.checked_sub(self.address)?;
                let begin = self.start + usize::try_from(offset).ok()?;
                let size = usize::try_from(u32_at(self.exe, data + 4)?).ok()?;
                return self.exe.get(begin..begin.checked_add(size)?);
            }
            directory = usize::try_from(next & 0x7FFF_FFFF).ok()?;
        }
        None
    }
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ds-emulator-{name}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("temp folder");
        dir
    }

    /// A `size`×`size` PNG, all `rgba`.
    fn png(size: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut icon = Vec::new();
        let mut encoder = png::Encoder::new(&mut icon, size, size);
        encoder.set_color(png::ColorType::Rgba);
        encoder
            .write_header()
            .and_then(|mut writer| writer.write_image_data(&rgba.repeat((size * size) as usize)))
            .expect("encoded");
        icon
    }

    /// A resource of a test program: its ID and data.
    type Resource = (u32, Vec<u8>);

    /// A Windows program, PE32+, with nothing but `icons`, each its
    /// pictures (with their width, as an icon group says it) and the
    /// pictures' IDs, in a `.rsrc` section loaded at 0x2000.
    fn program_with_icons(icons: &[&[(u8, &Vec<u8>)]]) -> Vec<u8> {
        const ADDRESS: u32 = 0x2000;
        const RAW: usize = 0x200;
        // The tree: types, then IDs, then one language each, then the
        // data entries, then the data.
        let pictures: Vec<(u32, &[u8])> = icons
            .iter()
            .flat_map(|group| group.iter())
            .enumerate()
            .map(|(index, (_, data))| (index as u32 + 1, data.as_slice()))
            .collect();
        let mut groups = Vec::new();
        let mut next_id = 1;
        for group in icons {
            let mut directory = vec![0, 0, 1, 0];
            directory.extend((group.len() as u16).to_le_bytes());
            for (width, data) in group.iter() {
                directory.extend([*width, *width, 0, 0, 1, 0, 32, 0]);
                directory.extend((data.len() as u32).to_le_bytes());
                directory.extend((next_id as u16).to_le_bytes());
                next_id += 1;
            }
            groups.push(directory);
        }
        let kinds: [(u32, Vec<Resource>); 2] = [
            (
                RT_ICON,
                pictures
                    .iter()
                    .map(|(id, data)| (*id, data.to_vec()))
                    .collect(),
            ),
            (
                RT_GROUP_ICON,
                groups
                    .into_iter()
                    .enumerate()
                    .map(|(index, data)| (index as u32 + 1, data))
                    .collect(),
            ),
        ];
        let directory = |entries: usize| {
            let mut bytes = vec![0; 12];
            bytes.extend(0_u16.to_le_bytes());
            bytes.extend((entries as u16).to_le_bytes());
            bytes
        };
        // Lay out the directories first, then fill in their offsets.
        let leaves: usize = kinds.iter().map(|(_, items)| items.len()).sum();
        let types_size = 16 + 8 * kinds.len();
        let ids_size: usize = kinds.iter().map(|(_, items)| 16 + 8 * items.len()).sum();
        let languages_size = leaves * (16 + 8);
        let entries_at = types_size + ids_size + languages_size;
        let data_at = entries_at + leaves * 16;
        let mut tree = directory(kinds.len());
        let mut ids = Vec::new();
        let mut languages = Vec::new();
        let mut entries = Vec::new();
        let mut data = Vec::new();
        let mut id_at = types_size;
        let mut language_at = types_size + ids_size;
        let mut leaf = 0;
        for (kind, items) in &kinds {
            tree.extend(kind.to_le_bytes());
            tree.extend((0x8000_0000 | id_at as u32).to_le_bytes());
            ids.extend(directory(items.len()));
            for (id, bytes) in items {
                ids.extend(id.to_le_bytes());
                ids.extend((0x8000_0000 | language_at as u32).to_le_bytes());
                languages.extend(directory(1));
                languages.extend(1033_u32.to_le_bytes());
                languages.extend(((entries_at + leaf * 16) as u32).to_le_bytes());
                entries.extend((ADDRESS + (data_at + data.len()) as u32).to_le_bytes());
                entries.extend((bytes.len() as u32).to_le_bytes());
                entries.extend([0; 8]);
                data.extend(bytes);
                language_at += 24;
                leaf += 1;
            }
            id_at += 16 + 8 * items.len();
        }
        let section: Vec<u8> = [tree, ids, languages, entries, data].concat();
        let mut exe = vec![0; RAW];
        exe[..2].copy_from_slice(b"MZ");
        exe[0x3C] = 0x40;
        exe[0x40..0x44].copy_from_slice(b"PE\0\0");
        exe[0x46] = 1; // one section
        exe[0x54] = 240; // the size of PE32+'s optional header
        let optional = 0x58;
        exe[optional..optional + 2].copy_from_slice(&0x20B_u16.to_le_bytes());
        let resources = optional + 112 + 2 * 8;
        exe[resources..resources + 4].copy_from_slice(&ADDRESS.to_le_bytes());
        exe[resources + 4..resources + 8].copy_from_slice(&(section.len() as u32).to_le_bytes());
        let header = optional + 240;
        exe[header..header + 5].copy_from_slice(b".rsrc");
        exe[header + 12..header + 16].copy_from_slice(&ADDRESS.to_le_bytes());
        exe[header + 16..header + 20].copy_from_slice(&(section.len() as u32).to_le_bytes());
        exe[header + 20..header + 24].copy_from_slice(&(RAW as u32).to_le_bytes());
        exe.extend(section);
        exe
    }

    #[test]
    fn a_programs_first_icon_is_saved_at_its_largest() {
        let dir = temp_dir("exe-icon");
        let colour = [0x10, 0x20, 0x30, 0xFF];
        let small = png(16, [0xFF; 4]);
        let other = png(64, [0; 4]);
        // A 48×48 BMP picture, as older icons are: its header, with the
        // height doubled for the mask, then the pixels and the mask.
        let mut bmp = Vec::new();
        for field in [40_u32, 48, 96] {
            bmp.extend(field.to_le_bytes());
        }
        bmp.extend([1, 0, 32, 0]);
        bmp.extend([0; 24]);
        bmp.extend([0x30, 0x20, 0x10, 0xFF].repeat(48 * 48));
        bmp.extend([0; 48 * 8]);
        for large in [png(48, colour), bmp] {
            let exe = program_with_icons(&[&[(16, &small), (48, &large)], &[(64, &other)]]);
            let path = dir.join("Game.exe");
            fs::write(&path, exe).expect("program");
            let to = dir.join("icons/game.png");
            save(&path, &to).expect("saved");
            assert!(crate::boxart::is_png(&to));
            let saved = image::open(&to).expect("decoded").to_rgba8();
            assert_eq!(
                saved.dimensions(),
                (48, 48),
                "the largest of the first group"
            );
            assert_eq!(saved.get_pixel(0, 0).0, colour);
        }
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_game_exe_beside_a_player_gives_the_icon() {
        let dir = temp_dir("game-exe-icon");
        let player = dir.join(format!("mkxp-z.{}", std::env::consts::ARCH));
        fs::write(&player, b"\x7fELF").expect("player");
        let to = dir.join("icon.png");
        assert!(save(&player, &to).is_err(), "no Game.exe, no icon");
        let colour = [0x40, 0x80, 0xC0, 0xFF];
        let icon = png(32, colour);
        fs::write(dir.join("GAME.EXE"), program_with_icons(&[&[(32, &icon)]])).expect("Game.exe");
        save(&player, &to).expect("saved");
        let saved = image::open(&to).expect("decoded").to_rgba8();
        assert_eq!(saved.get_pixel(1, 1).0, colour);
        assert!(exe_icon(b"MZ not a program").is_none());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg_attr(
        not(target_os = "macos"),
        ignore = "macOS only: a Mac app's icon is read with plutil and sips, and a Mac app plays only on macOS; elsewhere the icon comes from Game.exe (tested above)"
    )]
    fn the_icon_named_in_info_plist_is_saved_as_a_png() {
        let dir = temp_dir("app-icon");
        let app = dir.join("Game.app");
        fs::create_dir_all(app.join("Contents/Resources")).expect("app folders");
        let plist = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
            <plist version=\"1.0\"><dict>\
            <key>CFBundleIconFile</key><string>icon.png</string>\
            </dict></plist>\n";
        fs::write(app.join("Contents/Info.plist"), plist).expect("Info.plist");
        fs::write(app.join("Contents/Resources/icon.png"), png(2, [0xFF; 4])).expect("icon");
        let to = dir.join("icons/game.png");
        save(&app, &to).expect("saved");
        assert!(crate::boxart::is_png(&to));
        fs::remove_file(app.join("Contents/Resources/icon.png")).expect("removed");
        assert!(save(&app, &dir.join("missing.png")).is_err());
        fs::remove_dir_all(&dir).ok();
    }
}
