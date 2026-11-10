//! Box art for the library: each game's No-Intro name from libretro's game
//! databases, and its box art from thumbnails.libretro.com, both kept in
//! the cache folder. Downloads go through `curl`, which macOS and Windows
//! have, and Linux systems nearly always do, as [`emu_os::fetch`] runs it:
//! from `https://` addresses only, and each held to a size.
//!
//! A game gets a picture whenever libretro has one: its box art by its
//! No-Intro name or by its title among the pictures libretro lists, or
//! else its title screen. Should the thumbnail server fail, pictures come
//! from libretro-thumbnails on GitHub instead (the lists come only from
//! the server, and are kept a month). Pictures the cache loses, as when
//! macOS clears it, are fetched again, and DS icons and RPG Maker players'
//! icons made again.
//!
//! Everything here blocks; run it off the UI thread.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, bail};
use emu_os::fetch::{Allow, Download};

use crate::app_icon;
use crate::game_db::{self, GameDb};
use crate::library::Game;
use crate::persist;
use crate::rom_info::{self, ICON_SIZE, Platform};

const DATABASES: &str =
    "https://raw.githubusercontent.com/libretro/libretro-database/master/metadat/no-intro";
const THUMBNAILS: &str = "https://thumbnails.libretro.com";
/// The same pictures, one repository a system, named as
/// [`Platform::libretro_name`] with `_` for each space.
const MIRROR: &str = "https://raw.githubusercontent.com/libretro-thumbnails";

/// libretro's folders of pictures: box art, and title screens.
const BOX_ART: &str = "Named_Boxarts";
const TITLE_SCREENS: &str = "Named_Titles";

/// How many times bigger than its 32 pixels a DS icon is saved, so it
/// stays sharp when the library scales it up.
const ICON_SCALE: u32 = 4;

/// Where downloads may come from: `https://` addresses, and in tests a
/// stand-in server on this computer.
const ALLOW: Allow = Allow::or_loopback_if(cfg!(test));
/// The most a game database, a list of pictures or a store catalog may be.
/// The biggest are about 2 MB.
pub const MAX_PAGE: u64 = 32 << 20;
/// The most a picture may be.
pub const MAX_PICTURE: u64 = 16 << 20;

/// Where downloaded databases, box art and DS icons are kept, e.g.
/// `~/Library/Caches/ds-emulator` on macOS.
pub fn cache_dir() -> Option<PathBuf> {
    Some(emu_os::cache_dir()?.join("ds-emulator"))
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
/// the databases for its platform, unless it was looked up before, and a
/// picture of it (see [`find_art`]), downloaded to [`art_path`] unless
/// already there; and makes its icon again if the cache lost it. Calls
/// `found` with each game and its name, if a database knows it, as soon
/// as that game is done.
///
/// Databases missing from `databases` are downloaded first, all at once.
/// A game whose database or picture cannot be downloaded gets an error,
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
        .filter(|game| !game.looked_up)
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

/// Looks `game` up in `databases`, unless it was looked up before, and
/// finds its picture and icon if the cache has none. Fails with the
/// reason in `failures` if a database it needs is missing.
fn look_up(
    cache: &Path,
    game: &Game,
    databases: &HashMap<Platform, Arc<GameDb>>,
    failures: &HashMap<Platform, String>,
) -> anyhow::Result<Option<String>> {
    let name = if game.looked_up {
        game.name.clone()
    } else {
        name_in_databases(game, databases, failures)?
    };
    restore_icon(cache, game);
    if !is_png(&art_path(cache, game.crc32)) {
        // A picture that can't be had now is looked for again at the next
        // launch, as the game still has none; the name found stands.
        find_art(cache, game, name.as_deref()).ok();
    }
    Ok(name)
}

/// The No-Intro name of `game` in the first of `databases` for its
/// platform that knows it.
fn name_in_databases(
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
            return Ok(Some(name));
        }
    }
    Ok(None)
}

