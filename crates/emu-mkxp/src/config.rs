//! The hidden player's `mkxp.json`: the game's own settings, run from the
//! game's folder with the bridge script loaded first.

use std::path::Path;

use serde_json::{Map, Value};

/// The hidden player's settings: the game's own, `game_settings` (its
/// `mkxp.json`, if it has one), with `gameFolder` set to `game` and
/// `bridge` loaded before the game's own preload scripts. mkxp-z reads
/// every other path in them from the game's folder, as before.
///
/// # Errors
///
/// Fails if `game_settings` is not a JSON object, comments and trailing
/// commas aside, or a path is not UTF-8.
pub fn player_settings(
    game_settings: Option<&str>,
    game: &Path,
    bridge: &Path,
) -> Result<String, String> {
    let mut settings = match game_settings {
        Some(text) => match serde_json::from_str(&plain_json(text)) {
            Ok(Value::Object(settings)) => settings,
            Ok(_) => return Err("mkxp.json is not a JSON object".to_owned()),
            Err(error) => return Err(format!("mkxp.json is not valid JSON: {error}")),
        },
        None => Map::new(),
    };
    let path = |path: &Path| {
        path.to_str()
            .map(|path| Value::String(path.to_owned()))
            .ok_or_else(|| format!("{} is not a UTF-8 path", path.display()))
    };
    settings.insert("gameFolder".to_owned(), path(game)?);
    let mut preload = vec![path(bridge)?];
    if let Some(Value::Array(scripts)) = settings.remove("preloadScript") {
        preload.extend(scripts);
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
        )
        .expect("settings");
        let settings: Value = serde_json::from_str(&text).expect("JSON");
        assert_eq!(settings["gameFolder"], "/Games/My Game");
        assert_eq!(settings["rgssVersion"], 1);
        assert_eq!(
            settings["preloadScript"],
            serde_json::json!([
                "/Support/bridge.rb",
                "mkxp-z/win32_wrap.rb",
                "mkxp-z/game.rb"
            ])
        );
        let bare = player_settings(None, Path::new("/g"), Path::new("/b.rb")).expect("settings");
        let bare: Value = serde_json::from_str(&bare).expect("JSON");
        assert_eq!(bare["preloadScript"], serde_json::json!(["/b.rb"]));
        assert!(player_settings(Some("[1]"), Path::new("/g"), Path::new("/b.rb")).is_err());
    }
}
