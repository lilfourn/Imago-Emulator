//! Box art for the library: each game's No-Intro name from libretro's game
//! databases, and its box art from thumbnails.libretro.com, both kept in
//! the cache folder. Downloads go through `curl`, which macOS has.
//!
//! Everything here blocks; run it off the UI thread.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

use anyhow::{Context as _, bail};

use crate::game_db::{self, GameDb};
use crate::library::Game;
use crate::persist;
use crate::rom_info::{ICON_SIZE, Platform};

const DATABASES: &str =
    "https://raw.githubusercontent.com/libretro/libretro-database/master/metadat/no-intro";
const THUMBNAILS: &str = "https://thumbnails.libretro.com";

/// How many times bigger than its 32 pixels a DS icon is saved, so it
/// stays sharp when the library scales it up.
const ICON_SCALE: u32 = 4;

/// Where downloaded databases, box art and DS icons are kept, e.g.
/// `~/Library/Caches/ds-emulator` on macOS.
pub fn cache_dir() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("ds-emulator"))
}

/// Where the box art of the ROM with checksum `crc32` is kept.
pub fn art_path(cache: &Path, crc32: u32) -> PathBuf {
    cache.join("boxart").join(format!("{crc32:08X}.png"))
}

/// Where the banner icon of the DS ROM with checksum `crc32`, or the icon
/// of the player app of the RPG Maker game with that checksum, is kept.
pub fn icon_path(cache: &Path, crc32: u32) -> PathBuf {
    cache.join("icons").join(format!("{crc32:08X}.png"))
}

/// The game database for `platform`, downloaded into `cache` the first
/// time.
///
/// # Errors
///
/// Fails if the database is not cached and cannot be downloaded.
pub fn database(cache: &Path, platform: Platform) -> anyhow::Result<GameDb> {
    let name = platform.libretro_name();
    let path = cache.join("databases").join(format!("{name}.dat"));
    if !path.exists() {
        let url = format!("{DATABASES}/{}.dat", encode(name));
        let Some(bytes) = download(&url)? else {
            bail!("no game database for {name}");
        };
        // Checked before it is kept, so a bad download is tried again.
        let text = String::from_utf8_lossy(&bytes);
        if GameDb::parse(&text).is_empty() {
            bail!("the game database for {name} came down empty or garbled");
        }
        write(&path, &bytes)?;
    }
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let db = GameDb::parse(&text);
    if db.is_empty() {
        // Kept by an older version without checking: fetch it again next
        // time rather than find nothing in it for good.
        fs::remove_file(&path).ok();
        bail!("the game database for {name} was empty or garbled");
    }
    Ok(db)
}

/// Each platform's game database, parsed, kept between lookups.
pub type Databases = Mutex<HashMap<Platform, Arc<GameDb>>>;

/// How many games are looked up at once.
const WORKERS: usize = 8;

/// Looks up each of `games`, [`WORKERS`] at a time: its No-Intro name in
/// the databases for its platform and its box art, downloaded to
/// [`art_path`] unless already there. Calls `found` with each game and
/// its name, if a database knows it, as soon as that game is done.
///
/// Databases missing from `databases` are downloaded first, all at once.
/// A game whose database or box art cannot be downloaded gets an error,
/// so it can be looked up again later.
pub fn look_up_all(
    cache: &Path,
    games: &[Game],
    databases: &Databases,
    found: impl Fn(&Game, anyhow::Result<Option<String>>) + Sync,
) {
    let known = |databases: &Databases| lock(databases).clone();
    let have = known(databases);
    let mut missing: Vec<Platform> = games
        .iter()
        .flat_map(|game| databases_for(game.platform))
        .filter(|platform| !have.contains_key(platform))
        .copied()
        .collect();
    missing.sort_by_key(|platform| platform.tag());
    missing.dedup();
    let failures: HashMap<Platform, String> = thread::scope(|scope| {
        let loads: Vec<_> = missing
            .iter()
            .map(|&platform| scope.spawn(move || (platform, database(cache, platform))))
            .collect();
        loads
            .into_iter()
            .filter_map(|load| {
                let (platform, db) = load.join().expect("a database download does not panic");
                match db {
                    Ok(db) => {
                        lock(databases).insert(platform, Arc::new(db));
                        None
                    }
                    Err(error) => Some((platform, format!("{error:#}"))),
                }
            })
            .collect()
    });
    let have = known(databases);
    let next = AtomicUsize::new(0);
    thread::scope(|scope| {
        for _ in 0..WORKERS.min(games.len()) {
            scope.spawn(|| {
                while let Some(game) = games.get(next.fetch_add(1, Ordering::Relaxed)) {
                    found(game, look_up(cache, game, &have, &failures));
                }
            });
        }
    });
}

