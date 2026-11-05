//! The source map: which fan sites and wikis players trust for a game.
//!
//! The map is `sources.toml`, built into the app. It holds about thirty
//! series, each matched by its titles and with its sites in order of
//! trust; the sites for fan games and ROM hacks; and the fallback sites
//! every game gets after its series. [Research §5] says how each site was
//! chosen and checked, and `scripts/guide/check-sources.sh` checks them
//! again by hand.
//!
//! [Research §5]: ../../../docs/research/game-chat.md

use std::sync::OnceLock;

use serde::Deserialize;

use crate::game::{GameContext, Kind};
use crate::pictures::Wiki;

/// The most sources a game gets. Web search's domain list stays short,
/// and so does the system prompt, which lists every source.
pub const MAX_SOURCES: usize = 12;

/// The source map, as written.
const SOURCE_MAP: &str = include_str!("../sources.toml");

/// Words matching ignores: titles place them differently ("Legend of
/// Zelda, The"), or leave them out ("Pokémon Emerald Version").
const IGNORED_WORDS: [&str; 2] = ["the", "version"];

/// How Guide can reach a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// Its pages can be fetched directly.
    Fetch,
    /// Only through web search: it is behind a bot check.
    Search,
    /// A MediaWiki site with its API at this path, such as "/w/api.php".
    MediaWiki {
        /// The API's path on the domain.
        api: String,
    },
    /// A JSON API at this base URL.
    Api {
        /// The base URL.
        base: String,
    },
}

/// One trusted site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// Its name, such as "Serebii".
    pub name: String,
    /// Its domain, such as "serebii.net", or a wildcard such as
    /// "*.fandom.com".
    pub domain: String,
    /// How it can be reached.
    pub reach: Reach,
    /// What it is good for, for the system prompt.
    pub good_for: String,
    /// Where on the site this game starts, such as
    /// "/omegarubyalphasapphire/", if the map knows.
    pub start: Option<String>,
}

/// The sources for `game`, most trusted first: its series' sources, then
/// the fallback ones every game gets, without repeats and at most
/// [`MAX_SOURCES`].
///
/// - The series is the first in the map with a title in the game's lookup
///   name, else in its library title, ignoring case and accents.
/// - A fan game gets the fan-game sources before its series'. An unlisted
///   copy of a series that has hacks, such as a Pokémon ROM hack, gets
///   them after its series'.
/// - A site's start for the game is filled in when the map knows it,
///   except for fan games, whose names only look like the games they
///   borrow from.
/// - When the list is long, the fallback sources are the ones left out.
pub fn sources_for(game: &GameContext) -> Vec<Source> {
    let map = map();
    let names = [words(&game.lookup_name()), words(&game.title)];
    let series = series_of(map, &names);
    let series_sources = series.map_or(&[][..], |series| &series.sources[..]);
    let hack = game.kind == Kind::Unlisted
        && series.is_some_and(|series| map.hacks_of.contains(&series.id));
    let names = if game.kind == Kind::FanGame {
        &[][..]
    } else {
        &names[..]
    };

    let mut sources = Vec::new();
    let add = |entries: &[Entry], sources: &mut Vec<Source>| {
        for entry in entries {
            let taken = sources
                .iter()
                .any(|source: &Source| source.domain == entry.source.domain);
            if !taken && sources.len() < MAX_SOURCES {
                sources.push(entry.for_game(names));
            }
        }
    };
    if game.kind == Kind::FanGame {
        add(&map.fan_games, &mut sources);
    }
    add(series_sources, &mut sources);
    if hack {
        add(&map.fan_games, &mut sources);
    }
    add(&map.fallback, &mut sources);
    sources
}

