//! Where per-game files live on disk, and how they are written.
//!
//! Battery saves and save states sit next to the ROM, as in mGBA and
//! melonDS: `game.nds` gets `game.sav` and `game.ss1`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The battery save file for `rom`.
pub fn backup_path(rom: &Path) -> PathBuf {
    rom.with_extension("sav")
}

/// The save state file for `rom` in `slot`.
pub fn state_path(rom: &Path, slot: u8) -> PathBuf {
    rom.with_extension(format!("ss{slot}"))
}

/// Replaces `path` with `bytes` so that a crash mid-write never leaves a
/// half-written file: the bytes go to a sibling temporary file first, which
/// is then renamed over `path`.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    fs::write(&temporary, bytes)?;
    fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_files_sit_next_to_the_rom() {
        let rom = Path::new("/games/platinum.nds");
        assert_eq!(backup_path(rom), Path::new("/games/platinum.sav"));
        assert_eq!(state_path(rom, 1), Path::new("/games/platinum.ss1"));
    }

    #[test]
    fn write_atomic_replaces_contents_and_leaves_no_temporary() {
        let dir = std::env::temp_dir().join(format!("emu-app-persist-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("game.sav");

        write_atomic(&path, b"first").expect("first write");
        write_atomic(&path, b"second").expect("second write");

        assert_eq!(fs::read(&path).expect("read back"), b"second");
        assert!(!dir.join("game.sav.tmp").exists());
        fs::remove_dir_all(&dir).expect("clean up");
    }
}
