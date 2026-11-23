//! What the app reads from a 3DS game file's headers: its format, whether
//! it is encrypted, its title, its extdata ID and a CRC-32 that identifies
//! it.
//!
//! Games are 2–4 GB, so only the few small regions the headers point at
//! are read: the first sector, the game partition's header and extended
//! header, its ExeFS header and the icon (SMDH) it holds. Offsets come
//! from the file, so every read is capped and every field is looked up
//! with bounds checks; a file cut short reads as one without the missing
//! parts.
//!
//! The formats, all little-endian, sizes in media units of 0x200 bytes:
//!
//! - A cartridge image (NCSD) has `NCSD` at 0x100, its title ID at 0x108
//!   and a table of eight partitions at 0x120, each an offset and a size.
//!   The first partition is the game's executable.
//! - An executable (NCCH) has `NCCH` at 0x100, its size at 0x104, its
//!   program ID at 0x118, its product code at 0x150, its extended header's
//!   size at 0x180 and its flags at 0x188; the eighth flag's 0x04 bit says
//!   its contents are not encrypted. Its ExeFS's offset and size are at
//!   0x1A0, and the ExeFS's file named `icon` is the SMDH.
//! - The extended header follows the executable's header, at 0x200, and
//!   is encrypted with the rest. Its access control info, at 0x200 in it,
//!   holds storage info at 0x30, eight bytes of which, at 0x230 in the
//!   extended header, are the game's extdata ID. An application may open
//!   only the extdata its extended header names, so an ID of zero means it
//!   has none. When bit 1 of the storage info's last byte is set
//!   (extended save data access), its first and third eight bytes hold six
//!   20-bit save IDs instead, three each, from the top bits down; the app
//!   takes the first that is set, from the third eight bytes first, as
//!   Azahar does.
//! - An installable title (CIA) holds its executable as its first content,
//!   read as any executable is (see [`crate::cia`]).
//! - Homebrew (3DSX) has `3DSX` at 0 and its header's size at 4; a header
//!   of at least 0x2C bytes gives the SMDH's offset and size at 0x20.
//! - An icon (SMDH) has `SMDH` at 0 and sixteen titles from 0x8, one per
//!   language, 0x200 bytes each, starting with a 0x80-byte UTF-16 name.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use crate::cia;

/// How a 3DS game is packaged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// A cartridge image, NCSD (`.3ds`, `.cci`).
    Cartridge,
    /// One executable partition, NCCH (`.cxi`).
    Executable,
    /// An installable title, CIA (`.cia`), which plays as the executable
    /// it holds (see [`crate::cia`]).
    Installable,
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
    /// The ID of the extra data (extdata) it may keep beside its save
    /// data, from its executable's extended header, if it names one and is
    /// not encrypted.
    pub extdata_id: Option<u64>,
}

/// The unit the headers measure offsets and sizes in.
pub(crate) const MEDIA_UNIT: u64 = 0x200;
/// The size of an NCSD, NCCH or ExeFS header, and of what is read to tell
/// the formats apart.
pub(crate) const HEADER_SIZE: usize = 0x200;
/// The size of the part of an executable's extended header that the app
/// reads: its system control info and access control info.
const EXHEADER_SIZE: usize = 0x400;
/// Where the storage info is in an extended header: in its access control
/// info, at 0x200, the ARM11 local system capabilities' storage info, at
/// 0x30.
const STORAGE_INFO: usize = 0x230;
/// The size of the storage info.
const STORAGE_INFO_SIZE: usize = 0x20;
/// The bit, in the storage info's last byte, that says it lists save IDs
/// the game may open instead of its extdata ID.
const EXTENDED_SAVE_ACCESS: u8 = 0x02;
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
/// A `.cia`, which has no magic number of its own, is known by its
/// extension; other files by their contents.
///
/// # Errors
///
/// Fails if the file cannot be read or is not a 3DS game.
pub fn read(path: &Path) -> io::Result<Header> {
    let mut file = File::open(path)?;
    if cia::is_cia(path) {
        return installable(&mut file, path);
    }
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
            extdata_id: None,
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
    let encrypted = is_encrypted(ncch);
    let (title, extdata_id) = if encrypted {
        (None, None)
    } else {
        (
            executable_title(file, ncch, at)?,
            executable_extdata(file, ncch, at)?,
        )
    };
    Ok(Header {
        format,
        encrypted,
        title,
        title_id: u64_at(ncch, 0x118).filter(|&id| id != 0),
        product_code: ncch.get(0x150..0x160).and_then(product_code),
        extdata_id,
    })
}

