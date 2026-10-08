//! The functions scripts call: the `emu`, `memory`, `joypad`, `gui` and
//! `console` tables, and `print`.
//!
//! None of them touch the core directly. What a script asks for collects
//! in [`State::output`] as plain data, and the three that need the core
//! (`memory` reads and writes, `emu.registers`) call functions the
//! [`crate::Script`] lends the registry's host table only while it runs
//! the script with a core in hand.

use std::fmt::Display;

use emu_core::debug::Breakpoint;
use emu_core::{Button, Buttons, Core, Input};
use mlua::{AppDataRef, AppDataRefMut, Function, Lua, Table, Value, Variadic};

use crate::{Color, Draw, Joypad, Output, StateRequest};

/// Registry key of the table that holds the core's functions while a
/// script runs: `read`, `write` and `registers`.
pub const HOST: &str = "emu_script.host";

/// Registry key of Lua's own `tostring`, which a script cannot replace
/// for `console.log`.
const TOSTRING: &str = "emu_script.tostring";

/// Drawing past this many shapes in one frame draws nothing more.
const MOST_DRAWS: usize = 10_000;

/// Logging past this many lines between frames logs one note, then
/// nothing more.
const MOST_LINES: usize = 1_000;

/// Text drawn longer than this many characters is cut.
const LONGEST_TEXT: usize = 256;

/// Save-state slots scripts can use, as in the app's `.ss1` to `.ss9`.
const SLOTS: std::ops::RangeInclusive<i64> = 1..=9;

/// Facts about the machine a script runs on, read when it starts.
#[derive(Debug, Clone, Copy, Default)]
pub struct Machine {
    /// `"gb"`, `"gba"` or `"nds"`.
    pub system: &'static str,
    pub players: usize,
    pub screens: usize,
    /// CPUs the debugger knows; 0 for a core without one.
    pub cpus: usize,
}

impl Machine {
    pub fn of(core: &mut dyn Core) -> Self {
        let info = core.info();
        Self {
            system: info.system.id(),
            players: info.players.max(1),
            screens: info.screens.len(),
            cpus: core.debugger().map_or(0, |debugger| debugger.cpus().len()),
        }
    }
}

/// What a script's functions keep between calls.
#[derive(Default)]
pub struct State {
    pub machine: Machine,
    /// Frames finished since the script started.
    pub frame: u64,
    /// The input each player's last frame ran with.
    pub inputs: Vec<Input>,
    /// What the script asked for since the frontend last took it.
    pub output: Output,
    pub on_frame: Vec<Function>,
    pub on_exec: Vec<(Breakpoint, Function)>,
}

/// Puts the script's tables in `lua`'s globals and an empty [`State`] in
/// its app data.
pub fn install(lua: &Lua) -> mlua::Result<()> {
    lua.set_app_data(State::default());
    lua.set_named_registry_value(HOST, lua.create_table()?)?;
    let globals = lua.globals();
    lua.set_named_registry_value(TOSTRING, globals.get::<Function>("tostring")?)?;
    globals.set("emu", emu(lua)?)?;
    globals.set("memory", memory(lua)?)?;
    globals.set("joypad", joypad(lua)?)?;
    globals.set("gui", gui(lua)?)?;
    let console = console(lua)?;
    globals.set("print", console.get::<Function>("log")?)?;
    globals.set("console", console)?;
    Ok(())
}

/// The script's state, or an error rather than a panic if it is in use:
/// Lua can run a finalizer while the state is borrowed.
pub fn state(lua: &Lua) -> mlua::Result<AppDataRef<'_, State>> {
    lua.try_app_data_ref()
        .ok()
        .flatten()
        .ok_or_else(|| mlua::Error::runtime("the script's state is busy"))
}

/// The script's state to change; see [`state`].
pub fn state_mut(lua: &Lua) -> mlua::Result<AppDataRefMut<'_, State>> {
    lua.try_app_data_mut()
        .ok()
        .flatten()
        .ok_or_else(|| mlua::Error::runtime("the script's state is busy"))
}