/// The wiki whose pictures answers about `game` show
/// ([`crate::pictures`]): its series' `pictures` wiki if the map names
/// one, else the series' first MediaWiki site on a host of its own. `None`
/// for a game in no series, as the fallback sites' pictures are of whole
/// games, not what is in them.
pub fn picture_wiki(game: &GameContext) -> Option<Wiki> {
    let map = map();
    let series = series_of(map, &[words(&game.lookup_name()), words(&game.title)])?;
    if let Some(wiki) = &series.pictures {
        return Some(wiki.clone());
    }
    series.sources.iter().find_map(|entry| {
        let source = &entry.source;
        match &source.reach {
            Reach::MediaWiki { api } if !source.domain.contains(['*', '/']) => Some(Wiki {
                name: source.name.clone(),
                api: format!("https://{}{api}", source.domain),
            }),
            _ => None,
        }
    })
}

/// The first series in the map with a title in one of `names`, tried in
/// order.
fn series_of<'m>(map: &'m Map, names: &[Vec<String>]) -> Option<&'m Series> {
    names
        .iter()
        .find_map(|name| map.series.iter().find(|series| series.matches(name)))
}

/// The domains web search and fetch may use for these sources, in order.
/// A domain another one covers, such as "metroid.fandom.com" beside
/// "*.fandom.com", is left out.
pub fn domains(sources: &[Source]) -> Vec<String> {
    let mut domains: Vec<String> = Vec::new();
    for source in sources {
        let (host, path) = split_domain(&source.domain);
        let covered = sources.iter().any(|other| {
            other.domain != source.domain && covers(&other.domain, host, &format!("/{path}"))
        });
        if !covered && !domains.contains(&source.domain) {
            domains.push(source.domain.clone());
        }
    }
    domains
}

/// A short name for the site a URL is on: the source map's name for it if
/// it has one ("Serebii"), else its host without "www.".
///
/// The most specific domain wins, so "layton.fandom.com" is "Layton Wiki"
/// while any other Fandom wiki is "Fandom".
pub fn site_name(url: &str) -> String {
    let (host, path) = host_and_path(url);
    let map = map();
    map.series
        .iter()
        .flat_map(|series| &series.sources)
        .chain(&map.fan_games)
        .chain(&map.fallback)
        .map(|entry| &entry.source)
        .filter(|source| covers(&source.domain, &host, path))
        .max_by_key(|source| source.domain.len())
        .map_or_else(
            || host.strip_prefix("www.").unwrap_or(&host).to_owned(),
            |source| source.name.clone(),
        )
}

/// The parsed map.
#[derive(Debug)]
struct Map {
    /// The series, in the order they are tried.
    series: Vec<Series>,
    /// The sources for fan games and hacks.
    fan_games: Vec<Entry>,
    /// The series whose unlisted copies get the fan-game sources.
    hacks_of: Vec<String>,
    /// The sources every game gets.
    fallback: Vec<Entry>,
}

/// One series.
#[derive(Debug)]
struct Series {
    /// Its id, such as "pokemon".
    id: String,
    /// Its titles, as words.
    titles: Vec<Vec<String>>,
    /// Its sources, most trusted first.
    sources: Vec<Entry>,
    /// Where its pictures come from, if not its first MediaWiki site.
    pictures: Option<Wiki>,
}

impl Series {
    /// Whether one of its titles is in `name`.
    fn matches(&self, name: &[String]) -> bool {
        self.titles
            .iter()
            .any(|title| name.windows(title.len()).any(|words| words == &title[..]))
    }
}

/// A source with the games it knows the start of.
#[derive(Debug)]
struct Entry {
    /// The source, with no start.
    source: Source,
    /// Games, as names in words, and where each starts.
    games: Vec<(Vec<String>, String)>,
}

impl Entry {
    /// The source with its start for the first of `names` it knows.
    fn for_game(&self, names: &[Vec<String>]) -> Source {
        let start = names.iter().find_map(|name| {
            self.games
                .iter()
                .filter(|(game, _)| name.starts_with(game))
                .max_by_key(|(game, _)| game.len())
                .map(|(_, start)| start.clone())
        });
        Source {
            start,
            ..self.source.clone()
        }
    }
}

/// The map as `sources.toml` writes it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMap {
    series: Vec<RawSeries>,
    fan_games: RawFanGames,
    fallback: RawFallback,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSeries {
    id: String,
    titles: Vec<String>,
    pictures: Option<RawPictures>,
    sources: Vec<RawSource>,
}

