//! A game's life in the window: opened, loaded off the UI thread, run,
//! and stopped, by the player or after an error.
//!
//! [`Game`] says where the window is with its game, and its methods are
//! the only ways from one state to another. The window's part, which
//! needs the window itself, is the methods of [`EmulatorView`] here:
//! opening games ([`EmulatorView::open`]), starting one that has loaded
//! in place of the one running, stopping, starting over and closing.
//!
//! The order of things is what matters here. A running game is stopped,
//! which writes its saves and its checkpoint, before the game that takes
//! its place starts: once that one has loaded, so that a file that fails
//! to load costs nothing, or before it loads at all when the two may not
//! overlap (see [`stops_before_loading`]).

// A part of the view, split off: it shares the view's names.
use super::*;

/// Games on their way to being played, loading off the UI thread.
#[derive(Debug)]
pub(super) struct Load<L> {
    /// The games, as they were asked for.
    games: Vec<PathBuf>,
    /// What loads them. Dropping it gives the load up.
    task: L,
}

/// Where the window is with its game. `S` is the running game's session
/// and `L` what loads the next: the window's own `Session` and task, or
/// stand-ins in tests.
#[derive(Debug)]
pub(super) enum Game<S, L> {
    /// No game: the library shows.
    Idle,
    /// Games are loading, and none runs meanwhile: none did, or it had to
    /// stop first.
    Loading(Load<L>),
    /// A game runs. Others may be loading to take its place, which it
    /// keeps until they have loaded.
    Running(S, Option<Load<L>>),
    /// A game's thread has stopped after an error. Its last frame still
    /// shows, and it is still the window's game until it is closed or
    /// another starts.
    Stopped(S, Option<Load<L>>),
}

impl<S, L> Game<S, L> {
    /// The window's game, running or stopped after an error.
    pub(super) fn session(&self) -> Option<&S> {
        match self {
            Self::Running(session, _) | Self::Stopped(session, _) => Some(session),
            Self::Idle | Self::Loading(_) => None,
        }
    }

    /// The window's game, running or stopped after an error.
    pub(super) fn session_mut(&mut self) -> Option<&mut S> {
        match self {
            Self::Running(session, _) | Self::Stopped(session, _) => Some(session),
            Self::Idle | Self::Loading(_) => None,
        }
    }

    /// The games loading now, and what loads them.
    pub(super) fn loading(&self) -> Option<(&[PathBuf], &L)> {
        let load = match self {
            Self::Loading(load) => Some(load),
            Self::Running(_, load) | Self::Stopped(_, load) => load.as_ref(),
            Self::Idle => None,
        };
        load.map(|load| (load.games.as_slice(), &load.task))
    }

    /// Whether what is loading now is exactly `games`; for no games,
    /// whether nothing is.
    pub(super) fn loads(&self, games: &[PathBuf]) -> bool {
        self.loading().map_or(&[][..], |(loading, _)| loading) == games
    }

    /// `games` start loading with `task`, in place of any that were: that
    /// load is given up. A game that runs keeps running.
    pub(super) fn load(&mut self, games: Vec<PathBuf>, task: L) {
        let load = Load { games, task };
        *self = match std::mem::replace(self, Self::Idle) {
            Self::Idle | Self::Loading(_) => Self::Loading(load),
            Self::Running(session, _) => Self::Running(session, Some(load)),
            Self::Stopped(session, _) => Self::Stopped(session, Some(load)),
        };
    }

    /// The load under way has finished, with a game to start or without.
    pub(super) fn loaded(&mut self) {
        *self = match std::mem::replace(self, Self::Idle) {
            Self::Idle | Self::Loading(_) => Self::Idle,
            Self::Running(session, _) => Self::Running(session, None),
            Self::Stopped(session, _) => Self::Stopped(session, None),
        };
    }

