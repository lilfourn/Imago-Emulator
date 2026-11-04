//! The system prompt, built from the game and its sources.
//!
//! CONTRACT STUB — to be implemented (see the task brief). Keep the public
//! signatures.

use crate::game::GameContext;
use crate::sources::Source;

/// Bumped whenever the prompt's wording changes, so saved chats and tests
/// can tell which prompt an answer came from.
pub const VERSION: u32 = 1;

/// The system prompt for `game`. `sources` are the game's trusted sites in
/// order; `lookups` says whether web search and fetch are on.
pub fn system_prompt(game: &GameContext, sources: &[Source], lookups: bool) -> String {
    let _ = (sources, lookups);
    format!(
        "You are Guide, the in-game helper inside DS Emulator. The player is playing {} ({}).",
        game.title,
        game.platform.label()
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
        ["Where am I?", "What should I do next?", "How do I beat this?"]
    }
}