/// Looks `game` up in `databases`, failing with the reason in `failures`
/// if one it needs is missing.
fn look_up(
    cache: &Path,
    game: &Game,
    databases: &HashMap<Platform, Arc<GameDb>>,
    failures: &HashMap<Platform, String>,
) -> anyhow::Result<Option<String>> {
    for &platform in databases_for(game.platform) {
        let Some(db) = databases.get(&platform) else {
            match failures.get(&platform) {
                Some(reason) => bail!("{reason}"),
                None => bail!("no game database for {}", platform.libretro_name()),
            }
        };
        if let Some(name) = name_in(db, game) {
            if !is_png(&art_path(cache, game.crc32)) {
                download_art(cache, platform, &name, game.crc32)?;
            }
            return Ok(Some(name));
        }
    }
    Ok(None)
}

/// The databases, even if a lookup panicked while holding them.
fn lock(databases: &Databases) -> MutexGuard<'_, HashMap<Platform, Arc<GameDb>>> {
    databases.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The No-Intro name `db` gives `game`: by checksum, then game code, then
/// the ROM's file name, which is often the No-Intro name already.
pub fn name_in(db: &GameDb, game: &Game) -> Option<String> {
    let stem = game.path.file_stem()?.to_string_lossy();
    db.by_crc(game.crc32)
        .or_else(|| {
            game.serial
                .as_deref()
                .and_then(|serial| db.by_serial(serial))
        })
        .or_else(|| db.by_title(&stem.replace('_', " ")))
        .map(str::to_owned)
}

/// The platforms whose databases may list a `platform` game: Game Boy
/// Color games that also run on a Game Boy can be in either. None list
/// RPG Maker games.
pub fn databases_for(platform: Platform) -> &'static [Platform] {
    match platform {
        Platform::Gb => &[Platform::Gb, Platform::Gbc],
        Platform::Gbc => &[Platform::Gbc, Platform::Gb],
        Platform::Gba => &[Platform::Gba],
        Platform::Nds => &[Platform::Nds],
        Platform::N3ds => &[Platform::N3ds],
        Platform::RpgMaker => &[],
    }
}

/// Downloads the box art of the `platform` game called `name` to
/// [`art_path`], trying each file name it may have. Whether any was found.
///
/// # Errors
///
/// Fails if the thumbnail server cannot be reached.
pub fn download_art(
    cache: &Path,
    platform: Platform,
    name: &str,
    crc32: u32,
) -> anyhow::Result<bool> {
    let folder = format!(
        "{THUMBNAILS}/{}/Named_Boxarts",
        encode(platform.libretro_name())
    );
    let path = art_path(cache, crc32);
    for file in game_db::thumbnail_names(name) {
        let url = format!("{folder}/{}", encode(&file));
        if let Some(bytes) = download(&url)? {
            // A page in place of the picture would hide the DS icon for
            // good.
            if !bytes.starts_with(PNG_SIGNATURE) {
                bail!("{url} is not a PNG");
            }
            write(&path, &bytes)?;
            return Ok(true);
        }
    }
    Ok(false)
}