/// The headers of the installable title (CIA) at `path`, open as `file`:
/// its executable's, with the title ID its title metadata gives if the
/// executable has none. Of an executable encrypted with the title key,
/// nothing more can be read.
fn installable(file: &mut File, path: &Path) -> io::Result<Header> {
    let layout = cia::layout(file, path)?;
    if layout.encrypted {
        return Ok(Header {
            format: Format::Installable,
            encrypted: true,
            title: None,
            title_id: layout.title_id,
            product_code: None,
            extdata_id: None,
        });
    }
    let ncch = read_at(file, layout.offset, HEADER_SIZE)?;
    let header = executable(file, &ncch, layout.offset, Format::Installable)?;
    Ok(Header {
        title_id: header.title_id.or(layout.title_id),
        ..header
    })
}

/// Whether the executable partition whose header is `ncch` is encrypted:
/// whether its NoCrypto flag is clear.
pub(crate) fn is_encrypted(ncch: &[u8]) -> bool {
    ncch.get(0x18F).is_none_or(|flags| flags & NO_CRYPTO == 0)
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
    let Some(exefs) = at.checked_add(u64::from(offset) * MEDIA_UNIT) else {
        return Ok(None);
    };
    let files = read_at(file, exefs, HEADER_SIZE)?;
    match icon_entry(&files) {
        Some((offset, size)) => {
            icon_title(file, exefs + HEADER_SIZE as u64 + u64::from(offset), size)
        }
        None => Ok(None),
    }
}

/// The extdata ID in the extended header of the unencrypted executable
/// partition at `at`, whose header is `ncch`, if it has an extended header
/// that names one.
fn executable_extdata(file: &mut File, ncch: &[u8], at: u64) -> io::Result<Option<u64>> {
    let has_exheader = u32_at(ncch, 0x180)
        .is_some_and(|size| usize::try_from(size).is_ok_and(|size| size >= EXHEADER_SIZE));
    if !has_exheader {
        return Ok(None);
    }
    let exheader = read_at(file, at + HEADER_SIZE as u64, EXHEADER_SIZE)?;
    Ok(extdata_id(&exheader))
}

