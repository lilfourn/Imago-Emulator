//! What the app reads from a 3DS game file's headers: its format, whether
//! it is encrypted, its title and a CRC-32 that identifies it.
//!
//! Games are 2–4 GB, so only the few small regions the headers point at
//! are read: the first sector, the game partition's header, its ExeFS
//! header and the icon (SMDH) it holds. Offsets come from the file, so
//! every read is capped and every field is looked up with bounds checks;
//! a file cut short reads as one without the missing parts.
//!
//! The formats, all little-endian, sizes in media units of 0x200 bytes:
//!
//! - A cartridge image (NCSD) has `NCSD` at 0x100, its title ID at 0x108
//!   and a table of eight partitions at 0x120, each an offset and a size.
//!   The first partition is the game's executable.
//! - An executable (NCCH) has `NCCH` at 0x100, its program ID at 0x118,
//!   its product code at 0x150 and its flags at 0x188; the eighth flag's
//!   0x04 bit says its contents are not encrypted. Its ExeFS's offset and
//!   size are at 0x1A0, and the ExeFS's file named `icon` is the SMDH.
//! - Homebrew (3DSX) has `3DSX` at 0 and its header's size at 4; a header
//!   of at least 0x2C bytes gives the SMDH's offset and size at 0x20.
//! - An icon (SMDH) has `SMDH` at 0 and sixteen titles from 0x8, one per
//!   language, 0x200 bytes each, starting with a 0x80-byte UTF-16 name.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

/// How a 3DS game is packaged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// A cartridge image, NCSD (`.3ds`, `.cci`).
    Cartridge,
    /// One executable partition, NCCH (`.cxi`).
    Executable,
    /// Homebrew, 3DSX (`.3dsx`).
    Homebrew,
    /// One of Azahar's zstd-compressed forms (`.zcci`, `.zcxi`, `.z3dsx`).
    Compressed,
}

/// What a 3DS game file's headers say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// How the game is packaged.
    pub format: Format,
    /// Whether its executable partition is encrypted, which the core
    /// refuses. Always false for homebrew and compressed files.
    pub encrypted: bool,
    /// The English title from its icon (SMDH), if it has one the app can
    /// read: not when it is encrypted or compressed.
    pub title: Option<String>,
    /// Its 64-bit title ID (NCSD or NCCH program ID), if it has one.
    pub title_id: Option<u64>,
    /// Its product code, as `CTR-P-ABCD`, if it has one.
    pub product_code: Option<String>,
}

/// The unit the headers measure offsets and sizes in.
const MEDIA_UNIT: u64 = 0x200;
/// The size of an NCSD, NCCH or ExeFS header, and of what is read to tell
/// the formats apart.
const HEADER_SIZE: usize = 0x200;
/// The size of an icon (SMDH), the most read for one.
const SMDH_SIZE: usize = 0x36C0;
/// The NCCH flag, in its eighth flag byte, that says its contents are not
/// encrypted.
const NO_CRYPTO: u8 = 0x04;
/// The smallest 3DSX header that says where its icon is.
const EXTENDED_3DSX_HEADER: u16 = 0x2C;
/// The index of the English title among an icon's sixteen.
const ENGLISH: usize = 1;
/// The extensions of Azahar's compressed forms, which have nothing the app
/// can read without unpacking them.
const COMPRESSED: [&str; 3] = ["zcci", "zcxi", "z3dsx"];

/// Files up to this size are identified by all of their bytes.
const WHOLE_FILE_LIMIT: u64 = 64 << 20;
/// How much of a larger file identifies it, with its length.
const IDENTITY_PREFIX: u64 = 1 << 20;

/// Reads the headers of the 3DS game at `path`, without reading the rest.
///
/// # Errors
///
/// Fails if the file cannot be read or is not a 3DS game.
pub fn read(path: &Path) -> io::Result<Header> {
    let mut file = File::open(path)?;
    let start = read_at(&mut file, 0, HEADER_SIZE)?;
    if has_magic(&start, 0x100, b"NCSD") {
        cartridge(&mut file, &start)
    } else if has_magic(&start, 0x100, b"NCCH") {
        executable(&mut file, &start, 0, Format::Executable)
    } else if has_magic(&start, 0, b"3DSX") {
        homebrew(&mut file, &start)
    } else if is_compressed(path) {
        Ok(Header {
            format: Format::Compressed,
            encrypted: false,
            title: None,
            title_id: None,
            product_code: None,
        })
    } else {
        Err(invalid(format!(
            "{} is not a 3DS game: it is not a cartridge image, an executable or homebrew",
            path.display()
        )))
    }
}