/// Downloads a picture of `game` to [`art_path`], the first there is of:
/// its box art by its No-Intro `name`, if it has one; the box art libretro
/// lists under the game's title, or its name's; and its title screen by
/// its name. Whether one was found. An RPG Maker game has none.
///
/// Only names libretro's lists of pictures hold are downloaded, so a game
/// libretro has no picture of costs nothing once the lists are kept. A
/// list that can't be had is no help in matching titles, and every name a
/// picture may have is tried instead.
///
/// # Errors
///
/// Fails if neither the thumbnail server nor its mirror can be reached.
pub fn find_art(cache: &Path, game: &Game, name: Option<&str>) -> anyhow::Result<bool> {
    let platforms = databases_for(game.platform);
    let by_name = |folder: &str| -> anyhow::Result<bool> {
        let Some(name) = name else {
            return Ok(false);
        };
        for &platform in platforms {
            let listed: Option<HashSet<String>> = listing(cache, platform, folder)
                .ok()
                .map(|files| files.into_iter().collect());
            let files = game_db::thumbnail_names(name);
            let files = files
                .iter()
                .filter(|file| listed.as_ref().is_none_or(|listed| listed.contains(*file)));
            for file in files {
                if let Some(bytes) = picture(platform, folder, file)? {
                    save_art(cache, game.crc32, &bytes)?;
                    return Ok(true);
                }
            }
        }
        Ok(false)
    };
    if by_name(BOX_ART)? {
        return Ok(true);
    }
    let mut keys = vec![game_db::title_key(&game.title)];
    keys.extend(name.map(game_db::title_key));
    keys.retain(|key| !key.is_empty());
    for &platform in platforms.iter().filter(|_| !keys.is_empty()) {
        let Ok(listed) = listing(cache, platform, BOX_ART) else {
            continue;
        };
        let same_title = listed.iter().map(String::as_str).filter(|file| {
            let stem = file.strip_suffix(".png").unwrap_or(file);
            keys.contains(&game_db::title_key(stem))
        });
        let Some(file) = game_db::likeliest(same_title) else {
            continue;
        };
        if let Some(bytes) = picture(platform, BOX_ART, file)? {
            save_art(cache, game.crc32, &bytes)?;
            return Ok(true);
        }
    }
    by_name(TITLE_SCREENS)
}

/// How long a list of libretro's pictures is kept before it is fetched
/// again, for the pictures added since.
const LISTING_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// The file names of the pictures in libretro's `folder` for `platform`,
/// from the thumbnail server's listing of it, kept in `cache` for
/// [`LISTING_AGE`], and kept longer should the server fail.
///
/// # Errors
///
/// Fails if the listing is not cached and cannot be downloaded.
fn listing(cache: &Path, platform: Platform, folder: &str) -> anyhow::Result<Vec<String>> {
    let name = platform.libretro_name();
    let path = cache
        .join("listings")
        .join(format!("{name} - {folder}.txt"));
    let kept = fs::read_to_string(&path)
        .ok()
        .filter(|text| !text.trim().is_empty())
        .map(|text| text.lines().map(str::to_owned).collect::<Vec<_>>());
    let fresh = fs::metadata(&path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age < LISTING_AGE);
    if let Some(kept) = &kept
        && fresh
    {
        return Ok(kept.clone());
    }
    let fetched = (|| {
        let url = format!("{THUMBNAILS}/{}/{folder}/", encode(name));
        let Some(page) = fetch(&url, 120)? else {
            bail!("libretro lists no {folder} for {name}");
        };
        let files = linked_pictures(&String::from_utf8_lossy(&page));
        if files.is_empty() {
            bail!("libretro's list of {folder} for {name} came down empty or garbled");
        }
        write(&path, files.join("\n").as_bytes())?;
        Ok(files)
    })();
    match (fetched, kept) {
        (Ok(files), _) => Ok(files),
        (Err(_), Some(kept)) => Ok(kept),
        (Err(error), None) => Err(error),
    }
}

