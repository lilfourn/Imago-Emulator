//! Guide, the chat beside a running game ([PRD](../../../docs/PRD-game-chat.md)):
//! what it needs from OpenRouter, without a window.
//!
//! - [`login`] connects an OpenRouter account the way Imago does: OAuth
//!   PKCE through the browser, back to a loopback port, which mints a key
//!   for this Mac.
//! - [`key`] keeps that key in a file only its owner can read, with
//!   `OPENROUTER_API_KEY` overriding it.
//! - [`api`] calls OpenRouter through `curl`, as the app's other downloads
//!   do: the key exchange and the key's own details.
//!
//! Nothing here touches the network until the player asks to connect.

pub mod api;
mod curl;
#[cfg(test)]
mod fake;
pub mod key;
pub mod login;
pub mod pkce;
mod url;
