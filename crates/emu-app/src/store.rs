//! The store: free homebrew games for the systems the app plays, from two
//! community catalogs that share them with their authors' leave.
//!
//! - [Homebrew Hub](https://hh.gbdev.io), gbdev's archive of Game Boy,
//!   Game Boy Color and Game Boy Advance homebrew, which hosts each game's
//!   ROM and screenshots and has an API for apps.
//! - [Universal-DB](https://db.universal-team.net), the DS and 3DS homebrew
//!   catalog, whose games download from their authors' own releases.
//!
//! Nothing here is ever a commercial game: both catalogs list only homebrew
//! that its authors let be shared, and the entries that port a commercial
//! game or carry its data are withheld (see
//! `docs/decisions/0010-game-store.md`).
//! Games that `scripts/store-compat.py` found not to start in the app are
//! left out, by the verdicts in `store_compat.toml`.
//!
//! The catalogs are kept in the cache folder and fetched again once a day.
//! A game is installed into its own folder under the games folder, where
//! its saves sit beside it. Downloads go through `curl`, as box art does:
//! from `https://` addresses only, so a game or a picture a catalog lists
//! at any other is not offered, and each held to a size, a game to the
//! most a cartridge of its system holds. A game is written to a file as
//! it comes, as a 3DS one can be 4 GB.
//! Everything here blocks; run it off the UI thread.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Cursor, Read as _};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, bail};
use serde_json::Value;

use crate::archive;
use crate::boxart;
use crate::leftovers::WorkFolder;
use crate::persist;
use crate::rom_info::Platform;

/// Homebrew Hub's API and files.
const HOMEBREW_HUB: &str = "https://hh3.gbdev.io";
/// How many entries to ask Homebrew Hub for at once.
const PAGE: usize = 500;
/// The most pages asked for, whatever number Homebrew Hub gives: ten times
/// the four its 1,600 entries make today.
const MAX_PAGES: u64 = 40;
/// Catalog entries the store never offers, whatever the catalog or the
/// checks say:
///
/// - crack groups' intros and trainers named after, shipped with or
///   holding the commercial game they were attached to;
/// - ports of commercial games, and engines that need one's data, such as
///   Raptor's DS and 3DS port, which carries its shareware data;
/// - fan ports and remakes that carry a commercial game's own art, sound
///   or music (Balatro's, Cookie Clicker's, Mario Kart's, Super Photo
///   Studio's cast), or a whole game that is not their porter's to share
///   (OFF, Doki Doki Literature Club);
/// - an installer that patches the console's boot firmware.
///
/// Judged by their ROMs' headers, sizes and contents, or by what their own
/// pages and files say; `scripts/store-compat.py` reads this list too.
const WITHHELD: [&str; 56] = [
    "hh:benjamin-bluemchen-2",
    "hh:bounced-5",
    "hh:bounced-5-trainer",
    "hh:dangan-gb-4",
    "hh:denki-blocks",
    "hh:harvest-moon-3",
    "hh:hs-gbc-intro-1",
    "hh:jet-set-willy-v2-beta-2",
    "hh:pooh-and-tigger-hunny-safari-7",
    "hh:prince-naseem-boxing-one-giant-moo",
    "hh:shantae-8",
    "hh:test-menu-v1-2-5",
    "hh:tonka-construction-site-6",
    "hh:true-lies-1",
    "hh:vip",
    "hh:vip-5",
    "hh:wwf-betrayal-5-are-you-real",
    "udb:3d-pinball---space-cadet",
    "udb:3dscookieclicker",
    "udb:balatro3ds",
    "udb:bart-bash",
    "udb:bugdom",
    "udb:cro-mag-rally",
    "udb:ctgp-7-downloader",
    "udb:ctrquake",
    "udb:ddlc-love",
    "udb:devilutionx",
    "udb:dungeons-of-daggorath-3ds",
    "udb:eduke3d",
    "udb:emerald-online-3ds",
    "udb:geometry-dash-3ds",
    "udb:halo-renewed-solitude",
    "udb:halo-revamped",
    "udb:ioquake3ds",
    "udb:lego-island",
    "udb:level256-installer",
    "udb:mario-kart-64-3ds",
    "udb:off",
    "udb:omnispeak-3ds",
    "udb:opentitus-3ds",
    "udb:papas-pizzeria",
    "udb:postal-1-3ds",
    "udb:prboom",
    "udb:prboom3ds",
    "udb:quake2ctr",
    "udb:raptor-ds3ds",
    "udb:sonic-1--2",
    "udb:sonic-cd",
    "udb:sonic-mania",
    "udb:super-photo-studio",
    "udb:thextech",
    "udb:xash3ds",
    "udb:xrick-3ds",
    "udb:yet-another-mario-kart-clone-3ds",
    "udb:zelda-alttp-3ds",
    "udb:zelda-tmc-3ds",
];
/// Universal-DB's whole catalog, as one JSON list.
const UNIVERSAL_DB: &str = "https://db.universal-team.net/data/full.json";

/// How long a fetched catalog is used before it is fetched again.
const FRESH_FOR: Duration = Duration::from_secs(24 * 60 * 60);
/// The longest a game's download may take.
const DOWNLOAD_SECONDS: u32 = 600;
/// Covers narrower than this many pixels are enlarged, by whole pixels, to
/// at least this wide, so pixel art stays sharp when the store scales it.
const COVER_WIDTH: u32 = 480;
/// The most pixels a cover is enlarged to, about five times a 3DS's two
/// screens at twice their size. A picture is enlarged fewer times over,
/// or not at all, to stay within it.
const COVER_PIXELS: u64 = 4_000_000;

/// Where a game in the store comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    /// gbdev's Homebrew Hub: Game Boy, Game Boy Color and GBA games.
    HomebrewHub,
    /// Universal-DB: DS and 3DS games.
    UniversalDb,
}

impl Source {
    /// The short name that starts a game's [`Offer::id`].
    pub fn tag(self) -> &'static str {
        match self {
            Self::HomebrewHub => "hh",
            Self::UniversalDb => "udb",
        }
    }

    /// The catalog's name, as the store credits it.
    pub fn name(self) -> &'static str {
        match self {
            Self::HomebrewHub => "Homebrew Hub",
            Self::UniversalDb => "Universal-DB",
        }
    }

    /// The file the catalog is kept in, in the store's cache folder.
    fn cache_file(self) -> &'static str {
        match self {
            Self::HomebrewHub => "homebrew-hub.json",
            Self::UniversalDb => "universal-db.json",
        }
    }
}

/// What sort of program a game in the store is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A game.
    Game,
    /// A demo: graphics and music to watch.
    Demo,
    /// A music cartridge.
    Music,
    /// A tool or test program.
    Tool,
}

impl Kind {
    /// Every kind, in the order the store's filter shows them.
    pub const ALL: [Self; 4] = [Self::Game, Self::Demo, Self::Music, Self::Tool];

    /// The kind's name, plural, for the filter.
    pub fn plural(self) -> &'static str {
        match self {
            Self::Game => "Games",
            Self::Demo => "Demos",
            Self::Music => "Music",
            Self::Tool => "Tools",
        }
    }

    /// Homebrew Hub's `typetag`. Entries without one are games.
    fn from_typetag(tag: Option<&str>) -> Self {
        match tag {
            Some("demo") => Self::Demo,
            Some("music") => Self::Music,
            Some("tool" | "homebrew") => Self::Tool,
            _ => Self::Game,
        }
    }
}

/// A game the store offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    /// The catalog it is from.
    pub source: Source,
    /// Its name in that catalog.
    pub slug: String,
    pub title: String,
    /// Who made it, names joined.
    pub developer: String,
    /// The system it is for.
    pub platform: Platform,
    pub kind: Kind,
    /// What the catalog says of it, maybe empty.
    pub description: String,
    /// Its genres and the like, tidied, at most a few.
    pub tags: Vec<String>,
    /// The licence it is shared under, if the catalog says.
    pub license: Option<String>,
    /// Its website or source code: an `http://` or `https://` link.
    pub website: Option<String>,
    /// When the catalog listed it, as an RFC 3339 date, which sorts as
    /// text.
    pub added: String,
    /// The file to download, and what to call it.
    pub url: String,
    pub file: String,
    /// Its screenshots, the cover first.
    pub screenshots: Vec<String>,
    /// Whether it was seen to start in the app.
    pub tested: bool,
}