/// A series' picture wiki, as `sources.toml` writes it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPictures {
    name: String,
    domain: String,
    api: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFanGames {
    hacks_of: Vec<String>,
    sources: Vec<RawSource>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFallback {
    sources: Vec<RawSource>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSource {
    name: String,
    domain: String,
    reach: RawReach,
    api: Option<String>,
    good_for: String,
    #[serde(default)]
    games: Vec<RawGame>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawReach {
    Fetch,
    Search,
    MediaWiki,
    Api,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGame {
    names: Vec<String>,
    start: String,
}

/// The map, parsed the first time it is needed.
fn map() -> &'static Map {
    static MAP: OnceLock<Map> = OnceLock::new();
    MAP.get_or_init(|| parse(SOURCE_MAP).unwrap_or_else(|error| panic!("sources.toml: {error}")))
}

/// Parses and checks a source map.
fn parse(text: &str) -> Result<Map, String> {
    let raw: RawMap = toml::from_str(text).map_err(|error| error.to_string())?;
    let series = raw
        .series
        .into_iter()
        .map(|series| {
            let titles = series
                .titles
                .iter()
                .map(|title| match words(title) {
                    words if words.is_empty() => Err(format!("{}: an empty title", series.id)),
                    words => Ok(words),
                })
                .collect::<Result<_, _>>()?;
            let sources = entries(series.sources, &series.id)?;
            let pictures = series
                .pictures
                .map(|raw| {
                    let fits = !raw.name.trim().is_empty()
                        && raw.api.starts_with('/')
                        && !raw.domain.is_empty()
                        && raw.domain == raw.domain.to_ascii_lowercase()
                        && !raw.domain.contains(['*', '/', ':']);
                    fits.then(|| Wiki {
                        name: raw.name,
                        api: format!("https://{}{}", raw.domain, raw.api),
                    })
                    .ok_or_else(|| format!("{}: a bad pictures wiki", series.id))
                })
                .transpose()?;
            Ok(Series {
                id: series.id,
                titles,
                sources,
                pictures,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    if let Some(id) = raw
        .fan_games
        .hacks_of
        .iter()
        .find(|id| !series.iter().any(|series| &series.id == *id))
    {
        return Err(format!("fan_games: no series {id}"));
    }
    Ok(Map {
        series,
        fan_games: entries(raw.fan_games.sources, "fan_games")?,
        hacks_of: raw.fan_games.hacks_of,
        fallback: entries(raw.fallback.sources, "fallback")?,
    })
}

/// Checks a list's sources, which `place` names in errors.
fn entries(sources: Vec<RawSource>, place: &str) -> Result<Vec<Entry>, String> {
    sources
        .into_iter()
        .map(|raw| {
            let where_ = format!("{place}: {}", raw.name);
            let reach = match (raw.reach, raw.api) {
                (RawReach::Fetch, None) => Reach::Fetch,
                (RawReach::Search, None) => Reach::Search,
                (RawReach::MediaWiki, Some(api)) if api.starts_with('/') => {
                    Reach::MediaWiki { api }
                }
                (RawReach::Api, Some(base)) if base.starts_with("https://") => Reach::Api { base },
                _ => return Err(format!("{where_}: its reach and api don't fit")),
            };
            let domain_ok = !raw.domain.is_empty()
                && !raw.domain.contains("://")
                && raw.domain == raw.domain.to_ascii_lowercase()
                && !raw.domain.starts_with("www.");
            if raw.name.trim().is_empty() || raw.good_for.trim().is_empty() || !domain_ok {
                return Err(format!("{where_}: a missing name or use, or a bad domain"));
            }
            let mut games = Vec::new();
            for game in raw.games {
                if !game.start.starts_with('/') {
                    return Err(format!("{where_}: start {} isn't a path", game.start));
                }
                for name in game.names {
                    match words(&name) {
                        words if words.is_empty() => {
                            return Err(format!("{where_}: an empty game name"));
                        }
                        words => games.push((words, game.start.clone())),
                    }
                }
            }
            Ok(Entry {
                source: Source {
                    name: raw.name,
                    domain: raw.domain,
                    reach,
                    good_for: raw.good_for,
                    start: None,
                },
                games,
            })
        })
        .collect()
}

/// `text` as the words titles are matched by: lowercase, without accents
/// or punctuation, and without [`IGNORED_WORDS`]. Apostrophes join words
/// ("Yoshi's" is "yoshis").
fn words(text: &str) -> Vec<String> {
    let mut folded = String::with_capacity(text.len());
    for c in text.chars().flat_map(char::to_lowercase) {
        match c {
            '\'' | '\u{2019}' => {}
            'à'..='å' | 'ā' => folded.push('a'),
            'æ' => folded.push_str("ae"),
            'ç' => folded.push('c'),
            'è'..='ë' | 'ē' => folded.push('e'),
            'ì'..='ï' | 'ī' => folded.push('i'),
            'ñ' => folded.push('n'),
            'ò'..='ö' | 'ø' | 'ō' => folded.push('o'),
            'œ' => folded.push_str("oe"),
            'ß' => folded.push_str("ss"),
            'ù'..='ü' | 'ū' => folded.push('u'),
            'ý' | 'ÿ' => folded.push('y'),
            c => folded.push(c),
        }
    }
    folded
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty() && !IGNORED_WORDS.contains(word))
        .map(str::to_owned)
        .collect()
}

/// A domain's host and its path without the leading slash:
/// "ign.com/wikis" is ("ign.com", "wikis").
fn split_domain(domain: &str) -> (&str, &str) {
    domain.split_once('/').unwrap_or((domain, ""))
}

/// Whether `domain` covers a URL on `host` at `path`: the host is the
/// domain or one of its subdomains (only subdomains for a wildcard), and
/// the path is under the domain's.
fn covers(domain: &str, host: &str, path: &str) -> bool {
    let (domain_host, domain_path) = split_domain(domain);
    let subdomain_of = |parent: &str| {
        host.strip_suffix(parent)
            .is_some_and(|sub| sub.len() > 1 && sub.ends_with('.'))
    };
    let host_ok = match domain_host.strip_prefix("*.") {
        Some(parent) => subdomain_of(parent),
        None => host == domain_host || subdomain_of(domain_host),
    };
    let path_ok = domain_path.is_empty()
        || path
            .strip_prefix('/')
            .and_then(|path| path.strip_prefix(domain_path.trim_end_matches('/')))
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'));
    host_ok && path_ok
}

/// A URL's host, lowercase, and its path.
fn host_and_path(url: &str) -> (String, &str) {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let host = host.split(':').next().unwrap_or(host).trim_end_matches('.');
    (host.to_ascii_lowercase(), path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::Platform;

    fn game(title: &str, release: Option<&str>, kind: Kind) -> GameContext {
        GameContext {
            title: title.to_owned(),
            release: release.map(str::to_owned),
            platform: Platform::Ds,
            serial: None,
            kind,
            play_time: None,
        }
    }

    fn retail(title: &str, release: Option<&str>) -> Vec<Source> {
        sources_for(&game(title, release, Kind::Retail))
    }

    fn names(sources: &[Source]) -> Vec<&str> {
        sources.iter().map(|source| source.name.as_str()).collect()
    }

    fn start<'a>(sources: &'a [Source], name: &str) -> Option<&'a str> {
        sources
            .iter()
            .find(|source| source.name == name)
            .and_then(|source| source.start.as_deref())
    }

    #[test]
    fn the_map_parses_with_unique_domains_and_one_name_per_domain() {
        let map = parse(SOURCE_MAP).expect("sources.toml parses");
        assert!(map.series.len() >= 25, "{} series", map.series.len());
        let lists = map
            .series
            .iter()
            .map(|series| (series.id.as_str(), &series.sources))
            .chain([("fan_games", &map.fan_games), ("fallback", &map.fallback)]);
        let mut named: Vec<(&str, &str)> = Vec::new();
        for (id, sources) in lists {
            assert!(!sources.is_empty(), "{id} has no sources");
            for (i, entry) in sources.iter().enumerate() {
                let source = &entry.source;
                assert!(
                    sources[..i]
                        .iter()
                        .all(|other| other.source.domain != source.domain),
                    "{id} lists {} twice",
                    source.domain
                );
                match named.iter().find(|(domain, _)| *domain == source.domain) {
                    Some((_, name)) => assert_eq!(*name, source.name, "{}", source.domain),
                    None => named.push((source.domain.as_str(), source.name.as_str())),
                }
            }
        }
        let ids: Vec<&str> = map.series.iter().map(|series| series.id.as_str()).collect();
        for (i, id) in ids.iter().enumerate() {
            assert!(!ids[..i].contains(id), "series {id} twice");
        }
    }

    #[test]
    fn a_broken_map_says_what_is_wrong() {
        let no_api = r#"
            [fan_games]
            hacks_of = []
            sources = []
            [fallback]
            [[fallback.sources]]
            name = "StrategyWiki"
            domain = "strategywiki.org"
            reach = "mediawiki"
            good_for = "walkthroughs"
            [[series]]
            id = "zelda"
            titles = ["Zelda"]
            sources = []
        "#;
        assert!(parse(no_api).unwrap_err().contains("StrategyWiki"));
        let unknown = no_api
            .replace("hacks_of = []", "hacks_of = [\"mario\"]")
            .replace("good_for", "api = \"/w/api.php\"\ngood_for");
        assert_eq!(parse(&unknown).unwrap_err(), "fan_games: no series mario");
    }

    #[test]
    fn series_match_no_intro_names_and_library_titles() {
        let cases = [
            ("Legend of Zelda, The - The Minish Cap (USA)", "Zelda Wiki"),
            ("The Legend of Zelda: Spirit Tracks", "Zelda Wiki"),
            ("New Super Mario Bros. (USA)", "Super Mario Wiki"),
            ("Yoshi's Island DS", "Super Mario Wiki"),
            ("Mario & Luigi - Superstar Saga (USA)", "Super Mario Wiki"),
            ("Kirby - Nightmare in Dream Land (USA)", "WiKirby"),
            ("Metroid - Zero Mission (USA)", "Metroid Wiki"),
            ("Fire Emblem - The Sacred Stones (USA)", "Fire Emblem Wiki"),
            (
                "Golden Sun - The Lost Age (USA, Europe)",
                "Golden Sun Universe",
            ),
            ("Advance Wars 2 - Black Hole Rising (USA)", "Wars Wiki"),
            ("Animal Crossing: Wild World", "Nookipedia"),
            (
                "Professor Layton and the Curious Village (USA)",
                "Layton Wiki",
            ),
            ("Phoenix Wright - Ace Attorney (USA)", "Ace Attorney Wiki"),
            ("Apollo Justice: Ace Attorney", "Ace Attorney Wiki"),
            ("Castlevania - Aria of Sorrow (USA)", "Castlevania Wiki"),
            (
                "Mega Man Battle Network 3 - Blue (USA)",
                "Mega Man Knowledge Base",
            ),
            (
                "Dragon Quest IX - Sentinels of the Starry Skies (USA)",
                "Dragon Quest Wiki",
            ),
            ("Dragon Warrior Monsters (USA, Europe)", "Dragon Quest Wiki"),
            (
                "Final Fantasy Tactics Advance (USA, Australia)",
                "Final Fantasy Wiki",
            ),
            ("Mother 3 (Japan)", "WikiBound"),
            (
                "Yu-Gi-Oh! 5D's - World Championship 2010 (USA)",
                "Yugipedia",
            ),
            ("World Ends with You, The (USA)", "TWEWY Wiki"),
            ("Pokémon Omega Ruby", "Bulbapedia"),
            (
                "Pokemon Mystery Dungeon - Explorers of Sky (USA)",
                "Mystery Dungeon Wiki",
            ),
        ];
        for (title, first) in cases {
            let sources = retail(title, None);
            assert_eq!(sources[0].name, first, "{title}");
        }
    }

    #[test]
    fn the_release_name_is_tried_before_the_title() {
        let sources = retail(
            "My copy",
            Some("Legend of Zelda, The - Link's Awakening DX (USA, Europe) (Rev 2)"),
        );
        assert_eq!(names(&sources)[..2], ["Zelda Wiki", "Zelda Dungeon"]);
        let sources = retail("Golden Sun", Some("Unknown (USA)"));
        assert_eq!(sources[0].name, "Golden Sun Universe");
    }

    #[test]
    fn every_main_series_pokemon_game_starts_at_its_serebii_section() {
        let cases = [
            ("Pokemon - Red Version (USA, Europe) (SGB Enhanced)", "/rb/"),
            (
                "Pokemon - Blue Version (USA, Europe) (SGB Enhanced)",
                "/rb/",
            ),
            ("Pocket Monsters - Midori (Japan) (SGB Enhanced)", "/rb/"),
            (
                "Pokemon - Yellow Version - Special Pikachu Edition (USA, Europe) (CGB+SGB Enhanced)",
                "/yellow/",
            ),
            (
                "Pokemon - Gold Version (USA, Europe) (SGB Enhanced)",
                "/gs/",
            ),
            (
                "Pokemon - Silver Version (USA, Europe) (SGB Enhanced)",
                "/gs/",
            ),
            (
                "Pokemon - Crystal Version (USA, Europe) (Rev 1)",
                "/crystal/",
            ),
            ("Pokemon - Ruby Version (USA)", "/rubysapphire/"),
            (
                "Pokemon - Sapphire Version (USA, Europe) (Rev 2)",
                "/rubysapphire/",
            ),
            ("Pokemon - Emerald Version (USA, Europe)", "/emerald/"),
            (
                "Pokemon - FireRed Version (USA, Europe) (Rev 1)",
                "/fireredleafgreen/",
            ),
            (
                "Pokemon - LeafGreen Version (USA, Europe)",
                "/fireredleafgreen/",
            ),
            ("Pokemon - Diamond Version (USA) (Rev 5)", "/diamondpearl/"),
            ("Pokemon - Pearl Version (USA) (Rev 5)", "/diamondpearl/"),
            ("Pokemon - Platinum Version (USA) (Rev 1)", "/platinum/"),
            ("Pokemon - HeartGold Version (USA)", "/heartgoldsoulsilver/"),
            (
                "Pokemon - SoulSilver Version (USA)",
                "/heartgoldsoulsilver/",
            ),
            (
                "Pokemon - Black Version (USA, Europe) (NDSi Enhanced)",
                "/blackwhite/",
            ),
            (
                "Pokemon - White Version (USA, Europe) (NDSi Enhanced)",
                "/blackwhite/",
            ),
            (
                "Pokemon - Black Version 2 (USA, Europe) (NDSi Enhanced)",
                "/black2white2/",
            ),
            (
                "Pokemon - White Version 2 (USA, Europe) (NDSi Enhanced)",
                "/black2white2/",
            ),
            ("Pokemon X (USA) (En,Ja,Fr,De,Es,It,Ko)", "/xy/"),
            ("Pokemon Y (Europe) (En,Ja,Fr,De,Es,It,Ko)", "/xy/"),
            (
                "Pokemon Omega Ruby (USA) (Rev 2)",
                "/omegarubyalphasapphire/",
            ),
            ("Pokemon Alpha Sapphire (USA)", "/omegarubyalphasapphire/"),
            ("Pokemon Sun (USA)", "/sunmoon/"),
            ("Pokemon Moon (Europe)", "/sunmoon/"),
            ("Pokemon Ultra Sun (USA)", "/ultrasunultramoon/"),
            ("Pokemon Ultra Moon (Europe)", "/ultrasunultramoon/"),
            ("Pokemon Ranger - Shadows of Almia (USA)", "/ranger2/"),
            ("Pokemon Ranger (USA)", "/ranger/"),
        ];
        for (release, path) in cases {
            let sources = retail("Some title", Some(release));
            assert_eq!(start(&sources, "Serebii"), Some(path), "{release}");
        }
        for (title, path) in [
            ("Pokémon Omega Ruby", "/omegarubyalphasapphire/"),
            ("Pokémon Black 2", "/black2white2/"),
            ("POKéMON HeartGold Version", "/heartgoldsoulsilver/"),
            ("Pokémon Soul Silver", "/heartgoldsoulsilver/"),
        ] {
            assert_eq!(
                start(&retail(title, None), "Serebii"),
                Some(path),
                "{title}"
            );
        }
    }

    #[test]
    fn spin_offs_named_like_main_games_get_no_wrong_start() {
        let pinball = retail("Pokemon Pinball - Ruby & Sapphire (USA)", None);
        assert_eq!(pinball[0].name, "Bulbapedia");
        assert_eq!(start(&pinball, "Serebii"), None);
        let red = retail("Pokemon Mystery Dungeon - Red Rescue Team (USA)", None);
        assert_eq!(red[0].name, "Mystery Dungeon Wiki");
        assert_eq!(start(&red, "Serebii"), None);
        let sky = retail("Pokemon Mystery Dungeon - Explorers of Sky (USA)", None);
        assert_eq!(start(&sky, "Serebii"), Some("/dungeonsky/"));
    }

    #[test]
    fn a_retail_game_gets_its_series_then_the_fallback() {
        let sources = retail("Pokémon Platinum", None);
        assert_eq!(
            names(&sources),
            [
                "Bulbapedia",
                "Serebii",
                "PokémonDB",
                "PokeAPI",
                "Smogon",
                "StrategyWiki",
                "GameFAQs",
                "IGN",
                "Fandom",
            ]
        );
        let api = &sources[3].reach;
        assert_eq!(
            api,
            &Reach::Api {
                base: "https://pokeapi.co/api/v2/".to_owned()
            }
        );
        let unknown = retail("Tetris DS", Some("Tetris DS (USA)"));
        assert_eq!(
            names(&unknown),
            ["StrategyWiki", "GameFAQs", "IGN", "Fandom"]
        );
    }

    #[test]
    fn a_randomized_copy_uses_its_base_games_sources() {
        let random = sources_for(&game(
            "Pokemon Emerald (Random 42)",
            None,
            Kind::Randomized {
                base: "Pokémon Emerald".to_owned(),
            },
        ));
        assert_eq!(random[0].name, "Bulbapedia");
        assert_eq!(start(&random, "Serebii"), Some("/emerald/"));
        assert!(!names(&random).contains(&"PokéCommunity"));
    }

    #[test]
    fn a_fan_game_gets_the_fan_sites_first() {
        let reborn = sources_for(&game("Pokémon Reborn", None, Kind::FanGame));
        assert_eq!(
            names(&reborn)[..6],
            [
                "Fandom",
                "Miraheze",
                "Eevee Expo",
                "Relic Castle",
                "PokéCommunity",
                "romhack.ing"
            ]
        );
        assert_eq!(names(&reborn)[6], "Bulbapedia");
        assert!(reborn.len() <= MAX_SOURCES);
        let ruby = sources_for(&game("Pokémon Ruby Remake", None, Kind::FanGame));
        assert_eq!(start(&ruby, "Serebii"), None);
        let other = sources_for(&game("Some RPG", None, Kind::FanGame));
        assert_eq!(other[0].name, "Fandom");
        assert_eq!(other.last().map(|source| source.name.as_str()), Some("IGN"));
    }

    #[test]
    fn an_unlisted_pokemon_game_gets_the_hack_sites_after_its_series() {
        let hack = sources_for(&game("Pokemon Emerald Rogue", None, Kind::Unlisted));
        let names = names(&hack);
        assert_eq!(names[0], "Bulbapedia");
        let community = names.iter().position(|name| *name == "PokéCommunity");
        let smogon = names.iter().position(|name| *name == "Smogon");
        assert!(community > smogon && smogon.is_some());
        assert_eq!(hack.len(), MAX_SOURCES);
        let mario = sources_for(&game("Super Mario Bros Hack", None, Kind::Unlisted));
        assert_eq!(mario[0].name, "Super Mario Wiki");
        assert!(mario.iter().all(|source| source.name != "PokéCommunity"));
    }

    #[test]
    fn pictures_come_from_the_series_wiki() {
        let wiki = |title: &str, kind: Kind| {
            picture_wiki(&game(title, None, kind)).map(|wiki| (wiki.name, wiki.api))
        };
        let pokemon = Some((
            "Pokémon Wiki".to_owned(),
            "https://pokemon.fandom.com/api.php".to_owned(),
        ));
        assert_eq!(wiki("Pokémon Platinum", Kind::Retail), pokemon);
        assert_eq!(wiki("Pokémon Reborn", Kind::FanGame), pokemon);
        assert_eq!(wiki("Pokemon Emerald Rogue", Kind::Unlisted), pokemon);
        // Without a `pictures` wiki, the series' first MediaWiki site.
        assert_eq!(
            wiki("The Legend of Zelda: The Minish Cap", Kind::Retail),
            Some((
                "Zelda Wiki".to_owned(),
                "https://zeldawiki.wiki/w/api.php".to_owned()
            ))
        );
        // A game in no series has none.
        assert_eq!(wiki("Tetris", Kind::Retail), None);
    }

    #[test]
    fn domains_drop_repeats_and_those_a_wildcard_covers() {
        let metroid = retail("Metroid Fusion", Some("Metroid Fusion (USA)"));
        assert_eq!(
            domains(&metroid),
            [
                "metroidwiki.org",
                "strategywiki.org",
                "gamefaqs.gamespot.com",
                "ign.com/wikis",
                "*.fandom.com",
            ]
        );
        for title in ["Pokémon Reborn", "Pokémon Platinum", "Kirby"] {
            for kind in [Kind::Retail, Kind::Unlisted, Kind::FanGame] {
                let list = domains(&sources_for(&game(title, None, kind)));
                assert!(list.len() <= MAX_SOURCES, "{title}");
            }
        }
    }

    #[test]
    fn site_names_come_from_the_map_by_the_most_specific_domain() {
        let cases = [
            ("https://www.serebii.net/omegarubyalphasapphire/", "Serebii"),
            ("https://serebii.net/", "Serebii"),
            ("HTTPS://WWW.Serebii.NET:443/xy/", "Serebii"),
            (
                "https://bulbapedia.bulbagarden.net/wiki/Ralts",
                "Bulbapedia",
            ),
            ("https://reborn.fandom.com/wiki/Onyx_Ward", "Fandom"),
            (
                "https://pokemonunbound.miraheze.org/wiki/Mega_Stones",
                "Miraheze",
            ),
            ("https://layton.fandom.com/wiki/Puzzle:001", "Layton Wiki"),
            ("https://www.ign.com/wikis/pokemon-emerald", "IGN"),
            ("https://www.ign.com/articles/review", "ign.com"),
            ("https://www.smogon.com/dex/sv/pokemon/ralts/", "Smogon"),
            ("https://www.smogon.com/forums/", "smogon.com"),
            ("https://fandom.com/", "fandom.com"),
            ("https://www.example.org/page", "example.org"),
            ("https://notserebii.net/", "notserebii.net"),
            ("https://pokeapi.co/api/v2/pokemon/ralts", "PokeAPI"),
        ];
        for (url, name) in cases {
            assert_eq!(site_name(url), name, "{url}");
        }
    }

    #[test]
    fn words_ignore_case_accents_punctuation_and_articles() {
        assert_eq!(
            words("Legend of Zelda, The - Link's Awakening DX"),
            ["legend", "of", "zelda", "links", "awakening", "dx"]
        );
        assert_eq!(words("POKéMON Émerald Version"), ["pokemon", "emerald"]);
        assert_eq!(words("Yu-Gi-Oh! 5D's"), ["yu", "gi", "oh", "5ds"]);
    }
}
