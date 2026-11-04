//! The source map: which fan sites and wikis players trust for a game.
//!
//! CONTRACT STUB — to be implemented (see the task brief). Keep the public
//! types and signatures; the app and `prompt` use them.

use crate::game::GameContext;

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
/// the fallback ones every game gets, without repeats.
pub fn sources_for(game: &GameContext) -> Vec<Source> {
    let _ = game;
    vec![Source {
        name: "StrategyWiki".to_owned(),
        domain: "strategywiki.org".to_owned(),
        reach: Reach::MediaWiki {
            api: "/w/api.php".to_owned(),
        },
        good_for: "walkthroughs".to_owned(),
        start: None,
    }]
}

/// The domains web search and fetch may use for these sources.
pub fn domains(sources: &[Source]) -> Vec<String> {
    sources.iter().map(|source| source.domain.clone()).collect()
}

/// A short name for the site a URL is on: the source map's name for it if
/// it has one ("Serebii"), else its host without "www.".
pub fn site_name(url: &str) -> String {
    let host = url
        .split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or_default();
    host.trim_start_matches("www.").to_owned()
}
