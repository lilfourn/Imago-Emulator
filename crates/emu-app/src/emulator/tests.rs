//! Tests of the emulation thread, with cores made for them.

use std::path::PathBuf;
use std::sync::atomic::AtomicU8;
use std::time::Duration;

use emu_core::{
    Button, Buttons, CoreInfo, FrameRate, RomPatch, ScreenSize, StateError, StereoSample, System,
    TouchPoint,
};

use emu_core::Debugger;
use emu_core::debug::{CpuInfo, Instruction, Register, Stops};

use super::*;
use crate::latch::{BUTTON_FRAMES, TOUCH_FRAMES};

const SCREENS: [ScreenSize; 1] = [ScreenSize::new(1, 1)];

/// A core whose single pixel is its frame counter.
struct Counter {
    frames: [Frame; 1],
}

impl Counter {
    fn count(&self) -> u32 {
        self.frames[0].pixels()[0]
    }

    fn set(&mut self, count: u32) {
        self.frames[0].pixels_mut()[0] = count;
    }
}

impl Core for Counter {
    fn info(&self) -> CoreInfo {
        CoreInfo {
            system: System::Gb,
            screens: &SCREENS,
            touch_screen: None,
            players: 1,
            // 1000 frames per second keeps the test fast.
            frame_rate: FrameRate::new(1000, 1),
            audio_sample_rate: 48_000,
            state_version: 1,
        }
    }

    fn run_frame(&mut self, _: &[Input], _: &mut Vec<StereoSample>) {
        self.set(self.count() + 1);
    }

    fn screens(&self) -> &[Frame] {
        &self.frames
    }

    fn reset(&mut self) {
        self.set(0);
    }

    fn save_state(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.count().to_le_bytes());
    }

    fn load_state(&mut self, state: &[u8]) -> Result<(), StateError> {
        let bytes = state.try_into().map_err(|_| StateError::Truncated)?;
        self.set(u32::from_le_bytes(bytes));
        Ok(())
    }

    /// The counter is the only memory, at addresses 0-3.
    fn peek(&self, _: usize, address: u32) -> Option<u8> {
        self.count().to_le_bytes().get(address as usize).copied()
    }

    fn poke(&mut self, _: usize, address: u32, value: u8) -> bool {
        let mut bytes = self.count().to_le_bytes();
        let Some(byte) = bytes.get_mut(address as usize) else {
            return false;
        };
        *byte = value;
        self.set(u32::from_le_bytes(bytes));
        true
    }

    fn debugger(&mut self) -> Option<&mut dyn Debugger> {
        Some(self)
    }
}

/// Gives a test core a debugger with one CPU, whose memory is the
/// core's `peek` and `poke` and which runs frames whole, so tests can
/// watch memory through the debugger's snapshot.
macro_rules! peek_debugger {
    ($core:ty) => {
        impl Debugger for $core {
            fn cpus(&self) -> Vec<CpuInfo> {
                vec![CpuInfo {
                    name: "TEST",
                    player: 0,
                }]
            }
            fn registers(&self, _: usize) -> Vec<Register> {
                Vec::new()
            }
            fn status(&self, _: usize) -> String {
                String::new()
            }
            fn pc(&self, _: usize) -> u32 {
                0
            }
            fn read(&self, _: usize, address: u32) -> Option<u8> {
                self.peek(0, address)
            }
            fn write(&mut self, _: usize, address: u32, value: u8) -> bool {
                self.poke(0, address, value)
            }
            fn disassemble(&self, _: usize, address: u32) -> Instruction {
                Instruction {
                    address,
                    bytes: vec![0],
                    text: String::new(),
                    call: false,
                }
            }
            fn run_until(
                &mut self,
                inputs: &[Input],
                audio: &mut Vec<StereoSample>,
                _: &Stops,
            ) -> RunOutcome {
                self.run_frame(inputs, audio);
                RunOutcome {
                    frame_done: true,
                    stop: None,
                }
            }
        }
    };
}

peek_debugger!(Counter);
peek_debugger!(Keypad);

/// Opens the debugger's view, with its memory from `address`.
fn watch_memory(emulator: &Emulator, address: u32) {
    let view = debugger::View {
        memory: address,
        ..debugger::View::default()
    };
    emulator.send(Command::Debug(debugger::Command::View(Some(view))));
}

/// Waits for the debugger's snapshot to pass `test`.
fn wait_for(emulator: &Emulator, test: impl Fn(&Snapshot) -> bool) -> Arc<Snapshot> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(snapshot) = emulator.debug_snapshot().filter(|shown| test(shown)) {
            return snapshot;
        }
        assert!(Instant::now() < deadline, "the debugger never showed it");
        thread::sleep(Duration::from_millis(1));
    }
}

fn start(rom_path: PathBuf) -> (Emulator, async_channel::Receiver<Event>) {
    let core = Box::new(Counter {
        frames: [Frame::new(SCREENS[0])],
    });
    let loaded = Loaded {
        core,
        files: vec![GameFiles::of(&rom_path)],
        rom_paths: vec![rom_path],
        rom_crc32: 0,
        live_sound: None,
        own_saves: OwnSaves::None,
    };
    let (events, receiver) = async_channel::unbounded();
    (
        Emulator::start(loaded, events, None).expect("the threads start"),
        receiver,
    )
}

/// The count in the latest frame, read where it is published, as no
/// window takes frames in these tests.
fn latest_count(emulator: &Emulator) -> u32 {
    let latest = emulator.latest.frames.lock().expect("not poisoned");
    latest.1[0].pixels()[0]
}

#[test]
fn taken_frames_are_the_latest_and_their_buffers_come_back() {
    let (emulator, _events) = start(PathBuf::from("/nonexistent/taken.gb"));
    let mut frames = Vec::new();
    let (mut seen, mut count) = (0, 0);
    // Taken as a window does, while the frames run.
    for _ in 0..20 {
        let sequence = loop {
            if let Some(sequence) = emulator.take_latest(seen, &mut frames) {
                break sequence;
            }
            thread::sleep(Duration::from_millis(1));
        };
        assert!(sequence > seen);
        assert!(frames[0].pixels()[0] > count, "a newer frame");
        (seen, count) = (sequence, frames[0].pixels()[0]);
    }
    emulator.send(Command::TogglePause);
    thread::sleep(Duration::from_millis(20));
    let seen = emulator.take_latest(seen, &mut frames).unwrap_or(seen);
    let paused = frames[0].pixels()[0];
    assert_eq!(emulator.take_latest(seen, &mut frames), None);
    assert_eq!(frames[0].pixels()[0], paused, "kept while none is newer");
}

#[test]
fn copied_screens_match_whatever_the_buffers_held() {
    let filled = |width, height, value| {
        let mut frame = Frame::new(ScreenSize::new(width, height));
        frame.pixels_mut().fill(value);
        frame
    };
    let shapes: [&[(u16, u16)]; 5] = [
        &[],
        &[(4, 3)],
        &[(4, 3), (4, 3)],
        &[(8, 6), (4, 3)],
        &[(4, 3), (4, 3), (2, 2)],
    ];
    for (from, screens_shape) in (0..).zip(shapes) {
        for frames_shape in shapes {
            let screens: Vec<Frame> = screens_shape
                .iter()
                .map(|&(width, height)| filled(width, height, from))
                .collect();
            let mut frames: Vec<Frame> = frames_shape
                .iter()
                .map(|&(width, height)| filled(width, height, 99))
                .collect();
            copy_screens(&mut frames, &screens);
            assert_eq!(frames, screens, "{frames_shape:?} -> {screens_shape:?}");
        }
    }
}

/// Every frame's inputs, one per player, as a core saw them.
type Seen = Arc<Mutex<Vec<Vec<Input>>>>;

/// A two-player core that records every frame's inputs.
struct Recorder {
    seen: Seen,
    frames: [Frame; 1],
}

impl Core for Recorder {
    fn info(&self) -> CoreInfo {
        CoreInfo {
            system: System::Gba,
            screens: &SCREENS,
            touch_screen: None,
            players: 2,
            frame_rate: FrameRate::new(1000, 1),
            audio_sample_rate: 48_000,
            state_version: 1,
        }
    }

    fn run_frame(&mut self, inputs: &[Input], _: &mut Vec<StereoSample>) {
        self.seen.lock().expect("unpoisoned").push(inputs.to_vec());
    }

    fn screens(&self) -> &[Frame] {
        &self.frames
    }

    fn reset(&mut self) {}

    fn save_state(&self, _: &mut Vec<u8>) {}

    fn load_state(&mut self, _: &[u8]) -> Result<(), StateError> {
        Ok(())
    }
}

fn start_recorder() -> (Emulator, Seen) {
    let seen = Seen::default();
    let loaded = Loaded {
        core: Box::new(Recorder {
            seen: Arc::clone(&seen),
            frames: [Frame::new(SCREENS[0])],
        }),
        files: vec![GameFiles::of(&PathBuf::from("/nonexistent/recorder.gba"))],
        rom_paths: vec![PathBuf::from("/nonexistent/recorder.gba")],
        rom_crc32: 0,
        live_sound: None,
        own_saves: OwnSaves::None,
    };
    let (events, _) = async_channel::unbounded();
    (
        Emulator::start(loaded, events, None).expect("the threads start"),
        seen,
    )
}

/// Waits for the emulator to run `count` more frames.
fn run_frames(seen: &Seen, count: usize) {
    let target = seen.lock().expect("unpoisoned").len() + count;
    let deadline = Instant::now() + Duration::from_secs(5);
    while seen.lock().expect("unpoisoned").len() < target {
        assert!(Instant::now() < deadline, "frames stopped running");
        thread::sleep(Duration::from_millis(1));
    }
}

/// The number of recorded frames in which `player`'s input passed
/// `test`.
fn frames_where(seen: &Seen, player: usize, test: impl Fn(&Input) -> bool) -> usize {
    let seen = seen.lock().expect("unpoisoned");
    seen.iter().filter(|inputs| test(&inputs[player])).count()
}

const TAP: Input = Input {
    buttons: Buttons::NONE,
    touch: Some(TouchPoint { x: 10, y: 20 }),
    microphone: 0,
    gentle: false,
};