fn emu(lua: &Lua) -> mlua::Result<Table> {
    let emu = lua.create_table()?;
    emu.set(
        "frame",
        lua.create_function(|lua, ()| Ok(i64::try_from(state(lua)?.frame).unwrap_or(i64::MAX)))?,
    )?;
    emu.set(
        "system",
        lua.create_function(|lua, ()| Ok(state(lua)?.machine.system))?,
    )?;
    // Yields the script's main coroutine, which the next frame resumes.
    let frameadvance = lua
        .load("local yield = coroutine.yield\nreturn function() yield() end")
        .set_name("=emu.frameadvance")
        .eval::<Function>()?;
    emu.set("frameadvance", frameadvance)?;
    emu.set(
        "on_frame",
        lua.create_function(|lua, callback: Value| {
            let callback = function(lua, "emu.on_frame", callback)?;
            state_mut(lua)?.on_frame.push(callback);
            Ok(())
        })?,
    )?;
    emu.set(
        "on_exec",
        lua.create_function(|lua, (address, callback, cpu): (Value, Value, Value)| {
            const NAME: &str = "emu.on_exec";
            let address = self::address(lua, NAME, &address)?;
            let callback = function(lua, NAME, callback)?;
            let cpus = state(lua)?.machine.cpus;
            if cpus == 0 {
                return Err(fail(lua, NAME, "this core cannot stop at an address"));
            }
            let cpu = numbered(lua, NAME, "the CPU", &cpu, cpus)?;
            state_mut(lua)?
                .on_exec
                .push((Breakpoint { cpu, address }, callback));
            Ok(())
        })?,
    )?;
    emu.set(
        "pause",
        lua.create_function(|lua, ()| {
            state_mut(lua)?.output.pause = true;
            Ok(())
        })?,
    )?;
    emu.set(
        "save_state",
        lua.create_function(|lua, slot: Value| {
            let slot = slot_number(lua, "emu.save_state", &slot)?;
            state_mut(lua)?.output.states.push(StateRequest::Save(slot));
            Ok(())
        })?,
    )?;
    emu.set(
        "load_state",
        lua.create_function(|lua, slot: Value| {
            let slot = slot_number(lua, "emu.load_state", &slot)?;
            state_mut(lua)?.output.states.push(StateRequest::Load(slot));
            Ok(())
        })?,
    )?;
    emu.set(
        "registers",
        lua.create_function(|lua, cpu: Value| {
            let cpus = state(lua)?.machine.cpus.max(1);
            let cpu = numbered(lua, "emu.registers", "the CPU", &cpu, cpus)?;
            host(lua, "registers")?.call::<Table>(cpu)
        })?,
    )?;
    Ok(emu)
}

fn memory(lua: &Lua) -> mlua::Result<Table> {
    const READS: [(&str, &str, u32); 3] = [
        ("read_u8", "memory.read_u8", 1),
        ("read_u16", "memory.read_u16", 2),
        ("read_u32", "memory.read_u32", 4),
    ];
    const WRITES: [(&str, &str, u32); 3] = [
        ("write_u8", "memory.write_u8", 1),
        ("write_u16", "memory.write_u16", 2),
        ("write_u32", "memory.write_u32", 4),
    ];
    let memory = lua.create_table()?;
    for (key, name, bytes) in READS {
        let read = lua.create_function(move |lua, (address, cpu): (Value, Value)| {
            let address = self::address(lua, name, &address)?;
            let cpu = cpu_number(lua, name, &cpu)?;
            host(lua, "read")?.call::<Option<u32>>((cpu, address, bytes))
        })?;
        memory.set(key, read)?;
    }
    for (key, name, bytes) in WRITES {
        let write =
            lua.create_function(move |lua, (address, value, cpu): (Value, Value, Value)| {
                let address = self::address(lua, name, &address)?;
                // Only the low bytes are written, so -1 is all ones.
                let value = whole(&value).ok_or_else(|| {
                    fail(
                        lua,
                        name,
                        format!("the value must be a whole number, not {}", shown(&value)),
                    )
                })? as u32;
                let cpu = cpu_number(lua, name, &cpu)?;
                host(lua, "write")?.call::<bool>((cpu, address, bytes, value))
            })?;
        memory.set(key, write)?;
    }
    Ok(memory)
}

