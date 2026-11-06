//! Gamepad input through gilrs, on its own thread.
//!
//! Controller events become plain [`PadEvent`]s that name the controller
//! and the control (`"South"`, `"DPadUp"`, `"LeftStickLeft"`). The view maps
//! them through the same bindings as the keyboard. Analog sticks act as
//! digital directions, with hysteresis so a stick resting near the
//! threshold does not chatter.
//!
//! The right stick also works like a mouse: while it is held over, the
//! thread sends [`PadEvent::Pointer`] motion, faster the further it is
//! pushed. The view uses it to move the touch cursor.
//!
//! gilrs reads controllers through IOKit on macOS, Windows.Gaming.Input on
//! Windows (which sends events only while one of the app's windows has
//! the focus), and evdev on Linux. Each names controls the same way and
//! maps them with the same SDL controller database, built in, and analog
//! triggers press `LeftTrigger2` and `RightTrigger2` past three quarters
//! of the way on all three. Linux needs read access to the controller's
//! `/dev/input/event*` device, which a desktop session gives the user at
//! the screen; without it the controller isn't seen, and the thread says
//! so on stderr.

use std::collections::{HashMap, HashSet};
use std::thread;
use std::time::{Duration, Instant};

use gilrs::{Axis, EventType, GamepadId, Gilrs};

/// Stick deflection that presses a direction.
const PRESS_THRESHOLD: f32 = 0.5;
/// Deflection below which a pressed direction releases.
const RELEASE_THRESHOLD: f32 = 0.3;

/// Right-stick deflection ignored as drift.
const POINTER_DEAD_ZONE: f32 = 0.15;
/// How often pointer motion is sent while the right stick is held over.
const POINTER_INTERVAL: Duration = Duration::from_millis(8);
/// How long to wait for controller events while nothing is moving.
const IDLE_WAIT: Duration = Duration::from_millis(250);

/// Something a controller did.
#[derive(Debug, Clone, PartialEq)]
pub enum PadEvent {
    /// A control changed state.
    Control {
        /// The controller's UUID as 32 hex digits, for per-controller
        /// profiles.
        controller: String,
        /// gilrs's name for the control, or a stick direction such as
        /// `"LeftStickUp"`.
        control: String,
        /// Pressed or released.
        pressed: bool,
    },
    /// The right stick moved the pointer: 1.0 along an axis for each second
    /// it is held fully over that way.
    Pointer {
        /// Distance right; negative is left.
        dx: f32,
        /// Distance down; negative is up.
        dy: f32,
    },
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
    #[cfg(target_os = "linux")]
    for pad in unreadable_pads(std::path::Path::new(JOYSTICKS)) {
        eprintln!(
            "ds-emulator: cannot read the gamepad at {}: sign in to a desktop session, or join \
             the input group, to play with it",
            pad.display()
        );
    }
    let mut sticks = Sticks::default();
    let mut held: HashMap<GamepadId, HashSet<String>> = HashMap::new();
    // Each controller's right stick, x right and y up as gilrs reports.
    let mut right_sticks: HashMap<GamepadId, (f32, f32)> = HashMap::new();
    let mut motion = Motion::default();
    let mut last_poll = Instant::now();

