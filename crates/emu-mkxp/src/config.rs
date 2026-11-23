//! The hidden player's `mkxp.json`: the game's own settings, run from the
//! game's folder with the bridge script loaded first, drawing with OpenGL
//! on macOS and Linux, and never in its own window.

use std::path::Path;

use serde_json::{Map, Value};

use crate::run::Run;

/// mkxp-z's `renderer` setting for OpenGL. The bridge reads each frame
/// back from the GPU, which took 7.8 ms a frame with mkxp-z's usual Metal
/// and 1.2 ms with OpenGL, for the same pixels. On Linux it is the system's
/// OpenGL. On Windows mkxp-z draws through ANGLE whatever the setting, and
/// its OpenGL there is ANGLE over the driver's, so it keeps its own choice,
/// Direct3D 11.
const OPENGL: u64 = 2;

/// The settings in `text`, a game's `mkxp.json`, as mkxp-z reads it.
///
/// # Errors
///
/// Fails if `text` is not a JSON object, comments and trailing commas
/// aside.
pub fn game_settings(text: &str) -> Result<Map<String, Value>, String> {
    match serde_json::from_str(&plain_json(text)) {
        Ok(Value::Object(settings)) => Ok(settings),
        Ok(_) => Err("mkxp.json is not a JSON object".to_owned()),
        Err(error) => Err(format!("mkxp.json is not valid JSON: {error}")),
    }
}

/// The hidden player's settings: the game's own, `game_settings` (its
/// `mkxp.json`, if it has one), with `gameFolder` set to `game` and
/// `bridge` loaded before the game's own preload scripts. mkxp-z reads
/// every other path in them from the game's folder, as before. It draws
/// with OpenGL, unless the game chose how, but on Windows.
///
/// For a run of the game (the folder it is in, and what it is), the run's
/// scripts are loaded after the game's own, and its saves are kept under
/// the run's name, in the game's organisation or mkxp-z's.
///
/// # Errors
///
/// Fails if `game_settings` is not a JSON object, comments and trailing
/// commas aside, or a path is not UTF-8.
pub fn player_settings(
    game_settings: Option<&str>,
    game: &Path,
    bridge: &Path,
    run: Option<(&Path, &Run)>,
) -> Result<String, String> {
    let mut settings = match game_settings {
        Some(text) => self::game_settings(text)?,
        None => Map::new(),
    };
    let path = |path: &Path| {
        path.to_str()
            .map(|path| Value::String(path.to_owned()))
            .ok_or_else(|| format!("{} is not a UTF-8 path", path.display()))
    };
    settings.insert("gameFolder".to_owned(), path(game)?);
    if !cfg!(windows) {
        settings
            .entry("renderer")
            .or_insert_with(|| Value::from(OPENGL));
    }
    // Its window is never seen, so it need not draw there: the bridge
    // lifts mkxp-z's frame limit, so with frame skipping on, mkxp-z is
    // always behind and skips it. Drawing there stalled a frame by about
    // 10 ms, every 50 or so. The frames sent are drawn on their own.
    settings.insert("frameSkip".to_owned(), Value::Bool(true));
    let mut preload = vec![path(bridge)?];
    if let Some(Value::Array(scripts)) = settings.remove("preloadScript") {
        preload.extend(scripts);
    }
    if let Some((folder, run)) = run {
        for script in run.scripts(folder) {
            preload.push(path(&script)?);
        }
        let organisation = crate::run::organisation(Some(&Value::Object(settings.clone())));
        settings.insert("dataPathOrg".to_owned(), Value::String(organisation));
        settings.insert("dataPathApp".to_owned(), Value::String(run.saves.clone()));
    }
    settings.insert("preloadScript".to_owned(), Value::Array(preload));
    let text = serde_json::to_string_pretty(&Value::Object(settings))
        .expect("a JSON object always serializes");
    Ok(format!("{text}\n"))
}