    /// `session` is the window's game from now on. The one before, if any,
    /// was taken out with [`Game::end`] and stopped first.
    pub(super) fn run(&mut self, session: S) {
        *self = match std::mem::replace(self, Self::Idle) {
            Self::Idle => Self::Running(session, None),
            Self::Loading(load) => Self::Running(session, Some(load)),
            Self::Running(_, load) | Self::Stopped(_, load) => Self::Running(session, load),
        };
    }

    /// The running game's thread has stopped after an error.
    pub(super) fn halted(&mut self) {
        *self = match std::mem::replace(self, Self::Idle) {
            Self::Running(session, load) => Self::Stopped(session, load),
            other => other,
        };
    }

    /// Takes the window's game out, to stop it. What is loading goes on.
    pub(super) fn end(&mut self) -> Option<S> {
        let (session, next) = match std::mem::replace(self, Self::Idle) {
            Self::Running(session, load) | Self::Stopped(session, load) => {
                (Some(session), load.map_or(Self::Idle, Self::Loading))
            }
            other => (None, other),
        };
        *self = next;
        session
    }
}

impl EmulatorView {
    /// Loads the ROMs off the UI thread, then starts them: one game, a DS
    /// game with a GBA game in its slot, or two games joined by a link
    /// cable, or an RPG Maker game from its folder.
    pub(super) fn open(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths
            .iter()
            .any(|path| archive::is_archive(path) || romhack::is_patch(path))
        {
            self.prepare_and_open(paths, window, cx);
            return;
        }
        // Where 3DS games can't play yet, say so at once: not after a
        // moment of "Downloading the 3DS core…", nor once the running game
        // has stopped for it.
        if let Some(why) = refused(&paths) {
            self.show_notice(why, cx);
            return;
        }
        // A game opened from the command line is the same game, in the
        // library too, whatever folder it was opened from.
        let paths: Vec<PathBuf> = paths.iter().map(|path| library::absolute(path)).collect();
        // A second click while the game loads would start it twice.
        if self.game.loads(&paths) {
            return;
        }
        let stops_first = self.game.session().is_some_and(|session| {
            stops_before_loading(session.info.system, &session.files, &paths)
        });
        if stops_first {
            self.save_checkpoint();
            let stopped = self.stop(window, cx);
            self.keep_for_start(stopped);
        }
        let name = paths
            .iter()
            .map(|path| file_name(path))
            .collect::<Vec<_>>()
            .join(" + ");
        self.show_notice(loading_notice(&paths, &name), cx);
        let dmg_colors = self.dmg_colors;
        let games = paths.clone();
        let open = cx
            .background_executor()
            .spawn(async move { emulator::open(&paths, dmg_colors) });
        let loading = cx.spawn_in(window, async move |this, cx| {
            let opened = open.await;
            this.update_in(cx, |this, window, cx| {
                this.game.loaded();
                match opened {
                    Ok(opened) => this.start(opened, &name, window, cx),
                    // Long enough to read why, as for a game another copy
                    // of the app has open.
                    Err(error) => this.show_warning(format!("{error:#}"), cx),
                }
            })
            .ok();
        });
        self.game.load(games, loading);
    }

