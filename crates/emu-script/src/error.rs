//! Why a script stopped.

use std::fmt;

/// A script failed: it would not compile, raised an error, or broke a
/// sandbox limit. The message is one line, such as
/// `hud.lua:12: attempt to index a nil value (global 'player')`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptError(String);

impl ScriptError {
    /// Says why, in Lua's words where it has them.
    pub(crate) fn from_lua(error: &mlua::Error) -> Self {
        Self(message(error))
    }

    /// The message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ScriptError {}

/// The innermost cause, without the tracebacks mlua wraps around errors
/// raised in Rust callbacks.
fn message(error: &mlua::Error) -> String {
    match error {
        mlua::Error::CallbackError { cause, .. } | mlua::Error::WithContext { cause, .. } => {
            message(cause)
        }
        // Errors in coroutines carry Lua's traceback after the message.
        mlua::Error::RuntimeError(text) | mlua::Error::SyntaxError { message: text, .. } => text
            .split("\nstack traceback:")
            .next()
            .unwrap_or_default()
            .to_owned(),
        mlua::Error::MemoryError(_) => format!(
            "the script needed more than {} MiB of memory",
            crate::sandbox::MEMORY_LIMIT >> 20
        ),
        mlua::Error::BadArgument { pos, cause, .. } => {
            format!("bad argument #{pos}: {}", message(cause))
        }
        other => other.to_string(),
    }
}
