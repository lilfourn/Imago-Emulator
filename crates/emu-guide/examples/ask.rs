//! Asks Guide one question from the command line, as the panel would, and
//! prints the stream, what it drew on, how long it took and what it cost,
//! then finds the pictures the answer asked for on the game's wiki.
//! Run by hand, never in CI: it spends the connected account's credits.
//!
//! ```text
//! cargo run -p emu-guide --example ask -- \
//!     --game "Pokemon Omega Ruby (USA)" [--platform 3ds] [--effort low] \
//!     [--no-lookups] [--screen shot.png] "where can I catch Ralts?"
//! ```
//!
//! The key is the app's (Settings > Guide > Connect OpenRouter), or
//! `OPENROUTER_API_KEY`.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use emu_guide::api::Endpoints;
use emu_guide::chat::{self, Effort, Event, Message, Request, Stop};
use emu_guide::game::{GameContext, Kind, Platform};
use emu_guide::key::KeyStore;
use emu_guide::markdown::{Block, Document};
use emu_guide::pictures::PictureStore;
use emu_guide::{prompt, sources};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut game = "Pokemon Omega Ruby (USA)".to_owned();
    let mut platform = Platform::ThreeDs;
    let mut effort = Effort::Low;
    let mut lookups = true;
    let mut kind = Kind::Retail;
    let mut screens = Vec::new();
    let mut question = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--game" => game = args.next().expect("--game NAME"),
            "--platform" => {
                platform = match args.next().as_deref() {
                    Some("gb") => Platform::GameBoy,
                    Some("gbc") => Platform::GameBoyColor,
                    Some("gba") => Platform::GameBoyAdvance,
                    Some("nds") => Platform::Ds,
                    Some("rpgmaker") => {
                        kind = Kind::FanGame;
                        Platform::RpgMaker
                    }
                    _ => Platform::ThreeDs,
                }
            }
            "--effort" => {
                effort = match args.next().as_deref() {
                    Some("none") => Effort::None,
                    Some("medium") => Effort::Medium,
                    Some("high") => Effort::High,
                    _ => Effort::Low,
                }
            }
            "--no-lookups" => lookups = false,
            "--screen" => screens.push(PathBuf::from(args.next().expect("--screen FILE"))),
            _ => question = Some(arg),
        }
    }
    let question = question.expect("a question");
    let title = game.split(" (").next().unwrap_or(&game).to_owned();
    let context = GameContext {
        title,
        release: (kind == Kind::Retail).then(|| game.clone()),
        platform,
        serial: None,
        kind,
        play_time: None,
    };
    let folder = dirs_home().join("Library/Application Support/ds-emulator/OpenRouter");
    let key = KeyStore::new(folder)
        .load()
        .expect("connect OpenRouter in the app first");
    let sources = sources::sources_for(&context);
    let domains = sources::domains(&sources);
    let wiki = sources::picture_wiki(&context);
    eprintln!("sources: {domains:?}");
    eprintln!("pictures: {wiki:?}");
    let images: Vec<Arc<[u8]>> = screens
        .iter()
        .map(|path| Arc::from(std::fs::read(path).expect("screenshot").into_boxed_slice()))
        .collect();
    let request = Request {
        model: chat::MODEL.to_owned(),
        system: prompt::system_prompt(&context, &sources, lookups, wiki.as_ref()),
        messages: vec![Message::user(question, images)],
        effort,
        lookups: lookups.then_some(domains),
        session: format!("ask-{}", std::process::id()),
    };
    let started = Instant::now();
    let mut first = None;
    let mut answer = String::new();
    let mut stdout = std::io::stdout();
    chat::ask(
        &Endpoints::openrouter(),
        key.secret(),
        &request,
        &Stop::new(),
        &mut |event| match event {
            Event::Text(text) => {
                first.get_or_insert_with(|| started.elapsed());
                answer.push_str(&text);
                print!("{text}");
                stdout.flush().ok();
            }
            Event::Working => {}
            Event::ReasoningDetails(details) => {
                let kinds: Vec<_> = details.iter().map(|d| d["type"].to_string()).collect();
                eprintln!("[reasoning details {kinds:?} at {:.2?}]", started.elapsed());
            }
            other => eprintln!("[{other:?} at {:.2?}]", started.elapsed()),
        },
    );
    println!();
    eprintln!(
        "first text {:.2?}, total {:.2?}",
        first.unwrap_or_default(),
        started.elapsed()
    );
    let Some(wiki) = wiki else {
        return;
    };
    // Kept apart from the app's, so a run never fills its cache.
    let store = PictureStore::new(std::env::temp_dir().join("emu-guide-ask-pictures"));
    for block in Document::parse(&answer).blocks() {
        for picture in pictures_in(block) {
            let target = picture.target.as_deref().unwrap_or_default();
            let fetched = Instant::now();
            match store.fetch(&wiki, target) {
                Ok(found) => eprintln!(
                    "picture {target}: {} bytes, {} ({:.2?})",
                    found.bytes.len(),
                    found.page,
                    fetched.elapsed()
                ),
                Err(error) => eprintln!("picture {target}: {error}"),
            }
        }
    }
}

/// The pictures in `block` and the blocks inside it.
fn pictures_in(block: &Block) -> Vec<&emu_guide::markdown::Picture> {
    match block {
        Block::Pictures(pictures) => pictures.iter().collect(),
        Block::List { items, .. } => items.iter().flatten().flat_map(pictures_in).collect(),
        Block::Quote(blocks) => blocks.iter().flat_map(pictures_in).collect(),
        _ => Vec::new(),
    }
}

fn dirs_home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").expect("HOME"))
}