/// `text`, a JSON document as mkxp-z reads one, as plain JSON: without its
/// `//` and `/* */` comments, or commas before a closing bracket.
fn plain_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => out.extend(chars.next()),
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push(c);
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut last = ' ';
                for c in chars.by_ref() {
                    if last == '*' && c == '/' {
                        break;
                    }
                    last = c;
                }
            }
            (',', _) => {
                let rest = chars.clone().find(|c| !c.is_whitespace());
                if !matches!(rest, Some('}' | ']')) {
                    out.push(c);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_and_trailing_commas_go_but_strings_stay() {
        let text = "{\n  // a comment\n  \"a\": \"// not a comment\", /* inline */\n  \"b\": [1, 2,],\n  \"c\": \"quote \\\" and , ]\",\n}\n";
        let json: Value = serde_json::from_str(&plain_json(text)).expect("plain JSON");
        assert_eq!(
            json,
            serde_json::json!({"a": "// not a comment", "b": [1, 2], "c": "quote \" and , ]"})
        );
    }

    #[test]
    fn the_game_runs_from_its_folder_with_the_bridge_first() {
        let game = "{\n    // The game's own.\n    \"rgssVersion\": 1,\n    \"preloadScript\": [\n        \"mkxp-z/win32_wrap.rb\",\n        \"mkxp-z/game.rb\",\n    ],\n}\n";
        let text = player_settings(
            Some(game),
            Path::new("/Games/My Game"),
            Path::new("/Support/bridge.rb"),
            None,
        )
        .expect("settings");
        let settings: Value = serde_json::from_str(&text).expect("JSON");
        assert_eq!(settings["gameFolder"], "/Games/My Game");
        assert_eq!(settings["rgssVersion"], 1);
        if cfg!(windows) {
            assert_eq!(settings.get("renderer"), None, "mkxp-z's own choice");
        } else {
            assert_eq!(settings["renderer"], 2, "OpenGL");
        }
        assert_eq!(settings["frameSkip"], true);
        assert_eq!(
            settings["preloadScript"],
            serde_json::json!([
                "/Support/bridge.rb",
                "mkxp-z/win32_wrap.rb",
                "mkxp-z/game.rb"
            ])
        );
        let bare =
            player_settings(None, Path::new("/g"), Path::new("/b.rb"), None).expect("settings");
        let bare: Value = serde_json::from_str(&bare).expect("JSON");
        assert_eq!(bare["preloadScript"], serde_json::json!(["/b.rb"]));
        let metal = player_settings(
            Some("{\"renderer\": 4}"),
            Path::new("/g"),
            Path::new("/b.rb"),
            None,
        )
        .expect("settings");
        let metal: Value = serde_json::from_str(&metal).expect("JSON");
        assert_eq!(metal["renderer"], 4, "the game's own choice stays");
        assert!(player_settings(Some("[1]"), Path::new("/g"), Path::new("/b.rb"), None).is_err());
    }

    #[test]
    fn a_run_loads_its_scripts_last_and_saves_under_its_own_name() {
        let game = "{\"dataPathOrg\": \"Studio\", \"dataPathApp\": \"My Game\", \"preloadScript\": [\"mkxp-z/game.rb\"]}";
        let run = Run {
            game: Path::new("/Games/My Game").to_owned(),
            preload: vec!["randomizer.rb".to_owned()],
            saves: "My Game (Random 123456)".to_owned(),
        };
        let folder = Path::new("/Hacks/My Game (Random 123456)");
        let settings = |game: Option<&str>| -> Value {
            let text = player_settings(
                game,
                Path::new("/Games/My Game"),
                Path::new("/Support/bridge.rb"),
                Some((folder, &run)),
            )
            .expect("settings");
            serde_json::from_str(&text).expect("JSON")
        };
        let with = settings(Some(game));
        assert_eq!(
            with["gameFolder"], "/Games/My Game",
            "the game's own folder"
        );
        let script = folder.join("randomizer.rb");
        assert_eq!(
            with["preloadScript"],
            serde_json::json!(["/Support/bridge.rb", "mkxp-z/game.rb", script.to_str()])
        );
        assert_eq!(with["dataPathOrg"], "Studio");
        assert_eq!(with["dataPathApp"], "My Game (Random 123456)");
        // A game that names no organisation: mkxp-z's.
        let bare = settings(None);
        assert_eq!(bare["dataPathOrg"], "mkxp-z");
        assert_eq!(bare["dataPathApp"], "My Game (Random 123456)");
    }
}
