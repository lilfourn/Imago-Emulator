//! Games' No-Intro names, from libretro-database's clrmamepro dat files,
//! and what follows from a name: the title the library shows and the file
//! names its box art may have on thumbnails.libretro.com.
//!
//! A dat file lists one `game ( … )` block per release:
//!
//! ```text
//! game (
//!     name "Legend of Zelda, The - The Minish Cap (USA)"
//!     region "USA"
//!     serial "BZME"
//!     rom ( name "….gba" size 16777216 crc ABCEBBB1 md5 … serial "BZME" )
//! )
//! ```

use std::collections::HashMap;

/// The releases of one system, found by checksum, game code or title.
#[derive(Debug, Default)]
pub struct GameDb {
    by_crc: HashMap<u32, String>,
    by_serial: HashMap<String, String>,
    /// Every release of each game, keyed by [`base_name`], lowercased.
    by_title: HashMap<String, Vec<String>>,
}

impl GameDb {
    /// The releases in a dat file's `text`. Lines it does not understand
    /// are skipped; the first release with a checksum or code keeps it.
    pub fn parse(text: &str) -> Self {
        let mut db = Self::default();
        let mut name: Option<String> = None;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with("game (") {
                name = None;
            } else if let Some(rest) = line.strip_prefix("name ") {
                name = quoted(rest).map(str::to_owned);
            } else if line.starts_with("rom (")
                && let Some(name) = &name
            {
                if let Some(crc) =
                    field(line, "crc").and_then(|crc| u32::from_str_radix(crc, 16).ok())
                {
                    db.by_crc.entry(crc).or_insert_with(|| name.clone());
                }
                if let Some(serial) = field(line, "serial").and_then(quoted) {
                    db.by_serial
                        .entry(serial.to_owned())
                        .or_insert_with(|| name.clone());
                }
                db.by_title
                    .entry(base_name(name).to_lowercase())
                    .or_default()
                    .push(name.clone());
            }
        }
        db
    }

    /// Whether it lists no releases at all, as a truncated or garbled
    /// download would.
    pub fn is_empty(&self) -> bool {
        self.by_title.is_empty()
    }

    /// The release whose ROM has CRC-32 `crc`.
    pub fn by_crc(&self, crc: u32) -> Option<&str> {
        self.by_crc.get(&crc).map(String::as_str)
    }

    /// The first release with game code `serial`, as in a GBA or DS
    /// header.
    pub fn by_serial(&self, serial: &str) -> Option<&str> {
        self.by_serial.get(serial).map(String::as_str)
    }

    /// The release called `title`, ignoring case: that very release if
    /// listed, or else the likeliest of those with the same name outside
    /// brackets, preferring USA, then world and European releases, and
    /// official ones to betas, prototypes and pirate copies.
    pub fn by_title(&self, title: &str) -> Option<&str> {
        let releases = self.by_title.get(&base_name(title).to_lowercase())?;
        releases
            .iter()
            .find(|name| name.eq_ignore_ascii_case(title))
            .or_else(|| releases.iter().min_by_key(|name| preference(name)))
            .map(String::as_str)
    }
}

/// How unlikely release `name` is to be the one meant, lowest first: by
/// region, with unofficial and unfinished releases last.
fn preference(name: &str) -> usize {
    const UNLIKELY: [&str; 9] = [
        "(Pirate)", "(Beta", "(Proto", "(Demo", "(Sample", "(Unl)", "(Hack", "(Kiosk", "(Debug",
    ];
    const REGIONS: [&str; 4] = ["USA", "World", "Europe", "Japan"];
    let tags = &name[base_name(name).len()..];
    let unlikely = UNLIKELY.iter().any(|tag| tags.contains(tag));
    let region = REGIONS
        .iter()
        .position(|region| tags.contains(region))
        .unwrap_or(REGIONS.len());
    usize::from(unlikely) * 100 + region
}

/// The text between the first pair of double quotes in `text`.
fn quoted(text: &str) -> Option<&str> {
    let start = text.find('"')? + 1;
    let length = text[start..].find('"')?;
    Some(&text[start..start + length])
}

/// The value after the word `key` in a `rom ( … )` line.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let mut words = line.split_whitespace();
    words.find(|&word| word == key)?;
    words.next()
}

/// A No-Intro name without its bracketed tags: `"Tetris (World) (Rev 1)"`
/// is `"Tetris"`.
pub fn base_name(name: &str) -> &str {
    let end = [" (", " ["]
        .iter()
        .filter_map(|bracket| name.find(bracket))
        .min()
        .unwrap_or(name.len());
    name[..end].trim()
}

/// The title the library shows for No-Intro `name`: no tags, the article
/// in front and a colon before a subtitle, so
/// `"Legend of Zelda, The - The Minish Cap (USA)"` shows as
/// `"The Legend of Zelda: The Minish Cap"`.
pub fn display_title(name: &str) -> String {
    let parts: Vec<String> = base_name(name).split(" - ").map(article_first).collect();
    match parts.split_first() {
        Some((first, [])) => first.clone(),
        Some((first, rest)) => format!("{first}: {}", rest.join(" - ")),
        None => String::new(),
    }
}

