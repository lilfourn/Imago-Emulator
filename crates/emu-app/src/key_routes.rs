//! The keys held down, and where each went when it was pressed: to the
//! library, the game menu, the game or Guide's composer.
//!
//! A key held down repeats, and each repeat is a key-down of its own. A
//! repeat goes only where its press went, so Return held to choose
//! Library in the game menu does not go on to start a game there, and W
//! held while walking does not run through the menu Escape opens. The
//! key's next press goes wherever keys go then.
//!
//! GPUI says whether a key-down repeats, but on macOS the repeats of keys
//! that type nothing (the arrows, Escape, Backspace) come as fresh
//! presses. A key-down for a key already down is a repeat whatever it
//! says, so a key whose key-up never comes must be forgotten with
//! [`KeyRoutes::clear`], or its next press would count as a repeat.

use std::collections::HashMap;

/// Where keys go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// The library, while no game runs.
    Library,
    /// The game menu, while it is open over the game.
    Menu,
    /// The game.
    Game,
    /// Guide's composer, while it has the caret.
    Guide,
}

/// The keys held down, by GPUI key name, with where each went when
/// pressed.
#[derive(Debug, Default)]
pub struct KeyRoutes {
    pressed: HashMap<String, Route>,
}

impl KeyRoutes {
    /// Notes that `key` was pressed while keys went to `route`.
    pub fn press(&mut self, key: &str, route: Route) {
        self.pressed.insert(key.to_owned(), route);
    }

    /// Where `key` went when pressed, if it is down.
    pub fn route_of(&self, key: &str) -> Option<Route> {
        self.pressed.get(key).copied()
    }

    /// Forgets `key`, let go of.
    pub fn release(&mut self, key: &str) {
        self.pressed.remove(key);
    }

    /// Forgets every key, as when key-ups may never come.
    pub fn clear(&mut self) {
        self.pressed.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_down_from_its_press_to_its_release() {
        let mut routes = KeyRoutes::default();
        assert_eq!(routes.route_of("enter"), None);
        routes.press("enter", Route::Menu);
        assert_eq!(routes.route_of("enter"), Some(Route::Menu));
        routes.release("enter");
        assert_eq!(routes.route_of("enter"), None);
    }

    #[test]
    fn each_key_keeps_its_own_route() {
        let mut routes = KeyRoutes::default();
        routes.press("w", Route::Game);
        routes.press("down", Route::Menu);
        assert_eq!(routes.route_of("w"), Some(Route::Game));
        assert_eq!(routes.route_of("down"), Some(Route::Menu));
    }

    #[test]
    fn clear_forgets_keys_whose_key_up_may_never_come() {
        let mut routes = KeyRoutes::default();
        routes.press("s", Route::Game);
        routes.press("1", Route::Library);
        routes.clear();
        assert_eq!(routes.route_of("s"), None);
        assert_eq!(routes.route_of("1"), None);
    }
}