fn joypad(lua: &Lua) -> mlua::Result<Table> {
    let joypad = lua.create_table()?;
    joypad.set(
        "get",
        lua.create_function(|lua, player: Value| {
            let players = state(lua)?.machine.players;
            let player = numbered(lua, "joypad.get", "the player", &player, players)?;
            let held = state(lua)?
                .inputs
                .get(player)
                .map_or(Buttons::NONE, |input| input.buttons);
            let table = lua.create_table()?;
            for button in Button::ALL {
                table.raw_set(button.name(), held.contains(button))?;
            }
            Ok(table)
        })?,
    )?;
    joypad.set(
        "set",
        lua.create_function(|lua, (buttons, player): (Value, Value)| {
            const NAME: &str = "joypad.set";
            let Value::Table(buttons) = buttons else {
                return Err(fail(
                    lua,
                    NAME,
                    format!(
                        "give a table of buttons, such as {{a = true}}, not {}",
                        shown(&buttons)
                    ),
                ));
            };
            let players = state(lua)?.machine.players;
            let player = numbered(lua, NAME, "the player", &player, players)?;
            let mut change = Joypad::default();
            for pair in buttons.pairs::<Value, Value>() {
                let (key, held) = pair?;
                let button = match &key {
                    Value::String(name) => name.to_string_lossy().parse::<Button>().ok(),
                    _ => None,
                };
                let Some(button) = button else {
                    return Err(fail(
                        lua,
                        NAME,
                        format!(
                            "there is no button {}; the buttons are a, b, x, y, l, r, \
                             up, down, left, right, start and select",
                            shown(&key)
                        ),
                    ));
                };
                // Releases win, so the table's order never matters.
                if matches!(held, Value::Nil | Value::Boolean(false)) {
                    change.release.set(button, true);
                    change.press.set(button, false);
                } else if !change.release.contains(button) {
                    change.press.set(button, true);
                }
            }
            let mut state = state_mut(lua)?;
            let pads = &mut state.output.joypad;
            if pads.len() <= player {
                pads.resize(player + 1, Joypad::default());
            }
            pads[player] = pads[player].then(change);
            Ok(())
        })?,
    )?;
    Ok(joypad)
}

fn gui(lua: &Lua) -> mlua::Result<Table> {
    let gui = lua.create_table()?;
    gui.set(
        "text",
        lua.create_function(
            |lua, (x, y, text, color, screen): (Value, Value, Value, Value, Value)| {
                const NAME: &str = "gui.text";
                let x = coordinate(lua, NAME, "x", &x)?;
                let y = coordinate(lua, NAME, "y", &y)?;
                let text: String = to_text(lua, text)?.chars().take(LONGEST_TEXT).collect();
                let color = self::color(lua, NAME, &color, Color::WHITE)?;
                let screen = screen_number(lua, NAME, &screen)?;
                draw(
                    lua,
                    Draw::Text {
                        screen,
                        x,
                        y,
                        text,
                        color,
                    },
                )
            },
        )?,
    )?;
    gui.set(
        "box",
        lua.create_function(|lua, (x1, y1, x2, y2, fill, outline, screen): BoxArgs| {
            const NAME: &str = "gui.box";
            let (x, width) = span(
                coordinate(lua, NAME, "x1", &x1)?,
                coordinate(lua, NAME, "x2", &x2)?,
            );
            let (y, height) = span(
                coordinate(lua, NAME, "y1", &y1)?,
                coordinate(lua, NAME, "y2", &y2)?,
            );
            let fill = color(lua, NAME, &fill, Color::NONE)?;
            let usual = if fill.is_visible() {
                fill
            } else {
                Color::WHITE
            };
            let outline = color(lua, NAME, &outline, usual)?;
            let screen = screen_number(lua, NAME, &screen)?;
            draw(
                lua,
                Draw::Rect {
                    screen,
                    x,
                    y,
                    width,
                    height,
                    fill,
                    outline,
                },
            )
        })?,
    )?;
    Ok(gui)
}

/// `gui.box`'s arguments: two corners, two colours and a screen.
type BoxArgs = (Value, Value, Value, Value, Value, Value, Value);

fn console(lua: &Lua) -> mlua::Result<Table> {
    let console = lua.create_table()?;
    console.set(
        "log",
        lua.create_function(|lua, values: Variadic<Value>| {
            let parts = values
                .into_iter()
                .map(|value| to_text(lua, value))
                .collect::<mlua::Result<Vec<_>>>()?;
            let line = parts.join("\t");
            let mut state = state_mut(lua)?;
            let log = &mut state.output.log;
            match log.len() {
                length if length < MOST_LINES => log.push(line),
                MOST_LINES => log.push(format!(
                    "(more than {MOST_LINES} lines since the last frame; the rest are left out)"
                )),
                _ => {}
            }
            Ok(())
        })?,
    )?;
    Ok(console)
}

/// Adds `shape` to this frame's drawing, unless there is already plenty.
fn draw(lua: &Lua, shape: Draw) -> mlua::Result<()> {
    let mut state = state_mut(lua)?;
    if state.output.draws.len() < MOST_DRAWS {
        state.output.draws.push(shape);
    }
    Ok(())
}

/// The core's function `name`, lent while the script runs.
fn host(lua: &Lua, name: &str) -> mlua::Result<Function> {
    lua.named_registry_value::<Table>(HOST)?.raw_get(name)
}

/// `value` as Lua's own `tostring` writes it.
fn to_text(lua: &Lua, value: Value) -> mlua::Result<String> {
    let tostring: Function = lua.named_registry_value(TOSTRING)?;
    Ok(tostring.call::<mlua::LuaString>(value)?.to_string_lossy())
}