#[test]
fn a_press_and_release_between_two_frames_still_reaches_the_game() {
    let (emulator, seen) = start_recorder();
    let a = Input {
        buttons: [Button::A].into_iter().collect(),
        ..Input::default()
    };
    // Nothing runs while paused, so every press and release below
    // arrives between the same two frames, as a trackpad's tap can.
    emulator.send(Command::TogglePause);
    let release = Input::default();
    for (player, input) in [(0, TAP), (0, release), (1, a), (1, release)] {
        emulator.send(Command::SetInput { player, input });
    }
    emulator.send(Command::TogglePause);
    run_frames(&seen, 10);
    let touched = frames_where(&seen, 0, |input| input.touch.is_some());
    let pressed = frames_where(&seen, 1, |input| input.buttons.contains(Button::A));
    assert_eq!(
        (touched, pressed),
        (usize::from(TOUCH_FRAMES), usize::from(BUTTON_FRAMES))
    );
    // Each player saw only their own.
    assert_eq!(frames_where(&seen, 0, |input| !input.buttons.is_empty()), 0);
    assert_eq!(frames_where(&seen, 1, |input| input.touch.is_some()), 0);
}

#[test]
fn turbo_alternates_frame_by_frame_past_the_latch() {
    let (emulator, seen) = start_recorder();
    let a: Buttons = [Button::A].into_iter().collect();
    emulator.send(Command::SetTurbo {
        player: 1,
        buttons: a,
        frames: 1,
    });
    run_frames(&seen, 10);
    let held_until = seen.lock().expect("unpoisoned").len();
    emulator.send(Command::SetTurbo {
        player: 1,
        buttons: Buttons::NONE,
        frames: 1,
    });
    // A press under way finishes; then nothing.
    run_frames(&seen, 3);
    let released_from = seen.lock().expect("unpoisoned").len();
    run_frames(&seen, 5);
    let seen_now = seen.lock().expect("unpoisoned").clone();
    let pressed: Vec<bool> = seen_now[..held_until]
        .iter()
        .map(|inputs| inputs[1].buttons.contains(Button::A))
        .collect();
    // Frames may run before the command arrives.
    let start = pressed.iter().position(|a| *a).expect("turbo pressed A");
    let turbo = &pressed[start..];
    assert!(
        turbo.windows(2).all(|pair| pair[0] != pair[1]),
        "a one-frame press is not held for the latch's minimum: {turbo:?}"
    );
    let after = &seen_now[released_from..];
    assert!(after.iter().all(|inputs| inputs[1].buttons.is_empty()));
    assert_eq!(frames_where(&seen, 0, |input| !input.buttons.is_empty()), 0);
}

#[test]
fn reset_drops_a_release_still_waiting() {
    let (emulator, seen) = start_recorder();
    emulator.send(Command::TogglePause);
    emulator.send(Command::SetInput {
        player: 0,
        input: TAP,
    });
    emulator.send(Command::SetInput {
        player: 0,
        input: Input::default(),
    });
    emulator.send(Command::Reset);
    emulator.send(Command::TogglePause);
    run_frames(&seen, 10);
    assert_eq!(frames_where(&seen, 0, |input| input.touch.is_some()), 0);
}

#[test]
fn release_input_lets_go_of_one_player_at_once() {
    let (emulator, seen) = start_recorder();
    let held = |buttons: &[Button]| Input {
        buttons: buttons.iter().copied().collect(),
        ..Input::default()
    };
    emulator.send(Command::TogglePause);
    // Player 1 taps A, holds Left and a turbo B; player 0 holds A.
    for input in [held(&[Button::A]), held(&[]), held(&[Button::Left])] {
        emulator.send(Command::SetInput { player: 1, input });
    }
    emulator.send(Command::SetTurbo {
        player: 1,
        buttons: [Button::B].into_iter().collect(),
        frames: 3,
    });
    emulator.send(Command::SetInput {
        player: 0,
        input: held(&[Button::A]),
    });
    emulator.send(Command::ReleaseInput { player: 1 });
    emulator.send(Command::TogglePause);
    run_frames(&seen, 10);
    assert_eq!(
        frames_where(&seen, 1, |input| *input != Input::default()),
        0
    );
    assert!(frames_where(&seen, 0, |input| input.buttons.contains(Button::A)) >= 10);
}

