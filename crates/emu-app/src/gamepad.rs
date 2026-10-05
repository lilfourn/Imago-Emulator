//! Gamepad input through gilrs, on its own thread.
//!
//! Controller events become plain [`PadEvent`]s that name the controller
//! and the control (`"South"`, `"DPadUp"`, `"LeftStickLeft"`). The view maps
//! them through the same bindings as the keyboard. Analog sticks act as
//! digital directions, with hysteresis so a stick resting near the
//! threshold does not chatter.

use std::collections::{HashMap, HashSet};
use std::thread;
use std::time::Duration;

use gilrs::{Axis, EventType, GamepadId, Gilrs};

/// Stick deflection that presses a direction.
const PRESS_THRESHOLD: f32 = 0.5;
/// Deflection below which a pressed direction releases.
const RELEASE_THRESHOLD: f32 = 0.3;

/// A control on a controller changed state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PadEvent {
    /// The controller's UUID as 32 hex digits, for per-controller profiles.
    pub controller: String,
    /// gilrs's name for the control, or a stick direction such as
    /// `"LeftStickUp"`.
    pub control: String,
    /// Pressed or released.
    pub pressed: bool,
}

/// Starts the gamepad thread, which sends events to `events` until the
/// receiver is dropped.
///
/// # Errors
///
/// Fails if the thread cannot be spawned. A missing gamepad subsystem is
/// reported on stderr and leaves the thread idle.
pub fn spawn(events: async_channel::Sender<PadEvent>) -> std::io::Result<()> {
    thread::Builder::new()
        .name("gamepad".to_owned())
        .spawn(move || run(&events))?;
    Ok(())
}

fn run(events: &async_channel::Sender<PadEvent>) {
    let mut gilrs = match Gilrs::new() {
        Ok(gilrs) => gilrs,
        Err(error) => {
            eprintln!("ds-emulator: gamepads unavailable: {error}");
            return;
        }
    };
    let mut sticks = Sticks::default();
    let mut held: HashMap<GamepadId, HashSet<String>> = HashMap::new();

    while !events.is_closed() {
        let Some(event) = gilrs.next_event_blocking(Some(Duration::from_millis(250))) else {
            continue;
        };
        let controller = hex(&gilrs.gamepad(event.id).uuid());
        let mut send = |control: String, pressed: bool| {
            let pad = held.entry(event.id).or_default();
            if pressed {
                pad.insert(control.clone());
            } else {
                pad.remove(&control);
            }
            events
                .try_send(PadEvent {
                    controller: controller.clone(),
                    control,
                    pressed,
                })
                .ok();
        };
        match event.event {
            EventType::ButtonPressed(button, _) => send(format!("{button:?}"), true),
            EventType::ButtonReleased(button, _) => send(format!("{button:?}"), false),
            EventType::AxisChanged(axis, value, _) => {
                for (control, pressed) in sticks.update(event.id, axis, value) {
                    send(control.to_owned(), pressed);
                }
            }
            EventType::Disconnected => {
                for control in held.remove(&event.id).unwrap_or_default() {
                    events
                        .try_send(PadEvent {
                            controller: controller.clone(),
                            control,
                            pressed: false,
                        })
                        .ok();
                }
            }
            _ => {}
        }
    }
}

/// Turns stick axes into digital directions.
#[derive(Debug, Default)]
struct Sticks {
    /// Current direction per axis: -1, 0 or 1.
    directions: HashMap<(GamepadId, Axis), i8>,
}

impl Sticks {
    /// Returns the direction controls released and pressed by `axis`
    /// moving to `value`.
    fn update(&mut self, pad: GamepadId, axis: Axis, value: f32) -> Vec<(&'static str, bool)> {
        let Some((negative, positive)) = direction_names(axis) else {
            return Vec::new();
        };
        let previous = self.directions.get(&(pad, axis)).copied().unwrap_or(0);
        let direction = next_direction(previous, value);
        if direction == previous {
            return Vec::new();
        }
        self.directions.insert((pad, axis), direction);
        let name = |direction: i8| if direction < 0 { negative } else { positive };
        let mut changes = Vec::with_capacity(2);
        if previous != 0 {
            changes.push((name(previous), false));
        }
        if direction != 0 {
            changes.push((name(direction), true));
        }
        changes
    }
}

/// The direction a stick axis at `value` points, given the `previous`
/// one; between the two thresholds the previous direction holds.
fn next_direction(previous: i8, value: f32) -> i8 {
    if value >= PRESS_THRESHOLD {
        1
    } else if value <= -PRESS_THRESHOLD {
        -1
    } else if value.abs() < RELEASE_THRESHOLD {
        0
    } else {
        previous
    }
}

/// Names for an axis's (negative, positive) directions. gilrs reports up
/// as positive Y.
fn direction_names(axis: Axis) -> Option<(&'static str, &'static str)> {
    match axis {
        Axis::LeftStickX => Some(("LeftStickLeft", "LeftStickRight")),
        Axis::LeftStickY => Some(("LeftStickDown", "LeftStickUp")),
        Axis::RightStickX => Some(("RightStickLeft", "RightStickRight")),
        Axis::RightStickY => Some(("RightStickDown", "RightStickUp")),
        _ => None,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directions_have_hysteresis() {
        assert_eq!(next_direction(0, 0.6), 1);
        assert_eq!(next_direction(1, 0.4), 1);
        assert_eq!(next_direction(0, 0.4), 0);
        assert_eq!(next_direction(1, 0.1), 0);
        assert_eq!(next_direction(1, -0.7), -1);
    }

    #[test]
    fn uuids_print_as_hex() {
        assert_eq!(hex(&[0x0a, 0xff, 0x00]), "0aff00");
    }
}
