//! Lua scripts that run alongside a game (PRD UI-14), in the spirit of
//! BizHawk's, mGBA's and DeSmuME's: they read and write memory, press
//! buttons, save and load states and draw over the screens.
//!
//! ```lua
//! emu.on_frame(function()
//!     gui.text(2, 2, "frame " .. emu.frame())
//!     gui.text(2, 10, ("HP %d"):format(memory.read_u8(0xD16C)))
//! end)
//! ```
//!
//! A [`Script`] runs on the emulation thread, between frames, so it sees
//! the machine stand still and never races the core. The frontend calls
//! [`Script::start`] once, then [`Script::after_frame`] after every frame
//! it runs, and collects what the script asked for with
//! [`Script::take_output`]: log lines, shapes to draw over the screens,
//! buttons for the next frame, save states and pausing. That is plain
//! data ([`Output`]) the frontend acts on, so this crate knows nothing of
//! windows, files or threads.
//!
//! The functions scripts call are documented in `docs/scripting.md`.
//! Players, screens and CPUs are numbered from 1 in Lua, as Lua numbers
//! everything; the Rust side numbers them from 0, as the core does.
//!
//! # Determinism
//!
//! The same script with the same game and inputs makes the same calls and
//! the same frames: scripts cannot read the clock or files, and
//! `math.random` always starts from the same seed. (Lua does not promise an
//! order for `pairs` over string keys, so a script whose output depends on
//! that order may differ between runs.)
//!
//! # Sandbox
//!
//! Scripts get Lua's base library, `coroutine`, `table`, `string`, `utf8`
//! and `math`, but no `io`, `os`, `require` or `debug`, and `load` reads
//! source text only. A script may use 64 MiB of memory and run 100 million
//! instructions each time it is called, which stops scripts stuck in a
//! loop. Any error stops the script with a [`ScriptError`]; nothing a
//! script does can bring down the frontend.

mod api;
mod draw;
mod error;
mod joypad;
mod memory;
mod sandbox;

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::panic::{self, AssertUnwindSafe};

use emu_core::debug::{Breakpoint, RunOutcome, StopReason, Stops};
use emu_core::{Core, Input, StereoSample};
use mlua::chunk::ChunkMode;
use mlua::thread::ThreadStatus;
use mlua::{Function, IntoLuaMulti, Lua, Table, Thread};

pub use draw::{Color, Draw};
pub use error::ScriptError;
pub use joypad::Joypad;

/// A save state a script asked for, by slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateRequest {
    /// Save the machine to this slot.
    Save(u8),
    /// Load the machine from this slot.
    Load(u8),
}

/// What a script asked of the frontend since it was last asked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Output {
    /// Lines from `console.log` and `print`, oldest first.
    pub log: Vec<String>,
    /// What the `gui` functions drew, oldest first: drawn over the screens
    /// until the next frame's drawing replaces it.
    pub draws: Vec<Draw>,
    /// Per player, from player 0: buttons to press or release on the next
    /// frame. Players past the end press nothing.
    pub joypad: Vec<Joypad>,
    /// States to save or load before the next frame, in order.
    pub states: Vec<StateRequest>,
    /// The script asked to pause.
    pub pause: bool,
}

impl Output {
    /// `input`, what `player` holds, with the script's presses and
    /// releases laid over it.
    #[must_use]
    pub fn input(&self, player: usize, input: Input) -> Input {
        let pad = self.joypad.get(player).copied().unwrap_or_default();
        Input {
            buttons: pad.apply(input.buttons),
            ..input
        }
    }
}

/// A Lua script, compiled and waiting to run against a core.
///
/// Every method that runs Lua code takes the core, which the script reads
/// and writes only while that call lasts. A method that fails leaves the
/// script stopped for good: drop it.
pub struct Script {
    lua: Lua,
    /// The script's main body, until it ends: it runs in
    /// [`Script::start`], and again after each frame from wherever
    /// `emu.frameadvance` paused it.
    main: Option<Thread>,
}

impl Script {
    /// Compiles `source` in a fresh sandbox, naming it `name` in error
    /// messages. Nothing runs yet.
    ///
    /// # Errors
    ///
    /// Fails if the source has a syntax error.
    pub fn new(name: &str, source: &str) -> Result<Self, ScriptError> {
        let compile = || -> mlua::Result<Self> {
            let lua = sandbox::new()?;
            api::install(&lua)?;
            let main = lua
                .load(source)
                .set_name(format!("@{name}"))
                .set_mode(ChunkMode::Text)
                .into_function()?;
            let main = lua.create_thread(main)?;
            Ok(Self {
                lua,
                main: Some(main),
            })
        };
        compile().map_err(|error| ScriptError::from_lua(&error))
    }

