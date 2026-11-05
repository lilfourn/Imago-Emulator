//! The system prompt, built from the game and its sources
//! ([PRD §6.4](../../../docs/PRD-game-chat.md)).
//!
//! It is sent with every message, so it stays short, and it changes only
//! when the game, its sources, lookups or the hours played do, which keeps
//! it cacheable.

use std::time::Duration;

use crate::game::{GameContext, Kind};
use crate::pictures::Wiki;
use crate::sources::{Reach, Source};

/// Bumped whenever the prompt's wording changes, so saved chats and tests
/// can tell which prompt an answer came from.
pub const VERSION: u32 = 2;

/// How many sources the prompt suggests checking when it can't look
/// things up.
const SUGGESTED: usize = 4;

/// The system prompt for `game`. `sources` are the game's trusted sites in
/// order; `lookups` says whether web search and fetch are on; `pictures`
/// is the wiki answers' pictures come from, if the game has one.
pub fn system_prompt(
    game: &GameContext,
    sources: &[Source],
    lookups: bool,
    pictures: Option<&Wiki>,
) -> String {
    let mut lines = vec![
        format!(
            "You are Guide, the in-game helper inside DS Emulator. The player is playing {} on {}.{}",
            game.title.trim(),
            game.platform.label(),
            copy(game),
        ),
        kind_line(&game.kind),
        format!(
            "They are playing right now{}, so be quick and to the point.",
            game.play_time.map(hours_in).unwrap_or_default()
        ),
        String::new(),
        "How to answer:".to_owned(),
        "- The panel beside the game is narrow, about 360 points wide. Lead with the answer in \
         one or two sentences, then add at most a few short bullets or a small table of three \
         or four columns. No headings for short answers."
            .to_owned(),
        "- Use the game's own English names for places, items, moves and characters, spelled \
         as this version spells them."
            .to_owned(),
    ];
    if let Some(wiki) = pictures {
        lines.push(pictures_line(game, wiki));
    }
    if lookups {
        lines.push(
            "- For facts about this game, such as locations, encounter rates, item spots, \
             requirements and stats, search before you answer, then read the best page. Trust \
             these sources, in this order:"
                .to_owned(),
        );
        lines.extend(
            sources
                .iter()
                .enumerate()
                .map(|(i, source)| format!("  {}. {}", i + 1, source_line(source))),
        );
        lines.push(format!(
            "- Answer from what you read. Cite each fact right after the sentence it backs, with \
             a Markdown link whose text is exactly the site's name, such as {}; for a site not \
             listed, use its domain without \"www.\". The app shows these links as source chips, \
             so don't add a list of sources at the end.",
            example_link(sources)
        ));
        lines.push(
            "- If you can't find it, say so and suggest where to look. Never invent locations or \
             numbers."
                .to_owned(),
        );
    } else {
        let names: Vec<&str> = sources
            .iter()
            .take(SUGGESTED)
            .map(|source| source.name.as_str())
            .collect();
        let suggest = match names.split_last() {
            None => String::new(),
            Some((last, [])) => format!(" When unsure, suggest checking {last}."),
            Some((last, rest)) => {
                format!(
                    " When unsure, suggest checking {} or {last}.",
                    rest.join(", ")
                )
            }
        };
        lines.push(format!(
            "- You have no web access this time. Answer from what you know, flag anything you're \
             unsure of, and never invent locations or numbers.{suggest}"
        ));
    }
    lines.extend(
        [
            "- Spoilers: answer only what was asked. For \"how do I get past X\", give a hint \
             first and offer the full solution. Never reveal later story events unprompted.",
            "- If a screenshot is attached, use it to tell where the player is and what is \
             happening, and mention what you see when it matters.",
            "- Never help find or download ROMs of commercial games.",
        ]
        .map(str::to_owned),
    );
    let mut prompt = lines.join("\n");
    prompt.push('\n');
    prompt
}