    while !events.is_closed() {
        let velocity = pointer_velocity(right_sticks.values().copied());
        let wait = if velocity == (0.0, 0.0) {
            IDLE_WAIT
        } else {
            POINTER_INTERVAL
        };
        let next = gilrs.next_event_blocking(Some(wait));
        // The sticks held their position since the last poll; an event
        // below may move them.
        let now = Instant::now();
        if let Some((dx, dy)) = motion.advance(velocity, now - last_poll) {
            events.try_send(PadEvent::Pointer { dx, dy }).ok();
        }
        last_poll = now;
        let Some(event) = next else {
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
                .try_send(PadEvent::Control {
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
                let stick = right_sticks.entry(event.id).or_default();
                match axis {
                    Axis::RightStickX => stick.0 = value,
                    Axis::RightStickY => stick.1 = value,
                    _ => {}
                }
                for (control, pressed) in sticks.update(event.id, axis, value) {
                    send(control.to_owned(), pressed);
                }
            }
            EventType::Disconnected => {
                right_sticks.remove(&event.id);
                for control in held.remove(&event.id).unwrap_or_default() {
                    events
                        .try_send(PadEvent::Control {
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

/// Pointer velocity from right sticks at `(x, y)` with y up, in the units
/// of [`PadEvent::Pointer`] per second with y down. Small deflections are
/// ignored, and speed grows with the square of the deflection beyond that
/// for fine control.
fn pointer_velocity(sticks: impl IntoIterator<Item = (f32, f32)>) -> (f32, f32) {
    sticks
        .into_iter()
        .map(|(x, y)| {
            let deflection = x.hypot(y);
            if deflection <= POINTER_DEAD_ZONE {
                return (0.0, 0.0);
            }
            let strength = ((deflection - POINTER_DEAD_ZONE) / (1.0 - POINTER_DEAD_ZONE)).min(1.0);
            let speed = strength * strength / deflection;
            (x * speed, -y * speed)
        })
        .fold((0.0, 0.0), |total, (x, y)| (total.0 + x, total.1 + y))
}

/// Pointer motion gathered between sends, so a stream of axis events does
/// not become a stream of tiny moves.
#[derive(Debug, Default)]
struct Motion {
    pending: (f32, f32),
    pending_time: Duration,
}

impl Motion {
    /// Moves at `velocity` for `elapsed`. Returns the motion gathered once
    /// it is due to be sent: every [`POINTER_INTERVAL`], and when the
    /// pointer stops.
    fn advance(&mut self, velocity: (f32, f32), elapsed: Duration) -> Option<(f32, f32)> {
        let seconds = elapsed.as_secs_f32();
        self.pending.0 += velocity.0 * seconds;
        self.pending.1 += velocity.1 * seconds;
        self.pending_time += elapsed;
        let stopped = velocity == (0.0, 0.0);
        if !stopped && self.pending_time < POINTER_INTERVAL {
            return None;
        }
        self.pending_time = Duration::ZERO;
        let motion = std::mem::take(&mut self.pending);
        (motion != (0.0, 0.0)).then_some(motion)
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

/// Where udev links each joystick's device on Linux, by its name.
#[cfg(target_os = "linux")]
const JOYSTICKS: &str = "/dev/input/by-id";

/// The joysticks udev lists in `folder` that the app may not read, which
/// gilrs then passes over without a word.
#[cfg(target_os = "linux")]
fn unreadable_pads(folder: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut pads: Vec<_> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_joystick_event)
        })
        .filter(|path| {
            std::fs::File::open(path)
                .is_err_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)
        })
        .collect();
    pads.sort();
    pads
}

/// Whether `name`, in `/dev/input/by-id`, is udev's link to a joystick's
/// event device, the one gilrs reads, rather than its legacy `js` device
/// or a keyboard's or mouse's.
#[cfg(any(target_os = "linux", test))]
fn is_joystick_event(name: &str) -> bool {
    name.ends_with("-event-joystick")
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
    fn pointer_ignores_drift_and_points_down_the_screen() {
        assert_eq!(pointer_velocity([(0.1, -0.1)]), (0.0, 0.0));
        assert_eq!(pointer_velocity([(1.0, 0.0)]), (1.0, 0.0));
        assert_eq!(pointer_velocity([(0.0, 1.0)]), (0.0, -1.0));
        let (x, _) = pointer_velocity([(0.5, 0.0)]);
        assert!(0.0 < x && x < 0.25, "half over moves slowly: {x}");
    }

    #[test]
    fn motion_is_gathered_until_due() {
        let mut motion = Motion::default();
        let step = Duration::from_millis(5);
        assert_eq!(motion.advance((1.0, 0.0), step), None);
        let (dx, dy) = motion.advance((1.0, 0.0), step).expect("due after 10 ms");
        assert!((dx - 0.01).abs() < 1e-6 && dy == 0.0);
        // Whatever is left goes out when the stick stops.
        assert_eq!(motion.advance((0.0, 2.0), step), None);
        let (_, dy) = motion.advance((0.0, 0.0), step).expect("sent on stop");
        assert!((dy - 0.01).abs() < 1e-6);
        assert_eq!(motion.advance((0.0, 0.0), step), None);
    }

    #[test]
    fn only_joysticks_event_devices_count() {
        assert!(is_joystick_event(
            "usb-Microsoft_Controller_3039-event-joystick"
        ));
        assert!(!is_joystick_event("usb-Microsoft_Controller_3039-joystick"));
        assert!(!is_joystick_event(
            "usb-Logitech_USB_Receiver-if02-event-mouse"
        ));
        assert!(!is_joystick_event("usb-Apple_Keyboard-event-kbd"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_folder_without_joysticks_lists_none() {
        let missing = std::env::temp_dir().join("ds-emulator-no-such-folder");
        assert!(unreadable_pads(&missing).is_empty());
        let empty = std::env::temp_dir().join(format!("ds-emulator-pads-{}", std::process::id()));
        std::fs::create_dir_all(&empty).expect("temporary folder");
        // Readable, so gilrs sees it.
        std::fs::write(empty.join("usb-pad-event-joystick"), b"").expect("file");
        assert!(unreadable_pads(&empty).is_empty());
        std::fs::remove_dir_all(&empty).ok();
    }

    #[test]
    fn uuids_print_as_hex() {
        assert_eq!(hex(&[0x0a, 0xff, 0x00]), "0aff00");
    }
}
