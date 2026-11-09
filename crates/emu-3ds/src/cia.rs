//! Installable titles (CIA): the executable inside one, written out as a
//! `.cxi` the core opens.
//!
//! Azahar's core cannot install a title, but an unencrypted CIA's first
//! content is a plain executable (NCCH), which the core opens as it opens
//! a `.cxi`. [`extract`] writes that content out a piece at a time, as a
//! CIA can be gigabytes. A title whose executable is encrypted is refused:
//! nothing here decrypts anything, and the app holds no keys.
//!
//! The format, from 3dbrew's "CIA" and "Title metadata" pages:
//!
//! - The header is little-endian. Its own size is at 0 (0x2020); the sizes
//!   of the certificate chain, ticket, title metadata (TMD) and meta are at
//!   0x8, 0xC, 0x10 and 0x14, four bytes each; the contents' total size is
//!   at 0x18, eight bytes; and from 0x20, a 0x2000-byte bitmap says which
//!   contents the file holds, by index, content 0 in its first byte's top
//!   bit.
//! - The certificate chain, ticket, TMD, contents and meta follow the
//!   header in that order, each starting at a multiple of 64 bytes.
//! - The TMD is big-endian. Its signature type comes first and sets the
//!   size of the signature block. The TMD's header follows, with the title
//!   ID at 0x4C and the number of contents at 0x9E; then 64 content info
//!   records of 0x24 bytes, and a 0x30-byte chunk record for each content:
//!   its ID, index, type and size, at 0, 4, 6 and 8. Content 0 is the
//!   executable, and the type's 0x0001 bit says it is encrypted with the
//!   title key.
//! - The contents the file holds are stored in their chunk records' order,
//!   each starting at a multiple of 64 bytes.

use std::fs::{self, File};
use std::io::{self, BufWriter, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};

use crate::header::{self, HEADER_SIZE, MEDIA_UNIT, has_magic, read_at, u32_at, u64_at};

/// The size of a CIA's header with its bitmap of contents, the least a
/// header can be.
const CIA_HEADER_SIZE: usize = 0x2020;
/// Where the bitmap of the contents the file holds starts.
const CONTENT_BITMAP: usize = 0x20;
/// What every section and content starts at a multiple of.
const ALIGNMENT: u64 = 64;
/// The size of a TMD's header, after its signature block.
const TMD_HEADER_SIZE: usize = 0xC4;
/// The size of a TMD's 64 content info records, after its header.
const CONTENT_INFO_SIZE: usize = 0x24 * 64;
/// The size of one content chunk record.
const CHUNK_RECORD_SIZE: usize = 0x30;
/// The largest a TMD can be: the largest signature block, the header, the
/// content info records and a chunk record for each of 65,535 contents.
/// No more of one is read.
const MAX_TMD_SIZE: usize =
    0x240 + TMD_HEADER_SIZE + CONTENT_INFO_SIZE + CHUNK_RECORD_SIZE * 0xFFFF;
/// The chunk record type bit that says a content is encrypted with the
/// title key.
const ENCRYPTED_CONTENT: u16 = 0x0001;
/// How much of an earlier extraction is compared with the title's
/// executable to tell whether it is the same one: its start, which holds
/// its header and the hashes that cover the rest.
const COMPARED: u64 = 1 << 20;
/// The size of the buffer the executable is written through.
const WRITE_BUFFER: usize = 1 << 20;

/// Why a CIA's executable could not be written out.
#[derive(Debug, thiserror::Error)]
pub enum CiaError {
    /// The title's executable is encrypted, which the core refuses; the
    /// app decrypts nothing.
    #[error("{} is encrypted: Azahar plays only decrypted games", .0.display())]
    Encrypted(PathBuf),
    /// The file is not an installable title the app can read.
    #[error("{} is not a 3DS title the app can open: {why}", path.display())]
    Invalid {
        /// The file.
        path: PathBuf,
        /// What is wrong with it.
        why: &'static str,
    },
    /// A file could not be read or written.
    #[error("cannot {action} {}", path.display())]
    Io {
        /// `read` or `write`.
        action: &'static str,
        /// The file.
        path: PathBuf,
        /// What went wrong.
        source: io::Error,
    },
}

impl CiaError {
    /// The error for a failure to `action` (`read` or `write`) `path`.
    fn io(action: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            action,
            path: path.to_owned(),
            source,
        }
    }

    /// The error for the file at `path`, which is not a title the app can
    /// read, because of `why`.
    fn invalid(path: &Path, why: &'static str) -> Self {
        Self::Invalid {
            path: path.to_owned(),
            why,
        }
    }
}

