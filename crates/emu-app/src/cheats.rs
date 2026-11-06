//! Cheat files: `game.cht` next to the ROM, edited in the user's own text
//! editor and read back when the app regains focus. The format is
//! described in the `emu-cheats` crate.

use std::fs;
use std::io;
use std::path::Path;

/// What a new cheat file starts with.
const TEMPLATE: &str = "\
# Cheats for this game. Each cheat is a [name] line followed by its codes.
# Add \"off\" after the name to switch a cheat off. Save this file and
# switch back to ds-emulator to apply it.
#
# The device is detected from the codes; to choose it, add a tag after the
# name: raw (ADDRESS:VALUE in hex, every system), gs (Game Boy GameShark),
# gg (Game Boy Game Genie), cb (CodeBreaker), gsa or gsa-raw (GameShark
# Advance / Action Replay v1/v2), par3 or par3-raw (Pro Action Replay v3),
# ards (Action Replay DS).
# Note how to activate a cheat in a comment under its name.
#
# [Infinite money] ards
# # Hold L+R.
# 94000130 FCFF0000
# 62111938 00000000
# B2111938 00000000
# 20001DE4 000000FF
# D2000000 00000000
";

/// Creates `path` with an explanatory template unless it exists.
pub fn create_if_missing(path: &Path) -> io::Result<()> {
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(_) => fs::write(path, TEMPLATE),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

/// Opens `path` in the default text editor.
pub fn open_in_editor(path: &Path) -> io::Result<()> {
    emu_os::process::open_in_text_editor(path)
}

/// The text of the cheat file at `path`, or `None` if there is none.
pub fn read(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

/// `text` with cheat `index` of the `cheats` it parses to switched on or
/// off, by adding or removing `off` on its `[name]` line; everything else
/// is kept as written. `None` if that cheat has no `[name]` line: the
/// codes before the first one.
pub fn set_enabled(text: &str, cheats: usize, index: usize, enabled: bool) -> Option<String> {
    let headers: Vec<usize> = text
        .lines()
        .enumerate()
        .filter(|(_, line)| line.trim_start().starts_with('['))
        .map(|(number, _)| number)
        .collect();
    // Codes before the first header parse as a cheat of their own.
    let unnamed = cheats.checked_sub(headers.len())?;
    let target = *headers.get(index.checked_sub(unnamed)?)?;
    let lines: Vec<String> = text
        .lines()
        .enumerate()
        .map(|(number, line)| {
            if number != target {
                return line.to_owned();
            }
            let Some((name, flags)) = line.split_once(']') else {
                return line.to_owned();
            };
            let mut words: Vec<&str> = flags
                .split_whitespace()
                .filter(|word| {
                    !word.eq_ignore_ascii_case("on") && !word.eq_ignore_ascii_case("off")
                })
                .collect();
            if !enabled {
                words.push("off");
            }
            let flags = words.join(" ");
            if flags.is_empty() {
                format!("{name}]")
            } else {
                format!("{name}] {flags}")
            }
        })
        .collect();
    let mut toggled = lines.join("\n");
    if text.ends_with('\n') {
        toggled.push('\n');
    }
    Some(toggled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_enabled_rewrites_only_the_cheats_name_line() {
        let text = "# mine\n[Money] ards\n94000130 FCFF0000\n[Walls] OFF gsa\n1234\n";
        assert_eq!(
            set_enabled(text, 2, 0, false).as_deref(),
            Some("# mine\n[Money] ards off\n94000130 FCFF0000\n[Walls] OFF gsa\n1234\n")
        );
        assert_eq!(
            set_enabled(text, 2, 1, true).as_deref(),
            Some("# mine\n[Money] ards\n94000130 FCFF0000\n[Walls] gsa\n1234\n")
        );
        assert_eq!(set_enabled(text, 2, 2, true), None);
    }

    #[test]
    fn codes_before_the_first_name_cannot_be_switched() {
        let text = "02000000:01\n[Plain]\n02000001:02";
        assert_eq!(set_enabled(text, 2, 0, false), None);
        assert_eq!(
            set_enabled(text, 2, 1, false).as_deref(),
            Some("02000000:01\n[Plain] off\n02000001:02")
        );
    }

    #[test]
    fn the_template_parses_to_no_cheats() {
        let cheats = emu_cheats::parse(emu_core::System::Gba, TEMPLATE).expect("valid");
        assert!(cheats.is_empty());
    }

    #[test]
    fn the_template_example_is_a_valid_ds_cheat() {
        let example: String = TEMPLATE
            .lines()
            .skip_while(|line| !line.starts_with("# [Infinite money]"))
            .map(|line| format!("{}\n", line.strip_prefix("# ").unwrap_or(line)))
            .collect();
        let cheats = emu_cheats::parse(emu_core::System::Nds, &example).expect("valid");
        assert_eq!(cheats.len(), 1);
        assert_eq!(cheats[0].device, emu_cheats::Device::ActionReplayDs);
    }

    #[test]
    fn creating_keeps_an_existing_file() {
        let dir = std::env::temp_dir().join(format!("emu-cheats-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("game.cht");
        fs::write(&path, "[Mine]\n").expect("write");
        create_if_missing(&path).expect("create");
        assert_eq!(fs::read_to_string(&path).expect("read"), "[Mine]\n");
        fs::remove_file(&path).expect("remove");
        create_if_missing(&path).expect("create");
        assert!(
            fs::read_to_string(&path)
                .expect("read")
                .starts_with("# Cheats")
        );
        fs::remove_dir_all(&dir).ok();
    }
}