/// An error raised by `function`, at the line of the script that called
/// it.
fn fail(lua: &Lua, function: &str, message: impl Display) -> mlua::Error {
    let place = lua
        .inspect_stack(1, |debug| {
            match (debug.source().short_src, debug.current_line()) {
                (Some(source), Some(line)) => format!("{source}:{line}: "),
                _ => String::new(),
            }
        })
        .unwrap_or_default();
    mlua::Error::RuntimeError(format!("{place}{function}: {message}"))
}

/// `value` for an error message.
fn shown(value: &Value) -> String {
    match value {
        Value::Integer(number) => number.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => format!("{:?}", text.to_string_lossy()),
        other => other.type_name().to_owned(),
    }
}

/// `value` as a whole number, if it is one.
fn whole(value: &Value) -> Option<i64> {
    match *value {
        Value::Integer(number) => Some(number),
        // Within ±2^53 every whole float is exact.
        Value::Number(number) if number.fract() == 0.0 && number.abs() <= 9.0e15 => {
            Some(number as i64)
        }
        _ => None,
    }
}

fn function(lua: &Lua, name: &str, value: Value) -> mlua::Result<Function> {
    match value {
        Value::Function(function) => Ok(function),
        other => Err(fail(
            lua,
            name,
            format!("give a function to call, not {}", shown(&other)),
        )),
    }
}

fn address(lua: &Lua, name: &str, value: &Value) -> mlua::Result<u32> {
    whole(value)
        .and_then(|number| u32::try_from(number).ok())
        .ok_or_else(|| {
            fail(
                lua,
                name,
                format!(
                    "the address must be a whole number from 0 to 0xFFFFFFFF, not {}",
                    shown(value)
                ),
            )
        })
}

/// A thing numbered from 1, as `value` gives it (1 if nil), as an index
/// from 0 below `count`.
fn numbered(lua: &Lua, name: &str, what: &str, value: &Value, count: usize) -> mlua::Result<usize> {
    if value.is_nil() {
        return Ok(0);
    }
    whole(value)
        .and_then(|number| usize::try_from(number).ok())
        .filter(|number| (1..=count).contains(number))
        .map(|number| number - 1)
        .ok_or_else(|| {
            let range = if count == 1 {
                "1".to_owned()
            } else {
                format!("1 to {count}")
            };
            fail(
                lua,
                name,
                format!("{what} must be {range}, not {}", shown(value)),
            )
        })
}

fn cpu_number(lua: &Lua, name: &str, value: &Value) -> mlua::Result<usize> {
    let cpus = state(lua)?.machine.cpus.max(1);
    numbered(lua, name, "the CPU", value, cpus)
}

fn screen_number(lua: &Lua, name: &str, value: &Value) -> mlua::Result<usize> {
    let screens = state(lua)?.machine.screens.max(1);
    numbered(lua, name, "the screen", value, screens)
}

fn slot_number(lua: &Lua, name: &str, value: &Value) -> mlua::Result<u8> {
    whole(value)
        .filter(|slot| SLOTS.contains(slot))
        .and_then(|slot| u8::try_from(slot).ok())
        .ok_or_else(|| {
            fail(
                lua,
                name,
                format!(
                    "the slot must be {} to {}, not {}",
                    SLOTS.start(),
                    SLOTS.end(),
                    shown(value)
                ),
            )
        })
}

/// A position in screen pixels, rounded down.
fn coordinate(lua: &Lua, name: &str, what: &str, value: &Value) -> mlua::Result<i32> {
    let number = match *value {
        Value::Integer(number) => number as f64,
        Value::Number(number) if number.is_finite() => number,
        _ => {
            return Err(fail(
                lua,
                name,
                format!("{what} must be a number, not {}", shown(value)),
            ));
        }
    };
    // Far off any screen is as good as anywhere else off it.
    Ok(number.floor().clamp(-1.0e6, 1.0e6) as i32)
}

/// The first edge and the size of the span between two edges, both
/// included.
fn span(a: i32, b: i32) -> (i32, u32) {
    (a.min(b), a.abs_diff(b) + 1)
}

/// A colour argument: nil for `usual`, false for none, a number or text.
fn color(lua: &Lua, name: &str, value: &Value, usual: Color) -> mlua::Result<Color> {
    let color = match value {
        Value::Nil => Some(usual),
        Value::Boolean(false) => Some(Color::NONE),
        Value::String(text) => Color::parse(&text.to_string_lossy()),
        other => whole(other).and_then(Color::from_number),
    };
    color.ok_or_else(|| {
        fail(
            lua,
            name,
            format!(
                "{} is not a colour; give 0xRRGGBB, 0xAARRGGBB, \"#RRGGBBAA\" or a name such as \"red\"",
                shown(value)
            ),
        )
    })
}