/// How to show pictures from `wiki` ([`crate::pictures`]).
fn pictures_line(game: &GameContext, wiki: &Wiki) -> String {
    let (what, example, row) = if game.is_pokemon() {
        (
            "a Pokémon, a character such as a Gym Leader, or a route, town, cave or building",
            "![Route 210](wiki:Route_210)",
            "a Gym Leader's team",
        )
    } else {
        (
            "a character, enemy, boss or item, or a place",
            "![Page title](wiki:Page_title)",
            "a boss and its weak point",
        )
    };
    format!(
        "- Pictures: whenever the answer is about {what}, show it; a place's picture is its map, \
         so show the map of any place you send the player to or through. Put each picture after \
         the text it goes with, on a line of its own, as a Markdown image whose address is wiki: \
         and the page's title on the {}, spaces as underscores, such as {example}. The app shows \
         that page's main picture. Several on one line sit side by side, such as {row}. At most \
         four per answer, and never any other image address.",
        wiki.name
    )
}

/// Three starter questions for an empty chat about `game`.
pub fn starters(game: &GameContext) -> [&'static str; 3] {
    if game.is_pokemon() {
        [
            "Where am I?",
            "What should I do next?",
            "What's good against this gym?",
        ]
    } else {
        [
            "Where am I?",
            "What should I do next?",
            "How do I beat this?",
        ]
    }
}

/// Which copy it is: " This copy is "…", the USA release, revision 2, with
/// game code ….", or less when less is known.
fn copy(game: &GameContext) -> String {
    let serial = game
        .serial
        .as_deref()
        .map(str::trim)
        .filter(|serial| !serial.is_empty());
    let Some(release) = game
        .release
        .as_deref()
        .map(str::trim)
        .filter(|release| !release.is_empty())
    else {
        return serial.map_or_else(String::new, |serial| format!(" Its game code is {serial}."));
    };
    let mut details = Vec::new();
    let mut groups = release
        .split('(')
        .skip(1)
        .filter_map(|group| group.split_once(')').map(|(inside, _)| inside.trim()));
    // No-Intro names give the region first, then languages, revisions and
    // the rest.
    if let Some(region) = groups.next() {
        details.push(format!("the {region} release"));
    }
    for group in groups {
        if let Some(revision) = group.strip_prefix("Rev ") {
            details.push(format!("revision {revision}"));
        } else if let Some(version) = group
            .strip_prefix('v')
            .filter(|version| version.starts_with(|c: char| c.is_ascii_digit()))
        {
            details.push(format!("version {version}"));
        }
    }
    if let Some(serial) = serial {
        details.push(format!("with game code {serial}"));
    }
    let details: String = details.iter().map(|detail| format!(", {detail}")).collect();
    format!(" This copy is \"{release}\"{details}.")
}

/// What kind of copy it is, and so how far the wikis apply.
fn kind_line(kind: &Kind) -> String {
    match kind {
        Kind::Retail => "It is a retail release, so the wikis describe it.".to_owned(),
        Kind::Randomized { base } => format!(
            "It was randomized from {}: wild Pokémon, trainers, items and more differ from the \
             wikis. Trust the wikis for maps, story and mechanics but not for what appears \
             where, and suggest in-game ways to check, such as the Pokédex's area map.",
            base.trim()
        ),
        Kind::Unlisted => "It isn't in the No-Intro database, so it may be a ROM hack, a fan \
                           translation or homebrew, and details can differ from the wikis. Say \
                           so when a fact might not apply."
            .to_owned(),
        Kind::FanGame => "It is a fan game made with RPG Maker, not an official release. Wikis \
                          may be thin or describe the official games instead, so say when \
                          you're unsure."
            .to_owned(),
    }
}

/// ", about 12 hours in": whole hours, so the prompt changes at most
/// hourly.
fn hours_in(played: Duration) -> String {
    match played.as_secs() / 3600 {
        0 => ", less than an hour in".to_owned(),
        1 => ", about an hour in".to_owned(),
        hours => format!(", about {hours} hours in"),
    }
}