/// The extdata ID in the storage info of the extended header `exheader`,
/// if it names one (see the module's notes).
fn extdata_id(exheader: &[u8]) -> Option<u64> {
    let storage = exheader.get(STORAGE_INFO..STORAGE_INFO + STORAGE_INFO_SIZE)?;
    let extdata = u64_at(storage, 0)?;
    let id = if storage[STORAGE_INFO_SIZE - 1] & EXTENDED_SAVE_ACCESS == 0 {
        extdata
    } else {
        let save_ids = [u64_at(storage, 0x10)?, extdata];
        save_ids
            .into_iter()
            .flat_map(|packed| [40, 20, 0].map(|shift| (packed >> shift) & 0xF_FFFF))
            .find(|&id| id != 0)?
    };
    (id != 0).then_some(id)
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
        extdata_id: None,
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
pub(crate) fn read_at(file: &mut File, at: u64, len: usize) -> io::Result<Vec<u8>> {
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
pub(crate) fn has_magic(bytes: &[u8], at: usize, magic: &[u8]) -> bool {
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
pub(crate) fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// The little-endian `u64` at `at` in `bytes`, if it is all there.
pub(crate) fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

/// An error for a file that is not what it should be.
fn invalid(why: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::cia::tests::{PLAIN, cia};
    use std::fs;
    use std::path::PathBuf;

    pub(crate) const TITLE_ID: u64 = 0x0004_0000_0012_3400;
    /// The extdata ID the test executables' extended headers name.
    pub(crate) const EXTDATA_ID: u64 = 0x0001_23AB;
    const PRODUCT_CODE: &str = "CTR-P-ABCD";

    /// A fresh, empty folder for `test`.
    pub(crate) fn temp_dir(test: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("emu-3ds-header-{test}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// Writes `bytes` into `image` at `at`, growing it as needed.
    pub(crate) fn put(image: &mut Vec<u8>, at: usize, bytes: &[u8]) {
        if image.len() < at + bytes.len() {
            image.resize(at + bytes.len(), 0);
        }
        image[at..at + bytes.len()].copy_from_slice(bytes);
    }

    /// An icon with each of `names` as the short title in its language.
    pub(crate) fn smdh(names: &[(usize, &str)]) -> Vec<u8> {
        let mut icon = vec![0; 0x36C0];
        put(&mut icon, 0, b"SMDH");
        for &(language, name) in names {
            let text: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
            put(&mut icon, 0x8 + language * 0x200, &text);
            put(&mut icon, 0x8 + language * 0x200 + 0x80, b"L\0o\0n\0g\0");
        }
        icon
    }

    /// An executable partition, encrypted or not, whose extended header
    /// names [`EXTDATA_ID`] and whose ExeFS holds a code file and then
    /// `icon`.
    pub(crate) fn ncch(no_crypto: bool, icon: &[u8]) -> Vec<u8> {
        let mut image = vec![0; 0x200];
        put(&mut image, 0x100, b"NCCH");
        put(&mut image, 0x118, &TITLE_ID.to_le_bytes());
        put(&mut image, 0x150, PRODUCT_CODE.as_bytes());
        put(&mut image, 0x18F, &[if no_crypto { 0x04 } else { 0 }]);
        // The extended header, its storage info naming the extdata.
        put(&mut image, 0x180, &0x400_u32.to_le_bytes());
        put(&mut image, 0x200, b"TestGame");
        put(&mut image, 0x200 + 0x230, &EXTDATA_ID.to_le_bytes());
        // The ExeFS, after it: its header, a small code file, then the icon.
        let exefs = 0x200 + 0x400;
        let icon_offset = 0x200;
        let exefs_size = 0x200 + icon_offset + icon.len();
        put(&mut image, 0x1A0, &((exefs / 0x200) as u32).to_le_bytes());
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
        put(&mut image, exefs + 0x200, &[0xAA; 0x10]);
        put(&mut image, exefs + 0x200 + icon_offset, icon);
        image.resize(image.len().next_multiple_of(0x200), 0);
        let units = (image.len() / 0x200) as u32;
        put(&mut image, 0x104, &units.to_le_bytes());
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
        let icon = smdh(&[(0, "テストゲーム"), (1, "Test Game")]);
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
                extdata_id: Some(EXTDATA_ID),
            }
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_encrypted_cartridge_has_ids_but_no_title() {
        let dir = temp_dir("encrypted");
        let icon = smdh(&[(1, "Test Game")]);
        let path = write(&dir, "game.cci", &ncsd(&ncch(false, &icon)));
        let header = read(&path).expect("read");
        assert_eq!(header.format, Format::Cartridge);
        assert!(header.encrypted);
        assert_eq!(header.title, None);
        assert_eq!(header.title_id, Some(TITLE_ID));
        assert_eq!(header.product_code.as_deref(), Some(PRODUCT_CODE));
        assert_eq!(header.extdata_id, None, "its extended header is encrypted");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_installable_title_is_read_from_its_executable() {
        let dir = temp_dir("installable");
        let executable = ncch(true, &smdh(&[(1, "Test Game")]));
        let path = write(&dir, "game.CIA", &cia(&[(0, PLAIN, Some(&executable))]));
        assert_eq!(
            read(&path).expect("read"),
            Header {
                format: Format::Installable,
                encrypted: false,
                title: Some("Test Game".to_owned()),
                title_id: Some(TITLE_ID),
                product_code: Some(PRODUCT_CODE.to_owned()),
                extdata_id: Some(EXTDATA_ID),
            }
        );
        // Without the extension, it is not taken for one.
        let renamed = write(&dir, "game.3ds", &fs::read(&path).expect("read"));
        assert!(read(&renamed).is_err());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_encrypted_title_has_its_id_but_nothing_more() {
        let dir = temp_dir("installable-encrypted");
        let executable = ncch(true, &smdh(&[(1, "Test Game")]));
        let path = write(&dir, "game.cia", &cia(&[(0, 1, Some(&executable))]));
        assert_eq!(
            read(&path).expect("read"),
            Header {
                format: Format::Installable,
                encrypted: true,
                title: None,
                title_id: Some(TITLE_ID),
                product_code: None,
                extdata_id: None,
            }
        );
        let unflagged = write(
            &dir,
            "unflagged.cia",
            &cia(&[(0, PLAIN, Some(&ncch(false, &smdh(&[]))))]),
        );
        let header = read(&unflagged).expect("read");
        assert!(header.encrypted);
        assert_eq!(header.product_code.as_deref(), Some(PRODUCT_CODE));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_extdata_id_comes_from_the_extended_header() {
        let mut exheader = vec![0; 0x400];
        assert_eq!(extdata_id(&exheader), None, "zero: no extdata");
        put(&mut exheader, 0x230, &0x55D_u64.to_le_bytes());
        assert_eq!(extdata_id(&exheader), Some(0x55D));
        assert_eq!(extdata_id(&exheader[..0x230 + 8]), None, "cut short");
        assert_eq!(extdata_id(&[]), None);
    }

    #[test]
    fn extended_save_data_access_takes_the_first_save_id_listed() {
        let mut exheader = vec![0; 0x400];
        put(&mut exheader, 0x230 + 0x1F, &[0x02]);
        assert_eq!(extdata_id(&exheader), None, "none listed");
        // Three 20-bit IDs in each field, from the top: the field at 0x10
        // comes first.
        let packed = |ids: [u64; 3]| (ids[0] << 40) | (ids[1] << 20) | ids[2];
        put(
            &mut exheader,
            0x230,
            &packed([0x111, 0x222, 0x333]).to_le_bytes(),
        );
        assert_eq!(extdata_id(&exheader), Some(0x111));
        put(
            &mut exheader,
            0x230 + 0x10,
            &packed([0, 0x444, 0]).to_le_bytes(),
        );
        assert_eq!(extdata_id(&exheader), Some(0x444));
    }

    #[test]
    fn an_executable_without_an_extended_header_has_no_extdata() {
        let dir = temp_dir("no-exheader");
        let mut executable = ncch(true, &smdh(&[(1, "Test Game")]));
        put(&mut executable, 0x180, &0_u32.to_le_bytes());
        let path = write(&dir, "game.cxi", &executable);
        let header = read(&path).expect("read");
        assert_eq!(header.extdata_id, None);
        assert_eq!(header.title.as_deref(), Some("Test Game"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_bare_ncch_is_an_executable() {
        let dir = temp_dir("executable");
        let icon = smdh(&[(1, "Test Game")]);
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
        let icon = smdh(&[(1, "  "), (3, "Jeu\nde test"), (5, "Spiel")]);
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
        let icon = smdh(&[(1, "Homebrew\nApp")]);
        let path = write(&dir, "app.3dsx", &threedsx(0x2C, &icon));
        assert_eq!(
            read(&path).expect("read"),
            Header {
                format: Format::Homebrew,
                encrypted: false,
                title: Some("Homebrew App".to_owned()),
                title_id: None,
                product_code: None,
                extdata_id: None,
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
            ("game.cia", &[0x55; 0x4000][..]),
            ("empty.cia", &[][..]),
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
        let icon = smdh(&[(1, "Test Game")]);
        let images = [
            ("game.3ds", Format::Cartridge, ncsd(&ncch(true, &icon))),
            ("game.cxi", Format::Executable, ncch(true, &icon)),
            ("app.3dsx", Format::Homebrew, threedsx(0x2C, &icon)),
            (
                "game.cia",
                Format::Installable,
                cia(&[(0, PLAIN, Some(&ncch(true, &icon)))]),
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
        let mut homebrew = threedsx(0x2C, &[]);
        put(&mut homebrew, 0x20, &u32::MAX.to_le_bytes());
        put(&mut homebrew, 0x24, &u32::MAX.to_le_bytes());
        let path = write(&dir, "app.3dsx", &homebrew);
        assert_eq!(read(&path).expect("read").title, None);

        let mut cartridge = ncsd(&ncch(true, &smdh(&[(1, "Test Game")])));
        put(&mut cartridge, 0x120, &u32::MAX.to_le_bytes());
        let path = write(&dir, "game.3ds", &cartridge);
        assert_eq!(
            read(&path).expect_err("no partition").kind(),
            io::ErrorKind::InvalidData
        );

        let mut executable = ncch(true, &smdh(&[(1, "Test Game")]));
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
