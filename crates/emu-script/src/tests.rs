//! Scripts against a small test core.

use emu_core::debug::{CpuInfo, Instruction, Register, Stop};
use emu_core::{
    Button, Buttons, CoreInfo, Debugger, Frame, FrameRate, ScreenSize, StateError, System,
};

use super::*;

const SCREENS: [ScreenSize; 2] = [ScreenSize::new(4, 4), ScreenSize::new(4, 4)];

/// Instructions in one of the test core's frames.
const STEPS: u32 = 16;

/// A core with 256 bytes of RAM from address 0 and 16 of ROM from
/// 0x1000. Each frame runs [`STEPS`] instructions, counts itself in RAM
/// byte 0, and stores the buttons held in bytes 1 and 2. Its second CPU
/// sees RAM with the halves swapped.
struct TestCore {
    ram: [u8; 256],
    rom: [u8; 16],
    pc: u32,
    screens: [Frame; 2],
}

impl TestCore {
    fn new() -> Self {
        Self {
            ram: [0; 256],
            rom: *b"TEST ROM HEADER!",
            pc: 0,
            screens: [Frame::new(SCREENS[0]), Frame::new(SCREENS[1])],
        }
    }

    fn finish_frame(&mut self, inputs: &[Input]) {
        self.ram[0] = self.ram[0].wrapping_add(1);
        let held = inputs.first().map_or(0, |input| input.buttons.bits());
        self.ram[1..3].copy_from_slice(&held.to_le_bytes());
        self.screens[0].pixels_mut()[0] = u32::from(self.ram[0]);
    }

    fn index(cpu: usize, address: u32) -> u32 {
        if cpu == 1 { address ^ 0x80 } else { address }
    }
}

impl Core for TestCore {
    fn info(&self) -> CoreInfo {
        CoreInfo {
            system: System::Nds,
            screens: &SCREENS,
            touch_screen: Some(1),
            players: 1,
            frame_rate: FrameRate::new(60, 1),
            audio_sample_rate: 32_768,
            state_version: 1,
        }
    }

    fn run_frame(&mut self, inputs: &[Input], audio: &mut Vec<StereoSample>) {
        self.run_until(inputs, audio, &Stops::default());
    }

    fn screens(&self) -> &[Frame] {
        &self.screens
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn save_state(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.ram);
        out.extend_from_slice(&self.pc.to_le_bytes());
    }

    fn load_state(&mut self, state: &[u8]) -> Result<(), StateError> {
        let (ram, pc) = state.split_at_checked(256).ok_or(StateError::Truncated)?;
        self.ram.copy_from_slice(ram);
        self.pc = u32::from_le_bytes(pc.try_into().map_err(|_| StateError::Truncated)?);
        Ok(())
    }

    fn debugger(&mut self) -> Option<&mut dyn Debugger> {
        Some(self)
    }
}

impl Debugger for TestCore {
    fn cpus(&self) -> Vec<CpuInfo> {
        ["ONE", "TWO"]
            .map(|name| CpuInfo { name, player: 0 })
            .to_vec()
    }

    fn registers(&self, cpu: usize) -> Vec<Register> {
        let pc = if cpu == 0 { self.pc } else { 0 };
        vec![Register {
            name: "PC",
            value: pc,
            bits: 32,
        }]
    }

    fn status(&self, _: usize) -> String {
        String::new()
    }

    fn pc(&self, cpu: usize) -> u32 {
        if cpu == 0 { self.pc } else { 0 }
    }

    fn read(&self, cpu: usize, address: u32) -> Option<u8> {
        match address {
            0..0x100 => Some(self.ram[Self::index(cpu, address) as usize]),
            0x1000..0x1010 => Some(self.rom[address as usize - 0x1000]),
            _ => None,
        }
    }

    fn write(&mut self, cpu: usize, address: u32, value: u8) -> bool {
        let writable = address < 0x100;
        if writable {
            self.ram[Self::index(cpu, address) as usize] = value;
        }
        writable
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
        _: &mut Vec<StereoSample>,
        stops: &Stops,
    ) -> RunOutcome {
        loop {
            self.pc += 1;
            let frame_done = self.pc == STEPS;
            if frame_done {
                self.pc = 0;
                self.finish_frame(inputs);
            }
            let stop = stops
                .after_instruction(0, self.pc)
                .map(|reason| Stop { cpu: 0, reason });
            if frame_done || stop.is_some() {
                return RunOutcome { frame_done, stop };
            }
        }
    }
}