impl Offer {
    /// The game's name across both catalogs, as `store_compat.toml` gives
    /// it: `hh:2048gb`, `udb:ludonds`.
    pub fn id(&self) -> String {
        format!("{}:{}", self.source.tag(), self.slug)
    }

    /// Whether the game's title, maker or tags hold `query`, which is
    /// lower case.
    pub fn matches(&self, query: &str) -> bool {
        query.is_empty()
            || self.title.to_lowercase().contains(query)
            || self.developer.to_lowercase().contains(query)
            || self
                .tags
                .iter()
                .any(|tag| tag.to_lowercase().contains(query))
    }
}

/// The store's games, newest first, and whether they are fresh.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    pub offers: Vec<Offer>,
    /// Whether every catalog was fetched within the last day.
    pub fresh: bool,
}

/// Where the store keeps its catalogs and covers: a folder in the cache
/// folder.
pub fn cache_dir(cache: &Path) -> PathBuf {
    cache.join("store")
}

/// The catalogs as last fetched, without going online. Empty if they never
/// were.
pub fn cached(cache: &Path) -> Catalog {
    let folder = cache_dir(cache);
    let mut fresh = true;
    let mut offers = Vec::new();
    for source in [Source::HomebrewHub, Source::UniversalDb] {
        let path = folder.join(source.cache_file());
        let age = fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok());
        fresh &= age.is_some_and(|age| age < FRESH_FOR);
        if let Ok(text) = fs::read_to_string(&path) {
            offers.extend(parse(source, &text).unwrap_or_default());
        }
    }
    Catalog {
        offers: arrange(offers),
        fresh,
    }
}

