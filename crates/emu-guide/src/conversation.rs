//! Each game's chats, kept across launches: one JSON file a chat, and its
//! screenshots as PNG files in a folder beside it.
//!
//! ```text
//! Guide/
//!   {game}/                  one folder a game, named by its key
//!     {chat}.json            a chat: its questions and answers
//!     {chat}/1.png           the screenshots sent with it
//! ```
//!
//! The chat saved last is the one a game opens with.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::chat::{Citation, Usage};

/// How an answer ended.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "state", content = "why")]
pub enum Ending {
    /// Still streaming, or the app quit while it was.
    #[default]
    Streaming,
    /// Finished.
    Done,
    /// The player stopped it.
    Stopped,
    /// It failed, for this reason.
    Failed(String),
}

/// The model's answer to one question.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Answer {
    /// The answer's Markdown.
    pub text: String,
    /// The reasoning blocks, sent back unchanged with later questions.
    pub reasoning_details: Vec<Value>,
    /// Readable thinking, when the model gave any.
    pub thinking: String,
    /// The pages it drew on, without repeats.
    pub citations: Vec<Citation>,
    /// How long it thought before writing, in seconds.
    pub thought_for: Option<f32>,
    /// What it cost.
    pub usage: Option<Usage>,
    /// The model that answered.
    pub model: Option<String>,
    /// How it ended.
    pub ending: Ending,
}

impl Answer {
    /// Adds a cited page, unless it is there already.
    pub fn cite(&mut self, citation: Citation) {
        if !self.citations.iter().any(|known| known.url == citation.url) {
            self.citations.push(citation);
        }
    }
}

/// A question and its answer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Turn {
    /// What the player asked.
    pub question: String,
    /// The screenshots sent with it, by file name in the chat's folder.
    pub shots: Vec<String>,
    /// The answer.
    pub answer: Answer,
}

/// One chat about one game. A new one, by default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Chat {
    /// Its id, which names its file: when it started, in milliseconds
    /// since 1970.
    pub id: String,
    /// When it started, in seconds since 1970.
    pub started: u64,
    /// Its questions and answers.
    pub turns: Vec<Turn>,
}

impl Default for Chat {
    fn default() -> Self {
        Self::new()
    }
}

impl Chat {
    /// A new, empty chat.
    pub fn new() -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self {
            id: now.as_millis().to_string(),
            started: now.as_secs(),
            turns: Vec::new(),
        }
    }

    /// The first question, which names the chat in the list of chats.
    pub fn title(&self) -> &str {
        self.turns
            .first()
            .map_or("New chat", |turn| turn.question.as_str())
    }
}

/// A chat as the list of a game's chats shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// The chat's id.
    pub id: String,
    /// Its first question.
    pub title: String,
    /// When it started, in seconds since 1970.
    pub started: u64,
}

/// Where chats are kept: the `Guide` folder in the app's data folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatStore {
    root: PathBuf,
}