/// Starts `source` on `core`, failing the test if it does not start.
fn started(source: &str, core: &mut TestCore) -> Script {
    let mut script = Script::new("test.lua", source).expect("compiles");
    script.start(core).expect("starts");
    script
}

/// Runs one frame as a frontend would: with the script's buttons over
/// `held`, then the script's frame callbacks. Returns what it asked for.
fn frame(script: &mut Script, core: &mut TestCore, pending: &Output, held: Buttons) -> Output {
    let input = pending.input(
        0,
        Input {
            buttons: held,
            ..Input::default()
        },
    );
    let (outcome, called) = script.run_frame(core, &[input], &mut Vec::new(), &Stops::default());
    called.expect("exec callbacks run");
    assert!(outcome.frame_done);
    script
        .after_frame(core, &[input])
        .expect("frame callbacks run");
    script.take_output()
}

fn error_of(source: &str) -> String {
    let mut core = TestCore::new();
    let mut script = match Script::new("test.lua", source) {
        Ok(script) => script,
        Err(error) => return error.to_string(),
    };
    if let Err(error) = script.start(&mut core) {
        return error.to_string();
    }
    let mut pending = script.take_output();
    for _ in 0..3 {
        let input = pending.input(0, Input::default());
        let (_, called) = script.run_frame(&mut core, &[input], &mut Vec::new(), &Stops::default());
        if let Err(error) = called.and_then(|()| script.after_frame(&mut core, &[input])) {
            return error.to_string();
        }
        pending = script.take_output();
    }
    panic!("the script ran without an error")
}

#[test]
fn memory_reads_and_writes_little_endian_values() {
    let mut core = TestCore::new();
    core.ram[0x10..0x14].copy_from_slice(&[0x78, 0x56, 0x34, 0x12]);
    let mut script = started(
        r#"
        print(memory.read_u8(0x10), memory.read_u16(0x10), memory.read_u32(0x10))
        print(memory.read_u32(0x1000) == 0x54534554, memory.read_u8(0x2000))
        print(memory.write_u16(0x20, 0xBEEF), memory.write_u8(0x1000, 1))
        print(memory.write_u32(0x24, -1), memory.read_u8(0x10, 2))
        "#,
        &mut core,
    );
    assert_eq!(
        script.take_output().log,
        [
            "120\t22136\t305419896",
            "true\tnil",
            "true\tfalse",
            "true\t0",
        ]
        .map(String::from)
    );
    assert_eq!(core.ram[0x20..0x22], [0xEF, 0xBE]);
    assert_eq!(core.ram[0x24..0x28], [0xFF; 4]);
    assert_eq!(core.rom[0], b'T', "ROM is never written");
}

#[test]
fn the_second_cpu_sees_its_own_memory() {
    let mut core = TestCore::new();
    core.ram[0x90] = 7;
    let mut script = started("print(memory.read_u8(0x10, 2))", &mut core);
    assert_eq!(script.take_output().log, ["7"]);
}

#[test]
fn frame_callbacks_run_after_every_frame_and_count_it() {
    let mut core = TestCore::new();
    let mut script = started(
        r#"
        print("start", emu.frame(), emu.system())
        emu.on_frame(function() print("frame", emu.frame(), memory.read_u8(0)) end)
        "#,
        &mut core,
    );
    assert_eq!(script.take_output().log, ["start\t0\tnds"]);
    let first = frame(&mut script, &mut core, &Output::default(), Buttons::NONE);
    let second = frame(&mut script, &mut core, &first, Buttons::NONE);
    assert_eq!(first.log, ["frame\t1\t1"]);
    assert_eq!(second.log, ["frame\t2\t2"]);
    assert!(!script.finished());
}

#[test]
fn frameadvance_waits_for_the_next_frame() {
    let mut core = TestCore::new();
    let mut script = started(
        r#"
        for i = 1, 2 do
            print("before", emu.frame())
            emu.frameadvance()
        end
        print("done", emu.frame())
        "#,
        &mut core,
    );
    assert_eq!(script.take_output().log, ["before\t0"]);
    let first = frame(&mut script, &mut core, &Output::default(), Buttons::NONE);
    assert_eq!(first.log, ["before\t1"]);
    assert!(!script.finished());
    let second = frame(&mut script, &mut core, &first, Buttons::NONE);
    assert_eq!(second.log, ["done\t2"]);
    assert!(script.finished(), "nothing is left to run");
}