/// A CRC-32 that identifies the game at `path` in save states, read
/// quickly even from a 4 GB cartridge image.
///
/// A file of at most 64 MiB is identified by all of its bytes; a larger
/// one by its first MiB followed by its length, as eight little-endian
/// bytes.
///
/// # Errors
///
/// Fails if the file cannot be read.
pub fn identity(path: &Path) -> io::Result<u32> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut hasher = crc32fast::Hasher::new();
    if len <= WHOLE_FILE_LIMIT {
        hash_all(&mut hasher, file)?;
    } else {
        hash_all(&mut hasher, file.take(IDENTITY_PREFIX))?;
        hasher.update(&len.to_le_bytes());
    }
    Ok(hasher.finalize())
}

/// The headers of a cartridge image whose first sector is `ncsd`: its
/// game partition's, with the cartridge's title ID.
fn cartridge(file: &mut File, ncsd: &[u8]) -> io::Result<Header> {
    let media_id = u64_at(ncsd, 0x108).filter(|&id| id != 0);
    let at = u64::from(u32_at(ncsd, 0x120).unwrap_or(0)) * MEDIA_UNIT;
    let ncch = read_at(file, at, HEADER_SIZE)?;
    let header = executable(file, &ncch, at, Format::Cartridge)?;
    Ok(Header {
        title_id: media_id.or(header.title_id),
        ..header
    })
}

/// The headers of the executable partition at `at`, whose header is
/// `ncch`, packaged as `format`.
fn executable(file: &mut File, ncch: &[u8], at: u64, format: Format) -> io::Result<Header> {
    if ncch.len() < HEADER_SIZE || !has_magic(ncch, 0x100, b"NCCH") {
        return Err(invalid(
            "the game's executable partition is missing or cut short".to_owned(),
        ));
    }
    let encrypted = ncch.get(0x18F).is_none_or(|flags| flags & NO_CRYPTO == 0);
    let title = if encrypted {
        None
    } else {
        executable_title(file, ncch, at)?
    };
    Ok(Header {
        format,
        encrypted,
        title,
        title_id: u64_at(ncch, 0x118).filter(|&id| id != 0),
        product_code: ncch.get(0x150..0x160).and_then(product_code),
    })
}

/// The title in the icon in the ExeFS of the unencrypted executable
/// partition at `at`, whose header is `ncch`.
fn executable_title(file: &mut File, ncch: &[u8], at: u64) -> io::Result<Option<String>> {
    let (Some(offset), Some(size)) = (u32_at(ncch, 0x1A0), u32_at(ncch, 0x1A4)) else {
        return Ok(None);
    };
    if size == 0 {
        return Ok(None);
    }
    let exefs = at + u64::from(offset) * MEDIA_UNIT;
    let files = read_at(file, exefs, HEADER_SIZE)?;
    match icon_entry(&files) {
        Some((offset, size)) => {
            icon_title(file, exefs + HEADER_SIZE as u64 + u64::from(offset), size)
        }
        None => Ok(None),
    }
}

/// The offset and size of the file named `icon` among an ExeFS's ten.
fn icon_entry(files: &[u8]) -> Option<(u32, u32)> {
    files.chunks_exact(16).take(10).find_map(|entry| {
        if until_nul(&entry[..8]) == b"icon" {
            Some((u32_at(entry, 8)?, u32_at(entry, 12)?))
        } else {
            None
        }
    })
}

/// The headers of homebrew whose header starts `header`.
fn homebrew(file: &mut File, header: &[u8]) -> io::Result<Header> {
    let extended = u16_at(header, 4).is_some_and(|size| size >= EXTENDED_3DSX_HEADER);
    let title = match (u32_at(header, 0x20), u32_at(header, 0x24)) {
        (Some(offset), Some(size)) if extended => icon_title(file, u64::from(offset), size)?,
        _ => None,
    };
    Ok(Header {
        format: Format::Homebrew,
        encrypted: false,
        title,
        title_id: None,
        product_code: None,
    })
}

/// The title in the icon of `size` bytes at `at`, reading no more of it
/// than an icon holds.
fn icon_title(file: &mut File, at: u64, size: u32) -> io::Result<Option<String>> {
    let len = usize::try_from(size).map_or(SMDH_SIZE, |size| size.min(SMDH_SIZE));
    Ok(smdh_title(&read_at(file, at, len)?))
}