/// The PNG files a folder listing's page links to, by name.
fn linked_pictures(page: &str) -> Vec<String> {
    page.split("href=\"")
        .skip(1)
        .filter_map(|rest| decode(&rest[..rest.find('"')?]))
        .filter(|file| file.ends_with(".png") && !file.contains('/'))
        .collect()
}

/// `link`, percent-encoded and HTML-escaped, as plain text.
fn decode(link: &str) -> Option<String> {
    let link = link.replace("&amp;", "&");
    let mut bytes = Vec::with_capacity(link.len());
    let mut rest = link.as_bytes();
    while let Some((&byte, after)) = rest.split_first() {
        if byte == b'%' && after.len() >= 2 {
            let hex = std::str::from_utf8(&after[..2]).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            rest = &after[2..];
        } else {
            bytes.push(byte);
            rest = after;
        }
    }
    String::from_utf8(bytes).ok()
}

/// Makes the icon of `game` again if the cache lost it: a DS game's from
/// its ROM's banner, an RPG Maker game's from its player app.
fn restore_icon(cache: &Path, game: &Game) {
    let path = icon_path(cache, game.crc32);
    if path.exists() {
        return;
    }
    match game.platform {
        Platform::Nds => {
            if let Some(icon) = rom_info::nds_icon_in_file(&game.path) {
                save_icon(cache, game.crc32, &icon).ok();
            }
        }
        Platform::RpgMaker => {
            if let Some(player) = emu_cores::rpg_maker::player_app(&game.path) {
                app_icon::save(&player, &path).ok();
            }
        }
        _ => {}
    }
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

/// The picture `file` in libretro's `folder` of `platform` pictures, from
/// the thumbnail server or, should it fail, its mirror on GitHub; `None`
/// if there is none.
///
/// # Errors
///
/// Fails if neither can be reached, or the server sends something other
/// than a PNG: a page in its place would hide the DS icon for good.
fn picture(platform: Platform, folder: &str, file: &str) -> anyhow::Result<Option<Vec<u8>>> {
    let system = platform.libretro_name();
    let url = format!("{THUMBNAILS}/{}/{folder}/{}", encode(system), encode(file));
    let bytes = fetch_up_to(&url, 60, MAX_PICTURE).or_else(|error| {
        let repository = system.replace(' ', "_");
        let mirror = format!(
            "{MIRROR}/{}/master/{folder}/{}",
            encode(&repository),
            encode(file)
        );
        fetch_up_to(&mirror, 60, MAX_PICTURE).map_err(|_| error)
    })?;
    match bytes {
        Some(bytes) if !bytes.starts_with(PNG_SIGNATURE) => bail!("{url} is not a PNG"),
        found => Ok(found),
    }
}

/// Keeps `bytes`, a PNG, as the picture of the game with checksum
/// `crc32`.
fn save_art(cache: &Path, crc32: u32, bytes: &[u8]) -> io::Result<()> {
    write(&art_path(cache, crc32), bytes)
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

/// Downloads `url`, a game database, whole: its bytes, or `None` if the
/// server answered 404.
///
/// # Errors
///
/// As [`fetch`].
fn download(url: &str) -> anyhow::Result<Option<Vec<u8>>> {
    fetch(url, 60)
}

/// Downloads `url`, a page of at most [`MAX_PAGE`] bytes, whole, giving up
/// after `max_seconds`: its bytes, or `None` if the server answered 404.
///
/// # Errors
///
/// Fails if `url` is not an `https://` address, `curl` cannot run or
/// stops short, as when the connection drops or times out part-way, the
/// server cannot be reached or answers with an error other than 404, or
/// the download is too big.
pub fn fetch(url: &str, max_seconds: u32) -> anyhow::Result<Option<Vec<u8>>> {
    fetch_up_to(url, max_seconds, MAX_PAGE)
}

/// Downloads `url` as [`fetch`] does, if it is at most `max_bytes`.
///
/// # Errors
///
/// As [`fetch`].
pub fn fetch_up_to(url: &str, max_seconds: u32, max_bytes: u64) -> anyhow::Result<Option<Vec<u8>>> {
    found(url, download_of(url, max_seconds, max_bytes).bytes())
}

/// Downloads `url` into the file `to`, written as it comes, if it is at
/// most `max_bytes`: a game can be far too big to hold in memory. Whether
/// it was there to download: not if the server answered 404. A download
/// that fails leaves no file.
///
/// # Errors
///
/// As [`fetch`].
pub fn fetch_file(url: &str, to: &Path, max_seconds: u32, max_bytes: u64) -> anyhow::Result<bool> {
    let downloaded = download_of(url, max_seconds, max_bytes).to_file(to);
    Ok(found(url, downloaded)?.is_some())
}

/// The download of `url`, as the library and the store make one.
fn download_of(url: &str, seconds: u32, max_bytes: u64) -> Download<'_> {
    Download {
        url,
        allow: ALLOW,
        seconds,
        max_bytes,
        patient: false,
    }
}

/// What was `downloaded` from `url`, or `None` if the server answered 404,
/// or why it did not come, in words for the user.
fn found<T>(url: &str, downloaded: Result<T, emu_os::fetch::Error>) -> anyhow::Result<Option<T>> {
    use emu_os::fetch::Error;
    match downloaded {
        Ok(found) => Ok(Some(found)),
        Err(Error::Status(status)) if status == "404" => Ok(None),
        Err(Error::Status(status)) => bail!("cannot download {url}: HTTP status {status:?}"),
        // curl isn't installed, whatever the address.
        Err(error @ Error::NotStarted(_)) => bail!("{error}"),
        Err(error) => bail!("cannot download {url}: {error}"),
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

/// A stand-in server on this computer, for tests of downloads.
#[cfg(test)]
pub(crate) mod stand_in {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    /// Where the server is, such as `http://127.0.0.1:50123`, and how many
    /// requests it has had.
    pub(crate) struct Server {
        pub(crate) site: String,
        pub(crate) requests: Arc<AtomicUsize>,
    }

    /// Serves on a loopback port, answering each request with the status
    /// and body `reply` makes of its path. A body comes without a length,
    /// ended by the connection's close, as a server may send one.
    pub(crate) fn serve(reply: fn(&str) -> (u16, Vec<u8>)) -> Server {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a port");
        let site = format!("http://{}", listener.local_addr().expect("address"));
        let requests = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&requests);
        thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                counted.fetch_add(1, Ordering::SeqCst);
                thread::spawn(move || {
                    let mut head = Vec::new();
                    let mut chunk = [0; 1024];
                    while !head.windows(4).any(|end| end == b"\r\n\r\n") {
                        match stream.read(&mut chunk) {
                            Ok(read) if read > 0 => head.extend_from_slice(&chunk[..read]),
                            _ => return,
                        }
                    }
                    let head = String::from_utf8_lossy(&head).into_owned();
                    let path = head.split_whitespace().nth(1).unwrap_or_default();
                    let (status, body) = reply(path);
                    let mut answer =
                        format!("HTTP/1.1 {status} X\r\nconnection: close\r\n\r\n").into_bytes();
                    answer.extend(body);
                    // curl hangs up on a download it stops.
                    stream.write_all(&answer).ok();
                });
            }
        });
        Server { site, requests }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rom_info::RomInfo;

    #[test]
    fn an_address_that_reads_as_an_option_never_reaches_curl() {
        // As a catalog could give a game's or a picture's address.
        for url in ["-V", "--output=/tmp/x", "file:///etc/hosts"] {
            let error = fetch(url, 5).expect_err("refused");
            assert_eq!(
                format!("{error:#}"),
                format!("cannot download {url}: its address is not an https:// one")
            );
            let to =
                std::env::temp_dir().join(format!("ds-emulator-refused-{}", std::process::id()));
            assert!(fetch_file(url, &to, 5, MAX_PAGE).is_err(), "{url}");
            assert!(!to.exists(), "{url}");
        }
    }

    #[test]
    fn a_download_bigger_than_it_may_be_is_stopped() {
        let server = stand_in::serve(|path| match path {
            "/endless.json" => (200, vec![b'x'; MAX_PAGE as usize + (1 << 20)]),
            "/catalog.json" => (200, b"[]".to_vec()),
            _ => (404, b"<html>Not Found</html>".to_vec()),
        });
        let url = format!("{}/endless.json", server.site);
        // Its length alone, not megabytes of it, should this fail.
        let fetched = fetch(&url, 30).map(|bytes| bytes.map(|bytes| bytes.len()));
        let error = fetched.expect_err("too big");
        assert_eq!(
            format!("{error:#}"),
            format!("cannot download {url}: it is too big")
        );
        // A picture is held to less, and a file to what its caller says.
        let fetched =
            fetch_up_to(&url, 30, MAX_PICTURE).map(|bytes| bytes.map(|bytes| bytes.len()));
        assert!(fetched.is_err());
        let dir = std::env::temp_dir().join(format!("ds-emulator-capped-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("folder");
        let to = dir.join("game.gb");
        let error = fetch_file(&url, &to, 30, 1 << 20).expect_err("too big");
        assert_eq!(
            format!("{error:#}"),
            format!("cannot download {url}: it is too big")
        );
        assert!(!to.exists(), "nothing of it is kept");
        // What fits comes, and what is gone is said to be.
        let catalog = format!("{}/catalog.json", server.site);
        assert_eq!(
            fetch(&catalog, 30).expect("fetched").as_deref(),
            Some(&b"[]"[..])
        );
        assert!(fetch_file(&catalog, &to, 30, 2).expect("fetched"));
        assert_eq!(fs::read(&to).expect("kept"), b"[]");
        let gone = format!("{}/gone.json", server.site);
        assert_eq!(fetch(&gone, 30).expect("answered"), None);
        assert!(!fetch_file(&gone, &to, 30, 100).expect("answered"));
        assert!(!to.exists(), "the server's page is not kept as the file");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn encodes_names_for_urls() {
        assert_eq!(
            encode("Nintendo - Game Boy (USA, Europe).png"),
            "Nintendo%20-%20Game%20Boy%20%28USA%2C%20Europe%29.png"
        );
        assert_eq!(encode("Pokémon"), "Pok%C3%A9mon");
    }

    #[test]
    fn a_folder_listing_gives_the_pictures_it_links_to() {
        let page = r#"<tr><td><a href="/Nintendo%20-%20Nintendo%20DS/">Parent Directory</a></td></tr>
<tr><td><a href="'Chou'%20Kowai%20Hanashi%20DS%20(Japan).png">'Chou' Kowai Hanashi DS (Japan).png</a></td></tr>
<tr><td><a href="Mario%20_%20Luigi%20-%20Partners%20in%20Time%20(USA).png">x</a></td></tr>
<tr><td><a href="Tom%20&amp;%20Jerry%20(USA).png">x</a></td></tr>
<tr><td><a href="?C=N;O=D">Name</a></td></tr>"#;
        assert_eq!(
            linked_pictures(page),
            [
                "'Chou' Kowai Hanashi DS (Japan).png",
                "Mario _ Luigi - Partners in Time (USA).png",
                "Tom & Jerry (USA).png",
            ]
        );
        assert_eq!(decode("Pok%C3%A9mon").as_deref(), Some("Pokémon"));
        assert_eq!(decode("100%"), Some("100%".to_owned()));
        assert_eq!(decode("%ZZ"), None);
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