#[test]
fn joypad_set_presses_and_releases_on_the_next_frame_only() {
    let mut core = TestCore::new();
    let mut script = started(
        r#"
        emu.on_frame(function()
            local held = joypad.get()
            print(held.a, held.start, held.b)
            if emu.frame() == 1 then joypad.set({a = true, START = false}) end
        end)
        "#,
        &mut core,
    );
    let start: Buttons = [Button::Start].into_iter().collect();
    let first = frame(&mut script, &mut core, &Output::default(), start);
    let second = frame(&mut script, &mut core, &first, start);
    let third = frame(&mut script, &mut core, &second, start);
    assert_eq!(first.log, ["false\ttrue\tfalse"]);
    assert_eq!(
        second.log,
        ["true\tfalse\tfalse"],
        "A pressed, Start let go"
    );
    assert_eq!(third.log, ["false\ttrue\tfalse"], "for one frame only");
    let a_alone: Buttons = [Button::A].into_iter().collect();
    assert_eq!(second.input(0, Input::default()).buttons, Buttons::NONE);
    assert_eq!(first.input(0, Input::default()).buttons, a_alone);
}

#[test]
fn releases_win_over_presses_in_one_call() {
    let mut core = TestCore::new();
    let mut script = started("joypad.set({b = true, B = false, x = 1})", &mut core);
    let output = script.take_output();
    let held: Buttons = [Button::B].into_iter().collect();
    let expected: Buttons = [Button::X].into_iter().collect();
    assert_eq!(
        output
            .input(
                0,
                Input {
                    buttons: held,
                    ..Input::default()
                }
            )
            .buttons,
        expected
    );
}

#[test]
fn gui_calls_become_draws() {
    let mut core = TestCore::new();
    let mut script = started(
        r##"
        gui.text(1.5, 2, "HP " .. 42)
        gui.text(0, 0, 7, "#FF000080", 2)
        gui.box(3, 3, 1, 1)
        gui.box(0, 0, 0, 0, 0x00FF00, false)
        gui.box(0, 0, 3, 3, "blue", "red", 2)
        "##,
        &mut core,
    );
    assert_eq!(
        script.take_output().draws,
        [
            Draw::Text {
                screen: 0,
                x: 1,
                y: 2,
                text: "HP 42".into(),
                color: Color::WHITE,
            },
            Draw::Text {
                screen: 1,
                x: 0,
                y: 0,
                text: "7".into(),
                color: Color(0x80FF_0000),
            },
            Draw::Rect {
                screen: 0,
                x: 1,
                y: 1,
                width: 3,
                height: 3,
                fill: Color::NONE,
                outline: Color::WHITE,
            },
            Draw::Rect {
                screen: 0,
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                fill: Color(0xFF00_FF00),
                outline: Color::NONE,
            },
            Draw::Rect {
                screen: 1,
                x: 0,
                y: 0,
                width: 4,
                height: 4,
                fill: Color(0xFF00_00FF),
                outline: Color(0xFFFF_0000),
            },
        ]
    );
}

#[test]
fn drawing_is_per_frame() {
    let mut core = TestCore::new();
    let mut script = started(
        "emu.on_frame(function() if emu.frame() == 1 then gui.text(0, 0, 'once') end end)",
        &mut core,
    );
    let first = frame(&mut script, &mut core, &Output::default(), Buttons::NONE);
    let second = frame(&mut script, &mut core, &first, Buttons::NONE);
    assert_eq!(first.draws.len(), 1);
    assert!(second.draws.is_empty());
}

#[test]
fn print_and_console_log_write_like_lua_print() {
    let mut core = TestCore::new();
    let mut script = started(
        r#"
        console.log("a", 1, 2.5, nil, true)
        print(setmetatable({}, {__tostring = function() return "me" end}))
        tostring = nil
        print("still works")
        "#,
        &mut core,
    );
    assert_eq!(
        script.take_output().log,
        ["a\t1\t2.5\tnil\ttrue", "me", "still works"]
    );
}

