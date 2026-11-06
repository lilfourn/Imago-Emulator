//! Fixes a bug in the shader JIT of Azahar 2126.2's core for Apple
//! silicon, in the installed core.
//!
//! The JIT's `LG2` splits the exponent off its input and converts it to a
//! float with `UCVTF`, as an unsigned number, so the exponent of any input
//! below 1, which is negative, comes out as about 4.29 billion. Shaders
//! raise numbers below 1 to a power as `EX2(n * LG2(x))`, which then
//! overflows: Pokémon Omega Ruby draws Magikarp white, on the Vulkan and
//! software renderers alike. The core's interpreter and its x86-64 JIT
//! (`cvtsi2ss`) convert the exponent as signed and draw it right. The
//! arm64 JIT has had this bug since it was written for Citra in 2023, and
//! Azahar's code still has it.
//!
//! The core puts each instruction it emits together in a register, so the
//! fix changes the `MOVK` that makes `UCVTF S4, S4` (`0x7E21D884`) into
//! one that makes `SCVTF S4, S4` (`0x5E21D884`). It does so at both places
//! the core adds it to the code, the second used when the code buffer
//! grows. macOS checks each 4 KiB page of a library against its code
//! signature's hashes as it maps the page, so the fix also rehashes the
//! one page both sit in. The core's signature is ad hoc, made by the
//! linker without a key, so the fixed core is signed just as the original
//! was.
//!
//! Only the pinned core is touched, and only into the pinned result: the
//! digests before and after are both checked. The fixed core replaces the
//! original by a rename, so a helper still running the original keeps it.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use sha2::{Digest, Sha256};

/// The size of the pages a code signature hashes.
const PAGE: usize = 4096;

/// Words to change in a library, all to the same new value, and where its
/// code signature keeps its page hashes.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Fix<'a> {
    /// The library's SHA-256 before the fix, in lowercase hex.
    before: &'a str,
    /// Its SHA-256 after.
    after: &'a str,
    /// Where each little-endian word to change is.
    words: &'a [usize],
    /// What those words hold before the fix.
    from: u32,
    /// What they hold after.
    to: u32,
    /// Where the code signature's SHA-256 of the first page is; each
    /// page's follows the one before.
    page_hashes: usize,
}

/// The fix for Azahar 2126.2's arm64 `azahar_libretro.dylib`.
pub(crate) const LOG2: Fix<'static> = Fix {
    before: "61052384000aef22abaed712e46e87160d58999e99c2c6d84bc77587f04417ea",
    after: "1efb6c5ec8ecfa2e8c9f6cc4a8d2dea166f426fe58587c2bb8f102c4d363f047",
    words: &[0xB7_27B0, 0xB7_2880],
    // MOVK W8, #0x7E21, LSL #16
    from: 0x72AF_C428,
    // MOVK W8, #0x5E21, LSL #16
    to: 0x72AB_C428,
    // The code directory is at 0x1215D44, and its hashes 110 bytes in.
    page_hashes: 0x121_5DB2,
};

impl Fix<'_> {
    /// Whether the library at `path` may still want the fix: its first
    /// word to change holds what it did before. Cheap enough to ask each
    /// time a game opens.
    pub(crate) fn wanted(&self, path: &Path) -> io::Result<bool> {
        let mut word = [0; 4];
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(self.words[0] as u64))?;
        match file.read_exact(&mut word) {
            Ok(()) => Ok(u32::from_le_bytes(word) == self.from),
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Fixes the library at `path`, if it is the one this fix is for, and
    /// says whether it did.
    pub(crate) fn apply(&self, path: &Path) -> io::Result<bool> {
        let Some(fixed) = self.fixed(&fs::read(path)?) else {
            return Ok(false);
        };
        let temp = path.with_extension("fixing");
        fs::write(&temp, fixed)?;
        crate::install::set_mode(&temp, 0o755)?;
        fs::rename(&temp, path)?;
        Ok(true)
    }

    /// `library` with the fix, if it is the library this fix is for and
    /// comes out as the pinned result.
    fn fixed(&self, library: &[u8]) -> Option<Vec<u8>> {
        if sha256(library) != self.before {
            return None;
        }
        let mut bytes = library.to_vec();
        for &at in self.words {
            bytes
                .get_mut(at..at + 4)?
                .copy_from_slice(&self.to.to_le_bytes());
        }
        for &at in self.words {
            let page = at / PAGE;
            let hash = Sha256::digest(bytes.get(page * PAGE..(page + 1) * PAGE)?);
            let slot = self.page_hashes + page * hash.len();
            bytes
                .get_mut(slot..slot + hash.len())?
                .copy_from_slice(&hash);
        }
        (sha256(&bytes) == self.after).then_some(bytes)
    }
}

