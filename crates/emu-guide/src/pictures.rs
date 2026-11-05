//! Pictures in answers ([decision 0015]): the model puts a Markdown image
//! on a line of its own whose address is `wiki:` and a page's title, such
//! as `![Route 210](wiki:Route_210)`, and the panel shows that page's main
//! picture from the game's wiki: a Pokémon's artwork, a character, a
//! route's map.
//!
//! The picture is the one MediaWiki's PageImages picks for the page,
//! found through the wiki's API; a title the wiki lacks is searched for.
//! So the model never writes an image's address, which it would often
//! get wrong, and the app fetches only from the game's wiki and the image
//! host the wiki names, through `curl` and HTTPS. Pictures are kept in the
//! cache folder, so a chat opened again shows them at once.
//!
//! Everything here blocks; run it off the UI thread.
//!
//! [decision 0015]: ../../../docs/decisions/0015-guide-pictures-from-wikis.md

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{curl, url};

/// What a picture's address starts with.
pub const SCHEME: &str = "wiki:";
/// How wide a picture is asked for, in pixels: about twice the panel's
/// widest text, for Retina screens.
const WIDTH: u32 = 800;
/// The largest picture fetched, in bytes.
const MAX_BYTES: usize = 12 << 20;
/// The longest title MediaWiki allows, in bytes.
const MAX_TITLE: usize = 255;
/// How many search results are looked through for a picture.
const SEARCHED: u32 = 5;
/// How many pictures the cache keeps; the oldest go first.
const KEPT: usize = 400;

/// A wiki pictures come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wiki {
    /// Its name, for the prompt and captions: "Pokémon Wiki".
    pub name: String,
    /// Its API: "https://pokemon.fandom.com/api.php".
    pub api: String,
}

/// A picture, fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    /// The image file: PNG, JPEG, GIF or WebP, as the wiki keeps it.
    pub bytes: Vec<u8>,
    /// The page it illustrates, for its caption's link; empty if the wiki
    /// didn't say.
    pub page: String,
}

/// Why a picture didn't come.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum PictureError {
    /// Its address isn't `wiki:` and a title.
    #[error("not a wiki picture")]
    NotWiki,
    /// The wiki has no page with a picture by that title.
    #[error("{0} has no picture of that")]
    Missing(String),
    /// The wiki or its image host didn't answer, or not with a picture.
    #[error("{0}")]
    Network(String),
}

/// The page title in a picture's address: `wiki:Route_210` is "Route 210".
/// Underscores are spaces, `%XX` its byte, and a `#section` is dropped.
/// `None` for any other address, or a title MediaWiki couldn't have.
pub fn title(target: &str) -> Option<String> {
    let target = target.trim();
    let rest = target
        .get(..SCHEME.len())
        .filter(|scheme| scheme.eq_ignore_ascii_case(SCHEME))
        .map(|_| &target[SCHEME.len()..])?;
    let rest = rest.split('#').next().unwrap_or_default();
    let decoded = url::percent_decode(rest).replace('_', " ");
    if decoded
        .chars()
        .any(|c| c.is_control() || "<>[]{}|".contains(c))
    {
        return None;
    }
    let title = decoded.split_whitespace().collect::<Vec<_>>().join(" ");
    (!title.is_empty() && title.len() <= MAX_TITLE).then_some(title)
}

/// Pictures fetched from wikis, kept in a folder.
#[derive(Debug, Clone)]
pub struct PictureStore {
    dir: PathBuf,
}

impl PictureStore {
    /// Pictures kept in `dir`, such as
    /// `~/Library/Caches/ds-emulator/Guide pictures`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The picture for `target` on `wiki`: the kept one, else the wiki's,
    /// which is then kept.
    ///
    /// # Errors
    ///
    /// [`PictureError::NotWiki`] for an address that isn't a wiki title;
    /// otherwise as [`find`] and [`download`] fail.
    pub fn fetch(&self, wiki: &Wiki, target: &str) -> Result<Fetched, PictureError> {
        let title = title(target).ok_or(PictureError::NotWiki)?;
        let (image_path, page_path) = self.paths(wiki, &title);
        if let (Ok(bytes), Ok(page)) = (fs::read(&image_path), fs::read_to_string(&page_path))
            && !bytes.is_empty()
        {
            return Ok(Fetched { bytes, page });
        }
        let found = find(wiki, &title)?;
        let bytes = download(&found.image)?;
        // A picture that can't be kept is still shown.
        self.keep(&image_path, &page_path, &bytes, &found.page).ok();
        Ok(Fetched {
            bytes,
            page: found.page,
        })
    }

    /// Where the picture for `title` on `wiki` is kept, and its page's
    /// address.
    fn paths(&self, wiki: &Wiki, title: &str) -> (PathBuf, PathBuf) {
        let digest = Sha256::digest(format!("{}\n{title}", wiki.api).as_bytes());
        let name: String = digest[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        (
            self.dir.join(format!("{name}.picture")),
            self.dir.join(format!("{name}.page")),
        )
    }

    fn keep(&self, image: &Path, page: &Path, bytes: &[u8], address: &str) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        write(page, address.as_bytes())?;
        // The picture last: a page file alone is never read.
        write(image, bytes)?;
        prune(&self.dir, KEPT);
        Ok(())
    }
}