#[test]
fn long_text_is_cut() {
    let mut core = TestCore::new();
    let mut script = started(
        r#"
        print(("x"):rep(5000))
        gui.text(0, 0, ("é"):rep(300))
        "#,
        &mut core,
    );
    let output = script.take_output();
    assert_eq!(output.log[0].chars().count(), 4_097);
    assert!(output.log[0].ends_with("x…"));
    let Draw::Text { text, .. } = &output.draws[0] else {
        panic!("text");
    };
    assert_eq!(text.chars().count(), 257);
}

#[test]
fn state_slots_and_pausing_are_requests() {
    let mut core = TestCore::new();
    let mut script = started(
        "emu.save_state(3); emu.load_state(1); emu.pause()",
        &mut core,
    );
    let output = script.take_output();
    assert_eq!(
        output.states,
        [StateRequest::Save(3), StateRequest::Load(1)]
    );
    assert!(output.pause);
    assert_eq!(script.take_output(), Output::default(), "taken once");
}

/// A script used to queue as many requests as its instruction budget
/// let it make, tens of millions, each a state for the frontend to save
/// or load before the next frame.
#[test]
fn state_requests_past_the_first_few_are_left_out() {
    let mut core = TestCore::new();
    let mut script = started(
        r#"
        for turn = 1, 100000 do
            emu.save_state(turn % 9 + 1)
            emu.load_state(turn % 9 + 1)
        end
        emu.on_frame(function() emu.load_state(4) end)
        "#,
        &mut core,
    );
    let output = script.take_output();
    assert_eq!(output.states.len(), 18);
    // The first are kept, in the order they were asked for.
    assert_eq!(
        output.states[..3],
        [
            StateRequest::Save(2),
            StateRequest::Load(2),
            StateRequest::Save(3)
        ]
    );
    assert_eq!(
        output.log,
        ["(more than 18 states asked for since the last frame; the rest are left out)"]
    );
    // The count starts again once the frontend has taken them.
    let output = frame(&mut script, &mut core, &Output::default(), Buttons::NONE);
    assert_eq!(output.states, [StateRequest::Load(4)]);
    assert!(output.log.is_empty());
}

/// The machine stands still between a script's requests, so the same one
/// twice in a row is one.
#[test]
fn asking_for_the_same_state_twice_in_a_row_asks_once() {
    let mut core = TestCore::new();
    let mut script = started(
        r#"
        for _ = 1, 1000 do emu.save_state(1) end
        emu.load_state(2)
        emu.load_state(2)
        emu.save_state(1)
        emu.save_state(2)
        "#,
        &mut core,
    );
    let output = script.take_output();
    assert_eq!(
        output.states,
        [
            StateRequest::Save(1),
            StateRequest::Load(2),
            StateRequest::Save(1),
            StateRequest::Save(2)
        ]
    );
    assert!(output.log.is_empty());
}

#[test]
fn exec_callbacks_run_where_the_cpu_is_about_to_be() {
    let mut core = TestCore::new();
    let mut script = started(
        r#"
        emu.on_exec(5, function(address, cpu)
            print("exec", address, cpu, emu.registers().PC, memory.read_u8(0))
        end)
        "#,
        &mut core,
    );
    assert_eq!(script.breakpoints(), [Breakpoint { cpu: 0, address: 5 }]);
    let first = frame(&mut script, &mut core, &Output::default(), Buttons::NONE);
    let second = frame(&mut script, &mut core, &first, Buttons::NONE);
    assert_eq!(first.log, ["exec\t5\t1\t5\t0"]);
    assert_eq!(second.log, ["exec\t5\t1\t5\t1"]);
    assert_eq!(core.ram[0], 2, "both frames finished");
}

#[test]
fn the_frontends_own_breakpoints_still_stop_the_frame() {
    let mut core = TestCore::new();
    let mut script = started("emu.on_exec(5, function() print('exec') end)", &mut core);
    let stops = Stops {
        breakpoints: vec![
            Breakpoint { cpu: 0, address: 5 },
            Breakpoint { cpu: 0, address: 9 },
        ],
        ..Stops::default()
    };
    let input = [Input::default()];
    let (outcome, called) = script.run_frame(&mut core, &input, &mut Vec::new(), &stops);
    called.expect("callback runs");
    assert_eq!(
        outcome.stop.map(|stop| stop.reason),
        Some(StopReason::Breakpoint)
    );
    assert_eq!(core.pc, 5, "stopped after the callback ran");
    assert_eq!(script.take_output().log, ["exec"]);
    let (outcome, _) = script.run_frame(&mut core, &input, &mut Vec::new(), &stops);
    assert_eq!(core.pc, 9);
    assert!(!outcome.frame_done);
    let (outcome, _) = script.run_frame(&mut core, &input, &mut Vec::new(), &stops);
    assert!(outcome.frame_done && outcome.stop.is_none());
    assert!(script.take_output().log.is_empty(), "no callback at 9");
}