impl From<CiaError> for io::Error {
    fn from(error: CiaError) -> Self {
        match error {
            CiaError::Io { source, .. } => source,
            other => io::Error::new(io::ErrorKind::InvalidData, other.to_string()),
        }
    }
}

/// Where a CIA keeps its executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Layout {
    /// The title ID its TMD gives, if it is not zero.
    pub title_id: Option<u64>,
    /// Where its executable, content 0, starts in the file.
    pub offset: u64,
    /// The executable's size in bytes, as its chunk record gives it.
    pub size: u64,
    /// Whether the executable is encrypted with the title key.
    pub encrypted: bool,
}

/// Where the sections a CIA's header places start, and their sizes.
struct Sections {
    tmd: u64,
    tmd_size: u64,
    contents: u64,
    contents_size: u64,
}

/// One content's chunk record in a TMD.
#[derive(Debug, Clone, Copy)]
struct Chunk {
    index: u16,
    kind: u16,
    size: u64,
}

/// Whether `path` names an installable title, by its extension.
#[must_use]
pub fn is_cia(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cia"))
}

/// Writes the executable in the installable title at `cia` into the folder
/// `games` as a `.cxi` named after it, and returns where it is. An
/// executable written out before is used as it is; a different one of that
/// name is left alone, and this one is numbered: `Game (2).cxi`.
///
/// Blocking file I/O, a whole executable's worth the first time; call it
/// off the UI thread.
///
/// # Errors
///
/// Fails if the title's executable is encrypted, the file is not a title
/// the app can read, or a file cannot be read or written.
pub fn extract(cia: &Path, games: &Path) -> Result<PathBuf, CiaError> {
    let reading = |source| CiaError::io("read", cia, source);
    let mut file = File::open(cia).map_err(reading)?;
    let layout = layout(&mut file, cia)?;
    check_executable(&mut file, cia, &layout)?;
    let stem = cia.file_stem().unwrap_or_default().to_string_lossy();
    for number in 1.. {
        let path = if number == 1 {
            games.join(format!("{stem}.cxi"))
        } else {
            games.join(format!("{stem} ({number}).cxi"))
        };
        match same_executable(&path, &mut file, &layout) {
            Ok(true) => return Ok(path),
            Ok(false) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(CiaError::io("read", &path, error)),
        }
        let writing = |source| CiaError::io("write", &path, source);
        fs::create_dir_all(games).map_err(writing)?;
        write_out(&mut file, &layout, &path).map_err(writing)?;
        return Ok(path);
    }
    unreachable!("a free name is always found")
}

/// Reads where the installable title at `path`, open as `file`, keeps its
/// executable.
pub(crate) fn layout(file: &mut File, path: &Path) -> Result<Layout, CiaError> {
    let invalid = |why| CiaError::invalid(path, why);
    let reading = |source| CiaError::io("read", path, source);
    let header = read_at(file, 0, CIA_HEADER_SIZE).map_err(reading)?;
    let sections = sections(&header).map_err(invalid)?;
    let tmd_size =
        usize::try_from(sections.tmd_size).map_or(MAX_TMD_SIZE, |size| size.min(MAX_TMD_SIZE));
    let tmd = read_at(file, sections.tmd, tmd_size).map_err(reading)?;
    let (title_id, chunks) = chunk_records(&tmd).map_err(invalid)?;
    let (at, chunk) = executable_chunk(&chunks, &header[CONTENT_BITMAP..]).map_err(invalid)?;
    if at
        .checked_add(chunk.size)
        .is_none_or(|end| end > sections.contents_size)
    {
        return Err(invalid("its executable runs past its contents"));
    }
    Ok(Layout {
        title_id: (title_id != 0).then_some(title_id),
        offset: sections.contents + at,
        size: chunk.size,
        encrypted: chunk.kind & ENCRYPTED_CONTENT != 0,
    })
}

