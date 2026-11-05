//! The game being played, as Guide tells the model about it: which game,
//! on which system, and whether it is the release the wikis describe.

use std::time::Duration;

/// The system a game is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    /// The original Game Boy.
    GameBoy,
    /// The Game Boy Color.
    GameBoyColor,
    /// The Game Boy Advance.
    GameBoyAdvance,
    /// The Nintendo DS.
    Ds,
    /// The Nintendo 3DS.
    ThreeDs,
    /// A PC game made with RPG Maker (run through mkxp-z).
    RpgMaker,
}

impl Platform {
    /// Its name as players and wikis write it, such as "Nintendo DS".
    pub fn label(self) -> &'static str {
        match self {
            Self::GameBoy => "Game Boy",
            Self::GameBoyColor => "Game Boy Color",
            Self::GameBoyAdvance => "Game Boy Advance",
            Self::Ds => "Nintendo DS",
            Self::ThreeDs => "Nintendo 3DS",
            Self::RpgMaker => "PC (RPG Maker)",
        }
    }
}

/// What kind of copy the game is, which says whether the wikis apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A release listed in No-Intro's database: the wikis describe it.
    Retail,
    /// Not in the database: a ROM hack, a fan translation, homebrew or a
    /// release the database lacks. Its title comes from its file.
    Unlisted,
    /// A copy made by the randomizer (decision 0012) from `base`, the
    /// title it was made from: wild Pokémon, trainers and items differ
    /// from the wikis.
    Randomized {
        /// The title of the game it was made from.
        base: String,
    },
    /// A fan game made with RPG Maker, such as a Pokémon Essentials game.
    FanGame,
}

/// Everything Guide says about the game in each request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameContext {
    /// The title the library shows, such as "Pokémon Omega Ruby".
    pub title: String,
    /// The No-Intro name, with region and revision, such as
    /// "Pokemon Omega Ruby (USA) (Rev 2)", when the database knows it.
    pub release: Option<String>,
    /// The system it is for.
    pub platform: Platform,
    /// The game code, such as "ECRE" or "CTR-P-ECRA", if it has one.
    pub serial: Option<String>,
    /// What kind of copy it is.
    pub kind: Kind,
    /// How long it has been played, if known.
    pub play_time: Option<Duration>,
}

impl GameContext {
    /// The title as short as it reads well in "Ask about …": "Omega Ruby"
    /// for a Pokémon game, otherwise the title before any subtitle colon.
    pub fn short_title(&self) -> String {
        let title = self.title.trim();
        for prefix in ["Pokémon", "Pokemon"] {
            let Some(rest) = title.strip_prefix(prefix) else {
                continue;
            };
            // "Pokemon: Platinum Version", "Pokémon - Emerald Version"
            let rest = rest.trim_start_matches([' ', ':', '-']).trim();
            let rest = rest.strip_suffix(" Version").unwrap_or(rest).trim();
            if !rest.is_empty() {
                return rest.to_owned();
            }
        }
        title.to_owned()
    }

    /// The name the game is looked up by: the release's base name (the
    /// No-Intro name before its first parenthesis), else the title.
    pub fn lookup_name(&self) -> String {
        let release = self
            .release
            .as_deref()
            .map(|name| name.split(" (").next().unwrap_or(name).trim())
            .filter(|name| !name.is_empty());
        match (&self.kind, release) {
            (Kind::Randomized { base }, _) => base.clone(),
            (_, Some(release)) => release.to_owned(),
            _ => self.title.clone(),
        }
    }

    /// Whether the game belongs to the Pokémon series, which has its own
    /// starter questions and sources.
    pub fn is_pokemon(&self) -> bool {
        let name = self.lookup_name().to_lowercase();
        name.starts_with("pokemon") || name.starts_with("pokémon")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(title: &str, release: Option<&str>, kind: Kind) -> GameContext {
        GameContext {
            title: title.to_owned(),
            release: release.map(str::to_owned),
            platform: Platform::ThreeDs,
            serial: None,
            kind,
            play_time: None,
        }
    }

    #[test]
    fn short_titles_drop_the_series_name_of_pokemon_games() {
        let omega = game("Pokémon Omega Ruby", None, Kind::Retail);
        assert_eq!(omega.short_title(), "Omega Ruby");
        let platinum = game("Pokemon: Platinum Version", None, Kind::Retail);
        assert_eq!(platinum.short_title(), "Platinum");
        let zelda = game("The Legend of Zelda: The Minish Cap", None, Kind::Retail);
        assert_eq!(zelda.short_title(), "The Legend of Zelda: The Minish Cap");
    }

    #[test]
    fn games_are_looked_up_by_their_release_or_what_they_were_made_from() {
        let omega = game(
            "Pokémon Omega Ruby",
            Some("Pokemon Omega Ruby (USA) (Rev 2)"),
            Kind::Retail,
        );
        assert_eq!(omega.lookup_name(), "Pokemon Omega Ruby");
        assert!(omega.is_pokemon());
        let random = game(
            "Pokemon Emerald (Random 42)",
            None,
            Kind::Randomized {
                base: "Pokemon Emerald".to_owned(),
            },
        );
        assert_eq!(random.lookup_name(), "Pokemon Emerald");
        assert!(random.is_pokemon());
        let hack = game("Super Mario Bros Hack", None, Kind::Unlisted);
        assert_eq!(hack.lookup_name(), "Super Mario Bros Hack");
        assert!(!hack.is_pokemon());
    }
}