#[test]
fn a_step_onto_a_scripts_address_still_calls_it_once() {
    let mut core = TestCore::new();
    let mut script = started("emu.on_exec(1, function() print('exec') end)", &mut core);
    let step = Stops {
        step: Some(0),
        ..Stops::default()
    };
    let input = [Input::default()];
    let (outcome, called) = script.run_frame(&mut core, &input, &mut Vec::new(), &step);
    called.expect("callback runs");
    assert_eq!(outcome.stop.map(|stop| stop.reason), Some(StopReason::Step));
    assert_eq!(core.pc, 1);
    assert_eq!(script.take_output().log, ["exec"]);
    // Carrying on runs the instruction there, without calling again.
    let (outcome, _) = script.run_frame(&mut core, &input, &mut Vec::new(), &Stops::default());
    assert!(outcome.frame_done && outcome.stop.is_none());
    assert!(script.take_output().log.is_empty());
}

#[test]
fn errors_name_the_script_and_line() {
    assert_eq!(
        error_of("x = = 1"),
        "test.lua:1: unexpected symbol near '='"
    );
    assert_eq!(
        error_of("\nlocal t = nil\nprint(t.field)"),
        "test.lua:3: attempt to index a nil value (local 't')"
    );
    assert_eq!(
        error_of("emu.on_frame(function()\n  error('boom')\nend)"),
        "test.lua:2: boom"
    );
    assert_eq!(
        error_of("memory.read_u8('here')"),
        "test.lua:1: memory.read_u8: the address must be a whole number from 0 to 0xFFFFFFFF, \
         not \"here\""
    );
    assert_eq!(
        error_of("\n\ngui.text(0, 0, 'hi', 'white', 3)"),
        "test.lua:3: gui.text: the screen must be 1 to 2, not 3"
    );
    assert_eq!(
        error_of("joypad.set({turbo = true})"),
        "test.lua:1: joypad.set: there is no button \"turbo\"; the buttons are a, b, x, y, l, r, \
         up, down, left, right, start and select"
    );
    assert_eq!(
        error_of("emu.save_state(10)"),
        "test.lua:1: emu.save_state: the slot must be 1 to 9, not 10"
    );
    assert_eq!(
        error_of("gui.box(0, 0, 1, 1, 'mauve')"),
        "test.lua:1: gui.box: \"mauve\" is not a colour; give 0xRRGGBB, 0xAARRGGBB, \
         \"#RRGGBBAA\" or a name such as \"red\""
    );
    assert_eq!(
        error_of("emu.on_frame(function() emu.frameadvance() end)"),
        "emu.frameadvance waits for a frame only in the script's main body, not in a callback"
    );
}

#[test]
fn the_sandbox_has_no_files_processes_or_clock() {
    let mut core = TestCore::new();
    let mut script = started(
        r#"
        print(io, os, require, package, debug, dofile, loadfile, string.dump)
        print(load("return 1 + 1")())
        local compiled = "\27Lua" .. string.rep("\0", 32)
        print(load(compiled))
        "#,
        &mut core,
    );
    let log = script.take_output().log;
    assert_eq!(log[0], ["nil"; 8].join("\t"));
    assert_eq!(log[1], "2");
    assert!(
        log[2].starts_with("nil\tattempt to load a binary chunk"),
        "{}",
        log[2]
    );
}

#[test]
fn a_script_stuck_in_a_loop_is_stopped() {
    let error = error_of("while true do end");
    assert!(error.contains("is it stuck in a loop?"), "{error}");
    assert!(error.starts_with("test.lua:1: "), "{error}");
}

#[test]
fn pcall_cannot_catch_the_loop_limit() {
    let error = error_of(
        r#"
        emu.on_frame(function()
            while true do pcall(function() while true do end end) end
        end)
        "#,
    );
    assert!(error.contains("is it stuck in a loop?"), "{error}");
    let error = error_of(
        r#"
        local spin = coroutine.wrap(function() while true do end end)
        while true do pcall(spin) end
        "#,
    );
    assert!(error.contains("is it stuck in a loop?"), "{error}");
}