/// Where the TMD and the contents start in the CIA whose header, with its
/// bitmap, is `header`, and their sizes.
fn sections(header: &[u8]) -> Result<Sections, &'static str> {
    const OUT_OF_RANGE: &str = "its header's sizes are out of range";
    let size = |at| u32_at(header, at).map(u64::from);
    let (Some(own), Some(certificates), Some(ticket), Some(tmd_size), Some(contents_size)) = (
        size(0),
        size(0x8),
        size(0xC),
        size(0x10),
        u64_at(header, 0x18),
    ) else {
        return Err("its header is cut short");
    };
    if header.len() < CIA_HEADER_SIZE || own < CIA_HEADER_SIZE as u64 {
        return Err("its header is cut short");
    }
    let after = |start: u64, size: u64| start.checked_add(size).and_then(aligned);
    let tmd = aligned(own)
        .and_then(|chain| after(chain, certificates))
        .and_then(|tickets| after(tickets, ticket))
        .ok_or(OUT_OF_RANGE)?;
    let contents = after(tmd, tmd_size).ok_or(OUT_OF_RANGE)?;
    contents.checked_add(contents_size).ok_or(OUT_OF_RANGE)?;
    Ok(Sections {
        tmd,
        tmd_size,
        contents,
        contents_size,
    })
}

/// The title ID and the chunk records in the TMD `tmd`.
fn chunk_records(tmd: &[u8]) -> Result<(u64, Vec<Chunk>), &'static str> {
    const CUT_SHORT: &str = "its title metadata is cut short";
    let kind = be_u32(tmd, 0).ok_or(CUT_SHORT)?;
    let header =
        signature_block(kind).ok_or("its title metadata's signature is of an unknown type")?;
    let title_id = be_u64(tmd, header + 0x4C).ok_or(CUT_SHORT)?;
    let count = usize::from(be_u16(tmd, header + 0x9E).ok_or(CUT_SHORT)?);
    let start = header + TMD_HEADER_SIZE + CONTENT_INFO_SIZE;
    let records = tmd
        .get(start..start + count * CHUNK_RECORD_SIZE)
        .ok_or(CUT_SHORT)?;
    let chunks = records
        .chunks_exact(CHUNK_RECORD_SIZE)
        .map(|record| {
            Some(Chunk {
                index: be_u16(record, 4)?,
                kind: be_u16(record, 6)?,
                size: be_u64(record, 8)?,
            })
        })
        .collect::<Option<Vec<_>>>()
        .ok_or(CUT_SHORT)?;
    Ok((title_id, chunks))
}

/// The size of a TMD's signature block for the signature type `kind`: the
/// type, the signature and the padding after it, to a multiple of 64 bytes.
fn signature_block(kind: u32) -> Option<usize> {
    match kind {
        // RSA-4096, with SHA-1 or SHA-256.
        0x0001_0000 | 0x0001_0003 => Some(0x240),
        // RSA-2048, with SHA-1 or SHA-256.
        0x0001_0001 | 0x0001_0004 => Some(0x140),
        // Elliptic curves, with SHA-1 or SHA-256.
        0x0001_0002 | 0x0001_0005 => Some(0x80),
        _ => None,
    }
}

/// The executable's chunk record among `chunks`, and where it starts among
/// the contents, which hold, in the records' order, those present in
/// `bitmap`.
fn executable_chunk(chunks: &[Chunk], bitmap: &[u8]) -> Result<(u64, Chunk), &'static str> {
    let mut at = 0_u64;
    for chunk in chunks
        .iter()
        .filter(|chunk| is_present(bitmap, chunk.index))
    {
        if chunk.index == 0 {
            return Ok((at, *chunk));
        }
        at = at
            .checked_add(chunk.size)
            .and_then(aligned)
            .ok_or("its contents' sizes are out of range")?;
    }
    Err("it holds no executable")
}

/// Whether `bitmap`, a CIA header's, says the CIA holds the content whose
/// index is `index`.
fn is_present(bitmap: &[u8], index: u16) -> bool {
    bitmap
        .get(usize::from(index / 8))
        .is_some_and(|byte| byte & (0x80 >> (index % 8)) != 0)
}

/// Checks that the executable `layout` places in the CIA at `path`, open
/// as `file`, is all there, unencrypted, and as big as its own header says.
fn check_executable(file: &mut File, path: &Path, layout: &Layout) -> Result<(), CiaError> {
    let invalid = |why| CiaError::invalid(path, why);
    let reading = |source| CiaError::io("read", path, source);
    if layout.encrypted {
        return Err(CiaError::Encrypted(path.to_owned()));
    }
    let length = file.metadata().map_err(reading)?.len();
    if layout
        .offset
        .checked_add(layout.size)
        .is_none_or(|end| end > length)
    {
        return Err(invalid("it is cut short"));
    }
    let ncch = read_at(file, layout.offset, HEADER_SIZE).map_err(reading)?;
    if !has_magic(&ncch, 0x100, b"NCCH") {
        return Err(invalid("its first content is not a 3DS executable"));
    }
    if header::is_encrypted(&ncch) {
        return Err(CiaError::Encrypted(path.to_owned()));
    }
    let own_size = u32_at(&ncch, 0x104).map(|units| u64::from(units) * MEDIA_UNIT);
    if own_size != Some(layout.size) {
        return Err(invalid(
            "its executable's size differs from its title metadata's",
        ));
    }
    Ok(())
}