    /// Runs the script's main body against `core`, until it ends or waits
    /// for a frame with `emu.frameadvance`. Call it once, before the first
    /// frame.
    ///
    /// # Errors
    ///
    /// Fails if the script raises an error or breaks a sandbox limit.
    pub fn start(&mut self, core: &mut dyn Core) -> Result<(), ScriptError> {
        let machine = api::Machine::of(core);
        self.lua_error(api::state_mut(&self.lua).map(|mut state| state.machine = machine))?;
        self.enter(core, |_, main| match main {
            Some(main) => resume(main, ()),
            None => Ok(()),
        })
    }

    /// Tells the script a frame has finished, after which every player
    /// held `inputs`: its main body carries on from `emu.frameadvance`,
    /// then each `emu.on_frame` callback runs.
    ///
    /// # Errors
    ///
    /// Fails if the script raises an error or breaks a sandbox limit.
    pub fn after_frame(
        &mut self,
        core: &mut dyn Core,
        inputs: &[Input],
    ) -> Result<(), ScriptError> {
        let callbacks = self.lua_error(api::state_mut(&self.lua).map(|mut state| {
            state.frame += 1;
            state.inputs = inputs.to_vec();
            state.on_frame.clone()
        }))?;
        self.enter(core, |lua, main| {
            if let Some(main) = main {
                resume(main, ())?;
            }
            for callback in &callbacks {
                call(lua, callback, ())?;
            }
            Ok(())
        })
    }

    /// Where the script's `emu.on_exec` callbacks wait: stop there and
    /// call [`Script::at_breakpoint`], or let [`Script::run_frame`] do it.
    #[must_use]
    pub fn breakpoints(&self) -> Vec<Breakpoint> {
        let mut breakpoints = Vec::new();
        if let Ok(state) = api::state(&self.lua) {
            for (at, _) in &state.on_exec {
                if !breakpoints.contains(at) {
                    breakpoints.push(*at);
                }
            }
        }
        breakpoints
    }

    /// Calls the `emu.on_exec` callbacks waiting at `breakpoint`, whose
    /// CPU is about to run the instruction there.
    ///
    /// # Errors
    ///
    /// Fails if the script raises an error or breaks a sandbox limit.
    pub fn at_breakpoint(
        &mut self,
        core: &mut dyn Core,
        breakpoint: Breakpoint,
    ) -> Result<(), ScriptError> {
        let callbacks: Vec<Function> = self.lua_error(api::state(&self.lua).map(|state| {
            let waiting = state.on_exec.iter().filter(|(at, _)| *at == breakpoint);
            waiting.map(|(_, callback)| callback.clone()).collect()
        }))?;
        if callbacks.is_empty() {
            return Ok(());
        }
        let cpu = breakpoint.cpu + 1;
        self.enter(core, |lua, _| {
            for callback in &callbacks {
                call(lua, callback, (breakpoint.address, cpu))?;
            }
            Ok(())
        })
    }

    /// Runs the rest of `core`'s frame as
    /// [`Debugger::run_until`](emu_core::Debugger::run_until) does with
    /// `stops`, calling the script's `emu.on_exec` callbacks whenever a
    /// CPU reaches their address. Returns where `stops` stopped it, or the
    /// frame's end; a stop of the script's own never ends the run. With no
    /// callbacks and no `stops`, the frame runs at full speed.
    ///
    /// The result says whether a callback failed; the outcome is right
    /// either way, so a frontend can drop the script and carry on.
    pub fn run_frame(
        &mut self,
        core: &mut dyn Core,
        inputs: &[Input],
        audio: &mut Vec<StereoSample>,
        stops: &Stops,
    ) -> (RunOutcome, Result<(), ScriptError>) {
        loop {
            let waiting = self.breakpoints();
            let mut all = stops.clone();
            all.breakpoints.extend(&waiting);
            let outcome = match core.debugger() {
                Some(debugger) if !all.is_empty() => debugger.run_until(inputs, audio, &all),
                _ => {
                    core.run_frame(inputs, audio);
                    let done = RunOutcome {
                        frame_done: true,
                        stop: None,
                    };
                    return (done, Ok(()));
                }
            };
            let Some(stop) = outcome
                .stop
                .filter(|stop| stop.reason == StopReason::Breakpoint)
            else {
                return (outcome, Ok(()));
            };
            let Some(address) = core.debugger().map(|debugger| debugger.pc(stop.cpu)) else {
                return (outcome, Ok(()));
            };
            let at = Breakpoint {
                cpu: stop.cpu,
                address,
            };
            if !waiting.contains(&at) {
                return (outcome, Ok(()));
            }
            let called = self.at_breakpoint(core, at);
            let theirs = stops.breakpoints.contains(&at);
            if called.is_err() || theirs || outcome.frame_done {
                let outcome = RunOutcome {
                    frame_done: outcome.frame_done,
                    stop: outcome.stop.filter(|_| theirs),
                };
                return (outcome, called);
            }
        }
    }