/// A source in the trust list: its name, domain, use, and where this game
/// starts on it.
fn source_line(source: &Source) -> String {
    let mut line = format!(
        "{} ({}): {}.",
        source.name,
        source.domain,
        source.good_for.trim_end_matches('.')
    );
    if let Reach::Api { base } = &source.reach {
        line.push_str(&format!(" Its JSON API is at {base}."));
    }
    if let Some(start) = &source.start {
        line.push_str(&format!(
            " For this game, start at {}{start}.",
            source.domain
        ));
    }
    line
}

/// A citation in the form the app expects, on the first source with a
/// start for this game, else the first that isn't a wildcard.
fn example_link(sources: &[Source]) -> String {
    let source = sources
        .iter()
        .find(|source| source.start.is_some())
        .or_else(|| {
            sources
                .iter()
                .find(|source| !source.domain.starts_with("*."))
        });
    match source {
        Some(source) => format!(
            "[{}](https://{}{}…)",
            source.name,
            source.domain,
            source.start.as_deref().unwrap_or("/")
        ),
        None => "[StrategyWiki](https://strategywiki.org/…)".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::Platform;

    fn game(release: Option<&str>, serial: Option<&str>) -> GameContext {
        GameContext {
            title: "Pokémon Omega Ruby".to_owned(),
            release: release.map(str::to_owned),
            platform: Platform::ThreeDs,
            serial: serial.map(str::to_owned),
            kind: Kind::Retail,
            play_time: None,
        }
    }

    #[test]
    fn the_copy_names_its_region_revision_and_code() {
        let full = game(Some("Pokemon Omega Ruby (USA) (Rev 2)"), Some("CTR-P-ECRE"));
        assert_eq!(
            copy(&full),
            " This copy is \"Pokemon Omega Ruby (USA) (Rev 2)\", the USA release, revision 2, \
             with game code CTR-P-ECRE."
        );
        let languages = game(Some("Pokemon X (Europe) (En,Fr,De) (v1.5)"), None);
        assert_eq!(
            copy(&languages),
            " This copy is \"Pokemon X (Europe) (En,Fr,De) (v1.5)\", the Europe release, \
             version 1.5."
        );
        assert_eq!(copy(&game(None, Some("ECRE"))), " Its game code is ECRE.");
        assert_eq!(copy(&game(Some(" "), None)), "");
    }

    #[test]
    fn play_time_is_given_in_whole_hours() {
        assert_eq!(
            hours_in(Duration::from_secs(59 * 60)),
            ", less than an hour in"
        );
        assert_eq!(hours_in(Duration::from_secs(90 * 60)), ", about an hour in");
        assert_eq!(
            hours_in(Duration::from_secs(12 * 3600 + 59)),
            ", about 12 hours in"
        );
    }

    #[test]
    fn citations_are_shown_on_a_source_with_a_start() {
        let source = |name: &str, domain: &str, start: Option<&str>| Source {
            name: name.to_owned(),
            domain: domain.to_owned(),
            reach: Reach::Fetch,
            good_for: "things".to_owned(),
            start: start.map(str::to_owned),
        };
        let sources = [
            source("Fandom", "*.fandom.com", None),
            source("Bulbapedia", "bulbapedia.bulbagarden.net", None),
            source("Serebii", "serebii.net", Some("/xy/")),
        ];
        assert_eq!(
            example_link(&sources),
            "[Serebii](https://serebii.net/xy/…)"
        );
        assert_eq!(
            example_link(&sources[..2]),
            "[Bulbapedia](https://bulbapedia.bulbagarden.net/…)"
        );
        assert_eq!(
            example_link(&[]),
            "[StrategyWiki](https://strategywiki.org/…)"
        );
    }

    #[test]
    fn pokemon_games_get_their_own_starters() {
        assert_eq!(
            starters(&game(None, None))[2],
            "What's good against this gym?"
        );
    }
}