/// Whether the file at `path` is the executable `layout` places in `cia`:
/// as long, and alike in its first MiB.
fn same_executable(path: &Path, cia: &mut File, layout: &Layout) -> io::Result<bool> {
    let mut existing = File::open(path)?;
    if existing.metadata()?.len() != layout.size {
        return Ok(false);
    }
    let len = layout.size.min(COMPARED) as usize;
    Ok(read_at(&mut existing, 0, len)? == read_at(cia, layout.offset, len)?)
}

/// Writes the executable `layout` places in `cia` to `path`, through a
/// temporary file, so `path` is only ever the whole executable.
fn write_out(cia: &mut File, layout: &Layout, path: &Path) -> io::Result<()> {
    emu_os::file::write_atomic_with(path, None, |to| copy_executable(cia, layout, to))
}

/// Copies the executable `layout` places in `cia` into the new file `to`.
fn copy_executable(cia: &mut File, layout: &Layout, to: &mut File) -> io::Result<()> {
    cia.seek(SeekFrom::Start(layout.offset))?;
    let mut out = BufWriter::with_capacity(WRITE_BUFFER, to);
    let copied = io::copy(&mut (&*cia).take(layout.size), &mut out)?;
    if copied < layout.size {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the title ended before its executable did",
        ));
    }
    out.flush()
}

/// `at` rounded up to a multiple of 64 bytes, if that is a `u64`.
fn aligned(at: u64) -> Option<u64> {
    Some(at.checked_add(ALIGNMENT - 1)? & !(ALIGNMENT - 1))
}

/// The big-endian `u16` at `at` in `bytes`, if it is all there.
fn be_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