    /// Unpacks the archives among `paths` and makes the game of a ROM-hack
    /// patch among them, off the UI thread, then opens the games in their
    /// place (see [`prepare`]). Meanwhile the library shows the archive, or
    /// else the patch, as a game being added. A patch whose game is not
    /// found asks for it.
    pub(super) fn prepare_and_open(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let slow = paths
            .iter()
            .find(|path| archive::is_archive(path))
            .or_else(|| paths.iter().find(|path| romhack::is_patch(path)))
            .map_or_else(|| paths[0].clone(), |path| library::absolute(path));
        let (key, progress) = self
            .library
            .update(cx, |library, cx| library.start_adding(&slow, cx));
        let known = self.library.read(cx).checksums();
        let prepare = cx
            .background_executor()
            .spawn(async move { prepare(paths, &known, &progress) });
        cx.spawn_in(window, async move |this, cx| {
            let prepared = prepare.await;
            this.update_in(cx, |this, window, cx| {
                this.library
                    .update(cx, |library, cx| library.added(key, cx));
                match prepared {
                    Ok(paths) => this.open(paths, window, cx),
                    Err(MakeError::NoBase(patch)) => this.choose_base(patch, window, cx),
                    Err(MakeError::Failed(error)) => this.show_notice(error, cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// Asks for the game the patch at `patch` is for, then makes the
    /// patched game from it and opens that.
    pub(super) fn choose_base(
        &mut self,
        patch: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_notice(format!("Choose the game {} patches", file_name(&patch)), cx);
        self.prompt_for_file("Patch", window, cx, move |this, base, window, cx| {
            this.prepare_and_open(vec![base, patch], window, cx);
        });
    }

    /// Starts the game `opened`, called `name`, in place of any running.
    pub(super) fn start(
        &mut self,
        opened: Opened,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Opened {
            loaded,
            unwritable,
            locks,
        } = opened;
        // Stop the previous game first so its battery save is flushed.
        self.save_checkpoint();
        let stopped = self.stop(window, cx);
        let info = loaded.info();
        let (audio_output, audio_sink, no_sound) = match audio::open(info.audio_sample_rate) {
            Ok((output, sink)) => (Some(output), Some(sink), None),
            Err(error) => (None, None, Some(format!("No sound: {error:#}"))),
        };
        let (events, receiver) = async_channel::unbounded();
        let rom_paths = loaded.rom_paths().to_vec();
        let played = rom_paths.clone();
        self.library
            .update(cx, |library, cx| library.add(played, cx));
        let player_rom_paths = loaded.player_rom_paths().to_vec();
        let files = loaded.files().to_vec();
        let states = loaded.states();
        let own_saves = loaded.own_saves();
        let emulator = match Emulator::start(loaded, events, audio_sink) {
            Ok(emulator) => emulator,
            Err(error) => {
                // No game runs now: the one before has stopped.
                let notes = [Some(format!("Could not start {name}: {error}")), stopped];
                let notice: Vec<String> = notes.into_iter().flatten().collect();
                self.show_warning(notice.join(" · "), cx);
                window.set_window_title("Imago Emulator");
                self.library.update(cx, |library, cx| library.refresh(cx));
                return;
            }
        };
        // As the window lays the game out now; drawing it corrects this if
        // its screens come out another size.
        let viewport = window.viewport_size();
        let placements = self.shown_layout().place_consoles(
            info.screens,
            consoles(&info),
            (f32::from(viewport.width), f32::from(viewport.height)),
        );
        let render_scale = render_scale(
            self.resolution_3d,
            &info,
            &placements,
            window.scale_factor(),
        );
        emulator.send(Command::SetRenderScale(render_scale));
        emulator.send(Command::SetTextureUpscaling(
            self.picture.texture_upscaling(),
        ));
        // Pick up where the player left off, unless a battery save has
        // changed since: the emulation thread checks.
        emulator.send(Command::LoadCheckpoint(CHECKPOINT));
        let stranded: Vec<String> = rom_paths
            .iter()
            .zip(&files)
            .filter_map(|(rom, files)| {
                let plain = persist::stranded_save(rom, files)?;
                Some(format!(
                    "{} is not loaded, as it may be another game's of the same name; rename it {} to play on from it",
                    file_name(&plain),
                    file_name(&files.backup()),
                ))
            })
            .collect();
        let task = cx.spawn({
            let receiver = receiver.clone();
            async move |this, cx| {
                while let Ok(event) = receiver.recv().await {
                    if this.update(cx, |this, cx| this.handle(event, cx)).is_err() {
                        break;
                    }
                }
            }
        });
        self.game.run(Session {
            _events: task,
            events: receiver,
            emulator,
            info,
            rom_paths,
            player_rom_paths,
            files,
            states,
            own_saves,
            frames: Vec::new(),
            sequence: 0,
            cheat_texts: vec![None; info.players.max(1)],
            bindings: Vec::new(),
            correction: Correction::for_system(info.system),
            recording: false,
            images: Vec::new(),
            upscaling: Vec::new(),
            render_scale,
            keeping: Keeping::new(render_scale),
            ambient: None,
            display: None,
            taken_at: None,
            fade_wake: None,
            play: library::PlayClock::default(),
            _locks: locks,
            _audio: audio_output,
        });
        // The new emulator starts with nothing held; resend what is.
        self.sent = HeldState::default();
        self.sent_touch = None;
        self.stylus = None;
        self.cursor = None;
        self.active_player = 0;
        let bindings = self.reload_bindings();
        self.sync_held();
        self.sessions += 1;
        let game = self.guide_game(cx);
        self.guide.update(cx, |guide, cx| guide.set_game(game, cx));
        if info.players > 1 {
            // Linked consoles sit side by side; a DS's own two screens
            // keep the chosen layout.
            if consoles(&info) == 1 {
                self.linked_side_by_side = true;
            }
            window.set_window_title(&format!("{name} — {} link", info.system.name()));
        } else {
            window.set_window_title(&format!("{name} — {}", info.system.name()));
        }
        let running = format!("Running {name}");
        // Left on last time, so worth saying.
        let pace = if self.controls.fast_forward_latched() {
            Some(format!(
                "fast-forward on ({})",
                keys::shortcut(shortcut::FAST_FORWARD)
            ))
        } else if self.controls.slow_motion_latched() {
            Some(format!(
                "slow motion on ({})",
                keys::shortcut(shortcut::SLOW_MOTION)
            ))
        } else {
            None
        };
        let notes = [
            Some(running),
            no_sound,
            pace,
            bindings,
            self.reload_cheats(),
            (!stranded.is_empty()).then(|| stranded.join(" · ")),
            stopped,
            self.startup_notice.take(),
        ];
        let notice: Vec<String> = notes.into_iter().flatten().collect();
        match unwritable {
            // First, and long enough to read: nothing of this game will
            // be saved.
            Some(why) => self.show_warning(format!("{why} · {}", notice.join(" · ")), cx),
            None => self.show_notice(notice.join(" · "), cx),
        }
        if let Some(script) = self.startup_script.take() {
            self.run_script_file(script, cx);
        }
    }

    /// Stops the game, if one runs, and closes its menu. Returns what the
    /// emulation thread said as it stopped, such as a battery save it
    /// could not write, for the caller to show.
    pub(super) fn stop(&mut self, window: &mut Window, cx: &mut App) -> Option<String> {
        // A preview still loading belongs to the game stopping.
        self.slot_preview_task = None;
        self.linked_side_by_side = false;
        self.menu = None;
        self.record_play_time(cx);
        if self.guide.read(cx).composer_focused(window, cx) {
            window.focus(&self.focus);
        }
        self.guide.update(cx, |guide, cx| guide.set_game(None, cx));
        let mut session = self.game.end()?;
        let images = session.images.drain(..).flatten();
        let ambient = session.ambient.take().map(|glow| glow.image);
        for image in images.map(|(_, image)| image).chain(ambient) {
            window.drop_image(image).ok();
        }
        let said: Vec<String> = session
            .finish()
            .into_iter()
            .filter_map(checkpoint_notice)
            .collect();
        (!said.is_empty()).then(|| said.join(" · "))
    }

    /// Keeps `note` to show with the next game's start, after whatever
    /// shows meanwhile.
    pub(super) fn keep_for_start(&mut self, note: Option<String>) {
        let notes = [self.startup_notice.take(), note];
        let notes: Vec<String> = notes.into_iter().flatten().collect();
        self.startup_notice = (!notes.is_empty()).then(|| notes.join(" · "));
    }

    /// Erases the active player's progress, the session's checkpoint and
    /// the battery save of the game the menu names, and starts the session
    /// again. Other cartridges, such as a GBA game in a DS's slot or the
    /// other player's game, keep theirs. The battery save is kept as
    /// `game.sav.bak`, or `.bak2` and so on, never replacing an earlier
    /// one, in case it was a mistake; so are the saves an RPG Maker or 3DS
    /// game makes itself (see [`emulator::erase_progress`]).
    pub(super) fn start_over(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.game.session() else {
            return;
        };
        let roms = session.rom_paths.clone();
        let save = session.player_files_of(self.active_player).backup();
        let states = session.states.clone();
        let own_saves = session.own_saves.clone();
        // Stopping writes the battery saves, and stops the player or helper
        // a game's own saves are open in, so they are set aside after.
        let stopped = self.stop(window, cx);
        self.keep_for_start(stopped);
        // Said once the game is running again, after the loading notice.
        let said = match emulator::erase_progress(&states, &save, &own_saves) {
            Ok(said) => said,
            Err(error) => Some(format!("Cannot erase progress: {error}")),
        };
        self.keep_for_start(said);
        self.open(roms, window, cx);
        cx.notify();
    }

    /// Stops the game, keeping its progress as its checkpoint, and goes
    /// back to the library.
    pub(super) fn close_game(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.game.session().is_none() {
            return;
        }
        self.save_checkpoint();
        if let Some(said) = self.stop(window, cx) {
            self.show_notice(said, cx);
        }
        // The game just left goes first, with where it was left.
        self.library.update(cx, |library, cx| library.refresh(cx));
        window.set_window_title("Imago Emulator");
        cx.notify();
    }

    /// Restarts the running game linked to a second one: by link cable
    /// for the Game Boy and Game Boy Advance, over local wireless for the
    /// DS.
    pub(super) fn connect_link_cable(
        &mut self,
        _: &ConnectLinkCable,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let first = self.game.session().and_then(|session| {
            let single = session.info.players == 1;
            single.then(|| session.rom_paths[0].clone())
        });
        let Some(first) = first else {
            self.show_notice("Open a game first, then connect a second console", cx);
            return;
        };
        self.prompt_for_file("Connect", window, cx, move |this, second, window, cx| {
            this.open(vec![first, second], window, cx);
        });
    }

    /// Restarts the running DS game with a GBA game in its GBA slot,
    /// replacing any already there.
    pub(super) fn insert_gba_cartridge(
        &mut self,
        _: &InsertGbaCartridge,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let game = self
            .game
            .session()
            .filter(|session| has_gba_slot(session.info.system))
            .map(|session| session.rom_paths[0].clone());
        let Some(game) = game else {
            self.show_notice("Open a DS game first, then insert a GBA cartridge", cx);
            return;
        };
        self.prompt_for_file("Insert", window, cx, move |this, cartridge, window, cx| {
            this.open(vec![game, cartridge], window, cx);
        });
    }

    pub(super) fn drop_files(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(path) = paths.paths().first() {
            self.open(vec![path.clone()], window, cx);
        }
    }
}

/// Whether the running game, one for `system` whose cartridges have
/// `files`, must stop before the games at `paths` load, rather than once
/// they have loaded.
///
/// Stopping writes the running game's battery saves, and reopening one of
/// its games (to link it or insert a cartridge) must read those, not the
/// copy on disk from up to a second ago. And a 3DS or RPG Maker game plays
/// in a process of its own, on files only one process may have open: the
/// emulated 3DS's NAND and SD card, a game's player. So when the running
/// game or a new one is one of those, the old process has gone before the
/// new one starts.
fn stops_before_loading(system: System, files: &[GameFiles], paths: &[PathBuf]) -> bool {
    // Named as a session would name them: an RPG Maker game's after its
    // whole folder, dots and all.
    let saves: Vec<String> = paths
        .iter()
        .map(|path| persist::backup_key(&emulator::files_of(path).backup()))
        .collect();
    let reopening = files
        .iter()
        .any(|files| saves.contains(&persist::backup_key(&files.backup())));
    let apart = matches!(system, System::N3ds | System::RpgMaker)
        || paths.iter().any(|path| {
            emu_3ds::is_game(path) || emu_cores::rpg_maker::game_folder(path).is_some()
        });
    reopening || apart
}

/// The games to open for `paths`, unpacking archives and making the game
/// of a patch among them (see [`romhack::pair`]), from the one ROM with
/// it or else the game among `known` (checksum and path) it names. Tells
/// `progress` how far unpacking has got.
///
/// Blocking file I/O; call it off the UI thread.
fn prepare(
    paths: Vec<PathBuf>,
    known: &[(u32, PathBuf)],
    progress: &archive::Progress,
) -> Result<Vec<PathBuf>, MakeError> {
    let files = paths
        .into_iter()
        .map(|path| {
            if archive::is_archive(&path) {
                archive::unpack(&path, progress)
            } else {
                Ok(vec![path])
            }
        })
        .collect::<Result<Vec<_>, String>>()
        .map_err(MakeError::Failed)?;
    match romhack::pair(files).map_err(MakeError::Failed)? {
        (roms, None) => Ok(roms),
        (roms, Some(patch)) => {
            let base = roms.first().map(PathBuf::as_path);
            Ok(vec![romhack::make(&patch, base, known)?])
        }
    }
}

/// Why the games at `paths` can't be played on this system, if they
/// can't: 3DS games, until 3DS games play here (`emu_3ds::UNSUPPORTED`).
fn refused(paths: &[PathBuf]) -> Option<&'static str> {
    emu_3ds::UNSUPPORTED.filter(|_| paths.iter().any(|path| emu_3ds::is_game(path)))
}

/// What to say while the games at `paths`, called `name`, load: that the
/// 3DS core, or off macOS mkxp-z for an RPG Maker game, is being downloaded
/// first, the first time one is played, as that takes a while.
fn loading_notice(paths: &[PathBuf], name: &str) -> String {
    let downloading = matches!(paths, [path] if emu_3ds::is_game(path))
        && emu_3ds::data_dir().is_some_and(|data| emu_3ds::install::installed(&data).is_none());
    let rpg_maker = emu_cores::rpg_maker::data_dir().is_some_and(
        |work| matches!(paths, [path] if emu_cores::rpg_maker::downloads_player(path, &work)),
    );
    if downloading {
        format!(
            "Downloading the 3DS core, about {} MB, the first time only, then loading {name}…",
            emu_3ds::install::DOWNLOAD_MB
        )
    } else if rpg_maker {
        format!(
            "Downloading mkxp-z, the RPG Maker player, about {} MB, the first time only, then loading {name}…",
            emu_cores::rpg_maker::install::DOWNLOAD_MB
        )
    } else {
        format!("Loading {name}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window's part of opening games, with stand-ins: a session is
    /// its game's name, a load is its number, and what would stop or load
    /// a game is written in `log` instead. Its methods follow
    /// [`EmulatorView::open`], the end of its load, and
    /// [`EmulatorView::start`], step for step.
    #[derive(Debug)]
    struct Window {
        game: Game<String, u32>,
        loads: u32,
        log: Vec<String>,
    }

    impl Window {
        fn new() -> Self {
            Self {
                game: Game::Idle,
                loads: 0,
                log: Vec::new(),
            }
        }

        /// Opens `games`, stopping the running game first if `stops_first`
        /// (see [`stops_before_loading`]). Returns the load's number, or
        /// `None` if the games are loading already.
        fn open(&mut self, games: &[&str], stops_first: bool) -> Option<u32> {
            let games: Vec<PathBuf> = games.iter().map(PathBuf::from).collect();
            if self.game.loads(&games) {
                return None;
            }
            if stops_first && self.game.session().is_some() {
                self.stop();
            }
            self.loads += 1;
            self.log.push(format!("load {}", self.loads));
            self.game.load(games, self.loads);
            Some(self.loads)
        }

        /// The loader finishes load `load`, with the game `loaded` or
        /// without. A load given up meanwhile comes to nothing, as its
        /// task was dropped.
        fn finish(&mut self, load: u32, loaded: Option<&str>) {
            if self.game.loading().map(|(_, task)| *task) != Some(load) {
                self.log.push(format!("load {load} was given up"));
                return;
            }
            self.game.loaded();
            match loaded {
                Some(name) => {
                    self.stop();
                    self.log.push(format!("run {name}"));
                    self.game.run(name.to_owned());
                }
                None => self.log.push(format!("load {load} failed")),
            }
        }

        fn stop(&mut self) {
            if let Some(session) = self.game.end() {
                self.log.push(format!("stop {session}"));
            }
        }

        /// What happened since last asked.
        fn took(&mut self) -> Vec<String> {
            std::mem::take(&mut self.log)
        }
    }

    #[test]
    fn a_game_opens_loads_and_runs() {
        let mut window = Window::new();
        assert!(window.game.loads(&[]), "nothing loads");
        let load = window.open(&["ruby.gba"], false).expect("loads");
        assert!(matches!(window.game, Game::Loading(_)));
        assert!(window.game.session().is_none());
        // A second click while it loads does not start it twice.
        assert_eq!(window.open(&["ruby.gba"], false), None);
        window.finish(load, Some("ruby"));
        assert!(matches!(window.game, Game::Running(_, None)));
        assert_eq!(window.game.session(), Some(&"ruby".to_owned()));
        assert!(window.game.loads(&[]));
        assert_eq!(window.took(), ["load 1", "run ruby"]);
    }

    #[test]
    fn the_running_game_plays_on_until_the_next_has_loaded() {
        let mut window = Window::new();
        let load = window.open(&["ruby.gba"], false).expect("loads");
        window.finish(load, Some("ruby"));
        window.took();

        let load = window.open(&["emerald.gba"], false).expect("loads");
        assert!(matches!(window.game, Game::Running(_, Some(_))));
        assert_eq!(window.game.session(), Some(&"ruby".to_owned()));
        window.finish(load, Some("emerald"));
        assert_eq!(window.took(), ["load 2", "stop ruby", "run emerald"]);
        assert_eq!(window.game.session(), Some(&"emerald".to_owned()));

        // A file that fails to load costs the running game nothing.
        let load = window.open(&["notes.txt"], false).expect("loads");
        window.finish(load, None);
        assert_eq!(window.took(), ["load 3", "load 3 failed"]);
        assert!(matches!(window.game, Game::Running(_, None)));
        assert_eq!(window.game.session(), Some(&"emerald".to_owned()));
    }

    #[test]
    fn a_game_that_must_stop_first_has_stopped_before_the_next_loads() {
        let mut window = Window::new();
        let load = window.open(&["omega.3ds"], false).expect("loads");
        window.finish(load, Some("omega"));
        window.took();

        let load = window.open(&["alpha.3ds"], true).expect("loads");
        assert!(matches!(window.game, Game::Loading(_)));
        assert_eq!(window.took(), ["stop omega", "load 2"]);
        window.finish(load, Some("alpha"));
        assert_eq!(window.took(), ["run alpha"]);

        // Its load failing leaves no game running.
        let load = window.open(&["broken.3ds"], true).expect("loads");
        window.finish(load, None);
        assert_eq!(window.took(), ["stop alpha", "load 3", "load 3 failed"]);
        assert!(matches!(window.game, Game::Idle));
    }

    #[test]
    fn opening_other_games_gives_up_the_load_under_way() {
        let mut window = Window::new();
        let first = window.open(&["ruby.gba"], false).expect("loads");
        let second = window.open(&["emerald.gba"], false).expect("loads");
        assert!(window.game.loads(&[PathBuf::from("emerald.gba")]));
        window.finish(first, Some("ruby"));
        assert!(window.game.session().is_none());
        window.finish(second, Some("emerald"));
        assert_eq!(
            window.took(),
            ["load 1", "load 2", "load 1 was given up", "run emerald"]
        );
        // The same games again, once they run, load anew.
        assert!(window.open(&["emerald.gba"], true).is_some());
        assert_eq!(window.took(), ["stop emerald", "load 3"]);
    }

    #[test]
    fn a_game_stopped_after_an_error_is_the_window_s_until_it_is_closed() {
        let mut window = Window::new();
        let load = window.open(&["ruby.gba"], false).expect("loads");
        window.finish(load, Some("ruby"));
        window.game.halted();
        assert!(matches!(window.game, Game::Stopped(_, None)));
        assert_eq!(window.game.session(), Some(&"ruby".to_owned()));
        // Another game can load behind it, and takes its place.
        let load = window.open(&["emerald.gba"], false).expect("loads");
        assert!(matches!(window.game, Game::Stopped(_, Some(_))));
        window.finish(load, Some("emerald"));
        assert!(matches!(window.game, Game::Running(_, None)));
        window.took();

        // Closing takes the game out, and a load under way goes on.
        window.game.halted();
        let load = window.open(&["sapphire.gba"], false).expect("loads");
        window.stop();
        assert!(matches!(window.game, Game::Loading(_)));
        assert_eq!(window.game.end(), None, "none to stop twice");
        window.finish(load, Some("sapphire"));
        assert_eq!(window.took(), ["load 3", "stop emerald", "run sapphire"]);
        // Halting changes nothing while no game runs.
        let mut idle: Game<String, u32> = Game::Idle;
        idle.halted();
        assert!(matches!(idle, Game::Idle));
    }

    #[test]
    fn a_game_that_plays_in_another_process_never_starts_beside_the_running_one() {
        let dir = std::env::temp_dir().join(format!("emu-app-stops-first-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        // An RPG Maker game: a folder with its Game.ini and a player.
        let fan_game = dir.join("Fan Game");
        fs::create_dir_all(fan_game.join("Player.app")).expect("the player's folder");
        fs::write(fan_game.join("Game.ini"), b"Title=Fan Game").expect("write");
        fs::write(fan_game.join("Game.exe"), b"player").expect("write");
        let files = |name: &str| vec![GameFiles::of(&dir.join(name))];
        let paths = |name: &str| vec![dir.join(name)];

        // A game on one of the app's own cores plays on until another has
        // loaded, so a file that fails to load costs nothing.
        assert!(!stops_before_loading(
            System::Gba,
            &files("ruby.gba"),
            &paths("emerald.gba")
        ));
        // Reopened, as when it is linked or given a cartridge, it stops
        // first, so its battery save is on disk to read.
        assert!(stops_before_loading(
            System::Gba,
            &files("ruby.gba"),
            &[dir.join("ruby.gba"), dir.join("sapphire.gba")]
        ));
        // Two 3DS games would share the emulated console's SD card.
        assert!(stops_before_loading(
            System::N3ds,
            &files("omega.3ds"),
            &paths("alpha.3ds")
        ));
        // Whichever of the two plays in another process.
        assert!(stops_before_loading(
            System::N3ds,
            &files("omega.3ds"),
            &paths("emerald.gba")
        ));
        assert!(stops_before_loading(
            System::Gba,
            &files("ruby.gba"),
            &paths("alpha.cia")
        ));
        assert!(stops_before_loading(
            System::RpgMaker,
            &[GameFiles::of_folder(&fan_game)],
            &paths("emerald.gba")
        ));
        assert!(stops_before_loading(
            System::Gba,
            &files("ruby.gba"),
            std::slice::from_ref(&fan_game)
        ));
        fs::remove_dir_all(&dir).expect("clean up");
    }
}
