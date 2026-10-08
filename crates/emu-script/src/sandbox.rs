//! The Lua state a script runs in, and the limits that keep a script from
//! harming the app.
//!
//! - **Libraries**: the base library, `coroutine`, `table`, `string`,
//!   `utf8` and `math`. Not `io`, `os`, `package` (`require`) or `debug`:
//!   no files, processes, clock or C modules. `dofile`, `loadfile` and
//!   `string.dump` are removed, and `load` takes source text only, since
//!   Lua does not check compiled chunks.
//! - **Determinism**: `math.random` starts from seed 0, and nothing can
//!   read the clock, so a script makes the same choices on every run.
//! - **Memory**: at most [`MEMORY_LIMIT`]; past it allocations fail.
//! - **Time**: each call into the script may run [`INSTRUCTION_LIMIT`] Lua
//!   instructions. A script that runs longer, stuck in a loop, is stopped
//!   by yielding the coroutine it runs in, which `pcall` cannot catch.

use mlua::{HookTriggers, Lua, LuaOptions, StdLib, Value, VmState};

/// The most memory a script may use.
pub const MEMORY_LIMIT: usize = 64 << 20;

/// The most Lua instructions one call into a script may run: well under a
/// second.
pub const INSTRUCTION_LIMIT: u64 = 100_000_000;

/// How often the instruction count is checked.
const CHECK_EVERY: u32 = 10_000;

/// Instructions run since the last call into the script began, and
/// whether that broke the limit.
#[derive(Debug, Default)]
struct Budget {
    used: u64,
    spent: Option<String>,
}

/// A fresh, sandboxed Lua state.
pub fn new() -> mlua::Result<Lua> {
    let libs = StdLib::COROUTINE | StdLib::TABLE | StdLib::STRING | StdLib::UTF8 | StdLib::MATH;
    let lua = Lua::new_with(libs, LuaOptions::default())?;
    lua.set_memory_limit(MEMORY_LIMIT)?;
    let globals = lua.globals();
    globals.raw_set("dofile", Value::Nil)?;
    globals.raw_set("loadfile", Value::Nil)?;
    lua.load(
        r#"
        local load = load
        -- Text only; "..." keeps a missing environment missing.
        function _G.load(chunk, name, mode, ...)
            return load(chunk, name, "t", ...)
        end
        string.dump = nil
        math.randomseed(0)
        "#,
    )
    .set_name("=sandbox")
    .exec()?;

    lua.set_app_data(Budget::default());
    let triggers = HookTriggers::new().every_nth_instruction(CHECK_EVERY);
    lua.set_global_hook(triggers, |lua, debug| {
        let Some(mut budget) = lua.app_data_mut::<Budget>() else {
            return Ok(VmState::Continue);
        };
        budget.used += u64::from(CHECK_EVERY);
        if budget.used <= INSTRUCTION_LIMIT {
            return Ok(VmState::Continue);
        }
        if budget.spent.is_none() {
            let place = match (debug.source().short_src, debug.current_line()) {
                (Some(source), Some(line)) => format!("{source}:{line}: "),
                _ => String::new(),
            };
            budget.spent = Some(format!(
                "{place}the script ran {} million instructions without returning \
                 to the emulator; is it stuck in a loop?",
                INSTRUCTION_LIMIT / 1_000_000
            ));
        }
        // Yielding unwinds to whoever resumed this coroutine, through any
        // pcall; each coroutine yields in turn until the script's own does.
        Ok(VmState::Yield)
    })?;
    Ok(lua)
}

/// Starts a new call into the script with its whole instruction budget.
pub fn refill(lua: &Lua) {
    if let Some(mut budget) = lua.app_data_mut::<Budget>() {
        *budget = Budget::default();
    }
}

/// Fails if the call under way ran out of instructions.
pub fn check(lua: &Lua) -> mlua::Result<()> {
    match lua
        .app_data_ref::<Budget>()
        .and_then(|budget| budget.spent.clone())
    {
        Some(message) => Err(mlua::Error::RuntimeError(message)),
        None => Ok(()),
    }
}