/// Fetches both catalogs anew, keeps them, and returns the games in them.
/// A catalog that cannot be fetched is used as last kept.
///
/// # Errors
///
/// Fails, with why, if either catalog cannot be fetched; the games of the
/// other are then lost to the caller, but kept for [`cached`].
pub fn fetch(cache: &Path) -> anyhow::Result<Catalog> {
    let folder = cache_dir(cache);
    let (hub, udb) = std::thread::scope(|scope| {
        let hub = scope.spawn(|| fetch_homebrew_hub(HOMEBREW_HUB));
        let udb = scope.spawn(|| -> anyhow::Result<String> {
            let bytes = boxart::fetch(UNIVERSAL_DB, 120)?
                .with_context(|| format!("{UNIVERSAL_DB} is gone"))?;
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        });
        (
            hub.join().expect("fetching does not panic"),
            udb.join().expect("fetching does not panic"),
        )
    });
    let mut failure = None;
    for (source, text) in [(Source::HomebrewHub, hub), (Source::UniversalDb, udb)] {
        let kept = text.and_then(|text| {
            // Checked before it is kept, so a bad download is not.
            parse(source, &text)?;
            boxart::write(&folder.join(source.cache_file()), text.as_bytes())?;
            Ok(())
        });
        if let Err(error) = kept {
            failure.get_or_insert(error.context(format!("cannot fetch {}", source.name())));
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(cached(cache)),
    }
}

/// The whole catalog of Homebrew Hub, at `site`, a page at a time, as one
/// JSON object with every entry in `entries`.
///
/// Pages are asked for in slug order: in the default order, entries added
/// at the same moment move between pages, so some come twice and some
/// never. If any are still missing, all are asked for in one page. It says
/// how many pages there are, and no more than [`MAX_PAGES`] are asked for.
fn fetch_homebrew_hub(site: &str) -> anyhow::Result<String> {
    let get = |query: &str| -> anyhow::Result<Value> {
        let url = format!("{site}/api/search?{query}&sort=slug&order=asc");
        let bytes = boxart::fetch(&url, 120)?.with_context(|| format!("{url} is gone"))?;
        serde_json::from_slice(&bytes).context("Homebrew Hub's answer")
    };
    let mut entries = Vec::new();
    let mut slugs = HashSet::new();
    let mut total = 0;
    for page in 1..=MAX_PAGES {
        let mut body = get(&format!("results={PAGE}&page={page}"))?;
        total = body["results"].as_u64().unwrap_or(0);
        let pages = body["page_total"].as_u64().unwrap_or(0);
        if let Value::Array(more) = body["entries"].take() {
            for entry in more {
                if slugs.insert(entry["slug"].as_str().unwrap_or_default().to_owned()) {
                    entries.push(entry);
                }
            }
        }
        if page >= pages {
            break;
        }
    }
    if (entries.len() as u64) < total {
        let mut body = get(&format!("results={total}&page=1"))?;
        if let Value::Array(all) = body["entries"].take() {
            entries = all;
        }
    }
    Ok(serde_json::json!({ "entries": entries }).to_string())
}

/// The games in `source`'s catalog, as `text`, that the app can play and
/// that are not known not to start.
///
/// # Errors
///
/// Fails if `text` is not the catalog's JSON.
pub fn parse(source: Source, text: &str) -> anyhow::Result<Vec<Offer>> {
    read_offers(source, text, verdicts())
}

/// The games in `source`'s catalog, as `text`, that the app can play and
/// that `verdicts` does not say fail to start.
fn read_offers(
    source: Source,
    text: &str,
    verdicts: &HashMap<String, String>,
) -> anyhow::Result<Vec<Offer>> {
    let json: Value = serde_json::from_str(text).context("the catalog is not JSON")?;
    let entries = match source {
        Source::HomebrewHub => json["entries"].as_array(),
        Source::UniversalDb => json.as_array(),
    };
    let Some(entries) = entries else {
        bail!("{} sent no list of games", source.name());
    };
    let offers = entries.iter().filter_map(|entry| match source {
        Source::HomebrewHub => homebrew_hub_offer(entry),
        Source::UniversalDb => universal_db_offer(entry),
    });
    Ok(offers
        .filter(|offer| !WITHHELD.contains(&offer.id().as_str()))
        .filter_map(|mut offer| {
            match verdicts.get(&offer.id()).map(String::as_str) {
                None => {}
                Some("ok") => offer.tested = true,
                Some(_) => return None,
            }
            Some(offer)
        })
        .collect())
}

/// Newest first, then by title, each game once.
fn arrange(mut offers: Vec<Offer>) -> Vec<Offer> {
    offers.sort_by(|a, b| {
        b.added
            .cmp(&a.added)
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
    let mut seen = HashSet::new();
    offers.retain(|offer| seen.insert(offer.id()));
    offers
}

/// A Homebrew Hub entry, if it is for a system the app plays and has a
/// ROM to download.
fn homebrew_hub_offer(entry: &Value) -> Option<Offer> {
    let slug = entry["slug"].as_str()?;
    let base = entry["basepath"].as_str()?;
    let folder = format!(
        "{HOMEBREW_HUB}/static/{base}/entries/{}",
        boxart::encode(slug)
    );
    let url_of = |file: &str| {
        let path: Vec<String> = file.split('/').map(boxart::encode).collect();
        format!("{folder}/{}", path.join("/"))
    };
    let files = entry["files"].as_array()?;
    let playable = files.iter().filter(|file| {
        file["playable"].as_bool() == Some(true)
            && file["filename"].as_str().is_some_and(is_rom_name)
    });
    let chosen = playable
        .clone()
        .find(|file| file["default"].as_bool() == Some(true))
        .or_else(|| playable.clone().next())?;
    let file = chosen["filename"].as_str()?;
    let platform = match entry["platform"].as_str() {
        Some("GB") => Platform::Gb,
        Some("GBC") => Platform::Gbc,
        Some("GBA") => Platform::Gba,
        Some(_) => return None,
        None => platform_of_file(file)?,
    };
    let screenshots = strings(&entry["screenshots"])
        .into_iter()
        .map(|name| url_of(&name))
        .collect();
    Some(Offer {
        source: Source::HomebrewHub,
        slug: slug.to_owned(),
        title: text(&entry["title"]).unwrap_or_else(|| slug.to_owned()),
        developer: developers(&entry["developer"]),
        platform,
        kind: Kind::from_typetag(entry["typetag"].as_str()),
        description: text(&entry["description"]).unwrap_or_default(),
        tags: tidy_tags(&strings(&entry["tags"])),
        license: text(&entry["license"]).or_else(|| text(&entry["gameLicense"])),
        website: strings(&entry["website"])
            .into_iter()
            .chain(strings(&entry["repository"]))
            .find(|link| is_web_link(link)),
        added: text(&entry["firstadded_date"]).unwrap_or_default(),
        url: url_of(file),
        file: file_name(file)?,
        screenshots,
        tested: false,
    })
}

/// A Universal-DB entry, if it is a DS or 3DS game with a game, or an
/// archive of one, to download from an `https://` address. An entry for
/// both systems is offered once: as a DS game if it has a DS build to be
/// had, else as a 3DS game.
fn universal_db_offer(entry: &Value) -> Option<Offer> {
    let systems = strings(&entry["systems"]);
    let categories = strings(&entry["categories"]);
    if !categories.iter().any(|c| c == "game") {
        return None;
    }
    let for_ds = systems.iter().any(|system| system == "DS");
    let for_3ds = systems.iter().any(|system| system == "3DS");
    let slug = entry["slug"].as_str()?;
    let downloads = entry["downloads"].as_object()?;
    // The best of the downloads the app can have: those at an `https://`
    // address. An entry is left out only if it has none.
    let best = |score: &dyn Fn(&str) -> Option<i32>| {
        downloads
            .iter()
            .filter(|(_, download)| text(&download["url"]).is_some_and(|url| downloadable(&url)))
            .filter(|(name, _)| score(name).is_some())
            // The first of the best, as `max_by_key` would give the last.
            .rev()
            .max_by_key(|(name, _)| score(name))
    };
    // An archive named for the 3DS is an entry's 3DS build, not a DS one.
    let ds_build = |name: &str| download_score(name).filter(|_| !(for_3ds && names_3ds(name)));
    let ds = if for_ds { best(&ds_build) } else { None };
    let (platform, (file, download)) = match ds {
        Some(chosen) => (Platform::Nds, chosen),
        None if for_3ds => (Platform::N3ds, best(&download_score_3ds)?),
        None => return None,
    };
    let screenshots: Vec<String> = entry["screenshots"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|shot| text(&shot["url"]))
        .chain(text(&entry["image"]))
        .filter(|url| downloadable(url))
        .collect();
    let description = text(&entry["long_description"])
        .or_else(|| text(&entry["description"]))
        .unwrap_or_default();
    Some(Offer {
        source: Source::UniversalDb,
        slug: slug.to_owned(),
        title: text(&entry["title"]).unwrap_or_else(|| slug.to_owned()),
        developer: text(&entry["author"]).unwrap_or_default(),
        platform,
        kind: Kind::Game,
        description,
        tags: Vec::new(),
        license: text(&entry["license_name"]),
        website: text(&entry["website"])
            .into_iter()
            .chain(text(&entry["source"]))
            .chain(strings(&entry["urls"]))
            .find(|link| is_web_link(link)),
        added: text(&entry["created"]).unwrap_or_default(),
        url: text(&download["url"])?,
        file: file_name(file)?,
        screenshots,
        tested: false,
    })
}

/// Whether `link`, which a catalog gives as a game's website or source
/// code, is a web link, `http://` or `https://` and something after it.
/// The app opens a game's website in the browser, and the system would
/// open a link of any other kind, such as `file:`, with whatever program
/// takes it.
fn is_web_link(link: &str) -> bool {
    ["http://", "https://"].iter().any(|scheme| {
        link.strip_prefix(scheme)
            .is_some_and(|rest| !rest.is_empty())
    })
}

/// Whether the app downloads from `url`, an address a catalog gave: only
/// if it is an `https://` one, as [`boxart::fetch`] refuses any other.
fn downloadable(url: &str) -> bool {
    emu_os::fetch::Allow::Https.takes(url)
}

/// How good a choice the Universal-DB download called `name` is: a DS ROM
/// over an archive of one, a DS build over a 3DS one, a build made for
/// emulators over one for flash cards' file systems, sound over none.
/// `None` if it holds no DS game.
fn download_score(name: &str) -> Option<i32> {
    let lower = name.to_lowercase();
    let rom = lower.ends_with(".nds");
    if !rom && !is_archive_name(&lower) {
        return None;
    }
    let words = words(&lower);
    let mut score = i32::from(rom);
    if lower.contains("emulator") {
        score += 2;
    }
    if words.contains(&"fat") {
        score -= 2;
    }
    if words.contains(&"nds") {
        score += 1;
    }
    if words.contains(&"3ds") {
        score -= 3;
    }
    if lower.contains("nosound") || lower.contains("no-sound") {
        score -= 1;
    }
    Some(score)
}

/// Systems other than the 3DS that a download's name may be for.
const OTHER_SYSTEMS: [&str; 19] = [
    "android", "gamecube", "linux", "mac", "macos", "nds", "pc", "ps3", "psp", "switch", "vita",
    "wii", "wiiu", "win", "win32", "win64", "windows", "x64", "x86",
];

/// How good a choice the Universal-DB download called `name` is for a 3DS
/// game: homebrew's own `.3dsx` over an installable title (`.cia`) over an
/// archive of either. Of archives, one named for a `.3dsx` over one named
/// for the 3DS over one named for a `.cia`, any of them over one named for
/// another system, and of equals a ZIP over a 7-Zip file, which macOS's
/// `tar` cannot always unpack. `None` if it holds no 3DS game.
fn download_score_3ds(name: &str) -> Option<i32> {
    let lower = name.to_lowercase();
    if lower.ends_with(".3dsx") {
        return Some(20);
    }
    if lower.ends_with(".cia") {
        return Some(10);
    }
    if !is_archive_name(&lower) {
        return None;
    }
    let words = words(&lower);
    let mut score = 0;
    if words.contains(&"3dsx") {
        score += 2;
    }
    if words.contains(&"3ds") {
        score += 1;
    }
    if words.contains(&"cia") {
        score -= 1;
    }
    if words.iter().any(|word| OTHER_SYSTEMS.contains(word)) {
        score -= 3;
    }
    Some(2 * score + i32::from(lower.ends_with(".zip")))
}

/// Whether the download called `name` is named as a 3DS build.
fn names_3ds(name: &str) -> bool {
    let lower = name.to_lowercase();
    words(&lower)
        .iter()
        .any(|word| ["3ds", "3dsx", "cia"].contains(word))
}

/// Whether `lower`, a lower-case file name, is an archive the store
/// unpacks.
fn is_archive_name(lower: &str) -> bool {
    lower.ends_with(".zip") || lower.ends_with(".7z")
}

/// The words of `lower`'s name without its extension: its runs of letters
/// and digits.
fn words(lower: &str) -> Vec<&str> {
    Path::new(lower)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect()
}

/// Whether `name` is a ROM the app plays: a Game Boy, Game Boy Color or
/// GBA one, by its extension.
fn is_rom_name(name: &str) -> bool {
    platform_of_file(name).is_some()
}

/// The system a ROM called `name` is for, by its extension.
fn platform_of_file(name: &str) -> Option<Platform> {
    let extension = Path::new(name).extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "gb" => Some(Platform::Gb),
        "gbc" | "cgb" => Some(Platform::Gbc),
        "gba" | "agb" => Some(Platform::Gba),
        _ => None,
    }
}

/// The last part of `path`, if it is a plain file name.
fn file_name(path: &str) -> Option<String> {
    let name = safe_name(path.rsplit('/').next()?);
    (!name.is_empty()).then_some(name)
}

/// `name` with what a file name cannot safely hold replaced, and never
/// `.` or `..`; on Windows, never a device's name such as `CON` either
/// (see [`persist::safe_file_name`]). Empty if nothing is left.
fn safe_name(name: &str) -> String {
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " ._-()".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let name = name.trim().trim_start_matches('.');
    if name.is_empty() {
        return String::new();
    }
    persist::safe_file_name(name).into_owned()
}

/// A JSON string, trimmed, if it is one and not empty.
fn text(value: &Value) -> Option<String> {
    let text = value.as_str()?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// A JSON string, or the strings in a JSON list.
fn strings(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items.iter().filter_map(text).collect(),
        _ => text(value).into_iter().collect(),
    }
}

/// Homebrew Hub's `developer`: a name, a list of names, or a list (or one)
/// of `{ name, link, role }`.
fn developers(value: &Value) -> String {
    let name = |value: &Value| text(value).or_else(|| text(&value["name"]));
    let names: Vec<String> = match value {
        Value::Array(items) => items.iter().filter_map(name).collect(),
        _ => name(value).into_iter().collect(),
    };
    names.join(", ")
}

/// At most four of `tags`, without game jams' and events' names (or "No
/// AI"), each once whatever its case, capitalised.
fn tidy_tags(tags: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    tags.iter()
        .map(|tag| tag.trim())
        .filter(|tag| {
            let lower = tag.to_lowercase();
            !lower.contains(':')
                && !lower.chars().any(|c| c.is_ascii_digit())
                && !["jam", "compo", "showdown"]
                    .iter()
                    .any(|event| lower.contains(event))
                && !lower.split([' ', '-']).any(|word| word == "ai")
        })
        .filter(|tag| !tag.is_empty() && seen.insert(tag.to_lowercase()))
        .take(4)
        .map(|tag| {
            let mut chars = tag.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        })
        .collect()
}

/// What `scripts/store-compat.py` found of each game, by [`Offer::id`]:
/// `"ok"` if it started in the app, or why not.
fn verdicts() -> &'static HashMap<String, String> {
    static VERDICTS: OnceLock<HashMap<String, String>> = OnceLock::new();
    VERDICTS.get_or_init(|| parse_verdicts(include_str!("store_compat.toml")))
}

fn parse_verdicts(text: &str) -> HashMap<String, String> {
    #[derive(serde::Deserialize, Default)]
    struct File {
        #[serde(default)]
        verdicts: HashMap<String, String>,
    }
    toml::from_str::<File>(text)
        .map(|file| file.verdicts)
        .unwrap_or_default()
}

/// The folder `offer` is installed into, in the games folder `games`: one
/// per game, as its saves go beside it.
pub fn install_dir(games: &Path, offer: &Offer) -> PathBuf {
    games
        .join("Store")
        .join(offer.source.tag())
        .join(safe_name(&offer.slug))
}

/// Downloads `offer` into its [`install_dir`] in `games`, unpacking it if
/// it comes as an archive, and returns its ROMs: for a 3DS game, only the
/// best of those in its archive (see [`best_3ds_game`]). The download is
/// held to the most a cartridge of the game's system holds.
///
/// # Errors
///
/// Fails, with a message for the user, if the game cannot be downloaded,
/// is gone, cannot be unpacked (and why), or holds no ROM the app plays.
pub fn install(offer: &Offer, games: &Path) -> Result<Vec<PathBuf>, String> {
    let max_bytes = match offer.platform {
        Platform::N3ds => archive::MAX_3DS_GAME,
        _ => archive::MAX_ROM,
    };
    install_up_to(offer, games, max_bytes)
}

/// Installs `offer` as [`install`] does, if its download is at most
/// `max_bytes`.
fn install_up_to(offer: &Offer, games: &Path, max_bytes: u64) -> Result<Vec<PathBuf>, String> {
    let keeping = |error: std::io::Error| format!("Cannot keep {}: {error}", offer.title);
    // Downloaded into a file of a work folder as it comes, never into
    // memory: a game can be 4 GB.
    let scratch = WorkFolder::new("store").map_err(keeping)?;
    let download = scratch.path().join(&offer.file);
    match boxart::fetch_file(&offer.url, &download, DOWNLOAD_SECONDS, max_bytes) {
        Ok(true) => {}
        Ok(false) => return Err(format!("{} is no longer offered", offer.title)),
        Err(error) => return Err(format!("Cannot get {}: {error:#}", offer.title)),
    }
    let folder = install_dir(games, offer);
    let path = folder.join(&offer.file);
    // Some catalog entries are ZIP files named like the ROM inside.
    let zipped = !archive::is_archive(&path) && starts_as_zip(&download);
    if archive::is_archive(&path) || zipped {
        let download = if zipped {
            let zip = download.with_extension("zip");
            fs::rename(&download, &zip).map_err(keeping)?;
            zip
        } else {
            download
        };
        let unpacked = archive::unpack_to(&download, &folder);
        drop(scratch);
        // Why it failed, if it did: a full disk, a password or a program
        // to install is not an archive without a game.
        let games = unpacked.map_err(|error| format!("Cannot unpack {}: {error}", offer.title))?;
        if games.is_empty() {
            return Err(format!("{} holds no game the app plays", offer.title));
        }
        return Ok(match offer.platform {
            Platform::N3ds => best_3ds_game(games),
            _ => games,
        });
    }
    if emu_cores::system_for_path(&path).is_none() {
        return Err(format!("{} is not a game the app plays", offer.title));
    }
    fs::create_dir_all(&folder)
        .and_then(|()| archive::move_capped(&download, &path, Path::new(&offer.file)))
        .map_err(keeping)?;
    Ok(vec![path])
}

/// The one of `games`, unpacked from a 3DS game's archive, to add to the
/// library: its `.3dsx`, else its `.cia`, else another 3DS file, the first
/// of equals in the archive. Such an archive often holds the game twice, as
/// a `.3dsx` and a `.cia`, and it is one game. The others stay in its
/// folder, unused, as a file there may be one the library already has.
fn best_3ds_game(mut games: Vec<PathBuf>) -> Vec<PathBuf> {
    // Stable, so equals keep the archive's order.
    games.sort_by_key(|game| {
        let extension = game
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase);
        match extension.as_deref() {
            Some("3dsx") => 0,
            Some("cia") => 1,
            _ if emu_3ds::is_game(game) => 2,
            _ => 3,
        }
    });
    games.truncate(1);
    games
}