/// What every PNG file starts with.
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// Whether the file at `path` is there and starts as a PNG does: box art
/// an older version kept from a bad download is not.
pub fn is_png(path: &Path) -> bool {
    let mut start = [0; PNG_SIGNATURE.len()];
    fs::File::open(path)
        .and_then(|mut file| io::Read::read_exact(&mut file, &mut start))
        .is_ok_and(|()| start == PNG_SIGNATURE)
}

/// Saves a DS banner icon, 32×32 RGBA `pixels`, as a PNG at
/// [`icon_path`], scaled up by repeating pixels.
///
/// # Errors
///
/// Fails if the file cannot be written.
pub fn save_icon(cache: &Path, crc32: u32, pixels: &[u8]) -> io::Result<PathBuf> {
    let side = ICON_SIZE * ICON_SCALE;
    let row = |y: u32| {
        let source = (y / ICON_SCALE) as usize * ICON_SIZE as usize;
        (0..side as usize).flat_map(move |x| {
            let at = (source + x / ICON_SCALE as usize) * 4;
            pixels[at..at + 4].iter().copied()
        })
    };
    let scaled: Vec<u8> = (0..side).flat_map(row).collect();
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, side, side);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .and_then(|mut writer| writer.write_image_data(&scaled))
        .map_err(io::Error::other)?;
    let path = icon_path(cache, crc32);
    write(&path, &bytes)?;
    Ok(path)
}

/// Downloads `url`, whole: its bytes, or `None` if the server answered
/// 404.
///
/// # Errors
///
/// Fails if `curl` cannot run or stops short, as when the connection
/// drops or times out part-way, or the server cannot be reached or
/// answers with an error other than 404.
fn download(url: &str) -> anyhow::Result<Option<Vec<u8>>> {
    fetch(url, 60)
}

/// Downloads `url` as [`download`] does, giving up after `max_seconds`.
///
/// # Errors
///
/// As [`download`].
pub fn fetch(url: &str, max_seconds: u32) -> anyhow::Result<Option<Vec<u8>>> {
    let output = Command::new("curl")
        .args(["--silent", "--show-error", "--location", "--compressed"])
        .args([
            "--user-agent",
            concat!("ds-emulator/", env!("CARGO_PKG_VERSION")),
        ])
        .args(["--connect-timeout", "10", "--max-time"])
        .arg(max_seconds.to_string())
        .args(["--output", "-", "--write-out", "\n%{http_code}"])
        .arg(url)
        .output()
        .context("running curl")?;
    let mut body = output.stdout;
    let split = body.iter().rposition(|&b| b == b'\n').unwrap_or(0);
    let status = String::from_utf8_lossy(&body[split..]).trim().to_owned();
    // A transfer cut short still says 200; only curl's own status tells.
    if !output.status.success() {
        let why = String::from_utf8_lossy(&output.stderr);
        bail!("cannot download {url}: {}", why.trim());
    }
    match status.as_str() {
        "200" => {
            body.truncate(split);
            Ok(Some(body))
        }
        "404" => Ok(None),
        _ => bail!("cannot download {url}: HTTP status {status:?}"),
    }
}

/// Writes `bytes` to `path`, creating its folder.
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder)?;
    }
    persist::write_atomic(path, bytes)
}