/// The big-endian `u32` at `at` in `bytes`, if it is all there.
fn be_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// The big-endian `u64` at `at` in `bytes`, if it is all there.
fn be_u64(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::header::tests::{TITLE_ID, ncch, put, smdh, temp_dir};

    /// The type of a chunk that is not encrypted.
    pub(crate) const PLAIN: u16 = 0;

    /// A TMD signed with RSA-2048 and SHA-256, as homebrew's are, listing
    /// `contents`, each an index, a type and a size.
    fn tmd(contents: &[(u16, u16, u64)]) -> Vec<u8> {
        let header = 0x140;
        let records = header + TMD_HEADER_SIZE + CONTENT_INFO_SIZE;
        let mut tmd = vec![0; records + CHUNK_RECORD_SIZE * contents.len()];
        put(&mut tmd, 0, &0x0001_0004_u32.to_be_bytes());
        put(&mut tmd, 4, &[0x5A; 0x100]);
        put(&mut tmd, header, b"Root-CA00000003-CP0000000b");
        put(&mut tmd, header + 0x4C, &TITLE_ID.to_be_bytes());
        put(
            &mut tmd,
            header + 0x9E,
            &(contents.len() as u16).to_be_bytes(),
        );
        for (number, &(index, kind, size)) in contents.iter().enumerate() {
            let record = records + CHUNK_RECORD_SIZE * number;
            put(&mut tmd, record, &(number as u32).to_be_bytes());
            put(&mut tmd, record + 4, &index.to_be_bytes());
            put(&mut tmd, record + 6, &kind.to_be_bytes());
            put(&mut tmd, record + 8, &size.to_be_bytes());
        }
        tmd
    }

    /// A CIA listing `contents`, each an index, a type and the content's
    /// bytes, or `None` for one the TMD lists but the file leaves out; with
    /// stand-ins for a certificate chain and a ticket.
    pub(crate) fn cia(contents: &[(u16, u16, Option<&[u8]>)]) -> Vec<u8> {
        let listed: Vec<(u16, u16, u64)> = contents
            .iter()
            .map(|&(index, kind, bytes)| (index, kind, bytes.map_or(0x1000, |b| b.len() as u64)))
            .collect();
        let sections = [vec![0xCE; 0xA00], vec![0x71; 0x350], tmd(&listed)];
        let stored: Vec<&[u8]> = contents.iter().filter_map(|&(_, _, bytes)| bytes).collect();
        let contents_size: usize = stored
            .iter()
            .map(|bytes| bytes.len().next_multiple_of(64))
            .sum();
        let mut image = vec![0; CIA_HEADER_SIZE];
        put(&mut image, 0, &(CIA_HEADER_SIZE as u32).to_le_bytes());
        for (number, section) in sections.iter().enumerate() {
            put(
                &mut image,
                0x8 + 4 * number,
                &(section.len() as u32).to_le_bytes(),
            );
        }
        put(&mut image, 0x18, &(contents_size as u64).to_le_bytes());
        for &(index, _, bytes) in contents {
            if bytes.is_some() {
                image[CONTENT_BITMAP + usize::from(index / 8)] |= 0x80 >> (index % 8);
            }
        }
        for part in sections.iter().map(Vec::as_slice).chain(stored) {
            image.resize(image.len().next_multiple_of(64), 0);
            image.extend_from_slice(part);
        }
        image
    }

    /// A plain executable titled "Test Game", and a CIA holding it.
    pub(crate) fn plain_title() -> (Vec<u8>, Vec<u8>) {
        let executable = ncch(true, &smdh(&[(1, "Test Game")]));
        let image = cia(&[(0, PLAIN, Some(&executable))]);
        (executable, image)
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, bytes).expect("write title");
        path
    }

    #[test]
    fn an_unencrypted_title_gives_exactly_its_executable() {
        let dir = temp_dir("cia-extract");
        let executable = ncch(true, &smdh(&[(1, "Test Game")]));
        let manual = ncch(true, &smdh(&[(1, "Manual")]));
        let path = write(
            &dir,
            "Test Game.cia",
            &cia(&[(0, PLAIN, Some(&executable)), (1, PLAIN, Some(&manual))]),
        );
        let games = dir.join("Games");
        let extracted = extract(&path, &games).expect("extracts");
        assert_eq!(extracted, games.join("Test Game.cxi"));
        assert_eq!(fs::read(&extracted).expect("written"), executable);
        assert_eq!(
            fs::read_dir(&games).expect("games").count(),
            1,
            "no temporary file"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_executable_is_found_after_the_contents_before_it() {
        let dir = temp_dir("cia-order");
        let executable = ncch(true, &smdh(&[(1, "Test Game")]));
        // Not a multiple of 64 bytes, so the executable starts at the next.
        let manual = [0x33; 0x50];
        let image = cia(&[
            (2, PLAIN, None),
            (1, PLAIN, Some(&manual)),
            (0, PLAIN, Some(&executable)),
        ]);
        let path = write(&dir, "game.cia", &image);
        let mut file = File::open(&path).expect("open");
        let layout = layout(&mut file, &path).expect("layout");
        assert_eq!(layout.title_id, Some(TITLE_ID));
        assert_eq!(layout.size, executable.len() as u64);
        assert!(!layout.encrypted);
        assert_eq!(layout.offset % 64, 0);
        let extracted = extract(&path, &dir.join("Games")).expect("extracts");
        assert_eq!(fs::read(&extracted).expect("written"), executable);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_title_encrypted_with_its_title_key_is_refused() {
        let dir = temp_dir("cia-title-key");
        let executable = ncch(true, &smdh(&[(1, "Test Game")]));
        let path = write(
            &dir,
            "game.cia",
            &cia(&[(0, ENCRYPTED_CONTENT, Some(&executable))]),
        );
        let games = dir.join("Games");
        let error = extract(&path, &games).expect_err("encrypted");
        assert!(matches!(error, CiaError::Encrypted(_)), "{error}");
        assert_eq!(
            error.to_string(),
            format!(
                "{} is encrypted: Azahar plays only decrypted games",
                path.display()
            )
        );
        assert!(!games.exists(), "nothing is written");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_executable_without_its_no_crypto_flag_is_refused() {
        let dir = temp_dir("cia-no-crypto");
        let executable = ncch(false, &smdh(&[(1, "Test Game")]));
        let path = write(&dir, "game.cia", &cia(&[(0, PLAIN, Some(&executable))]));
        let games = dir.join("Games");
        let error = extract(&path, &games).expect_err("encrypted");
        assert!(matches!(error, CiaError::Encrypted(_)), "{error}");
        assert!(!games.exists(), "nothing is written");
        fs::remove_dir_all(&dir).ok();
    }

    /// What changes when the file at `path` is written again: its inode,
    /// as extraction renames a new file into place. Windows has no inodes
    /// in stable std, so there it is the file's modification time.
    #[cfg(unix)]
    fn version(path: &Path) -> u64 {
        use std::os::unix::fs::MetadataExt as _;
        fs::metadata(path).expect("metadata").ino()
    }

    #[cfg(not(unix))]
    fn version(path: &Path) -> std::time::SystemTime {
        fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .expect("metadata")
    }

    #[test]
    fn an_earlier_extraction_is_used_again_and_a_different_one_kept() {
        let dir = temp_dir("cia-again");
        let games = dir.join("Games");
        let (_, image) = plain_title();
        let path = write(&dir, "game.cia", &image);
        let first = extract(&path, &games).expect("extracts");
        let written = version(&first);
        assert_eq!(extract(&path, &games).expect("again"), first);
        assert_eq!(version(&first), written, "not written again");

        // Another title of the same name, from another folder.
        let other = ncch(true, &smdh(&[(1, "Another Game")]));
        let elsewhere = dir.join("elsewhere");
        fs::create_dir_all(&elsewhere).expect("folder");
        let path = write(&elsewhere, "game.cia", &cia(&[(0, PLAIN, Some(&other))]));
        let second = extract(&path, &games).expect("extracts");
        assert_eq!(second, games.join("game (2).cxi"));
        assert_eq!(fs::read(&second).expect("written"), other);
        assert_eq!(version(&first), written);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_executable_whose_size_disagrees_is_refused() {
        let dir = temp_dir("cia-size");
        let mut executable = ncch(true, &smdh(&[(1, "Test Game")]));
        put(&mut executable, 0x104, &1_u32.to_le_bytes());
        let path = write(&dir, "game.cia", &cia(&[(0, PLAIN, Some(&executable))]));
        let error = extract(&path, &dir.join("Games")).expect_err("refused");
        assert!(error.to_string().contains("size differs"), "{error}");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn titles_cut_short_are_refused_without_panicking() {
        let dir = temp_dir("cia-cut-short");
        let games = dir.join("Games");
        let (_, image) = plain_title();
        let path = dir.join("game.cia");
        for len in (0..image.len()).step_by(0x40).chain([image.len() - 1]) {
            fs::write(&path, &image[..len]).expect("write");
            let error = extract(&path, &games).expect_err("cut short");
            assert!(
                matches!(error, CiaError::Invalid { .. }),
                "cut to {len:#x}: {error}"
            );
        }
        assert!(!games.exists(), "nothing is written");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn garbage_and_absurd_sizes_are_refused() {
        let dir = temp_dir("cia-garbage");
        let games = dir.join("Games");
        let (_, image) = plain_title();
        let tmd = 0x2040 + 0xA00 + 0x380;
        let mut cases: Vec<Vec<u8>> = vec![vec![0x55; 0x4000], Vec::new()];
        for (at, value) in [
            (0x0, &u32::MAX.to_le_bytes()[..]),
            (0x10, &u32::MAX.to_le_bytes()),
            (0x18, &u64::MAX.to_le_bytes()),
            (0x18, &0x200_u64.to_le_bytes()),
            (CONTENT_BITMAP, &[0]),
            (tmd, &0xDEAD_BEEF_u32.to_be_bytes()),
            (tmd + 0x140 + 0x9E, &u16::MAX.to_be_bytes()),
            (tmd + 0x140 + 0xC4 + 0x900 + 8, &u64::MAX.to_be_bytes()),
        ] {
            let mut broken = image.clone();
            put(&mut broken, at, value);
            cases.push(broken);
        }
        for (number, bytes) in cases.iter().enumerate() {
            let path = write(&dir, "game.cia", bytes);
            let error = extract(&path, &games).expect_err("refused");
            assert!(
                matches!(error, CiaError::Invalid { .. }),
                "case {number}: {error}"
            );
            assert_eq!(io::Error::from(error).kind(), io::ErrorKind::InvalidData);
        }
        assert!(!games.exists(), "nothing is written");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_title_cannot_be_read() {
        let dir = temp_dir("cia-missing");
        let path = dir.join("gone.cia");
        let error = extract(&path, &dir).expect_err("missing");
        assert_eq!(error.to_string(), format!("cannot read {}", path.display()));
        assert_eq!(io::Error::from(error).kind(), io::ErrorKind::NotFound);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cia_files_are_known_by_their_extension() {
        assert!(is_cia(Path::new("Game.CIA")));
        assert!(is_cia(Path::new("/games/game.cia")));
        assert!(!is_cia(Path::new("game.cxi")));
        assert!(!is_cia(Path::new("cia")));
    }
}