/// `"Legend of Zelda, The"` as `"The Legend of Zelda"`.
fn article_first(part: &str) -> String {
    for article in ["The", "A", "An"] {
        if let Some(title) = part.strip_suffix(&format!(", {article}")) {
            return format!("{article} {title}");
        }
    }
    part.to_owned()
}

/// File names, best first, that box art for No-Intro `name` may have on
/// thumbnails.libretro.com, which names some by the full name and some by
/// the name and region alone.
pub fn thumbnail_names(name: &str) -> Vec<String> {
    let base = base_name(name);
    let first_tag = name
        .find(')')
        .filter(|&end| name[..end].contains(" ("))
        .map(|end| &name[..=end]);
    let mut names = vec![name.to_owned()];
    names.extend(first_tag.map(str::to_owned));
    for region in ["USA", "USA, Europe", "World", "Europe", "Japan"] {
        names.push(format!("{base} ({region})"));
    }
    let mut seen = Vec::new();
    names
        .into_iter()
        .map(|name| format!("{}.png", thumbnail_safe(&name)))
        .filter(|name| {
            let new = !seen.contains(name);
            seen.push(name.clone());
            new
        })
        .collect()
}

/// `name` with the characters libretro's thumbnails replace by `_`.
fn thumbnail_safe(name: &str) -> String {
    name.chars()
        .map(|c| if "&*/:`<>?\\|\"".contains(c) { '_' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAT: &str = r#"clrmamepro (
	name "Nintendo - Game Boy Advance"
	version "2026.08.01"
)

game (
	name "Legend of Zelda, The - The Minish Cap (USA)"
	region "USA"
	serial "BZME"
	rom ( name "Legend of Zelda, The - The Minish Cap (USA).gba" size 16777216 crc ABCEBBB1 md5 A1 sha1 B4 serial "BZME" )
)

game (
	name "Mario & Luigi - Superstar Saga (USA, Australia)"
	region "USA"
	rom ( name "Mario & Luigi - Superstar Saga (USA, Australia).gba" size 8388608 crc 9E9844F5 md5 00 sha1 00 )
)
"#;

    #[test]
    fn finds_releases_by_checksum_code_and_title() {
        let db = GameDb::parse(DAT);
        let zelda = "Legend of Zelda, The - The Minish Cap (USA)";
        assert_eq!(db.by_crc(0xABCE_BBB1), Some(zelda));
        assert_eq!(db.by_serial("BZME"), Some(zelda));
        assert_eq!(
            db.by_title("mario & luigi - superstar saga (Europe)"),
            Some("Mario & Luigi - Superstar Saga (USA, Australia)")
        );
        assert_eq!(db.by_crc(0x1234_5678), None);
        assert_eq!(db.by_serial("Nintendo - Game Boy Advance"), None);
    }

    #[test]
    fn titles_find_the_likeliest_release() {
        let db = GameDb::parse(
            r#"game (
	name "Tetris (Japan) (En)"
	rom ( name "a.gb" size 1 crc 00000001 )
)
game (
	name "Tetris (World) (Rev 1) (Pirate)"
	rom ( name "b.gb" size 1 crc 00000002 )
)
game (
	name "Tetris (World) (Rev 1)"
	rom ( name "c.gb" size 1 crc 00000003 )
)
game (
	name "Tetris (Europe)"
	rom ( name "d.gb" size 1 crc 00000004 )
)
"#,
        );
        assert_eq!(db.by_title("Tetris"), Some("Tetris (World) (Rev 1)"));
        assert_eq!(db.by_title("tetris (europe)"), Some("Tetris (Europe)"));
        assert_eq!(db.by_title("Tetris (USA)"), Some("Tetris (World) (Rev 1)"));
    }

    #[test]
    fn titles_read_as_on_the_box() {
        assert_eq!(
            display_title("Legend of Zelda, The - The Minish Cap (USA)"),
            "The Legend of Zelda: The Minish Cap"
        );
        assert_eq!(
            display_title("Advance Wars 2 - Black Hole Rising (USA, Australia)"),
            "Advance Wars 2: Black Hole Rising"
        );
        assert_eq!(display_title("Metroid Fusion (USA)"), "Metroid Fusion");
        assert_eq!(display_title("Tetris [b1]"), "Tetris");
    }

    #[test]
    fn thumbnail_names_try_the_full_name_then_the_region_alone() {
        assert_eq!(
            thumbnail_names("007 - Blood Stone (Canada) (Fr)")[..3],
            [
                "007 - Blood Stone (Canada) (Fr).png",
                "007 - Blood Stone (Canada).png",
                "007 - Blood Stone (USA).png",
            ]
        );
        assert_eq!(
            thumbnail_names("Mario & Luigi - Superstar Saga (USA)")[0],
            "Mario _ Luigi - Superstar Saga (USA).png"
        );
        let names = thumbnail_names("Tetris (World)");
        assert_eq!(names[0], "Tetris (World).png");
        assert_eq!(
            names
                .iter()
                .filter(|name| *name == "Tetris (World).png")
                .count(),
            1
        );
    }
}