/// Writes `bytes` to `path` through a file beside it, so a reader never
/// sees half of it.
fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let partial = path.with_extension("partial");
    fs::write(&partial, bytes)?;
    fs::rename(&partial, path)
}

/// Deletes the oldest pictures in `dir` past the newest `kept`.
fn prune(dir: &Path, kept: usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut pictures: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "picture"))
        .filter_map(|path| Some((fs::metadata(&path).ok()?.modified().ok()?, path)))
        .collect();
    if pictures.len() <= kept {
        return;
    }
    pictures.sort();
    for (_, path) in &pictures[..pictures.len() - kept] {
        fs::remove_file(path).ok();
        fs::remove_file(path.with_extension("page")).ok();
    }
}

/// Where a page's picture is, and the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The picture, scaled by the wiki to [`WIDTH`] or less.
    pub image: String,
    /// The page's address.
    pub page: String,
}

/// The picture of the page called `title` on `wiki`, or, if there is no
/// such page or it has no picture, of the best search result for it that
/// has one.
///
/// # Errors
///
/// [`PictureError::Missing`] if neither has a picture;
/// [`PictureError::Network`] if the wiki doesn't answer.
pub fn find(wiki: &Wiki, title: &str) -> Result<Found, PictureError> {
    let common = format!(
        "action=query&format=json&formatversion=2&prop=pageimages%7Cinfo&inprop=url\
         &piprop=thumbnail&pithumbsize={WIDTH}"
    );
    let exact = format!(
        "{}?{common}&redirects=1&titles={}",
        wiki.api,
        url::encode(title)
    );
    if let Some(found) = first_picture(&get(&exact)?) {
        return Ok(found);
    }
    let searched = format!(
        "{}?{common}&generator=search&gsrlimit={SEARCHED}&gsrsearch={}",
        wiki.api,
        url::encode(title)
    );
    first_picture(&get(&searched)?).ok_or_else(|| PictureError::Missing(wiki.name.clone()))
}

/// Downloads the picture at `address`.
///
/// # Errors
///
/// [`PictureError::Network`] if it doesn't come, or is too big.
pub fn download(address: &str) -> Result<Vec<u8>, PictureError> {
    if !allowed(address) {
        return Err(PictureError::Network("not an HTTPS address".to_owned()));
    }
    let bytes = get(address)?;
    if bytes.is_empty() || bytes.len() > MAX_BYTES {
        return Err(PictureError::Network("not a picture".to_owned()));
    }
    Ok(bytes)
}

/// What `address` answers, following redirects to HTTPS only.
fn get(address: &str) -> Result<Vec<u8>, PictureError> {
    let max = MAX_BYTES.to_string();
    let reply = curl::request(
        &[
            "--location",
            "--proto-redir",
            "=https",
            "--max-filesize",
            &max,
        ],
        &[],
        address,
        20,
    )
    .map_err(PictureError::Network)?;
    match reply.status {
        200 => Ok(reply.body),
        status => Err(PictureError::Network(format!("HTTP {status}"))),
    }
}

/// Whether the app may fetch `address`: HTTPS, or in tests the stand-in
/// server.
fn allowed(address: &str) -> bool {
    address.starts_with("https://") || (cfg!(test) && address.starts_with("http://127.0.0.1:"))
}