impl ChatStore {
    /// Chats under `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn game_folder(&self, game: &str) -> PathBuf {
        self.root.join(safe_name(game))
    }

    fn chat_file(&self, game: &str, chat: &str) -> PathBuf {
        self.game_folder(game)
            .join(format!("{}.json", safe_name(chat)))
    }

    /// The folder `chat`'s screenshots are in.
    pub fn shot_folder(&self, game: &str, chat: &str) -> PathBuf {
        self.game_folder(game).join(safe_name(chat))
    }

    /// Where screenshot `name` of `chat` is.
    pub fn shot_path(&self, game: &str, chat: &str, name: &str) -> PathBuf {
        self.shot_folder(game, chat).join(safe_name(name))
    }

    /// `game`'s chats, newest first.
    pub fn list(&self, game: &str) -> Vec<Summary> {
        let Ok(entries) = fs::read_dir(self.game_folder(game)) else {
            return Vec::new();
        };
        let mut chats: Vec<(SystemTime, Summary)> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .filter_map(|entry| {
                let modified = entry.metadata().and_then(|m| m.modified()).ok()?;
                let chat = read_chat(&entry.path())?;
                Some((
                    modified,
                    Summary {
                        id: chat.id.clone(),
                        title: chat.title().to_owned(),
                        started: chat.started,
                    },
                ))
            })
            .collect();
        chats.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.id.cmp(&a.1.id)));
        chats.into_iter().map(|(_, summary)| summary).collect()
    }

    /// `game`'s chat `id`, if it is there and readable.
    pub fn load(&self, game: &str, id: &str) -> Option<Chat> {
        read_chat(&self.chat_file(game, id))
    }

    /// The chat `game` opens with: the one saved last.
    pub fn latest(&self, game: &str) -> Option<Chat> {
        let newest = self.list(game).into_iter().next()?;
        self.load(game, &newest.id)
    }

    /// Saves `chat` for `game`, replacing the file whole.
    ///
    /// # Errors
    ///
    /// If the folder or file can't be written.
    pub fn save(&self, game: &str, chat: &Chat) -> io::Result<()> {
        let folder = self.game_folder(game);
        fs::create_dir_all(&folder)?;
        let path = self.chat_file(game, &chat.id);
        let text = serde_json::to_vec_pretty(chat).map_err(io::Error::other)?;
        emu_os::file::write_atomic(&path, &text)
    }

    /// Keeps a screenshot for `chat`, returning its file name.
    ///
    /// # Errors
    ///
    /// If the folder or file can't be written.
    pub fn save_shot(&self, game: &str, chat: &str, png: &[u8]) -> io::Result<String> {
        let folder = self.shot_folder(game, chat);
        fs::create_dir_all(&folder)?;
        let taken = fs::read_dir(&folder)?.filter_map(Result::ok).count();
        let mut number = taken + 1;
        loop {
            let name = format!("{number}.png");
            let path = folder.join(&name);
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    file.write_all(png)?;
                    return Ok(name);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => number += 1,
                Err(error) => return Err(error),
            }
        }
    }

    /// Deletes `game`'s chat `id` and its screenshots.
    ///
    /// # Errors
    ///
    /// If something there can't be removed.
    pub fn delete(&self, game: &str, id: &str) -> io::Result<()> {
        match fs::remove_file(self.chat_file(game, id)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        match fs::remove_dir_all(self.shot_folder(game, id)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }
}

fn read_chat(path: &Path) -> Option<Chat> {
    let text = fs::read(path).ok()?;
    serde_json::from_slice(&text).ok()
}

/// `name` as a file name: letters, digits, `-`, `_` and `.` only, never
/// leading with a dot, so a key can't reach outside its folder.
fn safe_name(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let safe = safe.trim_start_matches('.');
    if safe.is_empty() {
        "_".to_owned()
    } else {
        safe.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str) -> ChatStore {
        let root =
            std::env::temp_dir().join(format!("emu-guide-chats-{name}-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        ChatStore::new(root)
    }

    #[test]
    fn chats_are_kept_and_the_latest_opens() {
        let store = store("kept");
        assert!(store.latest("nds-1234").is_none());
        let mut first = Chat::new();
        first.id = "1".to_owned();
        first.turns.push(Turn {
            question: "Where is Ralts?".to_owned(),
            ..Turn::default()
        });
        store.save("nds-1234", &first).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let mut second = Chat::new();
        second.id = "2".to_owned();
        second.turns.push(Turn {
            question: "And Abra?".to_owned(),
            answer: Answer {
                text: "Route 116.".to_owned(),
                ending: Ending::Done,
                ..Answer::default()
            },
            ..Turn::default()
        });
        store.save("nds-1234", &second).unwrap();
        assert_eq!(store.latest("nds-1234"), Some(second.clone()));
        let titles: Vec<_> = store
            .list("nds-1234")
            .into_iter()
            .map(|summary| summary.title)
            .collect();
        assert_eq!(titles, ["And Abra?", "Where is Ralts?"]);
        assert!(store.list("other").is_empty());
    }

    #[test]
    fn screenshots_are_numbered_and_go_with_their_chat() {
        let store = store("shots");
        let chat = Chat::new();
        store.save("gba-1", &chat).unwrap();
        let first = store.save_shot("gba-1", &chat.id, b"one").unwrap();
        let second = store.save_shot("gba-1", &chat.id, b"two").unwrap();
        assert_eq!((first.as_str(), second.as_str()), ("1.png", "2.png"));
        assert_eq!(
            fs::read(store.shot_path("gba-1", &chat.id, "2.png")).unwrap(),
            b"two"
        );
        store.delete("gba-1", &chat.id).unwrap();
        assert!(store.load("gba-1", &chat.id).is_none());
        assert!(!store.shot_folder("gba-1", &chat.id).exists());
        store.delete("gba-1", &chat.id).unwrap();
    }

    #[test]
    fn keys_cannot_reach_outside_the_folder() {
        assert_eq!(safe_name("../../etc"), "_.._etc");
        assert_eq!(safe_name(""), "_");
        assert_eq!(safe_name("nds-0a1b2c3d"), "nds-0a1b2c3d");
    }

    #[test]
    fn answers_cite_each_page_once_and_read_back_from_older_files() {
        let mut answer = Answer::default();
        let page = Citation {
            url: "https://serebii.net/a".to_owned(),
            title: "A".to_owned(),
            excerpt: String::new(),
        };
        answer.cite(page.clone());
        answer.cite(page);
        assert_eq!(answer.citations.len(), 1);
        let chat: Chat =
            serde_json::from_str(r#"{"id":"9","turns":[{"question":"q","answer":{"text":"a"}}]}"#)
                .unwrap();
        assert_eq!(chat.turns[0].answer.ending, Ending::Streaming);
        assert_eq!(chat.title(), "q");
    }
}