/// `text` percent-encoded for a URL path segment.
pub fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rom_info::RomInfo;

    #[test]
    fn encodes_names_for_urls() {
        assert_eq!(
            encode("Nintendo - Game Boy (USA, Europe).png"),
            "Nintendo%20-%20Game%20Boy%20%28USA%2C%20Europe%29.png"
        );
        assert_eq!(encode("Pokémon"), "Pok%C3%A9mon");
    }

    #[test]
    fn names_are_found_by_checksum_then_code_then_file_name() {
        let db = GameDb::parse(
            r#"game (
	name "Metroid Fusion (USA)"
	rom ( name "Metroid Fusion (USA).gba" size 1 crc 00000001 serial "AMTE" )
)
game (
	name "Advance Wars (USA)"
	rom ( name "Advance Wars (USA).gba" size 1 crc 00000002 serial "AWRE" )
)
"#,
        );
        let game = |file: &str, crc32: u32, serial: Option<&str>| {
            let info = RomInfo {
                platform: Platform::Gba,
                crc32,
                serial: serial.map(str::to_owned),
                header_title: None,
                banner_title: None,
            };
            Game::new(Path::new(file), &info)
        };
        assert_eq!(
            name_in(&db, &game("x.gba", 1, None)).as_deref(),
            Some("Metroid Fusion (USA)")
        );
        assert_eq!(
            name_in(&db, &game("x.gba", 9, Some("AWRE"))).as_deref(),
            Some("Advance Wars (USA)")
        );
        assert_eq!(
            name_in(&db, &game("Advance_Wars (Europe).gba", 9, None)).as_deref(),
            Some("Advance Wars (USA)")
        );
        assert_eq!(name_in(&db, &game("hack.gba", 9, Some("ZZZZ"))), None);
    }

    #[test]
    fn a_garbled_cached_database_is_dropped_to_fetch_again() {
        let cache = std::env::temp_dir().join(format!("ds-emulator-db-{}", std::process::id()));
        let name = Platform::Gba.libretro_name();
        let path = cache.join("databases").join(format!("{name}.dat"));
        write(&path, b"<html>Too many requests</html>").expect("written");
        let error = database(&cache, Platform::Gba).expect_err("refused");
        assert!(
            format!("{error:#}").contains("empty or garbled"),
            "{error:#}"
        );
        assert!(!path.exists(), "dropped");
        let dat =
            "game (\n\tname \"Metroid Fusion (USA)\"\n\trom ( name \"x.gba\" crc 00000001 )\n)\n";
        write(&path, dat.as_bytes()).expect("written");
        let db = database(&cache, Platform::Gba).expect("kept");
        assert_eq!(db.by_crc(1), Some("Metroid Fusion (USA)"));
        fs::remove_dir_all(&cache).ok();
    }

    #[test]
    fn only_pngs_count_as_box_art() {
        let cache = std::env::temp_dir().join(format!("ds-emulator-art-{}", std::process::id()));
        let path = art_path(&cache, 7);
        assert!(!is_png(&path), "missing");
        write(&path, b"<html>Not Found</html>").expect("written");
        assert!(!is_png(&path));
        write(&path, PNG_SIGNATURE).expect("written");
        assert!(is_png(&path));
        fs::remove_dir_all(&cache).ok();
    }

    #[test]
    fn icons_are_saved_scaled_up() {
        let cache = std::env::temp_dir().join(format!("ds-emulator-icon-{}", std::process::id()));
        let mut pixels = vec![0; (ICON_SIZE * ICON_SIZE * 4) as usize];
        pixels[0..4].copy_from_slice(&[0xFF, 0, 0, 0xFF]);
        let path = save_icon(&cache, 0x1234, &pixels).expect("written");
        assert_eq!(path, icon_path(&cache, 0x1234));
        let decoder = png::Decoder::new(io::Cursor::new(fs::read(&path).expect("read back")));
        let mut reader = decoder.read_info().expect("a PNG header");
        let mut decoded = vec![0; reader.output_buffer_size().expect("small image")];
        let info = reader.next_frame(&mut decoded).expect("one image");
        assert_eq!((info.width, info.height), (128, 128));
        let side = 128 * 4;
        assert_eq!(
            decoded[(3 * side) + 3 * 4..(3 * side) + 4 * 4],
            [0xFF, 0, 0, 0xFF]
        );
        assert_eq!(
            decoded[(4 * side) + 4 * 4..(4 * side) + 5 * 4],
            [0, 0, 0, 0]
        );
        fs::remove_dir_all(&cache).ok();
    }
}