#[test]
fn loops_inside_library_calls_are_stopped() {
    // A coroutine cannot yield from inside these, so the limit unwinds.
    for source in [
        r#"string.gsub("x", ".", function() while true do end end)"#,
        r#"string.gsub("x", ".", function()
            while true do pcall(function() while true do end end) end
        end)"#,
        r#"table.sort({3, 2, 1}, function() while true do end end)"#,
        r#"print(setmetatable({}, {__tostring = function() while true do end end}))"#,
        r#"string.gsub("x", ".", function()
            while true do
                coroutine.resume(coroutine.create(function()
                    string.gsub("y", ".", function() while true do end end)
                end))
            end
        end)"#,
        r#"emu.on_frame(function()
            table.sort({1, 2}, function() while true do pcall(error) end end)
        end)"#,
    ] {
        let error = error_of(source);
        assert!(
            error.contains("is it stuck in a loop?"),
            "{source}: {error}"
        );
    }
}

#[test]
fn a_stopped_script_can_be_dropped_and_another_run() {
    let mut core = TestCore::new();
    let mut script = Script::new(
        "test.lua",
        "string.gsub('x', '.', function() while true do end end)",
    )
    .expect("compiles");
    assert!(script.start(&mut core).is_err());
    drop(script);
    let mut script = started("print(memory.read_u8(0))", &mut core);
    assert_eq!(script.take_output().log, ["0"]);
}

#[test]
fn memory_is_limited() {
    let error = error_of("local t = {} for i = 1, 1e9 do t[i] = ('x'):rep(1000) .. i end");
    assert!(error.contains("more than 64 MiB"), "{error}");
}

#[test]
fn the_same_script_and_game_run_the_same_way() {
    let source = r#"
        local seen = {}
        emu.on_frame(function()
            local roll = math.random(1, 6)
            seen[#seen + 1] = roll
            memory.write_u8(0x40 + emu.frame(), roll)
            if roll > 3 then joypad.set({a = true}) end
            gui.text(0, 0, table.concat(seen, ","))
            print(roll, memory.read_u16(1))
        end)
    "#;
    let run = || {
        let mut core = TestCore::new();
        let mut script = started(source, &mut core);
        let mut outputs = vec![script.take_output()];
        for _ in 0..30 {
            let pending = outputs.last().cloned().unwrap_or_default();
            outputs.push(frame(&mut script, &mut core, &pending, Buttons::NONE));
        }
        let mut state = Vec::new();
        core.save_state(&mut state);
        (outputs, state)
    };
    assert_eq!(run(), run());
}

#[test]
fn a_failed_callback_still_reports_the_frame() {
    let mut core = TestCore::new();
    let mut script = started("emu.on_exec(15, function() error('no') end)", &mut core);
    let (outcome, called) = script.run_frame(
        &mut core,
        &[Input::default()],
        &mut Vec::new(),
        &Stops::default(),
    );
    assert!(called.is_err());
    assert!(!outcome.frame_done, "the frame is still under way");
    assert_eq!(
        outcome.stop, None,
        "and nothing of the frontend's stopped it"
    );
}

#[test]
fn scripts_cannot_set_finalizers() {
    // Lua runs __gc with hooks off, where no limit could stop a loop.
    assert_eq!(
        error_of("local t = setmetatable({}, {__gc = function() while true do end end})"),
        "test.lua:1: setmetatable: scripts cannot use __gc"
    );
    let mut core = TestCore::new();
    let mut script = started(
        r#"
        local t = setmetatable({}, {__index = function(_, key) return key end})
        print(t.anything)
        "#,
        &mut core,
    );
    assert_eq!(script.take_output().log, ["anything"]);
}

#[test]
fn metamethods_may_call_back_in() {
    let mut core = TestCore::new();
    let mut script = started(
        r#"
        local shown = setmetatable({}, {__tostring = function()
            emu.on_frame(function() end)
            return "frame " .. emu.frame() .. " byte " .. memory.read_u8(0)
        end})
        print(shown)
        gui.text(0, 0, shown)
        "#,
        &mut core,
    );
    let output = script.take_output();
    assert_eq!(output.log, ["frame 0 byte 0"]);
    assert!(matches!(&output.draws[0], Draw::Text { text, .. } if text == "frame 0 byte 0"));
}