/// Whether `bytes` are a ZIP file, by how it starts.
fn is_zip(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04")
}

/// Whether the file at `path` is a ZIP file, by how it starts.
fn starts_as_zip(path: &Path) -> bool {
    let mut start = [0; 4];
    fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut start))
        .is_ok_and(|()| is_zip(&start))
}

/// Where the picture at `url` is kept, as a PNG, in the cache folder
/// `cache`: named by a checksum of its URL.
pub fn picture_path(cache: &Path, url: &str) -> PathBuf {
    let name = format!("{:08X}-{}.png", crc32fast::hash(url.as_bytes()), url.len());
    cache_dir(cache).join("pictures").join(name)
}

/// Downloads the picture at `url` to [`picture_path`] as a PNG, unless it
/// is there already. Where it is, or `None` if it is gone or is not a
/// picture.
///
/// # Errors
///
/// Fails if the picture cannot be downloaded or kept.
pub fn download_picture(cache: &Path, url: &str) -> anyhow::Result<Option<PathBuf>> {
    let path = picture_path(cache, url);
    if boxart::is_png(&path) {
        return Ok(Some(path));
    }
    let picture = boxart::fetch_up_to(url, 60, boxart::MAX_PICTURE)?;
    let Some(png) = picture.as_deref().and_then(cover_png) else {
        return Ok(None);
    };
    boxart::write(&path, &png)?;
    Ok(Some(path))
}