/// The title in the icon `smdh`: its English name, or else the first name
/// it has in any language.
fn smdh_title(smdh: &[u8]) -> Option<String> {
    if !has_magic(smdh, 0, b"SMDH") {
        return None;
    }
    let name = |language: usize| {
        let at = 0x8 + language * 0x200;
        smdh.get(at..at + 0x80).and_then(utf16_text)
    };
    name(ENGLISH).or_else(|| (0..16).find_map(name))
}

/// The NUL-terminated UTF-16 text in `bytes`, its lines and runs of spaces
/// joined by single spaces, if it is not blank.
fn utf16_text(bytes: &[u8]) -> Option<String> {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .take_while(|&unit| unit != 0)
        .collect();
    let text = String::from_utf16_lossy(&units);
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then_some(text)
}

/// The NUL-padded ASCII product code in `bytes`, if it has one.
fn product_code(bytes: &[u8]) -> Option<String> {
    let code = std::str::from_utf8(until_nul(bytes)).ok()?.trim();
    (!code.is_empty() && code.chars().all(|c| c.is_ascii_graphic())).then(|| code.to_owned())
}

/// Whether `path` has the extension of one of Azahar's compressed forms.
fn is_compressed(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            COMPRESSED
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
}

/// Up to `len` bytes of `file` from `at`: fewer where the file ends first.
fn read_at(file: &mut File, at: u64, len: usize) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(at))?;
    let mut bytes = Vec::with_capacity(len);
    file.take(len as u64).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Feeds everything `reader` gives to `hasher`.