/// The first page in a PageImages answer, by search rank, with a picture
/// the app may fetch.
fn first_picture(body: &[u8]) -> Option<Found> {
    #[derive(Deserialize)]
    struct Reply {
        query: Option<Query>,
    }
    #[derive(Deserialize)]
    struct Query {
        #[serde(default)]
        pages: Vec<Page>,
    }
    #[derive(Deserialize)]
    struct Page {
        /// Its search rank, when searched.
        #[serde(default)]
        index: u32,
        thumbnail: Option<Thumbnail>,
        fullurl: Option<String>,
    }
    #[derive(Deserialize)]
    struct Thumbnail {
        source: String,
    }
    let reply: Reply = serde_json::from_slice(body).ok()?;
    let mut pages = reply.query?.pages;
    pages.sort_by_key(|page| page.index);
    pages.into_iter().find_map(|page| {
        let image = page.thumbnail?.source;
        allowed(&image).then(|| Found {
            image,
            page: page.fullurl.unwrap_or_default(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake;

    #[test]
    fn titles_come_from_wiki_addresses() {
        assert_eq!(title("wiki:Route_210").as_deref(), Some("Route 210"));
        assert_eq!(title(" Wiki:Mr._Mime ").as_deref(), Some("Mr. Mime"));
        assert_eq!(
            title("wiki:Pok%C3%A9mon_League#Gym").as_deref(),
            Some("Pokémon League")
        );
        assert_eq!(title("wiki:Farfetch'd").as_deref(), Some("Farfetch'd"));
        assert_eq!(title("wiki: Route  210 ").as_deref(), Some("Route 210"));
        assert_eq!(title("wiki:"), None);
        assert_eq!(title("wiki:_#x"), None);
        assert_eq!(title("wiki:a|b"), None);
        assert_eq!(title("wiki:a%0Ab"), None);
        assert_eq!(title(&format!("wiki:{}", "a".repeat(256))), None);
        assert_eq!(title("https://x.dev/a.png"), None);
        assert_eq!(title("map.png"), None);
    }

    #[test]
    fn the_first_ranked_page_with_a_picture_wins() {
        let body = br#"{"query":{"pages":[
            {"index":2,"title":"B","thumbnail":{"source":"https://img/b.png"},"fullurl":"https://w/B"},
            {"index":1,"title":"A","fullurl":"https://w/A"},
            {"index":3,"title":"C","thumbnail":{"source":"https://img/c.png"}}
        ]}}"#;
        assert_eq!(
            first_picture(body),
            Some(Found {
                image: "https://img/b.png".to_owned(),
                page: "https://w/B".to_owned(),
            })
        );
        let missing = br#"{"query":{"pages":[{"title":"X","missing":true}]}}"#;
        assert_eq!(first_picture(missing), None);
        let insecure = br#"{"query":{"pages":[{"thumbnail":{"source":"http://img/a.png"}}]}}"#;
        assert_eq!(first_picture(insecure), None);
        assert_eq!(first_picture(br#"{"batchcomplete":true}"#), None);
        assert_eq!(first_picture(b"<html>"), None);
    }

    /// The stand-in wiki: no page called "Route 210 Sinnoh", a search that
    /// finds "Route 210", and the picture itself.
    fn wiki_reply(request: &fake::Request) -> (u16, String) {
        let line = &request.line;
        if line.contains("/picture.png") {
            return (200, "PNG bytes".to_owned());
        }
        if line.contains("titles=Route%20210%20Sinnoh") {
            return (
                200,
                r#"{"query":{"pages":[{"title":"Route 210 Sinnoh","missing":true}]}}"#.to_owned(),
            );
        }
        if line.contains("generator=search") && line.contains("gsrsearch=Route%20210%20Sinnoh") {
            let host = request.header("host").unwrap_or_default();
            return (
                200,
                format!(
                    r#"{{"query":{{"pages":[{{"index":1,"title":"Route 210","thumbnail":{{"source":"http://{host}/picture.png"}},"fullurl":"https://w/Route_210"}}]}}}}"#
                ),
            );
        }
        (404, "{}".to_owned())
    }

    #[test]
    fn a_missing_title_is_searched_and_the_picture_kept() {
        let site = fake::serve(wiki_reply);
        let wiki = Wiki {
            name: "Test Wiki".to_owned(),
            api: format!("{site}/api.php"),
        };
        let dir = std::env::temp_dir().join(format!("emu-guide-pictures-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        let store = PictureStore::new(&dir);
        let fetched = store.fetch(&wiki, "wiki:Route_210_Sinnoh").unwrap();
        assert_eq!(fetched.bytes, b"PNG bytes");
        assert_eq!(fetched.page, "https://w/Route_210");
        // Kept: the same picture comes back with the wiki gone.
        let gone = Wiki {
            api: "http://127.0.0.1:9/api.php".to_owned(),
            ..wiki.clone()
        };
        let (image, _) = store.paths(&wiki, "Route 210 Sinnoh");
        assert!(image.exists());
        let kept_store = PictureStore { dir: dir.clone() };
        assert_eq!(
            kept_store.fetch(&wiki, "wiki:Route 210 Sinnoh").unwrap(),
            fetched
        );
        assert!(matches!(
            kept_store.fetch(&gone, "wiki:Route_210_Sinnoh"),
            Err(PictureError::Network(_))
        ));
        assert_eq!(
            store.fetch(&wiki, "wiki:Nowhere"),
            Err(PictureError::Network("HTTP 404".to_owned()))
        );
        assert_eq!(
            store.fetch(&wiki, "https://x.dev/a.png"),
            Err(PictureError::NotWiki)
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_cache_keeps_the_newest() {
        let dir = std::env::temp_dir().join(format!("emu-guide-prune-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        for n in 0..5 {
            let path = dir.join(format!("{n}.picture"));
            fs::write(&path, b"x").unwrap();
            fs::write(path.with_extension("page"), b"p").unwrap();
            let file = fs::File::options().write(true).open(&path).unwrap();
            let time = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(n * 60);
            file.set_modified(time).unwrap();
        }
        prune(&dir, 3);
        let mut left: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "2.page",
                "2.picture",
                "3.page",
                "3.picture",
                "4.page",
                "4.picture"
            ]
        );
        fs::remove_dir_all(&dir).ok();
    }
}