    /// Everything the script asked for since this was last called.
    pub fn take_output(&mut self) -> Output {
        api::state_mut(&self.lua)
            .map(|mut state| std::mem::take(&mut state.output))
            .unwrap_or_default()
    }

    /// Whether the script has nothing left to do: its main body ended and
    /// it left no callbacks.
    #[must_use]
    pub fn finished(&self) -> bool {
        self.main.is_none()
            && api::state(&self.lua)
                .is_ok_and(|state| state.on_frame.is_empty() && state.on_exec.is_empty())
    }

    /// Runs `run` with the core lent to the script's `memory` and
    /// `emu.registers` functions, and with a full instruction budget.
    /// `run` gets the main body, if it is waiting to carry on.
    ///
    /// Running out of instructions unwinds out of Lua (see `sandbox.rs`),
    /// and is caught here; so is any other panic, so that nothing a script
    /// does takes the frontend's thread down with it.
    fn enter(
        &mut self,
        core: &mut dyn Core,
        run: impl FnOnce(&Lua, Option<&Thread>) -> mlua::Result<()>,
    ) -> Result<(), ScriptError> {
        sandbox::refill(&self.lua);
        let main = self
            .main
            .clone()
            .filter(|main| main.status() == ThreadStatus::Resumable);
        let core = RefCell::new(core);
        let lua = &self.lua;
        let lent = panic::catch_unwind(AssertUnwindSafe(|| {
            lua.scope(|scope| {
                let host: Table = lua.named_registry_value(api::HOST)?;
                host.raw_set(
                    "read",
                    scope.create_function(|_, (cpu, address, bytes): (usize, u32, u32)| {
                        Ok(memory::read(&mut **core.borrow_mut(), cpu, address, bytes))
                    })?,
                )?;
                host.raw_set(
                    "write",
                    scope.create_function(
                        |_, (cpu, address, bytes, value): (usize, u32, u32, u32)| {
                            Ok(memory::write(
                                &mut **core.borrow_mut(),
                                cpu,
                                address,
                                bytes,
                                value,
                            ))
                        },
                    )?,
                )?;
                host.raw_set(
                    "registers",
                    scope.create_function(|lua, cpu: usize| {
                        // Let go of the core before building the table: Lua
                        // may run a finalizer that reads memory as it
                        // allocates.
                        let registers = match core.borrow_mut().debugger() {
                            Some(debugger) => debugger.registers(cpu),
                            None => Vec::new(),
                        };
                        let table = lua.create_table()?;
                        for register in registers {
                            table.raw_set(register.name, register.value)?;
                        }
                        Ok(table)
                    })?,
                )?;
                run(lua, main.as_ref())
            })
        }));
        let result = match lent {
            Ok(result) => result.map_err(|error| ScriptError::from_lua(&error)),
            Err(payload) => Err(ScriptError::from_panic(sandbox::spent(lua), &*payload)),
        };
        if self
            .main
            .as_ref()
            .is_some_and(|main| main.status() != ThreadStatus::Resumable)
        {
            self.main = None;
        }
        result
    }

    fn lua_error<T>(&self, result: mlua::Result<T>) -> Result<T, ScriptError> {
        result.map_err(|error| ScriptError::from_lua(&error))
    }
}

/// Carries on `thread` with `args` until it yields or ends. Fails if it
/// raised an error.
fn resume(thread: &Thread, args: impl IntoLuaMulti) -> mlua::Result<()> {
    thread.resume::<()>(args)
}

/// Calls `function` with `args` in a coroutine of its own, so that one
/// calling `emu.frameadvance` can be told why that fails.
fn call(lua: &Lua, function: &Function, args: impl IntoLuaMulti) -> mlua::Result<()> {
    let thread = lua.create_thread(function.clone())?;
    resume(&thread, args)?;
    if thread.status() == ThreadStatus::Resumable {
        return Err(mlua::Error::runtime(
            "emu.frameadvance waits for a frame only in the script's main body, not in a callback",
        ));
    }
    Ok(())
}