/// The SHA-256 of `bytes`, in lowercase hex.
fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const FROM: u32 = 0x72AF_C428;
    const TO: u32 = 0x72AB_C428;
    /// Words in the second of three pages.
    const WORDS: [usize; 2] = [PAGE + 8, PAGE + 1000];

    /// Three pages of code and a hash of each after them, like a signed
    /// library's.
    fn library() -> Vec<u8> {
        let mut bytes: Vec<u8> = (0..3 * PAGE).map(|i| (i * 7 % 251) as u8).collect();
        for at in WORDS {
            bytes[at..at + 4].copy_from_slice(&FROM.to_le_bytes());
        }
        for page in 0..3 {
            let hash = Sha256::digest(&bytes[page * PAGE..(page + 1) * PAGE]);
            bytes.extend_from_slice(&hash);
        }
        bytes
    }

    /// `library()` fixed by hand: the words changed and their page's hash
    /// made again.
    fn fixed_by_hand() -> Vec<u8> {
        let mut bytes = library();
        for at in WORDS {
            bytes[at..at + 4].copy_from_slice(&TO.to_le_bytes());
        }
        let hash = Sha256::digest(&bytes[PAGE..2 * PAGE]);
        bytes[3 * PAGE + 32..3 * PAGE + 64].copy_from_slice(&hash);
        bytes
    }

    /// A fix for `library()`, whose digests are kept by the caller.
    fn fix<'a>(before: &'a str, after: &'a str) -> Fix<'a> {
        Fix {
            before,
            after,
            words: &WORDS,
            from: FROM,
            to: TO,
            page_hashes: 3 * PAGE,
        }
    }

    fn temp_file(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("emu-3ds-fix-{test}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("create temp dir");
        dir.join("azahar_libretro.dylib")
    }

    #[test]
    fn changes_the_words_and_rehashes_only_their_page() {
        let (before, after) = (sha256(&library()), sha256(&fixed_by_hand()));
        let fixed = fix(&before, &after).fixed(&library()).expect("fixed");
        assert_eq!(fixed, fixed_by_hand());
        let first_and_last = |bytes: &[u8]| {
            let hashes = &bytes[3 * PAGE..];
            (hashes[..32].to_vec(), hashes[64..].to_vec())
        };
        assert_eq!(first_and_last(&fixed), first_and_last(&library()));
    }

    #[test]
    fn leaves_any_other_library_alone() {
        let (before, after) = (sha256(&library()), sha256(&fixed_by_hand()));
        let mut other = library();
        other[0] ^= 1;
        assert_eq!(fix(&before, &after).fixed(&other), None, "another library");
        assert_eq!(
            fix(&before, &before).fixed(&library()),
            None,
            "a result other than the pinned one"
        );
        assert_eq!(fix(&before, &after).fixed(&library()[..PAGE]), None);
    }

    #[test]
    fn replaces_the_file_and_a_running_copy_keeps_the_original() {
        let path = temp_file("apply");
        fs::write(&path, library()).expect("write");
        let (before, after) = (sha256(&library()), sha256(&fixed_by_hand()));
        let fix = fix(&before, &after);
        assert!(fix.wanted(&path).expect("wanted"));
        let mut running = File::open(&path).expect("open");

        assert!(fix.apply(&path).expect("apply"));
        assert_eq!(fs::read(&path).expect("read"), fixed_by_hand());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&path).expect("metadata").permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
        }
        let mut kept = Vec::new();
        running.read_to_end(&mut kept).expect("read the original");
        assert_eq!(kept, library());
        assert!(!path.with_extension("fixing").exists());

        assert!(!fix.wanted(&path).expect("wanted"), "fixed already");
        assert!(!fix.apply(&path).expect("apply again"));
        fs::write(&path, b"too short to hold the words").expect("write");
        assert!(!fix.wanted(&path).expect("a short file"));
        fs::remove_dir_all(path.parent().expect("dir")).ok();
    }
}
