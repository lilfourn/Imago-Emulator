//! The system prompt for one game of each kind, compared exactly with the
//! text files in `tests/prompts/`. After changing the prompt on purpose,
//! `UPDATE_PROMPTS=1 cargo test -p emu-guide --test prompts` rewrites them.

use std::path::PathBuf;
use std::time::Duration;

use emu_guide::game::{GameContext, Kind, Platform};
use emu_guide::prompt::{VERSION, system_prompt};
use emu_guide::sources::sources_for;

fn game(
    title: &str,
    release: Option<&str>,
    platform: Platform,
    serial: Option<&str>,
    kind: Kind,
    hours: Option<u64>,
) -> GameContext {
    GameContext {
        title: title.to_owned(),
        release: release.map(str::to_owned),
        platform,
        serial: serial.map(str::to_owned),
        kind,
        play_time: hours.map(|hours| Duration::from_secs(hours * 3600 + 600)),
    }
}

/// Compares the prompt for `game` with `tests/prompts/{name}.txt`, or
/// rewrites the file when `UPDATE_PROMPTS` is set.
fn check(name: &str, game: &GameContext, lookups: bool) {
    let prompt = system_prompt(game, &sources_for(game), lookups);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/prompts")
        .join(format!("{name}.txt"));
    if std::env::var_os("UPDATE_PROMPTS").is_some() {
        std::fs::write(&path, &prompt).expect("write the snapshot");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "{}: {error}; run with UPDATE_PROMPTS=1 to write it",
            path.display()
        )
    });
    assert!(
        prompt == expected,
        "the prompt for {name} changed; if on purpose, run with UPDATE_PROMPTS=1 and bump \
         prompt::VERSION (now {VERSION}).\n--- expected\n{expected}\n--- got\n{prompt}"
    );
    let words = prompt.split_whitespace().count();
    assert!(words < 600, "the prompt for {name} has {words} words");
}

#[test]
fn retail_omega_ruby() {
    let game = game(
        "Pokémon Omega Ruby",
        Some("Pokemon Omega Ruby (USA) (Rev 2)"),
        Platform::ThreeDs,
        Some("CTR-P-ECRE"),
        Kind::Retail,
        Some(12),
    );
    check("omega_ruby", &game, true);
}

#[test]
fn retail_minish_cap() {
    let game = game(
        "The Legend of Zelda: The Minish Cap",
        Some("Legend of Zelda, The - The Minish Cap (USA)"),
        Platform::GameBoyAdvance,
        Some("BZME"),
        Kind::Retail,
        Some(0),
    );
    check("minish_cap", &game, true);
}

#[test]
fn randomized_emerald() {
    let game = game(
        "Pokémon Emerald (Randomized)",
        None,
        Platform::GameBoyAdvance,
        Some("BPEE"),
        Kind::Randomized {
            base: "Pokémon Emerald".to_owned(),
        },
        Some(3),
    );
    check("randomized_emerald", &game, true);
}

#[test]
fn unlisted_hack() {
    let game = game(
        "Pokemon Unbound",
        None,
        Platform::GameBoyAdvance,
        Some("BPRE"),
        Kind::Unlisted,
        None,
    );
    check("unlisted_hack", &game, true);
}

#[test]
fn rpg_maker_fan_game() {
    let game = game(
        "Pokémon Reborn",
        None,
        Platform::RpgMaker,
        None,
        Kind::FanGame,
        Some(40),
    );
    check("fan_game", &game, true);
}

#[test]
fn without_lookups() {
    let game = game(
        "Professor Layton and the Curious Village",
        Some("Professor Layton and the Curious Village (USA)"),
        Platform::Ds,
        Some("A5FE"),
        Kind::Retail,
        Some(1),
    );
    check("no_lookups", &game, false);
}