#[test]
fn counts_the_frames_shown() {
    let (emulator, events) = start(PathBuf::from("/nonexistent/counter.gb"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while emulator.frames_shown() < 10 {
        assert!(Instant::now() < deadline, "frames stopped running");
        thread::sleep(Duration::from_millis(1));
    }
    emulator.send(Command::TogglePause);
    wait_for_notice(&events, "Paused");
    // Paused: the count stands still, and matches the game's own,
    // which counts up from opaque black.
    let shown = emulator.frames_shown();
    assert_eq!(u64::from(latest_count(&emulator) & 0x00FF_FFFF), shown);
    thread::sleep(Duration::from_millis(20));
    assert_eq!(emulator.frames_shown(), shown);
}

#[test]
fn rewind_steps_back_through_frames() {
    let (emulator, _events) = start(PathBuf::from("/nonexistent/counter.gb"));
    thread::sleep(Duration::from_millis(200));
    emulator.send(Command::SetRewind(true));
    thread::sleep(Duration::from_millis(20));
    let rewinding = latest_count(&emulator);
    thread::sleep(Duration::from_millis(50));
    let rewound = latest_count(&emulator);
    assert!(rewound < rewinding, "{rewound} should be below {rewinding}");

    emulator.send(Command::SetRewind(false));
    thread::sleep(Duration::from_millis(50));
    assert!(latest_count(&emulator) > rewound);
}

#[test]
fn cheats_run_after_every_frame() {
    let (emulator, _events) = start(PathBuf::from("/nonexistent/counter.gb"));
    let cheats = emu_cheats::parse(System::Gb, "[Hold]\n0000:00\n").expect("valid");
    emulator.send(Command::SetCheats { player: 0, cheats });
    watch_memory(&emulator, 0);
    thread::sleep(Duration::from_millis(50));
    // Each frame adds one; the cheat then zeroes the low byte.
    let snapshot = emulator.debug_snapshot().expect("watching");
    assert_eq!(snapshot.memory[0], Some(0));
}

/// A GBA-like core with KEYINPUT at 0x04000130 and one byte of RAM at
/// 0x02000000, which every frame clears.
struct Keypad {
    keys: u16,
    ram: u8,
    frames: [Frame; 1],
}

impl Core for Keypad {
    fn info(&self) -> CoreInfo {
        CoreInfo {
            system: System::Gba,
            screens: &SCREENS,
            touch_screen: None,
            players: 1,
            frame_rate: FrameRate::new(1000, 1),
            audio_sample_rate: 48_000,
            state_version: 1,
        }
    }

    fn run_frame(&mut self, inputs: &[Input], _: &mut Vec<StereoSample>) {
        let input = inputs.first().copied().unwrap_or_default();
        self.keys = !input.buttons.bits() & 0x03FF;
        self.ram = 0;
    }

    fn screens(&self) -> &[Frame] {
        &self.frames
    }

    fn reset(&mut self) {}

    fn save_state(&self, _: &mut Vec<u8>) {}

    fn load_state(&mut self, _: &[u8]) -> Result<(), StateError> {
        Ok(())
    }

    fn peek(&self, _: usize, address: u32) -> Option<u8> {
        match address {
            0x0200_0000 => Some(self.ram),
            0x0400_0130 => Some(self.keys as u8),
            0x0400_0131 => Some((self.keys >> 8) as u8),
            _ => None,
        }
    }

    fn poke(&mut self, _: usize, address: u32, value: u8) -> bool {
        let ram = address == 0x0200_0000;
        if ram {
            self.ram = value;
        }
        ram
    }

    fn debugger(&mut self) -> Option<&mut dyn Debugger> {
        Some(self)
    }
}

#[test]
fn button_cheats_see_the_frame_s_own_input() {
    let loaded = Loaded {
        core: Box::new(Keypad {
            keys: 0x03FF,
            ram: 0,
            frames: [Frame::new(SCREENS[0])],
        }),
        files: vec![GameFiles::of(&PathBuf::from("/nonexistent/keypad.gba"))],
        rom_paths: vec![PathBuf::from("/nonexistent/keypad.gba")],
        rom_crc32: 0,
        live_sound: None,
        own_saves: OwnSaves::None,
    };
    let (events, _events) = async_channel::unbounded();
    let emulator = Emulator::start(loaded, events, None).expect("the threads start");
    // CodeBreaker: if A is held, write 1.
    let cheats = emu_cheats::parse(System::Gba, "D0000020 0001\n32000000 0001\n").expect("valid");
    emulator.send(Command::SetCheats { player: 0, cheats });
    watch_memory(&emulator, 0x0200_0000);
    emulator.send(Command::SetInput {
        player: 0,
        input: Input {
            buttons: [Button::A].into_iter().collect(),
            ..Input::default()
        },
    });
    thread::sleep(Duration::from_millis(50));
    // The frame cleared the byte; the cheat, seeing A, set it again.
    let snapshot = emulator.debug_snapshot().expect("watching");
    assert_eq!(snapshot.memory[0], Some(1));
}

/// A Game Boy running `program` at 0x0100 on the emulation thread.
fn start_game_boy(program: &[u8]) -> (Emulator, async_channel::Receiver<Event>) {
    let mut rom = vec![0; 0x8000];
    rom[0x100..0x100 + program.len()].copy_from_slice(program);
    rom[0x14D] = rom[0x134..0x14D]
        .iter()
        .fold(0u8, |sum, byte| sum.wrapping_sub(*byte).wrapping_sub(1));
    let core = emu_cores::create(System::Gb, &Media::new(&rom)).expect("valid rom");
    let loaded = Loaded {
        core,
        files: vec![GameFiles::of(&PathBuf::from("/nonexistent/debugged.gb"))],
        rom_paths: vec![PathBuf::from("/nonexistent/debugged.gb")],
        rom_crc32: 0,
        live_sound: None,
        own_saves: OwnSaves::None,
    };
    let (events, receiver) = async_channel::unbounded();
    (
        Emulator::start(loaded, events, None).expect("the threads start"),
        receiver,
    )
}

fn debug(emulator: &Emulator, command: debugger::Command) {
    emulator.send(Command::Debug(command));
}

#[test]
fn breakpoints_pause_mid_frame_and_steps_run_one_instruction() {
    // LD A, 0x42; LD (0xC000), A; INC A; NOP; JR -4 (back to INC A)
    let program = [0x3E, 0x42, 0xEA, 0x00, 0xC0, 0x3C, 0x00, 0x18, 0xFC];
    let (emulator, _events) = start_game_boy(&program);
    watch_memory(&emulator, 0xC000);
    let at_nop = Breakpoint {
        cpu: 0,
        address: 0x0106,
    };
    debug(
        &emulator,
        debugger::Command::SetStops {
            breakpoints: vec![at_nop],
            watchpoints: Vec::new(),
        },
    );
    let stopped = wait_for(&emulator, |shown| {
        matches!(shown.state, RunState::Stopped(_))
    });
    assert_eq!((stopped.pc, stopped.memory[0]), (0x0106, Some(0x42)));
    assert!(stopped.code.iter().any(|line| line.address == 0x0106));
    thread::sleep(Duration::from_millis(20));
    assert_eq!(
        emulator.debug_snapshot().expect("open").pc,
        0x0106,
        "still there"
    );

    debug(&emulator, debugger::Command::Step(0));
    let stepped = wait_for(&emulator, |shown| shown.pc == 0x0107);
    let step = emu_core::debug::Stop {
        cpu: 0,
        reason: StopReason::Step,
    };
    assert_eq!(stepped.state, RunState::Stopped(step));

    debug(
        &emulator,
        debugger::Command::Write {
            cpu: 0,
            address: 0xC000,
            value: 0x99,
        },
    );
    wait_for(&emulator, |shown| shown.memory[0] == Some(0x99));

    // Round the loop to the breakpoint again.
    debug(&emulator, debugger::Command::Continue);
    let again = wait_for(&emulator, |shown| shown.pc == 0x0106);
    assert!(
        matches!(again.state, RunState::Stopped(stop) if stop.reason == StopReason::Breakpoint)
    );
}

#[test]
fn stepping_over_a_call_stops_after_it_returns() {
    // CALL 0x0150; JR -5 (back to the CALL); and at 0x0150, INC A; RET.
    let mut program = vec![0; 0x52];
    program[..5].copy_from_slice(&[0xCD, 0x50, 0x01, 0x18, 0xFB]);
    program[0x50..].copy_from_slice(&[0x3C, 0xC9]);
    let (emulator, _events) = start_game_boy(&program);
    watch_memory(&emulator, 0xC000);
    debug(
        &emulator,
        debugger::Command::SetStops {
            breakpoints: vec![Breakpoint {
                cpu: 0,
                address: 0x0100,
            }],
            watchpoints: Vec::new(),
        },
    );
    wait_for(&emulator, |shown| {
        matches!(shown.state, RunState::Stopped(_))
    });
    debug(&emulator, debugger::Command::StepOver(0));
    let returned = wait_for(&emulator, |shown| shown.pc == 0x0103);
    let step = emu_core::debug::Stop {
        cpu: 0,
        reason: StopReason::Step,
    };
    assert_eq!(returned.state, RunState::Stopped(step));
}

#[test]
fn next_frame_runs_one_frame_then_pauses() {
    let (emulator, _events) = start(PathBuf::from("/nonexistent/counter.gb"));
    watch_memory(&emulator, 0);
    debug(&emulator, debugger::Command::Pause);
    let paused = wait_for(&emulator, |shown| shown.state == RunState::Paused);
    let before = paused.memory[0].expect("readable");
    debug(&emulator, debugger::Command::NextFrame);
    let after = wait_for(&emulator, |shown| {
        shown.state == RunState::Paused && shown.memory[0] != Some(before)
    });
    assert_eq!(after.memory[0], Some(before.wrapping_add(1)));
    thread::sleep(Duration::from_millis(20));
    let later = emulator.debug_snapshot().expect("open");
    assert_eq!(later.memory[0], after.memory[0], "still paused");
}

/// Every `set_rom_patches` call a core received.
type PatchLog = Arc<Mutex<Vec<(usize, Vec<RomPatch>)>>>;

/// A Game Boy-like core that records the ROM patches it is given.
struct Patchable {
    log: PatchLog,
    frames: [Frame; 1],
}

impl Core for Patchable {
    fn info(&self) -> CoreInfo {
        CoreInfo {
            system: System::Gb,
            screens: &SCREENS,
            touch_screen: None,
            players: 1,
            frame_rate: FrameRate::new(1000, 1),
            audio_sample_rate: 48_000,
            state_version: 1,
        }
    }

    fn run_frame(&mut self, _: &[Input], _: &mut Vec<StereoSample>) {}

    fn screens(&self) -> &[Frame] {
        &self.frames
    }

    fn reset(&mut self) {}

    fn save_state(&self, _: &mut Vec<u8>) {}

    fn load_state(&mut self, _: &[u8]) -> Result<(), StateError> {
        Ok(())
    }

    fn set_rom_patches(&mut self, player: usize, patches: &[RomPatch]) {
        let mut log = self.log.lock().expect("unpoisoned");
        log.push((player, patches.to_vec()));
    }
}

#[test]
fn cheat_lists_hand_their_rom_patches_to_the_core() {
    let log = PatchLog::default();
    let loaded = Loaded {
        core: Box::new(Patchable {
            log: Arc::clone(&log),
            frames: [Frame::new(SCREENS[0])],
        }),
        files: vec![GameFiles::of(&PathBuf::from("/nonexistent/patchable.gb"))],
        rom_paths: vec![PathBuf::from("/nonexistent/patchable.gb")],
        rom_crc32: 0,
        live_sound: None,
        own_saves: OwnSaves::None,
    };
    let (events, _events) = async_channel::unbounded();
    let emulator = Emulator::start(loaded, events, None).expect("the threads start");
    for text in ["[Jump]\n3E1-23F\n", "[Jump] off\n3E1-23F\n"] {
        let cheats = emu_cheats::parse(System::Gb, text).expect("valid");
        emulator.send(Command::SetCheats { player: 0, cheats });
    }
    // Dropping the handle applies the queued commands, then stops.
    drop(emulator);
    let jump = RomPatch {
        address: 0x0123,
        value: 0x3E,
        compare: None,
    };
    assert_eq!(
        *log.lock().expect("unpoisoned"),
        [(0, vec![jump]), (0, Vec::new())]
    );
}

/// Waits for a notice starting with `start`, returning it.
fn wait_for_notice(events: &async_channel::Receiver<Event>, start: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match events.try_recv() {
            Ok(event) if event.notice().is_some_and(|text| text.starts_with(start)) => {
                return event.notice().expect("a notice").into_owned();
            }
            Ok(_) => {}
            Err(_) => {
                assert!(Instant::now() < deadline, "no notice {start:?}");
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

/// The files in `dir` with `extension`.
fn files_with(dir: &Path, extension: &str) -> Vec<PathBuf> {
    let entries = fs::read_dir(dir).expect("read dir");
    let paths = entries.map(|entry| entry.expect("entry").path());
    paths
        .filter(|path| path.extension().is_some_and(|found| found == extension))
        .collect()
}

#[test]
fn screenshots_land_next_to_the_rom() {
    let dir = std::env::temp_dir().join(format!("emu-app-shots-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("temp dir");
    let (emulator, events) = start(dir.join("counter.gb"));
    emulator.send(Command::SaveScreenshot);
    let saved = wait_for_notice(&events, "Saved counter ");
    let pngs = files_with(&dir, "png");
    assert_eq!(pngs.len(), 1, "{saved}");
    let bytes = fs::read(&pngs[0]).expect("the screenshot");
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    drop(emulator);
    fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn recordings_hold_every_frame_run_and_none_while_paused() {
    let dir = std::env::temp_dir().join(format!("emu-app-movie-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("temp dir");
    let (emulator, events) = start(dir.join("counter.gb"));
    emulator.send(Command::TogglePause);
    wait_for_notice(&events, "Paused");
    let first = latest_count(&emulator);
    emulator.send(Command::ToggleRecording);
    wait_for_notice(&events, "Recording to counter ");
    // As fast as it goes, which the writer must keep up with.
    emulator.send(Command::SetSpeed(Speed::Unlimited));
    emulator.send(Command::TogglePause);
    thread::sleep(Duration::from_millis(50));
    emulator.send(Command::TogglePause);
    wait_for_notice(&events, "Paused");
    let last = latest_count(&emulator);
    thread::sleep(Duration::from_millis(20));
    emulator.send(Command::ToggleRecording);
    let saved = wait_for_notice(&events, "Saved counter ");
    assert!(saved.ends_with(" of video"), "{saved}");

    let movies = files_with(&dir, "mov");
    assert_eq!(movies.len(), 1);
    let bytes = fs::read(&movies[0]).expect("the movie");
    let jpegs = bytes
        .windows(4)
        .filter(|window| *window == [0xFF, 0xD8, 0xFF, 0xE0])
        .count();
    assert!(last > first + 10, "frames ran: {first} to {last}");
    assert_eq!(jpegs as u32, last - first, "one picture per frame run");
    drop(emulator);
    fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn durations_read_as_minutes_and_seconds() {
    assert_eq!(minutes_and_seconds(Duration::from_millis(83_900)), "1:23");
    assert_eq!(minutes_and_seconds(Duration::from_secs(3723)), "1:02:03");
}

#[test]
fn a_state_saved_loaded_or_refused_is_said_by_what_it_is() {
    let dir = empty_dir("state-events");
    let (emulator, events) = start(dir.join("counter.gb"));
    emulator.send(Command::SaveState(3));
    wait_for_event(&events, &Event::StateSaved(3));
    emulator.send(Command::LoadState(3));
    wait_for_event(&events, &Event::StateLoaded(3));
    // Nothing in slot 4: a failure, in the words it has always had.
    emulator.send(Command::LoadState(4));
    let deadline = Instant::now() + Duration::from_secs(5);
    let refused = loop {
        assert!(Instant::now() < deadline, "no failure for the empty slot");
        match events.try_recv() {
            Ok(Event::Failed(text)) => break text,
            Ok(Event::StateLoaded(slot)) => panic!("loaded the empty slot {slot}"),
            Ok(_) => {}
            Err(_) => thread::sleep(Duration::from_millis(1)),
        }
    };
    assert!(
        refused.starts_with("Could not load state: cannot read "),
        "{refused}"
    );
    // The same words, for what shows them and for what prints them.
    assert_eq!(
        Event::StateSaved(3).notice().as_deref(),
        Some("Saved state 3")
    );
    assert_eq!(
        Event::StateLoaded(3).words().as_deref(),
        Some("Loaded state 3")
    );
    assert_eq!(Event::Warning("kept".to_owned()).notice(), None);
    assert_eq!(
        Event::Warning("kept".to_owned()).words().as_deref(),
        Some("kept")
    );
    assert_eq!(Event::Frame.words(), None);
    drop(emulator);
    fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn save_states_round_trip_through_slot_files() {
    let dir = std::env::temp_dir().join(format!("emu-app-states-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("temp dir");
    let (emulator, events) = start(dir.join("counter.gb"));
    thread::sleep(Duration::from_millis(50));
    emulator.send(Command::TogglePause);
    thread::sleep(Duration::from_millis(20));
    let saved = latest_count(&emulator);
    emulator.send(Command::SaveState(2));
    emulator.send(Command::TogglePause);
    thread::sleep(Duration::from_millis(50));
    emulator.send(Command::LoadState(2));
    emulator.send(Command::TogglePause);
    thread::sleep(Duration::from_millis(20));
    assert_eq!(latest_count(&emulator), saved);

    // Both notices, however long they take: the state is written on
    // another thread, and Windows took longer than the sleeps above.
    let mut waiting = vec!["Saved state 2", "Loaded state 2"];
    let deadline = Instant::now() + Duration::from_secs(5);
    while !waiting.is_empty() {
        match events.try_recv() {
            Ok(event) => {
                if let Some(text) = event.notice() {
                    waiting.retain(|notice| *notice != text);
                }
            }
            Err(_) => {
                assert!(Instant::now() < deadline, "no notices {waiting:?}");
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
    drop(emulator);
    fs::remove_dir_all(&dir).expect("clean up");
}

fn run_script(emulator: &Emulator, source: &str) {
    emulator.send(Command::RunScript {
        name: "hud.lua".to_owned(),
        source: source.to_owned(),
    });
}

/// Waits for the script's drawing to pass `test`.
fn wait_for_overlay(emulator: &Emulator, test: impl Fn(&[Draw]) -> bool) -> Arc<[Draw]> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let overlay = emulator.overlay();
        if test(&overlay) {
            return overlay;
        }
        assert!(Instant::now() < deadline, "the script never drew it");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn scripts_read_write_and_draw_after_every_frame() {
    let (emulator, events) = start(PathBuf::from("/nonexistent/counter.gb"));
    run_script(
        &emulator,
        r#"
            emu.on_frame(function()
                gui.text(0, 0, "count " .. memory.read_u8(0))
                memory.write_u8(1, 1)
            end)
            "#,
    );
    wait_for_notice(&events, "Running hud.lua");
    let overlay = wait_for_overlay(&emulator, |drawn| !drawn.is_empty());
    assert!(
        matches!(&overlay[0], Draw::Text { text, .. } if text.starts_with("count ")),
        "{overlay:?}"
    );
    // Byte 1 of the counter, which the script keeps at 1.
    thread::sleep(Duration::from_millis(20));
    assert_eq!(latest_count(&emulator) >> 8 & 0xFF, 1);

    emulator.send(Command::StopScript);
    wait_for_notice(&events, "Stopped hud.lua");
    assert!(emulator.overlay().is_empty(), "stopping clears its drawing");
    emulator.send(Command::StopScript);
    wait_for_notice(&events, "No script is running");
}

#[test]
fn a_script_error_is_a_notice_and_stops_the_script() {
    let (emulator, events) = start(PathBuf::from("/nonexistent/counter.gb"));
    run_script(
        &emulator,
        r#"
            emu.on_frame(function()
                gui.box(0, 0, 0, 0)
                if emu.frame() == 3 then error("boom") end
            end)
            "#,
    );
    wait_for_notice(&events, "hud.lua:4: boom · script stopped");
    assert!(emulator.overlay().is_empty());
    run_script(&emulator, "x = = 1");
    wait_for_notice(&events, "hud.lua:1: unexpected symbol");
    // The game runs on.
    let before = latest_count(&emulator);
    thread::sleep(Duration::from_millis(20));
    assert!(latest_count(&emulator) > before);
}

#[test]
fn scripts_press_buttons_on_the_next_frame() {
    let (emulator, seen) = start_recorder();
    run_script(
        &emulator,
        "emu.on_frame(function() joypad.set({a = true}, 2) end)",
    );
    run_frames(&seen, 10);
    let a = |input: &Input| input.buttons.contains(Button::A);
    assert!(frames_where(&seen, 1, a) >= 5, "player 2 pressed A");
    assert_eq!(frames_where(&seen, 0, a), 0, "player 1 did not");
}

#[test]
fn scripts_save_states_and_pause() {
    let dir = std::env::temp_dir().join(format!("emu-app-script-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("temp dir");
    let (emulator, events) = start(dir.join("counter.gb"));
    run_script(
        &emulator,
        r#"
            emu.save_state(3)
            emu.on_frame(function()
                if emu.frame() == 5 then emu.pause() end
            end)
            "#,
    );
    wait_for_notice(&events, "Saved state 3");
    wait_for_notice(&events, "Paused by hud.lua");
    assert!(
        GameFiles::of(&dir.join("counter.gb"))
            .state(3, false)
            .exists()
    );
    let paused = latest_count(&emulator);
    thread::sleep(Duration::from_millis(20));
    assert_eq!(latest_count(&emulator), paused);
    drop(emulator);
    fs::remove_dir_all(&dir).expect("clean up");
}

/// A DS ROM whose two programs loop forever.
fn nds_rom() -> Vec<u8> {
    let mut rom = vec![0; 0x400];
    let mut put = |offset: usize, value: u32| {
        rom[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    };
    for (header, program, address) in [(0x20, 0x200, 0x0200_4000), (0x30, 0x300, 0x0238_0000)] {
        put(header, program);
        put(header + 4, address);
        put(header + 8, address);
        put(header + 12, 4);
        put(program as usize, 0xEAFF_FFFE); // B .
    }
    rom
}

/// A GBA ROM with 128 KiB of Flash.
fn gba_rom() -> Vec<u8> {
    let mut rom = vec![0; 0x400];
    rom[0xB2] = 0x96;
    rom[0x200..0x209].copy_from_slice(b"FLASH1M_V");
    rom
}

/// Writes `files` into a fresh directory named after `test`.
fn rom_dir(test: &str, files: &[(&str, &[u8])]) -> (PathBuf, Vec<PathBuf>) {
    let dir = std::env::temp_dir().join(format!("emu-app-{test}-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("temp dir");
    let paths = files
        .iter()
        .map(|(name, bytes)| {
            let path = dir.join(name);
            fs::write(&path, bytes).expect("write rom");
            path
        })
        .collect();
    (dir, paths)
}

#[test]
fn a_gba_game_after_a_ds_game_goes_in_the_gba_slot() {
    let (dir, paths) = rom_dir(
        "slot",
        &[("platinum.nds", &nds_rom()), ("emerald.gba", &gba_rom())],
    );
    let loaded = load(&paths, DmgColors::Grey).expect("loads");
    assert_eq!(
        (loaded.info().system, loaded.info().players),
        (System::Nds, 1)
    );
    assert_eq!(loaded.rom_paths().len(), 2);
    assert_eq!(loaded.player_rom_paths(), &paths[..1]);
    assert!(loaded.core.backup_generation(1).is_some(), "the GBA save");
    fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn a_ds_game_with_a_cartridge_in_names_its_states_apart_from_the_game_alone() {
    let (dir, paths) = rom_dir(
        "slot-states",
        &[("platinum.nds", &nds_rom()), ("emerald.gba", &gba_rom())],
    );
    let alone = load(&paths[..1], DmgColors::Grey).expect("loads");
    let slotted = load(&paths, DmgColors::Grey).expect("loads");
    assert_eq!(alone.states().path(1), dir.join("platinum.ss1"));
    assert_eq!(slotted.states().path(1), dir.join("platinum.emerald.ss1"));
    assert_eq!(
        slotted.states().path(CHECKPOINT),
        dir.join("platinum.emerald.ss0")
    );
    // Neither can load the other's, which their states say.
    assert_ne!(alone.states().rom_crc32(), slotted.states().rom_crc32());
    drop((alone, slotted));

    // Linked games keep the names they had.
    let (linked_dir, linked) = rom_dir(
        "slot-states-linked",
        &[("ruby.gba", &gba_rom()), ("sapphire.gba", &gba_rom())],
    );
    let loaded = load(&linked, DmgColors::Grey).expect("loads");
    assert_eq!(loaded.states().path(1), linked_dir.join("ruby.ls1"));
    fs::remove_dir_all(&dir).expect("clean up");
    fs::remove_dir_all(&linked_dir).expect("clean up");
}

#[test]
fn games_for_one_system_still_link() {
    let (dir, paths) = rom_dir(
        "link",
        &[("ruby.gba", &gba_rom()), ("sapphire.gba", &gba_rom())],
    );
    let loaded = load(&paths, DmgColors::Grey).expect("loads");
    assert_eq!(
        (loaded.info().system, loaded.info().players),
        (System::Gba, 2)
    );
    assert_eq!(loaded.player_rom_paths(), &paths[..]);
    let (dir_mixed, mixed) = rom_dir(
        "mixed",
        &[("ruby.gba", &gba_rom()), ("platinum.nds", &nds_rom())],
    );
    let error = load(&mixed, DmgColors::Grey)
        .err()
        .expect("a GBA game cannot take a DS game");
    assert!(format!("{error:#}").contains("same system"), "{error:#}");
    fs::remove_dir_all(&dir).expect("clean up");
    fs::remove_dir_all(&dir_mixed).expect("clean up");
}

#[test]
fn a_3ds_game_with_another_game_is_refused() {
    let (dir, paths) = rom_dir(
        "3ds-paired",
        &[("platinum.nds", &nds_rom()), ("Game.3ds", b"NCSD")],
    );
    for paths in [paths.clone(), paths.iter().rev().cloned().collect()] {
        let error = load(&paths, DmgColors::Grey).err().expect("refused");
        assert_eq!(
            format!("{error:#}"),
            "A 3DS game plays on its own: open it without another game"
        );
    }
    let twice = [paths[1].clone(), paths[1].clone()];
    assert!(load(&twice, DmgColors::Grey).is_err(), "nor with itself");
    fs::remove_dir_all(&dir).expect("clean up");
}

/// An installable title whose one content, its executable, is
/// encrypted with the title key: a header, a TMD signed with RSA-2048,
/// and the content.
fn encrypted_cia() -> Vec<u8> {
    let mut cia = vec![0; 0x2040];
    cia[0..4].copy_from_slice(&0x2020_u32.to_le_bytes());
    let tmd_size = 0x140 + 0xC4 + 0x900 + 0x30;
    cia[0x10..0x14].copy_from_slice(&(tmd_size as u32).to_le_bytes());
    cia[0x18..0x20].copy_from_slice(&0x200_u64.to_le_bytes());
    cia[0x20] = 0x80; // Content 0 is there.
    let mut tmd = vec![0; tmd_size];
    tmd[0..4].copy_from_slice(&0x0001_0004_u32.to_be_bytes());
    tmd[0x140 + 0x9E..0x140 + 0xA0].copy_from_slice(&1_u16.to_be_bytes());
    let record = 0x140 + 0xC4 + 0x900;
    tmd[record + 6..record + 8].copy_from_slice(&1_u16.to_be_bytes());
    tmd[record + 8..record + 16].copy_from_slice(&0x200_u64.to_be_bytes());
    cia.extend_from_slice(&tmd);
    cia.resize(cia.len().next_multiple_of(64) + 0x200, 0x99);
    cia
}

#[test]
fn an_encrypted_cia_is_refused_before_the_core_starts() {
    let (dir, paths) = rom_dir("3ds-encrypted-cia", &[("Title.cia", &encrypted_cia())]);
    let error = load(&paths, DmgColors::Grey).err().expect("refused");
    assert_eq!(
        format!("{error:#}"),
        format!(
            "{} is encrypted: Azahar plays only decrypted games",
            paths[0].display()
        )
    );
    fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn two_ds_games_link_over_local_wireless() {
    let (dir, paths) = rom_dir(
        "wireless",
        &[("white.nds", &nds_rom()), ("black.nds", &nds_rom())],
    );
    let loaded = load(&paths, DmgColors::Grey).expect("loads");
    assert_eq!(
        (loaded.info().system, loaded.info().players),
        (System::Nds, 2)
    );
    assert_eq!(loaded.info().screens.len(), 4);
    assert_eq!(loaded.player_rom_paths(), &paths[..]);
    fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn same_named_games_for_two_systems_keep_saves_of_their_own() {
    let (dir, paths) = rom_dir(
        "namesake-roms",
        &[("pokemon.nds", &nds_rom()), ("pokemon.gba", &gba_rom())],
    );
    // The GBA game goes in the DS game's slot, each with its own save.
    let loaded = load(&paths, DmgColors::Grey).expect("loads");
    assert!(loaded.core.backup_generation(1).is_some(), "the GBA save");
    let saves: Vec<PathBuf> = loaded.files().iter().map(GameFiles::backup).collect();
    assert_eq!(
        saves,
        [dir.join("pokemon.nds.sav"), dir.join("pokemon.gba.sav")]
    );
    fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn games_that_would_share_a_save_file_are_refused() {
    let (dir, paths) = rom_dir("shared-save", &[("ruby.gba", &gba_rom())]);
    let twice = [paths[0].clone(), paths[0].clone()];
    let error = load(&twice, DmgColors::Grey).err().expect("refused");
    assert_eq!(
        format!("{error:#}"),
        "ruby.gba and ruby.gba would both save to ruby.sav; rename one of them"
    );
    assert!(!dir.join("ruby.sav").exists());
    fs::remove_dir_all(&dir).expect("clean up");
}

/// A core with a one-byte battery save at address 0 and a byte of
/// other progress at address 1, both kept in its save states. Each
/// frame adds `tick` to the save, and panics once it reaches
/// `crash_at`.
struct Cart {
    save: u8,
    progress: u8,
    generation: u64,
    tick: u8,
    crash_at: Option<u8>,
    frames: [Frame; 1],
}

impl Core for Cart {
    fn info(&self) -> CoreInfo {
        CoreInfo {
            system: System::Gba,
            screens: &SCREENS,
            touch_screen: None,
            players: 1,
            frame_rate: FrameRate::new(1000, 1),
            audio_sample_rate: 48_000,
            state_version: 1,
        }
    }

    fn run_frame(&mut self, _: &[Input], _: &mut Vec<StereoSample>) {
        if self.tick > 0 {
            self.save = self.save.wrapping_add(self.tick);
            self.generation += 1;
        }
        if self.crash_at == Some(self.save) {
            panic!("the cartridge caught fire");
        }
    }

    fn screens(&self) -> &[Frame] {
        &self.frames
    }

    fn reset(&mut self) {}

    fn save_state(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&[self.save, self.progress]);
    }

    fn load_state(&mut self, state: &[u8]) -> Result<(), StateError> {
        let [save, progress]: [u8; 2] = state.try_into().map_err(|_| StateError::Truncated)?;
        self.save = save;
        self.progress = progress;
        // As the real cores do, so the loaded save gets written.
        self.generation += 1;
        Ok(())
    }

    fn backup_generation(&self, cartridge: usize) -> Option<u64> {
        (cartridge == 0).then_some(self.generation)
    }

    fn save_backup(&self, cartridge: usize, out: &mut Vec<u8>) {
        if cartridge == 0 {
            out.push(self.save);
        }
    }

    fn peek(&self, _: usize, address: u32) -> Option<u8> {
        match address {
            0 => Some(self.save),
            1 => Some(self.progress),
            _ => None,
        }
    }

    fn poke(&mut self, _: usize, address: u32, value: u8) -> bool {
        match address {
            0 => {
                self.save = value;
                self.generation += 1;
            }
            1 => self.progress = value,
            _ => return false,
        }
        true
    }

    fn debugger(&mut self) -> Option<&mut dyn Debugger> {
        Some(self)
    }
}

peek_debugger!(Cart);

/// Starts a [`Cart`] for the ROM `dir/game.gba`, with `save` as its
/// battery save, as if read from disk.
fn start_cart(
    dir: &Path,
    save: u8,
    tick: u8,
    crash_at: Option<u8>,
) -> (Emulator, async_channel::Receiver<Event>) {
    let loaded = Loaded {
        core: Box::new(Cart {
            save,
            progress: 0,
            generation: 0,
            tick,
            crash_at,
            frames: [Frame::new(SCREENS[0])],
        }),
        files: vec![GameFiles::of(&dir.join("game.gba"))],
        rom_paths: vec![dir.join("game.gba")],
        rom_crc32: 0,
        live_sound: None,
        own_saves: OwnSaves::None,
    };
    let (events, receiver) = async_channel::unbounded();
    (
        Emulator::start(loaded, events, None).expect("the threads start"),
        receiver,
    )
}

/// Writes `value` at `address` in a [`Cart`], as its game would.
fn poke(emulator: &Emulator, address: u32, value: u8) {
    debug(
        emulator,
        debugger::Command::Write {
            cpu: 0,
            address,
            value,
        },
    );
}

/// A [`Cart`]'s battery save and other progress, as the debugger
/// shows them after the commands sent so far.
fn cart_bytes(emulator: &Emulator) -> (u8, u8) {
    watch_memory(emulator, 0);
    let shown = wait_for(emulator, |shown| shown.memory[0].is_some());
    (
        shown.memory[0].expect("the save"),
        shown.memory[1].expect("the progress"),
    )
}

/// Waits for `expected`.
fn wait_for_event(events: &async_channel::Receiver<Event>, expected: &Event) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match events.try_recv() {
            Ok(event) if event == *expected => return,
            Ok(_) => {}
            Err(_) => {
                assert!(Instant::now() < deadline, "no {expected:?}");
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

/// A fresh, empty folder for `test`.
fn empty_dir(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("emu-app-{test}-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// Plays to battery save 1 and progress 7, then leaves a checkpoint
/// there.
fn leave_checkpoint(dir: &Path) {
    let (emulator, _events) = start_cart(dir, 0, 0, None);
    poke(&emulator, 0, 1);
    poke(&emulator, 1, 7);
    emulator.send(Command::SaveCheckpoint(0));
    drop(emulator);
    assert_eq!(fs::read(dir.join("game.sav")).expect("saved"), [1]);
}

#[test]
fn a_checkpoint_is_picked_up_with_the_battery_save_it_was_left_with() {
    let dir = empty_dir("checkpoint-same");
    leave_checkpoint(&dir);
    assert!(dir.join("game.ss0.saves").exists());
    let (emulator, events) = start_cart(&dir, 1, 0, None);
    emulator.send(Command::LoadCheckpoint(0));
    wait_for_event(&events, &Event::PickedUp(true));
    assert_eq!(cart_bytes(&emulator), (1, 7));
    drop(emulator);
    fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn a_checkpoint_never_rolls_back_a_newer_battery_save() {
    let dir = empty_dir("checkpoint-newer");
    let save = dir.join("game.sav");
    // Left off at 1, then saved in the game at 2 and quit without a
    // new checkpoint, as after a crash.
    let (emulator, _events) = start_cart(&dir, 0, 0, None);
    poke(&emulator, 0, 1);
    poke(&emulator, 1, 7);
    emulator.send(Command::SaveCheckpoint(0));
    poke(&emulator, 0, 2);
    drop(emulator);
    assert_eq!(fs::read(&save).expect("saved"), [2]);

    let (emulator, events) = start_cart(&dir, 2, 0, None);
    emulator.send(Command::LoadCheckpoint(0));
    wait_for_event(&events, &Event::PickedUp(false));
    assert_eq!(cart_bytes(&emulator), (2, 0));
    drop(emulator);
    assert_eq!(fs::read(&save).expect("kept"), [2]);

    // Nor one copied in from elsewhere.
    fs::write(&save, [9]).expect("copied in");
    let (emulator, events) = start_cart(&dir, 9, 0, None);
    emulator.send(Command::LoadCheckpoint(0));
    wait_for_event(&events, &Event::PickedUp(false));
    drop(emulator);
    assert_eq!(fs::read(&save).expect("kept"), [9]);
    fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn a_checkpoint_from_before_records_is_picked_up_only_with_its_own_save() {
    let dir = empty_dir("checkpoint-unrecorded");
    let save = dir.join("game.sav");
    leave_checkpoint(&dir);
    fs::remove_file(dir.join("game.ss0.saves")).expect("as older versions left it");
    let (emulator, events) = start_cart(&dir, 1, 0, None);
    emulator.send(Command::LoadCheckpoint(0));
    wait_for_event(&events, &Event::PickedUp(true));
    assert_eq!(cart_bytes(&emulator), (1, 7));
    drop(emulator);

    fs::write(&save, [3]).expect("saved since");
    let (emulator, events) = start_cart(&dir, 3, 0, None);
    emulator.send(Command::LoadCheckpoint(0));
    wait_for_event(&events, &Event::PickedUp(false));
    assert_eq!(cart_bytes(&emulator), (3, 0), "undone");
    drop(emulator);
    assert_eq!(fs::read(&save).expect("kept"), [3]);
    fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn an_empty_checkpoint_slot_is_no_matter() {
    let dir = empty_dir("checkpoint-none");
    let (emulator, events) = start_cart(&dir, 0, 0, None);
    emulator.send(Command::LoadCheckpoint(0));
    emulator.send(Command::TogglePause);
    wait_for_notice(&events, "Paused");
    drop(emulator);
    let said: Vec<Event> = std::iter::from_fn(|| events.try_recv().ok()).collect();
    assert!(
        !said
            .iter()
            .any(|event| matches!(event, Event::PickedUp(_)) || event.notice().is_some()),
        "{said:?}"
    );
    fs::remove_dir_all(&dir).expect("clean up");
}

/// A game that saves only when asked, as an RPG Maker game's player
/// does: its state is its place, which it will not save while `busy`,
/// as in a battle.
struct Asked {
    place: Arc<AtomicU8>,
    busy: Arc<AtomicBool>,
    frames: [Frame; 1],
}

impl Core for Asked {
    fn info(&self) -> CoreInfo {
        CoreInfo {
            system: System::RpgMaker,
            screens: &SCREENS,
            touch_screen: None,
            players: 1,
            frame_rate: FrameRate::new(1000, 1),
            audio_sample_rate: 48_000,
            state_version: 1,
        }
    }

    fn run_frame(&mut self, _: &[Input], _: &mut Vec<StereoSample>) {}

    fn screens(&self) -> &[Frame] {
        &self.frames
    }

    fn reset(&mut self) {}

    fn save_state(&self, out: &mut Vec<u8>) {
        self.try_save_state(out).ok();
    }

    fn load_state(&mut self, state: &[u8]) -> Result<(), StateError> {
        let [place]: [u8; 1] = state.try_into().map_err(|_| StateError::Truncated)?;
        self.place.store(place, Ordering::SeqCst);
        Ok(())
    }

    fn try_save_state(&self, out: &mut Vec<u8>) -> Result<(), String> {
        if self.busy.load(Ordering::SeqCst) {
            return Err("in a battle".to_owned());
        }
        out.push(self.place.load(Ordering::SeqCst));
        Ok(())
    }

    fn saves_every_frame(&self) -> bool {
        false
    }
}

/// An [`Asked`] game in the folder `dir/game` at `place`, which keeps
/// the saves it makes itself in `dir/saves`.
fn start_asked(
    dir: &Path,
    place: u8,
    busy: bool,
) -> (
    Emulator,
    async_channel::Receiver<Event>,
    Arc<AtomicU8>,
    Arc<AtomicBool>,
) {
    let game = dir.join("game");
    let (place, busy) = (
        Arc::new(AtomicU8::new(place)),
        Arc::new(AtomicBool::new(busy)),
    );
    let loaded = Loaded {
        core: Box::new(Asked {
            place: Arc::clone(&place),
            busy: Arc::clone(&busy),
            frames: [Frame::new(SCREENS[0])],
        }),
        files: vec![GameFiles::of_folder(&game)],
        rom_paths: vec![game],
        rom_crc32: 0,
        live_sound: None,
        own_saves: OwnSaves::RpgMaker(rpg_maker::SaveFolders::new(vec![dir.join("saves")])),
    };
    let (events, receiver) = async_channel::unbounded();
    (
        Emulator::start(loaded, events, None).expect("the threads start"),
        receiver,
        place,
        busy,
    )
}

/// Starts an [`Asked`] game at place 0 and picks up its checkpoint,
/// returning whether it was picked up and the place it is at then.
fn pick_up_asked(dir: &Path) -> (bool, u8) {
    let (emulator, events, place, _) = start_asked(dir, 0, false);
    emulator.send(Command::LoadCheckpoint(CHECKPOINT));
    let deadline = Instant::now() + Duration::from_secs(5);
    let picked = loop {
        match events.try_recv() {
            Ok(Event::PickedUp(picked)) => break picked,
            Ok(_) => {}
            Err(_) => {
                assert!(Instant::now() < deadline, "never picked up");
                thread::sleep(Duration::from_millis(1));
            }
        }
    };
    drop(emulator);
    (picked, place.load(Ordering::SeqCst))
}

#[test]
fn a_game_that_saves_only_when_asked_is_picked_up_where_it_was_left() {
    let dir = empty_dir("checkpoint-asked");
    fs::create_dir_all(dir.join("saves")).expect("save folder");
    fs::write(dir.join("saves/Game.rxdata"), b"saved in the game").expect("own save");
    let (emulator, _events, _, _) = start_asked(&dir, 5, false);
    emulator.send(Command::SaveCheckpoint(CHECKPOINT));
    drop(emulator);
    assert!(dir.join("game.ss0").exists(), "the checkpoint");
    let record = fs::read_to_string(dir.join("game.ss0.saves")).expect("its record");
    assert!(record.contains("Game.rxdata "), "{record}");
    assert_eq!(pick_up_asked(&dir), (true, 5));
    fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn a_game_that_saves_only_when_asked_is_never_picked_up_over_a_save_it_made_since() {
    let dir = empty_dir("checkpoint-asked-newer");
    let saves = dir.join("saves");
    fs::create_dir_all(&saves).expect("save folder");
    fs::write(saves.join("Game.rxdata"), b"first").expect("own save");
    let (emulator, _events, _, _) = start_asked(&dir, 5, false);
    emulator.send(Command::SaveCheckpoint(CHECKPOINT));
    drop(emulator);
    // Saved in the game since, as before a crash, or playing it on its
    // own.
    fs::write(saves.join("Game.rxdata"), b"second").expect("saved again");
    assert_eq!(pick_up_asked(&dir), (false, 0));
    // A save file of another name counts too.
    fs::write(saves.join("Game.rxdata"), b"first").expect("as it was");
    assert_eq!(pick_up_asked(&dir), (true, 5));
    fs::write(saves.join("Save2.rxdata"), b"another").expect("another save");
    assert_eq!(pick_up_asked(&dir), (false, 0));
    fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn a_game_that_cannot_be_saved_now_keeps_the_checkpoint_before() {
    let dir = empty_dir("checkpoint-asked-busy");
    let (emulator, events, place, busy) = start_asked(&dir, 5, false);
    emulator.send(Command::SaveCheckpoint(CHECKPOINT));
    // Its record is written last.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !dir.join("game.ss0.saves").exists() {
        assert!(Instant::now() < deadline, "no checkpoint");
        thread::sleep(Duration::from_millis(1));
    }
    place.store(6, Ordering::SeqCst);
    busy.store(true, Ordering::SeqCst);
    emulator.send(Command::SaveCheckpoint(CHECKPOINT));
    let said = wait_for_notice(&events, "Could not save where you left off");
    assert!(said.ends_with("in a battle"), "{said}");
    drop(emulator);
    assert_eq!(pick_up_asked(&dir), (true, 5));
    fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn a_game_that_saves_only_when_asked_with_no_record_is_left_alone() {
    let dir = empty_dir("checkpoint-asked-unrecorded");
    let (emulator, _events, _, _) = start_asked(&dir, 5, false);
    emulator.send(Command::SaveCheckpoint(CHECKPOINT));
    drop(emulator);
    fs::remove_file(dir.join("game.ss0.saves")).expect("no record");
    let (emulator, events, place, _) = start_asked(&dir, 0, false);
    emulator.send(Command::LoadCheckpoint(CHECKPOINT));
    emulator.send(Command::TogglePause);
    wait_for_notice(&events, "Paused");
    drop(emulator);
    let said: Vec<Event> = std::iter::from_fn(|| events.try_recv().ok()).collect();
    assert!(
        !said.iter().any(|event| matches!(event, Event::PickedUp(_))),
        "{said:?}"
    );
    assert_eq!(place.load(Ordering::SeqCst), 0);
    fs::remove_dir_all(&dir).expect("clean up");
}

/// Every file under `dir`, by its path from there, with what it holds.
fn tree(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, root: &Path, files: &mut Vec<(String, Vec<u8>)>) {
        for entry in fs::read_dir(dir).expect("read dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                walk(&path, root, files);
            } else {
                let name = path.strip_prefix(root).expect("under the root");
                let name = name.to_string_lossy().replace('\\', "/");
                files.push((name, fs::read(&path).expect("read")));
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, dir, &mut files);
    files.sort();
    files
}

/// `files` as [`tree`] gives them.
fn holding(files: &[(&str, &[u8])]) -> Vec<(String, Vec<u8>)> {
    let files = files.iter();
    files
        .map(|(name, bytes)| ((*name).to_owned(), bytes.to_vec()))
        .collect()
}

#[test]
fn starting_an_rpg_maker_game_over_sets_the_saves_it_made_aside() {
    let dir = empty_dir("start-over-rpg-maker");
    let game = dir.join("Game 1.2");
    let saves = dir.join("saves");
    fs::create_dir_all(&game).expect("the game's folder");
    fs::create_dir_all(&saves).expect("its save folder");
    // Saved in the game, with a second file; its checkpoint beside its
    // folder; and files that are no saves.
    fs::write(saves.join("Game.rxdata"), b"eight badges").expect("write");
    fs::write(saves.join("Save2.rxdata"), b"another file").expect("write");
    fs::write(saves.join("mkxp.json"), b"settings").expect("write");
    fs::write(game.join("Game.ini"), b"Title=Game").expect("write");
    for (name, bytes) in [
        ("Game 1.2.ss0", &b"checkpoint"[..]),
        ("Game 1.2.ss0.saves", b"record"),
        ("Game 1.2.ss0.png", b"picture"),
        ("Game 1.2.ss1", b"a state the player saved"),
    ] {
        fs::write(dir.join(name), bytes).expect("write");
    }
    let files = GameFiles::of_folder(&game);
    let states = StateFiles::new(files.clone(), false, None, System::RpgMaker, 0);
    let own = OwnSaves::RpgMaker(rpg_maker::SaveFolders::new(vec![saves.clone(), game]));

    let said = erase_progress(&states, &files.backup(), &own).expect("erased");
    assert_eq!(
        said.as_deref(),
        Some("Your 2 old saves are kept beside the game's, renamed to end in .bak")
    );
    assert_eq!(
        tree(&dir),
        holding(&[
            ("Game 1.2.ss1", b"a state the player saved"),
            ("Game 1.2/Game.ini", b"Title=Game"),
            ("saves/Game.rxdata.bak", b"eight badges"),
            ("saves/Save2.rxdata.bak", b"another file"),
            ("saves/mkxp.json", b"settings"),
        ])
    );

    // Started over again after playing on: the first is never replaced.
    fs::write(saves.join("Game.rxdata"), b"one badge").expect("write");
    let said = erase_progress(&states, &files.backup(), &own).expect("erased");
    assert_eq!(
        said.as_deref(),
        Some("Your old save is kept as Game.rxdata.bak2")
    );
    assert_eq!(
        fs::read(saves.join("Game.rxdata.bak")).expect("the first"),
        b"eight badges"
    );
    assert_eq!(
        fs::read(saves.join("Game.rxdata.bak2")).expect("the second"),
        b"one badge"
    );
    assert!(!saves.join("Game.rxdata").exists());
    // With nothing saved, there is nothing to say.
    assert_eq!(
        erase_progress(&states, &files.backup(), &own).expect("erased"),
        None
    );
    fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn starting_over_sets_the_battery_save_aside_and_removes_the_checkpoint() {
    let dir = empty_dir("start-over-battery");
    for (name, bytes) in [
        ("game.sav", &b"twenty hours"[..]),
        ("game.ss0", b"checkpoint"),
        ("game.ss0.saves", b"record"),
        ("game.ss0.png", b"picture"),
        ("game.ss2", b"a state the player saved"),
        ("game.cht", b"cheats"),
    ] {
        fs::write(dir.join(name), bytes).expect("write");
    }
    let files = GameFiles::of(&dir.join("game.gba"));
    let states = StateFiles::new(files.clone(), false, None, System::Gba, 0);
    let said = erase_progress(&states, &files.backup(), &OwnSaves::None).expect("erased");
    assert_eq!(
        said.as_deref(),
        Some("Your old save is kept as game.sav.bak")
    );
    assert_eq!(
        tree(&dir),
        holding(&[
            ("game.cht", b"cheats"),
            ("game.sav.bak", b"twenty hours"),
            ("game.ss2", b"a state the player saved"),
        ])
    );
    fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn starting_a_3ds_game_over_sets_its_save_folders_aside() {
    let dir = empty_dir("start-over-3ds");
    // A 3DS executable with a title ID, and the save data the core made
    // for it on its SD card.
    let mut rom = vec![0; 0x200];
    rom[0x100..0x104].copy_from_slice(b"NCCH");
    rom[0x118..0x120].copy_from_slice(&0x0004_0000_0011_C400_u64.to_le_bytes());
    let game = dir.join("Game.cxi");
    fs::write(&game, &rom).expect("write");
    let data = dir.join("3DS");
    let own = emu_3ds::SaveData::of(&game, &data);
    let ids = data.join("Azahar/sdmc/Nintendo 3DS/0000/0000");
    let save_data = ids.join("title/00040000/0011c400/data");
    fs::create_dir_all(save_data.join("00000001.sav")).expect("save data");
    fs::write(save_data.join("00000001.sav/main"), b"the game's save").expect("write");
    assert_eq!(
        own.folders(),
        std::slice::from_ref(&save_data),
        "found by its title ID"
    );

    let files = GameFiles::of(&game);
    let states = StateFiles::new(files.clone(), false, None, System::N3ds, 0);
    let said = erase_progress(&states, &files.backup(), &OwnSaves::ThreeDs(own));
    assert_eq!(
        said.expect("erased").as_deref(),
        Some("Your old save is kept on the 3DS's SD card, in folders renamed to end in .bak")
    );
    assert!(!save_data.exists());
    let kept = ids.join("title/00040000/0011c400/data.bak/00000001.sav/main");
    assert_eq!(fs::read(kept).expect("kept"), b"the game's save");
    fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn pausing_writes_the_battery_save() {
    let dir = empty_dir("pause-flush");
    let (emulator, events) = start_cart(&dir, 0, 1, None);
    thread::sleep(Duration::from_millis(20));
    emulator.send(Command::TogglePause);
    wait_for_notice(&events, "Paused");
    // Well before the timed flush, which never comes while paused.
    let (save, _) = cart_bytes(&emulator);
    let deadline = Instant::now() + Duration::from_secs(5);
    while fs::read(dir.join("game.sav")).ok() != Some(vec![save]) {
        assert!(Instant::now() < deadline, "the save was not written");
        thread::sleep(Duration::from_millis(1));
    }
    drop(emulator);
    fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn rewinding_past_a_save_made_in_the_game_undoes_it_and_keeps_no_copy() {
    let dir = empty_dir("rewind-save");
    let save = dir.join("game.sav");
    let (emulator, events) = start_cart(&dir, 0, 0, None);
    // Some play to rewind through, then a save in the game.
    thread::sleep(Duration::from_millis(100));
    poke(&emulator, 0, 5);
    emulator.send(Command::TogglePause);
    wait_for_notice(&events, "Paused");
    let deadline = Instant::now() + Duration::from_secs(5);
    while fs::read(&save).ok() != Some(vec![5]) {
        assert!(Instant::now() < deadline, "the save was not written");
        thread::sleep(Duration::from_millis(1));
    }
    emulator.send(Command::TogglePause);

    // Back to before it, as the player watches.
    emulator.send(Command::SetRewind(true));
    let deadline = Instant::now() + Duration::from_secs(5);
    while cart_bytes(&emulator).0 != 0 {
        assert!(Instant::now() < deadline, "never rewound past the save");
        thread::sleep(Duration::from_millis(1));
    }
    emulator.send(Command::SetRewind(false));
    drop(emulator);
    // The save is undone with everything else, and that is all.
    assert_eq!(fs::read(&save).expect("as the game has it"), [0]);
    let left: Vec<String> = files_with(&dir, "sav")
        .iter()
        .map(|path| file_name(path))
        .collect();
    assert_eq!(left, ["game.sav"]);
    assert_eq!(fs::read_dir(&dir).expect("read dir").count(), 1);
    let said: Vec<Event> = std::iter::from_fn(|| events.try_recv().ok()).collect();
    assert!(
        !said.iter().any(
            |event| matches!(event, Event::Notice(text) | Event::Warning(text) if text.contains("is kept as"))
        ),
        "{said:?}"
    );
    fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn loading_a_state_never_loses_the_save_on_disk() {
    let dir = empty_dir("load-state-save");
    let save = dir.join("game.sav");
    let (emulator, events) = start_cart(&dir, 0, 0, None);
    poke(&emulator, 0, 2);
    emulator.send(Command::SaveState(1));
    wait_for_notice(&events, "Saved state 1");
    poke(&emulator, 0, 20);
    emulator.send(Command::TogglePause);
    wait_for_notice(&events, "Paused");
    let deadline = Instant::now() + Duration::from_secs(5);
    while fs::read(&save).ok() != Some(vec![20]) {
        assert!(Instant::now() < deadline, "the save was not written");
        thread::sleep(Duration::from_millis(1));
    }
    emulator.send(Command::LoadState(1));
    wait_for_notice(&events, "Loaded state 1");
    // A warning, which stays up long enough to read where the save is.
    let kept = "Your save from before you loaded the state is kept as game.sav.before-load";
    wait_for_event(&events, &Event::Warning(kept.to_owned()));
    drop(emulator);
    assert_eq!(fs::read(&save).expect("the state's"), [2]);
    assert_eq!(
        fs::read(dir.join("game.sav.before-load")).expect("kept"),
        [20]
    );
    fs::remove_dir_all(&dir).expect("clean up");
}

/// Waits for the emulation thread to say it stopped, returning what
/// it said.
fn wait_for_stopped(events: &async_channel::Receiver<Event>) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match events.try_recv() {
            Ok(Event::Stopped(text)) => return text,
            Ok(_) => {}
            Err(_) => {
                assert!(Instant::now() < deadline, "never said it stopped");
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

/// Makes writing `save` fail, as a full or locked disk would, while
/// other files in its folder can still be written: a folder of its name
/// is in the way, which no file can be renamed over.
fn block(save: &Path) -> PathBuf {
    fs::create_dir(save).expect("in the way");
    save.to_owned()
}

#[test]
fn a_game_another_copy_of_the_app_has_open_is_refused_untouched() {
    let (dir, paths) = rom_dir("locked", &[("ruby.gba", &gba_rom())]);
    fs::write(dir.join("ruby.sav"), b"twenty hours").expect("write");
    // In a folder of their own, as they are in the app's data folder:
    // Windows lets no one else read a locked file, and `tree` reads all.
    let data = empty_dir("locked-data");
    let locks = data.join("Locks");
    // This copy opens it and keeps it for as long as it plays.
    let here = open_with(&paths, DmgColors::Grey, Some(&locks)).expect("opens");
    assert_eq!(here.locks.len(), 1);
    assert_eq!(here.unwritable, None);
    let before = tree(&dir);

    // Another copy, with the game's path spelled its own way.
    let respelled = [dir.join("./ruby.gba")];
    let refused = open_with(&respelled, DmgColors::Grey, Some(&locks))
        .err()
        .expect("refused");
    assert_eq!(
        format!("{refused:#}"),
        "ruby.gba is already open. If another copy of Imago Emulator is playing it, close it there first."
    );
    assert_eq!(tree(&dir), before, "nothing of it was read or written");
    // Nor linked to another game.
    let (other_dir, other) = rom_dir("locked-other", &[("sapphire.gba", &gba_rom())]);
    let linked = [other[0].clone(), paths[0].clone()];
    assert!(open_with(&linked, DmgColors::Grey, Some(&locks)).is_err());
    assert_eq!(
        fs::read(dir.join("ruby.sav")).expect("kept"),
        b"twenty hours"
    );
    assert!(!other_dir.join("sapphire.sav").exists());
    // The game that could have been held was let go again.
    drop(open_with(&other, DmgColors::Grey, Some(&locks)).expect("opens"));

    // Once the first copy closes the game, it opens.
    drop(here);
    let again = open_with(&paths, DmgColors::Grey, Some(&locks)).expect("opens");
    assert_eq!(again.locks.len(), 1);
    drop(again);
    // A game linked to itself is still refused for its shared save, not
    // as one open elsewhere.
    let twice = [paths[0].clone(), paths[0].clone()];
    let error = open_with(&twice, DmgColors::Grey, Some(&locks))
        .err()
        .expect("refused");
    assert!(
        format!("{error:#}").contains("would both save to"),
        "{error:#}"
    );
    // With nowhere to keep locks, games still open.
    fs::write(dir.join("file"), b"in the way").expect("write");
    let unlocked = open_with(&paths, DmgColors::Grey, Some(&dir.join("file/Locks")));
    assert!(unlocked.expect("opens").locks.is_empty());
    fs::remove_dir_all(&dir).expect("clean up");
    fs::remove_dir_all(&other_dir).expect("clean up");
    fs::remove_dir_all(&data).expect("clean up");
}

#[test]
fn a_wrong_file_chosen_to_go_with_a_game_this_copy_runs_is_what_the_failure_names() {
    let (dir, paths) = rom_dir(
        "locked-wrong",
        &[("ruby.gba", &gba_rom()), ("notes.txt", b"no game")],
    );
    let data = empty_dir("locked-wrong-data");
    let locks = data.join("Locks");
    let moved = dir.join("moved.gba");
    assert!(loadable(&paths[0]));
    assert!(!loadable(&paths[1]), "a file of no kind the app plays");
    assert!(!loadable(&moved), "a game that is no longer there");
    assert!(!loadable(&dir), "a folder that is no RPG Maker game");

    // The game runs, and holds its lock. Connect Second Console, with a
    // file that is no game: the game plays on behind the load, and the
    // failure says what is wrong with the file, not that the game is open.
    let here = open_with(&paths[..1], DmgColors::Grey, Some(&locks)).expect("opens");
    let wrong = open_with(&paths, DmgColors::Grey, Some(&locks))
        .err()
        .expect("refused");
    assert!(
        format!("{wrong:#}").contains("notes.txt is not a .gb, .gbc, .gba, .nds or .3ds file"),
        "{wrong:#}"
    );
    // Or with a game that has been moved away.
    let gone = open_with(&[paths[0].clone(), moved], DmgColors::Grey, Some(&locks))
        .err()
        .expect("refused");
    assert!(format!("{gone:#}").starts_with("cannot read "), "{gone:#}");
    assert!(format!("{gone:#}").contains("moved.gba"), "{gone:#}");
    // The game is as held as ever.
    let second = open_with(&paths[..1], DmgColors::Grey, Some(&locks))
        .err()
        .expect("refused");
    assert!(
        format!("{second:#}").contains("is already open"),
        "{second:#}"
    );
    drop(here);
    fs::remove_dir_all(&dir).expect("clean up");
    fs::remove_dir_all(&data).expect("clean up");
}

#[test]
fn the_library_names_a_game_s_files_as_its_session_does() {
    use crate::library::{self, Game};
    use crate::rom_info::{Platform, RomInfo};

    let dir = empty_dir("files-of-listed");
    // An RPG Maker game in a folder with a dot in its name, read by its
    // folder or by the player app in it.
    let folder = dir.join("Fan Game 1.2");
    fs::create_dir_all(folder.join("Player.app")).expect("the player's folder");
    fs::write(folder.join("Game.ini"), b"Title=Fan Game").expect("write");
    fs::write(folder.join("Game.exe"), b"player").expect("write");
    let listed = Game::rpg_maker(&folder, None);
    assert_eq!(library::files_of(&listed), files_of(&folder));
    assert_eq!(
        library::checkpoint_path(&listed),
        dir.join("Fan Game 1.2.ss0")
    );
    // Its files keep their names once the folder holds no game, or has
    // gone: no session can start to name them, but they are still there.
    fs::remove_file(folder.join("Game.ini")).expect("no game now");
    assert_eq!(files_of(&folder), GameFiles::of(&folder));
    assert_eq!(
        library::checkpoint_path(&listed),
        dir.join("Fan Game 1.2.ss0")
    );
    fs::remove_dir_all(&folder).expect("gone");
    assert_eq!(
        library::checkpoint_path(&listed),
        dir.join("Fan Game 1.2.ss0")
    );

    // A ROM: plain names, or with its extension once a namesake for
    // another system sits beside it, in the library as in a session.
    let info = |platform| RomInfo {
        platform,
        crc32: 1,
        serial: None,
        header_title: None,
        banner_title: None,
    };
    let rom = dir.join("platinum.nds");
    let listed = Game::new(&rom, &info(Platform::Nds));
    assert_eq!(library::files_of(&listed), files_of(&rom));
    assert_eq!(library::checkpoint_path(&listed), dir.join("platinum.ss0"));
    fs::write(dir.join("platinum.gba"), b"rom").expect("a namesake");
    assert_eq!(library::files_of(&listed), files_of(&rom));
    assert_eq!(
        library::checkpoint_path(&listed),
        dir.join("platinum.nds.ss0")
    );
    assert_eq!(
        files_of_listed(&rom, false).backup(),
        dir.join("platinum.nds.sav")
    );
    fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn a_game_in_a_folder_that_refuses_files_is_found_out_as_it_loads() {
    let dir = empty_dir("unwritable");
    let here = [GameFiles::of(&dir.join("game.gba"))];
    assert_eq!(unwritable(&here), None);
    assert_eq!(fs::read_dir(&dir).expect("read dir").count(), 0);
    // A DS game here with a GBA game from a folder that is gone.
    let gone = dir.join("gone");
    let both = [
        GameFiles::of(&dir.join("platinum.nds")),
        GameFiles::of(&gone.join("emerald.gba")),
    ];
    let why = unwritable(&both).expect("the second folder refuses");
    assert!(why.starts_with(&format!("{} cannot be written to (", gone.display())));
    assert!(
        why.ends_with(
            "so the app cannot keep this game's saves or save states there. Move the game to a folder you can write to."
        ),
        "{why}"
    );
    fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn a_panic_keeps_the_battery_save_and_says_the_game_stopped() {
    let dir = empty_dir("panic");
    let (emulator, events) = start_cart(&dir, 0, 1, Some(5));
    let said = wait_for_stopped(&events);
    assert!(said.contains("the cartridge caught fire"), "{said}");
    assert!(said.contains("are kept"), "{said}");
    assert_eq!(fs::read(dir.join("game.sav")).expect("written"), [5]);
    // The handle still stops cleanly.
    emulator.send(Command::TogglePause);
    drop(emulator);
    fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn a_panic_says_so_when_the_battery_save_could_not_be_written() {
    let dir = empty_dir("panic-unsaved");
    block(&dir.join("game.sav"));
    let (emulator, events) = start_cart(&dir, 0, 1, Some(5));
    let said = wait_for_stopped(&events);
    assert!(said.contains("may be lost"), "{said}");
    drop(emulator);
    fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn no_checkpoint_is_left_while_its_battery_save_cannot_be_written() {
    let dir = empty_dir("checkpoint-unsaved");
    let (state, record) = (dir.join("game.ss0"), dir.join("game.ss0.saves"));
    let (emulator, events) = start_cart(&dir, 0, 0, None);
    poke(&emulator, 0, 1);
    let in_the_way = block(&dir.join("game.sav"));
    emulator.send(Command::SaveCheckpoint(0));
    wait_for_notice(&events, "Could not save where you left off");
    assert!(!state.exists() && !record.exists());
    // Once the save can be written, so can the checkpoint.
    fs::remove_dir(&in_the_way).expect("out of the way");
    emulator.send(Command::SaveCheckpoint(0));
    drop(emulator);
    assert!(state.exists() && record.exists());
    assert_eq!(fs::read(dir.join("game.sav")).expect("written"), [1]);
    fs::remove_dir_all(&dir).expect("clean up");
}

#[test]
fn a_session_keeps_the_file_names_it_started_with() {
    let (dir, paths) = rom_dir("names-kept", &[("solo.gba", &gba_rom())]);
    let loaded = load(&paths, DmgColors::Grey).expect("loads");
    let (events, _events) = async_channel::unbounded();
    let emulator = Emulator::start(loaded, events, None).expect("the threads start");
    // A namesake arrives mid-game; this game's files stay as named.
    fs::write(dir.join("solo.nds"), nds_rom()).expect("written");
    emulator.send(Command::SaveCheckpoint(0));
    drop(emulator);
    assert!(dir.join("solo.ss0").exists());
    assert!(dir.join("solo.ss0.saves").exists());
    assert!(!dir.join("solo.gba.ss0").exists());
    fs::remove_dir_all(&dir).expect("clean up");
}
