//! The refresh rate of the output a window is on, under Wayland.
//!
//! GPUI follows the window from one output to the next (the one it is on
//! with the highest scale), but keeps only each output's name, place and
//! scale, not the refresh rate its mode gives, and doesn't share its
//! connection. So this asks the compositor again, on a short connection of
//! its own: each output's name and its current mode's rate. The window's
//! output is the one whose name gives the UUID GPUI names the window's
//! display by, a v5 UUID of the output's name.

use uuid::Uuid;
use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};

/// The newest `wl_output` asked for: version 4 gives outputs their names.
const OUTPUT_VERSION: u32 = 4;

/// An output the compositor told of.
#[derive(Debug, Clone, Default, PartialEq)]
struct Output {
    name: Option<String>,
    /// Its current mode's refreshes a second, in thousandths.
    millihertz: Option<i32>,
}

impl Output {
    /// Refreshes a second, if the mode says; 0 means it doesn't.
    fn hz(&self) -> Option<f64> {
        self.millihertz
            .filter(|millihertz| *millihertz > 0)
            .map(|millihertz| f64::from(millihertz) / 1000.0)
    }
}

/// What the compositor said, in the order it listed the outputs.
#[derive(Debug, Default)]
struct Outputs(Vec<Output>);

impl Dispatch<wl_registry::WlRegistry, ()> for Outputs {
    fn event(
        outputs: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
            && interface == "wl_output"
        {
            let index = outputs.0.len();
            outputs.0.push(Output::default());
            registry.bind::<wl_output::WlOutput, _, _>(
                name,
                version.min(OUTPUT_VERSION),
                queue,
                index,
            );
        }
    }
}

impl Dispatch<wl_output::WlOutput, usize> for Outputs {
    fn event(
        outputs: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(output) = outputs.0.get_mut(*index) else {
            return;
        };
        match event {
            wl_output::Event::Mode {
                flags: WEnum::Value(flags),
                refresh,
                ..
            } if flags.contains(wl_output::Mode::Current) => output.millihertz = Some(refresh),
            wl_output::Event::Name { name } => output.name = Some(name),
            _ => {}
        }
    }
}

/// How many times a second the output refreshes that GPUI names
/// `display`, or that every output refreshes at if it can't tell.
pub fn refresh_rate(display: Option<Uuid>) -> Option<f64> {
    let connection = Connection::connect_to_env().ok()?;
    let mut queue = connection.new_event_queue();
    let handle = queue.handle();
    connection.display().get_registry(&handle, ());
    let mut outputs = Outputs::default();
    // The first round trip lists the outputs, the second hears from each.
    queue.roundtrip(&mut outputs).ok()?;
    queue.roundtrip(&mut outputs).ok()?;
    choose(&outputs.0, display)
}

/// The rate of the output `display` names, as GPUI names them. Failing
/// that, as when the compositor gives outputs no names, the rate they all
/// share, if they do.
fn choose(outputs: &[Output], display: Option<Uuid>) -> Option<f64> {
    let named = display.and_then(|display| {
        outputs.iter().find(|output| {
            output
                .name
                .as_ref()
                .is_some_and(|name| Uuid::new_v5(&Uuid::NAMESPACE_DNS, name.as_bytes()) == display)
        })
    });
    if let Some(output) = named {
        return output.hz();
    }
    let (first, rest) = outputs.split_first()?;
    rest.iter()
        .all(|output| output.millihertz == first.millihertz)
        .then(|| first.hz())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(name: &str, millihertz: i32) -> Output {
        Output {
            name: Some(name.to_owned()),
            millihertz: Some(millihertz),
        }
    }

    fn gpui_name(name: &str) -> Option<Uuid> {
        Some(Uuid::new_v5(&Uuid::NAMESPACE_DNS, name.as_bytes()))
    }

    #[test]
    fn the_output_gpui_names_gives_the_rate() {
        let outputs = [output("eDP-1", 120_000), output("DP-2", 239_760)];
        assert_eq!(choose(&outputs, gpui_name("eDP-1")), Some(120.0));
        assert_eq!(choose(&outputs, gpui_name("DP-2")), Some(239.76));
    }

    #[test]
    fn without_a_name_only_a_rate_every_output_shares_counts() {
        let same = [output("eDP-1", 60_000), output("HDMI-A-1", 60_000)];
        assert_eq!(choose(&same, None), Some(60.0));
        assert_eq!(choose(&same, gpui_name("gone")), Some(60.0));
        let mixed = [output("eDP-1", 60_000), output("DP-2", 144_000)];
        assert_eq!(choose(&mixed, None), None);
        assert_eq!(choose(&[output("eDP-1", 59_940)], None), Some(59.94));
        assert_eq!(choose(&[], None), None);
    }

    #[test]
    fn a_rate_of_nothing_is_unknown() {
        assert_eq!(choose(&[output("virtual", 0)], gpui_name("virtual")), None);
        let silent = Output {
            name: Some("quiet".to_owned()),
            millihertz: None,
        };
        assert_eq!(choose(&[silent], None), None);
    }
}