fn hash_all(hasher: &mut crc32fast::Hasher, mut reader: impl Read) -> io::Result<()> {
    let mut buffer = vec![0; 1 << 16];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(read) => hasher.update(&buffer[..read]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

/// Whether `bytes` holds `magic` at `at`.
fn has_magic(bytes: &[u8], at: usize, magic: &[u8]) -> bool {
    bytes.get(at..at + magic.len()) == Some(magic)
}

/// `bytes` up to its first NUL.
fn until_nul(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    &bytes[..end]
}

/// The little-endian `u16` at `at` in `bytes`, if it is all there.
fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

/// The little-endian `u32` at `at` in `bytes`, if it is all there.
fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// The little-endian `u64` at `at` in `bytes`, if it is all there.
fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

/// An error for a file that is not what it should be.
fn invalid(why: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    const TITLE_ID: u64 = 0x0004_0000_0012_3400;
    const PRODUCT_CODE: &str = "CTR-P-ABCD";

    /// A fresh, empty folder for `test`.
    fn temp_dir(test: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("emu-3ds-header-{test}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// Writes `bytes` into `image` at `at`, growing it as needed.
    fn put(image: &mut Vec<u8>, at: usize, bytes: &[u8]) {
        if image.len() < at + bytes.len() {
            image.resize(at + bytes.len(), 0);
        }
        image[at..at + bytes.len()].copy_from_slice(bytes);
    }

    /// An icon with each of `names` as the short title in its language.
    fn smdh(names: &[(usize, &str)]) -> Vec<u8> {
        let mut icon = vec![0; SMDH_SIZE];
        put(&mut icon, 0, b"SMDH");
        for &(language, name) in names {
            let text: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
            put(&mut icon, 0x8 + language * 0x200, &text);
            put(&mut icon, 0x8 + language * 0x200 + 0x80, b"L\0o\0n\0g\0");
        }
        icon
    }

    /// An executable partition, encrypted or not, whose ExeFS holds a
    /// code file and then `icon`.
    fn ncch(no_crypto: bool, icon: &[u8]) -> Vec<u8> {
        let mut image = vec![0; HEADER_SIZE];
        put(&mut image, 0x100, b"NCCH");
        put(&mut image, 0x118, &TITLE_ID.to_le_bytes());
        put(&mut image, 0x150, PRODUCT_CODE.as_bytes());
        put(&mut image, 0x18F, &[if no_crypto { NO_CRYPTO } else { 0 }]);
        // The ExeFS: its header, a small code file, then the icon.
        let exefs = 0x200;
        let icon_offset = 0x200;
        let exefs_size = HEADER_SIZE + icon_offset + icon.len();
        put(&mut image, 0x1A0, &1_u32.to_le_bytes());
        put(
            &mut image,
            0x1A4,
            &(exefs_size.div_ceil(0x200) as u32).to_le_bytes(),
        );
        put(&mut image, exefs, b".code\0\0\0");
        put(&mut image, exefs + 8, &0_u32.to_le_bytes());
        put(&mut image, exefs + 12, &0x10_u32.to_le_bytes());
        put(&mut image, exefs + 16, b"icon\0\0\0\0");
        put(&mut image, exefs + 24, &(icon_offset as u32).to_le_bytes());
        put(&mut image, exefs + 28, &(icon.len() as u32).to_le_bytes());
        put(&mut image, exefs + HEADER_SIZE, &[0xAA; 0x10]);
        put(&mut image, exefs + HEADER_SIZE + icon_offset, icon);
        image.resize(image.len().next_multiple_of(0x200), 0);
        image
    }

    /// A cartridge image whose game partition is `ncch`, two sectors in.
    fn ncsd(ncch: &[u8]) -> Vec<u8> {
        let mut image = vec![0; 0x400];
        put(&mut image, 0x100, b"NCSD");
        put(&mut image, 0x108, &TITLE_ID.to_le_bytes());
        put(&mut image, 0x120, &2_u32.to_le_bytes());
        put(
            &mut image,
            0x124,
            &((ncch.len() / 0x200) as u32).to_le_bytes(),
        );
        image.extend_from_slice(ncch);
        image
    }

    /// Homebrew with a header of `header_size` bytes and `icon` after its
    /// (empty) code.
    fn threedsx(header_size: u16, icon: &[u8]) -> Vec<u8> {
        let mut image = vec![0; 0x100];
        put(&mut image, 0, b"3DSX");
        put(&mut image, 4, &header_size.to_le_bytes());
        put(&mut image, 0x20, &0x100_u32.to_le_bytes());
        put(&mut image, 0x24, &(icon.len() as u32).to_le_bytes());
        image.extend_from_slice(icon);
        image
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, bytes).expect("write game");
        path
    }

    #[test]
    fn a_cartridge_image_gives_its_ids_and_english_title() {
        let dir = temp_dir("cartridge");
        let icon = smdh(&[(0, "テストゲーム"), (ENGLISH, "Test Game")]);
        let path = write(&dir, "game.3ds", &ncsd(&ncch(true, &icon)));
        let header = read(&path).expect("read");
        assert_eq!(
            header,
            Header {
                format: Format::Cartridge,
                encrypted: false,
                title: Some("Test Game".to_owned()),
                title_id: Some(TITLE_ID),
                product_code: Some(PRODUCT_CODE.to_owned()),
            }
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_encrypted_cartridge_has_ids_but_no_title() {
        let dir = temp_dir("encrypted");
        let icon = smdh(&[(ENGLISH, "Test Game")]);
        let path = write(&dir, "game.cci", &ncsd(&ncch(false, &icon)));
        let header = read(&path).expect("read");
        assert_eq!(header.format, Format::Cartridge);
        assert!(header.encrypted);
        assert_eq!(header.title, None);
        assert_eq!(header.title_id, Some(TITLE_ID));
        assert_eq!(header.product_code.as_deref(), Some(PRODUCT_CODE));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_bare_ncch_is_an_executable() {
        let dir = temp_dir("executable");
        let icon = smdh(&[(ENGLISH, "Test Game")]);
        // The content decides, whatever the extension says.
        let path = write(&dir, "game.3ds", &ncch(true, &icon));
        let header = read(&path).expect("read");
        assert_eq!(header.format, Format::Executable);
        assert!(!header.encrypted);
        assert_eq!(header.title.as_deref(), Some("Test Game"));
        assert_eq!(header.title_id, Some(TITLE_ID));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_first_title_stands_in_for_a_missing_english_one() {
        let dir = temp_dir("languages");
        let icon = smdh(&[(ENGLISH, "  "), (3, "Jeu\nde test"), (5, "Spiel")]);
        let path = write(&dir, "game.cxi", &ncch(true, &icon));
        assert_eq!(
            read(&path).expect("read").title.as_deref(),
            Some("Jeu de test")
        );
        let blank = write(&dir, "blank.cxi", &ncch(true, &smdh(&[])));
        assert_eq!(read(&blank).expect("read").title, None);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn homebrew_takes_its_title_from_an_extended_header() {
        let dir = temp_dir("homebrew");
        let icon = smdh(&[(ENGLISH, "Homebrew\nApp")]);
        let path = write(&dir, "app.3dsx", &threedsx(EXTENDED_3DSX_HEADER, &icon));
        assert_eq!(
            read(&path).expect("read"),
            Header {
                format: Format::Homebrew,
                encrypted: false,
                title: Some("Homebrew App".to_owned()),
                title_id: None,
                product_code: None,
            }
        );
        let plain = write(&dir, "plain.3dsx", &threedsx(0x20, &icon));
        let header = read(&plain).expect("read");
        assert_eq!(header.format, Format::Homebrew);
        assert_eq!(header.title, None);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn compressed_files_are_known_by_their_extension() {
        let dir = temp_dir("compressed");
        for name in ["game.zcci", "game.ZCXI", "app.z3dsx"] {
            let path = write(&dir, name, b"\x28\xB5\x2F\xFDcompressed");
            let header = read(&path).expect("read");
            assert_eq!(header.format, Format::Compressed, "{name}");
            assert!(!header.encrypted);
            assert_eq!(header.title, None);
            assert_eq!(header.title_id, None);
        }
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn other_files_are_not_3ds_games() {
        let dir = temp_dir("garbage");
        for (name, bytes) in [
            ("game.3ds", &[0x55; 0x400][..]),
            ("empty.cci", &[][..]),
            ("game.nds", &[0; 0x200][..]),
        ] {
            let path = write(&dir, name, bytes);
            let error = read(&path).expect_err(name);
            assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{name}");
        }
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn files_cut_short_do_not_panic() {
        let dir = temp_dir("cut-short");
        let icon = smdh(&[(ENGLISH, "Test Game")]);
        let images = [
            ("game.3ds", Format::Cartridge, ncsd(&ncch(true, &icon))),
            ("game.cxi", Format::Executable, ncch(true, &icon)),
            (
                "app.3dsx",
                Format::Homebrew,
                threedsx(EXTENDED_3DSX_HEADER, &icon),
            ),
        ];
        for (name, format, image) in images {
            let path = dir.join(name);
            for len in (0..image.len()).step_by(0x40).chain([image.len() - 1]) {
                fs::write(&path, &image[..len]).expect("write");
                // An error will do as well as a header, as long as it is
                // not a panic.
                if let Ok(header) = read(&path) {
                    assert_eq!(header.format, format, "{name} cut to {len:#x}");
                }
            }
        }
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn absurd_offsets_and_sizes_are_survived() {
        let dir = temp_dir("absurd");
        let mut homebrew = threedsx(EXTENDED_3DSX_HEADER, &[]);
        put(&mut homebrew, 0x20, &u32::MAX.to_le_bytes());
        put(&mut homebrew, 0x24, &u32::MAX.to_le_bytes());
        let path = write(&dir, "app.3dsx", &homebrew);
        assert_eq!(read(&path).expect("read").title, None);

        let mut cartridge = ncsd(&ncch(true, &smdh(&[(ENGLISH, "Test Game")])));
        put(&mut cartridge, 0x120, &u32::MAX.to_le_bytes());
        let path = write(&dir, "game.3ds", &cartridge);
        assert_eq!(
            read(&path).expect_err("no partition").kind(),
            io::ErrorKind::InvalidData
        );

        let mut executable = ncch(true, &smdh(&[(ENGLISH, "Test Game")]));
        put(&mut executable, 0x1A0, &u32::MAX.to_le_bytes());
        let path = write(&dir, "game.cxi", &executable);
        assert_eq!(read(&path).expect("read").title, None);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_small_file_is_identified_by_all_of_it() {
        let dir = temp_dir("identity-small");
        let bytes: Vec<u8> = (0..200_000_u32).map(|i| (i % 251) as u8).collect();
        let path = write(&dir, "game.3dsx", &bytes);
        assert_eq!(identity(&path).expect("identity"), crc32fast::hash(&bytes));
        let other = write(&dir, "other.3dsx", &bytes[1..]);
        assert_ne!(
            identity(&other).expect("identity"),
            identity(&path).expect("identity")
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_large_file_is_identified_by_its_start_and_length() {
        let dir = temp_dir("identity-large");
        let path = dir.join("game.3ds");
        let file = File::create(&path).expect("create");
        // Sparse: four gigabytes of nothing, written in no time.
        file.set_len(4 << 30).expect("grow");
        let first = identity(&path).expect("identity");
        assert_eq!(identity(&path).expect("identity again"), first);
        let mut expected = crc32fast::Hasher::new();
        expected.update(&vec![0; 1 << 20]);
        expected.update(&(4_u64 << 30).to_le_bytes());
        assert_eq!(first, expected.finalize());
        file.set_len((4 << 30) + 0x200).expect("grow more");
        assert_ne!(identity(&path).expect("identity"), first, "length counts");
        fs::remove_dir_all(&dir).ok();
    }
}