/// Downloads `offer`'s cover: the first of its screenshots to be had, as
/// [`download_picture`] does.
///
/// # Errors
///
/// Fails if a picture cannot be downloaded or kept.
pub fn download_cover(cache: &Path, offer: &Offer) -> anyhow::Result<Option<PathBuf>> {
    for url in &offer.screenshots {
        if let Some(path) = download_picture(cache, url)? {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

/// The picture in `bytes` as a PNG, enlarged by whole pixels to at least
/// [`COVER_WIDTH`] wide, if that is no more than [`COVER_PIXELS`] in all,
/// or `None` if it is not a picture.
pub fn cover_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let picture = image::load_from_memory(bytes).ok()?.into_rgba8();
    let width = picture.width().max(1);
    // A narrow picture can be very tall: the most times over, each way,
    // that keep it a cover's size.
    let pixels = u64::from(width) * u64::from(picture.height().max(1));
    let most = u32::try_from((COVER_PIXELS / pixels).isqrt()).unwrap_or(u32::MAX);
    let factor = COVER_WIDTH.div_ceil(width).clamp(1, 8).min(most.max(1));
    let picture = if factor > 1 {
        image::imageops::resize(
            &picture,
            width * factor,
            picture.height() * factor,
            image::imageops::FilterType::Nearest,
        )
    } else {
        picture
    };
    let mut png = Vec::new();
    picture
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HUB: &str = r#"{"entries": [
        {"slug": "2048gb", "title": "2048gb", "developer": "Sanqui", "platform": "GB",
         "typetag": "game", "basepath": "database-gb", "license": "Zlib",
         "tags": ["Open Source", "Puzzle", "puzzle", "event:gbcompo21", "gbajam22", "No AI"],
         "repository": "https://github.com/Sanqui/2048-gb",
         "firstadded_date": "2018-01-17T16:33:33Z",
         "screenshots": ["1.png", "2.png"],
         "files": [{"filename": "2048.gb", "playable": true, "default": true}]},
        {"slug": "drakoko", "title": "Drakoko", "platform": "GB", "typetag": "demo",
         "developer": [{"name": "RYUHA Studio", "role": "all"}, {"name": "Someone"}],
         "basepath": "database-gb", "firstadded_date": "2026-10-19T17:37:55Z",
         "website": ["https://ryuha.example/drakoko", "https://kickstarter.example"],
         "screenshots": ["cover.png"],
         "files": [{"filename": "files/manual.pdf", "playable": false},
                   {"filename": "files/drakoko demo.gbc", "playable": true}]},
        {"slug": "no-platform", "title": "Colourful", "basepath": "database-gb",
         "firstadded_date": "2018-12-23T12:44:57Z", "screenshots": [],
         "files": [{"filename": "colour.cgb", "playable": true}]},
        {"slug": "nes-game", "title": "NES", "platform": "NES", "basepath": "database-nes",
         "firstadded_date": "2020-01-01", "screenshots": [],
         "files": [{"filename": "game.nes", "playable": true}]},
        {"slug": "no-files", "title": "Celeste", "platform": "GBA", "basepath": "database-gba",
         "firstadded_date": "2026-10-19T20:56:27Z", "screenshots": [], "files": []}
    ]}"#;

    const UDB: &str = r#"[
        {"slug": "dscraft", "title": "DScraft", "author": "smea", "systems": ["DS"],
         "categories": ["game"], "created": "2017-03-07T07:53:43Z",
         "source": "https://github.com/smealum/dscraft",
         "image": "https://db.example/dscraft.png",
         "downloads": {
            "DScraft_310811.7z": {"url": "https://db.example/DScraft_310811.7z"},
            "DScraft_fat_310811.7z": {"url": "https://db.example/DScraft_fat_310811.7z"}}},
        {"slug": "cs", "title": "Counter", "systems": ["DS"], "categories": ["game"],
         "created": "2021-11-23T15:51:58Z",
         "screenshots": [{"url": "https://db.example/cs1.png", "description": "a"}],
         "downloads": {
            "CS.1.0.zip": {"url": "https://x/CS.1.0.zip"},
            "CS.1.0.Emulator.Version.zip": {"url": "https://x/CS.1.0.Emulator.Version.zip"}}},
        {"slug": "ludo", "title": "LudoNDS", "systems": ["DS"], "categories": ["game"],
         "license_name": "GNU General Public License v3.0", "created": "2022-01-01",
         "downloads": {"LudoNDS.nds": {"url": "https://x/LudoNDS.nds"},
                       "LudoNDS.dsi": {"url": "https://x/LudoNDS.dsi"}}},
        {"slug": "3ds-only", "title": "3DS", "systems": ["3DS"], "categories": ["game"],
         "downloads": {"a.3dsx": {"url": "https://x/a.3dsx"}}},
        {"slug": "tool", "title": "Tool", "systems": ["DS"], "categories": ["utility"],
         "downloads": {"tool.nds": {"url": "https://x/tool.nds"}}}
    ]"#;

    fn by_slug<'a>(offers: &'a [Offer], slug: &str) -> &'a Offer {
        offers
            .iter()
            .find(|offer| offer.slug == slug)
            .unwrap_or_else(|| panic!("{slug} is offered"))
    }

    #[test]
    fn homebrew_hub_offers_its_playable_game_boy_and_gba_roms() {
        let offers = read_offers(Source::HomebrewHub, HUB, &HashMap::new()).expect("parses");
        let slugs: Vec<&str> = offers.iter().map(|offer| offer.slug.as_str()).collect();
        assert_eq!(
            slugs,
            ["2048gb", "drakoko", "no-platform"],
            "NES and file-less entries are left out"
        );

        let game = by_slug(&offers, "2048gb");
        assert_eq!(game.id(), "hh:2048gb");
        assert_eq!(game.platform, Platform::Gb);
        assert_eq!(game.kind, Kind::Game);
        assert_eq!(game.developer, "Sanqui");
        assert_eq!(game.license.as_deref(), Some("Zlib"));
        assert_eq!(game.tags, ["Open Source", "Puzzle"]);
        assert_eq!(
            game.website.as_deref(),
            Some("https://github.com/Sanqui/2048-gb")
        );
        assert_eq!(
            game.url,
            "https://hh3.gbdev.io/static/database-gb/entries/2048gb/2048.gb"
        );
        assert_eq!(game.file, "2048.gb");
        assert_eq!(
            game.screenshots[0],
            "https://hh3.gbdev.io/static/database-gb/entries/2048gb/1.png"
        );

        let demo = by_slug(&offers, "drakoko");
        assert_eq!(demo.kind, Kind::Demo);
        assert_eq!(demo.developer, "RYUHA Studio, Someone");
        assert_eq!(
            demo.website.as_deref(),
            Some("https://ryuha.example/drakoko")
        );
        assert_eq!(
            demo.url,
            "https://hh3.gbdev.io/static/database-gb/entries/drakoko/files/drakoko%20demo.gbc"
        );
        assert_eq!(demo.file, "drakoko demo.gbc");

        let colour = by_slug(&offers, "no-platform");
        assert_eq!(colour.platform, Platform::Gbc, "judged by its extension");
    }

    #[test]
    fn universal_db_offers_ds_games_choosing_the_best_download() {
        let offers = read_offers(Source::UniversalDb, UDB, &HashMap::new()).expect("parses");
        let slugs: Vec<&str> = offers.iter().map(|offer| offer.slug.as_str()).collect();
        assert_eq!(slugs, ["dscraft", "cs", "ludo", "3ds-only"], "not the tool");
        assert_eq!(by_slug(&offers, "3ds-only").platform, Platform::N3ds);
        let dscraft = by_slug(&offers, "dscraft");
        assert_eq!(
            dscraft.file, "DScraft_310811.7z",
            "not the flash-card build"
        );
        assert_eq!(dscraft.platform, Platform::Nds);
        assert_eq!(
            dscraft.website.as_deref(),
            Some("https://github.com/smealum/dscraft")
        );
        assert_eq!(dscraft.screenshots, ["https://db.example/dscraft.png"]);
        assert_eq!(by_slug(&offers, "cs").file, "CS.1.0.Emulator.Version.zip");
        assert_eq!(
            by_slug(&offers, "cs").screenshots[0],
            "https://db.example/cs1.png"
        );
        let ludo = by_slug(&offers, "ludo");
        assert_eq!(ludo.file, "LudoNDS.nds");
        assert_eq!(
            ludo.license.as_deref(),
            Some("GNU General Public License v3.0")
        );
    }

    #[test]
    fn only_games_and_pictures_at_https_addresses_are_offered() {
        let catalog = r#"[
            {"slug": "plain", "title": "Plain", "systems": ["DS"], "categories": ["game"],
             "downloads": {"plain.nds": {"url": "http://x/plain.nds"}}},
            {"slug": "option", "title": "Option", "systems": ["DS"], "categories": ["game"],
             "downloads": {"option.nds": {"url": "-o/tmp/option.nds"}}},
            {"slug": "file", "title": "File", "systems": ["3DS"], "categories": ["game"],
             "downloads": {"file.3dsx": {"url": "file:///etc/hosts"}}},
            {"slug": "fine", "title": "Fine", "systems": ["DS"], "categories": ["game"],
             "image": "http://x/fine.png",
             "screenshots": [{"url": "ftp://x/1.png"}, {"url": "https://x/2.png"}],
             "downloads": {"fine.nds": {"url": "https://x/fine.nds"}}}
        ]"#;
        let offers = read_offers(Source::UniversalDb, catalog, &HashMap::new()).expect("parses");
        let slugs: Vec<&str> = offers.iter().map(|offer| offer.slug.as_str()).collect();
        assert_eq!(slugs, ["fine"], "the app downloads over HTTPS alone");
        assert_eq!(offers[0].url, "https://x/fine.nds");
        assert_eq!(offers[0].screenshots, ["https://x/2.png"]);
    }

    #[test]
    fn a_game_is_offered_by_the_best_of_its_https_downloads() {
        // The ROM would be chosen over the archive, and the DS build over
        // the 3DS one, but neither is at an address the app downloads from.
        let catalog = r#"[
            {"slug": "mixed", "title": "Mixed", "systems": ["DS"], "categories": ["game"],
             "downloads": {"mixed.nds": {"url": "http://x/mixed.nds"},
                           "mixed.zip": {"url": "https://x/mixed.zip"},
                           "mixed_fat.zip": {"url": "https://x/mixed_fat.zip"}}},
            {"slug": "both", "title": "Both", "systems": ["DS", "3DS"], "categories": ["game"],
             "downloads": {"both.nds": {"url": "ftp://x/both.nds"},
                           "both.cia": {"url": "https://x/both.cia"}}},
            {"slug": "none", "title": "None", "systems": ["3DS"], "categories": ["game"],
             "downloads": {"none.3dsx": {"url": "http://x/none.3dsx"},
                           "none.cia": {"url": "http://x/none.cia"}}}
        ]"#;
        let offers = read_offers(Source::UniversalDb, catalog, &HashMap::new()).expect("parses");
        let slugs: Vec<&str> = offers.iter().map(|offer| offer.slug.as_str()).collect();
        assert_eq!(
            slugs,
            ["mixed", "both"],
            "not the one with no such download"
        );
        let mixed = by_slug(&offers, "mixed");
        assert_eq!(
            (mixed.file.as_str(), mixed.url.as_str()),
            ("mixed.zip", "https://x/mixed.zip"),
            "the best of those it can download"
        );
        let both = by_slug(&offers, "both");
        assert_eq!(
            both.platform,
            Platform::N3ds,
            "its 3DS build is the one to be had"
        );
        assert_eq!(both.url, "https://x/both.cia");
    }

    /// An offer of the game called `file`, for `platform`, at `url`.
    fn offer(url: String, file: &str, platform: Platform) -> Offer {
        Offer {
            source: Source::HomebrewHub,
            slug: "test-game".to_owned(),
            title: "Test Game".to_owned(),
            developer: String::new(),
            platform,
            kind: Kind::Game,
            description: String::new(),
            tags: Vec::new(),
            license: None,
            website: None,
            added: String::new(),
            url,
            file: file.to_owned(),
            screenshots: Vec::new(),
            tested: false,
        }
    }

    /// A ZIP file holding `bytes` as `name`.
    fn zip_of(name: &str, bytes: &[u8]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .expect("start");
        std::io::Write::write_all(&mut zip, bytes).expect("write");
        zip.finish().expect("finish").into_inner()
    }

    /// What the stand-in for a catalog's file server answers: a game, a
    /// ZIP of a game named as the ROM inside is, a file that is no game,
    /// a ZIP without a game, and one cut short.
    fn files(path: &str) -> (u16, Vec<u8>) {
        match path {
            "/game.gb" => (200, b"a Game Boy game".to_vec()),
            "/zipped.gba" => (200, zip_of("Inner.gba", b"a GBA game")),
            "/notes.txt" => (200, b"notes".to_vec()),
            "/notes.zip" => (200, zip_of("notes.txt", b"notes")),
            "/broken.zip" => (
                200,
                b"PK\x03\x04 and then nothing a ZIP should have".to_vec(),
            ),
            _ => (404, b"<html>Not Found</html>".to_vec()),
        }
    }

    #[test]
    fn a_download_that_cannot_be_unpacked_says_why() {
        let server = boxart::stand_in::serve(files);
        let games = std::env::temp_dir().join(format!("ds-emulator-why-{}", std::process::id()));
        fs::remove_dir_all(&games).ok();
        let at = |path: &str| format!("{}{path}", server.site);
        let broken = offer(at("/broken.zip"), "broken.zip", Platform::Nds);
        let error = install(&broken, &games).expect_err("not a ZIP file");
        assert!(error.starts_with("Cannot unpack Test Game: "), "{error}");
        assert!(
            !error.contains("ds-emulator-store-") && !error.contains("broken.zip"),
            "the game is named, not the file it was downloaded to: {error}"
        );
        // Only an archive that unpacks, and has none, holds no game.
        let notes = offer(at("/notes.zip"), "notes.zip", Platform::Nds);
        assert_eq!(
            install(&notes, &games),
            Err("Test Game holds no game the app plays".to_owned())
        );
        fs::remove_dir_all(&games).ok();
    }

    #[test]
    fn a_game_is_downloaded_straight_into_a_file_and_held_to_a_size() {
        use std::sync::atomic::Ordering;

        let server = boxart::stand_in::serve(files);
        let games = std::env::temp_dir().join(format!("ds-emulator-games-{}", std::process::id()));
        fs::remove_dir_all(&games).ok();
        let folder = games.join("Store").join("hh").join("test-game");
        let at = |path: &str| format!("{}{path}", server.site);

        let rom = offer(at("/game.gb"), "game.gb", Platform::Gb);
        let installed = install(&rom, &games).expect("installed");
        assert_eq!(installed, [folder.join("game.gb")]);
        assert_eq!(fs::read(&installed[0]).expect("kept"), b"a Game Boy game");
        // Installed again, it replaces itself.
        assert_eq!(install(&rom, &games), Ok(installed));
        // A ZIP named like the ROM inside is unpacked.
        let zipped = offer(at("/zipped.gba"), "zipped.gba", Platform::Gba);
        let unpacked = install(&zipped, &games).expect("unpacked");
        assert_eq!(unpacked, [folder.join("Inner.gba")]);
        assert_eq!(fs::read(&unpacked[0]).expect("kept"), b"a GBA game");

        let gone = offer(at("/gone.gb"), "gone.gb", Platform::Gb);
        assert_eq!(
            install(&gone, &games),
            Err("Test Game is no longer offered".to_owned())
        );
        let notes = offer(at("/notes.txt"), "notes.txt", Platform::Gb);
        assert_eq!(
            install(&notes, &games),
            Err("Test Game is not a game the app plays".to_owned())
        );
        // More than a cartridge holds is stopped, and nothing of it kept.
        fs::remove_file(folder.join("game.gb")).expect("remove");
        assert_eq!(
            install_up_to(&rom, &games, 4),
            Err(format!(
                "Cannot get Test Game: cannot download {}: it is too big",
                rom.url
            ))
        );
        assert!(!folder.join("game.gb").exists());
        assert_eq!(server.requests.load(Ordering::SeqCst), 6);
        // An address that is not one never reaches curl.
        for url in [
            "-V",
            "--output=/tmp/x",
            "file:///etc/hosts",
            "ftp://x/game.gb",
        ] {
            let option = offer(url.to_owned(), "game.gb", Platform::Gb);
            assert_eq!(
                install(&option, &games),
                Err(format!(
                    "Cannot get Test Game: cannot download {url}: its address is not an https:// one"
                ))
            );
        }
        assert!(!folder.join("game.gb").exists());
        let work_folders = fs::read_dir(emu_os::temp::work_dir().expect("work folders"))
            .expect("temp")
            .flatten()
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.starts_with(&format!("ds-emulator-store-{}-", std::process::id()))
            })
            .count();
        assert_eq!(work_folders, 0, "the downloads are removed");
        fs::remove_dir_all(&games).ok();
    }

    #[test]
    fn homebrew_hub_is_asked_for_only_so_many_pages() {
        use std::sync::atomic::Ordering;

        // A catalog that says it has 120 pages, each with one game.
        let server = boxart::stand_in::serve(|path| {
            let page = path
                .split("page=")
                .nth(1)
                .and_then(|rest| rest.split('&').next())
                .unwrap_or_default();
            let body = format!(
                r#"{{"results": 60000, "page_total": 120, "entries": [{{"slug": "game-{page}"}}]}}"#
            );
            (200, body.into_bytes())
        });
        let catalog = fetch_homebrew_hub(&server.site).expect("fetched");
        assert_eq!(
            server.requests.load(Ordering::SeqCst) as u64,
            MAX_PAGES + 1,
            "the pages it may ask for, then all at once for what is missing"
        );
        let catalog: Value = serde_json::from_str(&catalog).expect("JSON");
        assert!(catalog["entries"].is_array());
    }

    #[test]
    fn a_game_s_website_is_a_web_link_or_none() {
        // As the app opens it in the browser: nothing a catalog names that
        // the system would open with another program.
        let hub = r#"{"entries": [
            {"slug": "a", "title": "A", "platform": "GB", "basepath": "database-gb",
             "website": ["httpx://x/a", "file:///Applications/Calculator.app", "http://a.example/"],
             "repository": "https://github.com/a/a",
             "files": [{"filename": "a.gb", "playable": true}]},
            {"slug": "b", "title": "B", "platform": "GB", "basepath": "database-gb",
             "website": "javascript:alert(1)", "repository": "ssh://git@x/b.git",
             "files": [{"filename": "b.gb", "playable": true}]}
        ]}"#;
        let offers = read_offers(Source::HomebrewHub, hub, &HashMap::new()).expect("parses");
        assert_eq!(
            by_slug(&offers, "a").website.as_deref(),
            Some("http://a.example/"),
            "the first that is a web link"
        );
        assert_eq!(by_slug(&offers, "b").website, None);

        let udb = r#"[
            {"slug": "c", "title": "C", "systems": ["DS"], "categories": ["game"],
             "website": "file:///Applications/Calculator.app", "source": "https://github.com/c/c",
             "downloads": {"c.nds": {"url": "https://x/c.nds"}}},
            {"slug": "d", "title": "D", "systems": ["DS"], "categories": ["game"],
             "source": "smb://x/share", "urls": ["mailto:d@x", "https://", " https://d.example/ "],
             "downloads": {"d.nds": {"url": "https://x/d.nds"}}},
            {"slug": "e", "title": "E", "systems": ["DS"], "categories": ["game"],
             "website": "x-apple.systempreferences:com.apple.preference.security",
             "downloads": {"e.nds": {"url": "https://x/e.nds"}}}
        ]"#;
        let offers = read_offers(Source::UniversalDb, udb, &HashMap::new()).expect("parses");
        assert_eq!(
            by_slug(&offers, "c").website.as_deref(),
            Some("https://github.com/c/c")
        );
        assert_eq!(
            by_slug(&offers, "d").website.as_deref(),
            Some("https://d.example/"),
            "the first of its links that is one"
        );
        assert_eq!(by_slug(&offers, "e").website, None);
    }

    #[test]
    fn downloads_are_scored_rom_first_emulator_builds_first() {
        assert_eq!(download_score("game.dsi"), None);
        assert_eq!(download_score("game.3dsx"), None);
        assert!(download_score("game.nds") > download_score("game.zip"));
        assert!(download_score("game.Emulator.Version.zip") > download_score("game.nds"));
        assert!(download_score("game_fat_1.7z") < download_score("game_1.7z"));
        assert!(download_score("game-no-sound.nds") < download_score("game.nds"));
        assert_eq!(
            download_score("fatal.nds"),
            download_score("game.nds"),
            "only the word fat"
        );
        assert!(
            download_score("RAPTOR-NDS-V1.1.0.zip") > download_score("RAPTOR-3DS-SDL2-V0.9.2.zip")
        );
    }

    #[test]
    fn three_ds_downloads_are_scored_3dsx_then_cia_then_archives() {
        assert_eq!(download_score_3ds("game.nds"), None);
        assert_eq!(download_score_3ds("game.rar"), None);
        assert_eq!(download_score_3ds("qrcode.png"), None);
        assert!(download_score_3ds("game.3dsx") > download_score_3ds("game.cia"));
        assert!(download_score_3ds("game.cia") > download_score_3ds("Game-3DSX.zip"));
        assert!(
            download_score_3ds("SuperHaxagon-3DS-armhf.3dsx.zip")
                > download_score_3ds("SuperHaxagon-3DS-armhf.cia.zip")
        );
        assert!(
            download_score_3ds("Trogdor-v2.4-3ds.zip")
                > download_score_3ds("Trogdor-v2.4-3ds-cia.zip")
        );
        assert!(download_score_3ds("CrossCraft-3DS.zip") > download_score_3ds("Hax0r.zip"));
        for other in [
            "CrossCraft-Linux.zip",
            "SuDokuL-v1.5-psp.zip",
            "Snake2DS-PC-v1.0.zip",
        ] {
            assert!(
                download_score_3ds(other) < download_score_3ds("Hax0r.zip"),
                "{other} is for another system"
            );
        }
        assert!(
            download_score_3ds("srb2_3dsv1.1.3.zip") > download_score_3ds("srb2_3dsv1.1.3.7z"),
            "a ZIP over a 7-Zip file"
        );
        assert!(download_score_3ds("Game-3ds.7z") > download_score_3ds("Game.zip"));
    }

    #[test]
    fn a_3ds_archive_adds_its_3dsx_or_else_its_cia() {
        let paths = |names: &[&str]| names.iter().map(PathBuf::from).collect::<Vec<_>>();
        assert_eq!(
            best_3ds_game(paths(&["G/game.cia", "G/game.3dsx"])),
            paths(&["G/game.3dsx"])
        );
        assert_eq!(
            best_3ds_game(paths(&["G/Game.nds", "G/Game.3ds", "G/Game.CIA"])),
            paths(&["G/Game.CIA"])
        );
        assert_eq!(
            best_3ds_game(paths(&["G/Game.nds", "G/Game.3ds"])),
            paths(&["G/Game.3ds"])
        );
        assert_eq!(
            best_3ds_game(paths(&["G/ANARCH.3dsx", "G/ANARCH_QS.3dsx"])),
            paths(&["G/ANARCH.3dsx"]),
            "the first of equals"
        );
    }

    /// Universal-DB's 3DS entries, and entries for both the DS and the 3DS.
    const UDB_3DS: &str = r#"[
        {"slug": "craftus-reloaded", "title": "Craftus Reloaded", "author": "Kemal Afzal",
         "systems": ["3DS"], "categories": ["game"], "created": "2020-05-03T12:00:00Z",
         "license_name": "Other",
         "downloads": {
            "craftus-3ds.zip": {"url": "https://x/craftus-3ds.zip"},
            "craftus.cia": {"url": "https://x/craftus.cia"},
            "craftus.3dsx": {"url": "https://x/craftus.3dsx"}}},
        {"slug": "ludo3ds", "title": "Ludo3DS", "systems": ["3DS"], "categories": ["game"],
         "downloads": {
            "Ludo3DS-PC.zip": {"url": "https://x/Ludo3DS-PC.zip"},
            "Ludo3DS.cia": {"url": "https://x/Ludo3DS.cia"}}},
        {"slug": "super-haxagon", "title": "Super-Haxagon", "systems": ["3DS"],
         "categories": ["game"],
         "downloads": {
            "SuperHaxagon-3DS-armhf.cia.zip": {"url": "https://x/h.cia.zip"},
            "SuperHaxagon-3DS-armhf.3dsx.zip": {"url": "https://x/h.3dsx.zip"},
            "SuperHaxagon-Linux.zip": {"url": "https://x/h-linux.zip"}}},
        {"slug": "rar-only", "title": "RAR", "systems": ["3DS"], "categories": ["game"],
         "downloads": {"game.rar": {"url": "https://x/game.rar"},
                       "qrcode.png": {"url": "https://x/qrcode.png"}}},
        {"slug": "balatro3ds", "title": "Balatro3DS", "systems": ["3DS"], "categories": ["game"],
         "downloads": {"Balatro3DS.3dsx": {"url": "https://x/Balatro3DS.3dsx"}}},
        {"slug": "a-theme", "title": "Theme", "systems": ["3DS"], "categories": ["theme"],
         "downloads": {"theme.3dsx": {"url": "https://x/theme.3dsx"}}},
        {"slug": "dual", "title": "Dual", "systems": ["3DS", "DS"], "categories": ["game"],
         "downloads": {"Dual.cia": {"url": "https://x/Dual.cia"},
                       "Dual.nds": {"url": "https://x/Dual.nds"}}},
        {"slug": "dual-3ds-build", "title": "Dual 3DS", "systems": ["DS", "3DS"],
         "categories": ["game"],
         "downloads": {"Game-3DS.zip": {"url": "https://x/Game-3DS.zip"},
                       "Game.cia": {"url": "https://x/Game.cia"}}}
    ]"#;

    #[test]
    fn universal_db_offers_3ds_games_choosing_3dsx_then_cia_then_an_archive() {
        let offers = read_offers(Source::UniversalDb, UDB_3DS, &HashMap::new()).expect("parses");
        let craftus = by_slug(&offers, "craftus-reloaded");
        assert_eq!(craftus.platform, Platform::N3ds);
        assert_eq!(craftus.kind, Kind::Game);
        assert_eq!(craftus.id(), "udb:craftus-reloaded");
        assert_eq!(craftus.file, "craftus.3dsx");
        assert_eq!(craftus.url, "https://x/craftus.3dsx");
        assert_eq!(craftus.developer, "Kemal Afzal");
        assert_eq!(by_slug(&offers, "ludo3ds").file, "Ludo3DS.cia");
        let haxagon = by_slug(&offers, "super-haxagon");
        assert_eq!(
            haxagon.file, "SuperHaxagon-3DS-armhf.3dsx.zip",
            "the archive of the .3dsx, not of the .cia or the Linux build"
        );
        assert_eq!(haxagon.platform, Platform::N3ds);
        assert!(
            offers.iter().all(|offer| offer.slug != "rar-only"),
            "nothing it opens"
        );
        assert!(
            offers.iter().all(|offer| offer.slug != "a-theme"),
            "not a game"
        );
    }

    #[test]
    fn withheld_3ds_entries_are_never_offered_whatever_the_checks_say() {
        let verdicts = parse_verdicts("[verdicts]\n\"udb:balatro3ds\" = \"ok\"\n");
        let offers = read_offers(Source::UniversalDb, UDB_3DS, &verdicts).expect("parses");
        assert!(WITHHELD.contains(&"udb:balatro3ds"));
        assert!(offers.iter().all(|offer| offer.slug != "balatro3ds"));
        for id in WITHHELD {
            assert!(
                offers.iter().all(|offer| offer.id() != id),
                "{id} is withheld"
            );
        }
    }

    #[test]
    fn entries_for_both_systems_are_offered_once_as_ds_games_if_they_have_a_ds_build() {
        let offers = read_offers(Source::UniversalDb, UDB_3DS, &HashMap::new()).expect("parses");
        let dual: Vec<&Offer> = offers.iter().filter(|offer| offer.slug == "dual").collect();
        assert_eq!(dual.len(), 1, "once");
        assert_eq!(dual[0].platform, Platform::Nds);
        assert_eq!(dual[0].file, "Dual.nds", "the DS build");
        let three_ds = by_slug(&offers, "dual-3ds-build");
        assert_eq!(
            three_ds.platform,
            Platform::N3ds,
            "an archive named for the 3DS is not a DS build"
        );
        assert_eq!(three_ds.file, "Game.cia");
        let ids: Vec<String> = arrange(offers.clone()).iter().map(Offer::id).collect();
        let unique: HashSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "each game once: {ids:?}");
    }

    #[test]
    fn games_sort_newest_first_once_each() {
        let mut offers = read_offers(Source::HomebrewHub, HUB, &HashMap::new()).expect("parses");
        offers.extend(read_offers(Source::UniversalDb, UDB, &HashMap::new()).expect("parses"));
        offers.push(offers[0].clone());
        let arranged = arrange(offers);
        let ids: Vec<String> = arranged.iter().map(Offer::id).collect();
        assert_eq!(
            ids,
            [
                "hh:drakoko",
                "udb:ludo",
                "udb:cs",
                "hh:no-platform",
                "hh:2048gb",
                "udb:dscraft",
                "udb:3ds-only"
            ]
        );
    }

    #[test]
    fn games_known_not_to_start_are_left_out() {
        let verdicts = parse_verdicts(
            "# made by a script\n[verdicts]\n\"hh:2048gb\" = \"ok\"\n\"hh:drakoko\" = \"crash\"\n",
        );
        assert_eq!(verdicts.len(), 2);
        assert!(parse_verdicts("not toml [").is_empty());
        let offers = read_offers(Source::HomebrewHub, HUB, &verdicts).expect("parses");
        let ids: Vec<String> = offers.iter().map(Offer::id).collect();
        assert_eq!(ids, ["hh:2048gb", "hh:no-platform"]);
        assert!(offers[0].tested);
        assert!(!offers[1].tested, "not checked");
    }

    #[test]
    fn search_looks_at_titles_makers_and_tags() {
        let offers = read_offers(Source::HomebrewHub, HUB, &HashMap::new()).expect("parses");
        let game = by_slug(&offers, "2048gb");
        assert!(game.matches(""));
        assert!(game.matches("2048"));
        assert!(game.matches("sanq"));
        assert!(game.matches("puzzle"));
        assert!(!game.matches("racing"));
    }

    #[test]
    fn tags_lose_events_and_repeats() {
        let tags: Vec<String> = [
            "Adventure",
            "adventure",
            "event:gbajam24",
            "gbcompo21",
            "GBJam",
            "pixel art",
            "Puzzle",
            "RPG",
            "Retro",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            tidy_tags(&tags),
            ["Adventure", "Pixel art", "Puzzle", "RPG"]
        );
    }

    #[test]
    fn file_names_cannot_leave_their_folder() {
        assert_eq!(file_name("files/game.gb").as_deref(), Some("game.gb"));
        assert_eq!(file_name("../../evil.gb").as_deref(), Some("evil.gb"));
        assert_eq!(file_name("..").as_deref(), None);
        assert_eq!(safe_name("a/b:c"), "a_b_c");
        // Windows can't hold a device's name, or a dot at the end.
        let windows = |on_windows: &'static str, elsewhere: &'static str| {
            if cfg!(windows) { on_windows } else { elsewhere }
        };
        assert_eq!(safe_name("con"), windows("con_", "con"));
        assert_eq!(
            safe_name("Game v1.").as_str(),
            windows("Game v1", "Game v1.")
        );
        assert_eq!(
            file_name("files/NUL.gb").as_deref(),
            Some(windows("NUL_.gb", "NUL.gb"))
        );
        let offers = read_offers(Source::HomebrewHub, HUB, &HashMap::new()).expect("parses");
        let mut offer = offers[0].clone();
        offer.slug = "../../up".to_owned();
        let dir = install_dir(Path::new("/games"), &offer);
        assert!(dir.starts_with("/games/Store/hh"), "{}", dir.display());
        assert!(
            !dir.components()
                .any(|part| part == std::path::Component::ParentDir)
        );
    }

    #[test]
    fn zips_are_told_by_their_first_bytes() {
        assert!(is_zip(b"PK\x03\x04rest of the archive"));
        assert!(!is_zip(b"\x00\xC3\x50\x01 a Game Boy ROM"));
        assert!(!is_zip(b"PK"));
    }

    #[test]
    fn covers_become_sharp_pngs() {
        let mut picture = image::RgbaImage::new(160, 144);
        picture.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        let mut gif = Vec::new();
        picture
            .write_to(&mut Cursor::new(&mut gif), image::ImageFormat::Gif)
            .expect("encodes");
        let png = cover_png(&gif).expect("a picture");
        let cover = image::load_from_memory(&png).expect("decodes").into_rgba8();
        assert_eq!((cover.width(), cover.height()), (480, 432), "three times");
        assert_eq!(
            cover.get_pixel(2, 2),
            &image::Rgba([255, 0, 0, 255]),
            "by whole pixels"
        );
        assert_eq!(cover.get_pixel(3, 3).0[3], 0);
        assert!(cover_png(b"<html>Not Found</html>").is_none());
    }

    #[test]
    fn a_tall_narrow_picture_is_not_enlarged_past_a_cover_s_size() {
        // 100 by 8,000: five times over, it would be 20 million pixels.
        let picture = image::RgbaImage::from_pixel(100, 8000, image::Rgba([0, 0, 255, 255]));
        let mut png = Vec::new();
        picture
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("encodes");
        let cover = cover_png(&png).expect("a picture");
        let cover = image::load_from_memory(&cover)
            .expect("decodes")
            .into_rgba8();
        assert_eq!(
            (cover.width(), cover.height()),
            (200, 16000),
            "twice, the most whole times that fit"
        );
        // One past the size as it is stays as it is.
        let picture = image::RgbaImage::from_pixel(30, 150_000, image::Rgba([0, 0, 255, 255]));
        let mut png = Vec::new();
        picture
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("encodes");
        let cover = cover_png(&png).expect("a picture");
        let cover = image::load_from_memory(&cover)
            .expect("decodes")
            .into_rgba8();
        assert_eq!((cover.width(), cover.height()), (30, 150_000));
    }

    #[test]
    fn the_cached_catalog_is_read_without_going_online() {
        let cache = std::env::temp_dir().join(format!("ds-emulator-store-{}", std::process::id()));
        fs::remove_dir_all(&cache).ok();
        assert_eq!(cached(&cache), Catalog::default());
        let folder = cache_dir(&cache);
        boxart::write(
            &folder.join(Source::HomebrewHub.cache_file()),
            HUB.as_bytes(),
        )
        .expect("kept");
        boxart::write(
            &folder.join(Source::UniversalDb.cache_file()),
            UDB.as_bytes(),
        )
        .expect("kept");
        let catalog = cached(&cache);
        assert_eq!(catalog.offers.len(), 7);
        assert!(catalog.fresh);
        fs::remove_dir_all(&cache).ok();
    }
}
