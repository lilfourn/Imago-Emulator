//! The layout of a cheat list: `[name]` lines, each followed by its code
//! lines. What the codes mean is up to the device modules.

use crate::{Device, ParseError};

/// One cheat's lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Section<'a> {
    /// The `[name]` line's number, or the first code line's for codes
    /// before any name.
    pub(crate) line: usize,
    pub(crate) name: String,
    pub(crate) enabled: bool,
    /// The device tag after the name, if any.
    pub(crate) device: Option<Device>,
    /// The code lines, trimmed, with their line numbers from 1.
    pub(crate) lines: Vec<(usize, &'a str)>,
}

impl Section<'_> {
    fn unnamed(line: usize) -> Self {
        Self {
            line,
            name: "Unnamed".to_owned(),
            enabled: true,
            device: None,
            lines: Vec::new(),
        }
    }
}

/// Splits `text` into cheats. Blank lines and `#` comments are dropped;
/// code lines before the first `[name]` form a cheat named "Unnamed".
///
/// # Errors
///
/// Returns the first `[name]` line that cannot be read.
pub(crate) fn sections(text: &str) -> Result<Vec<Section<'_>>, ParseError> {
    let mut sections: Vec<Section<'_>> = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            let section = header(number, rest).map_err(|e| ParseError::new(number, e))?;
            sections.push(section);
            continue;
        }
        if sections.is_empty() {
            sections.push(Section::unnamed(number));
        }
        if let Some(section) = sections.last_mut() {
            section.lines.push((number, line));
        }
    }
    Ok(sections)
}

/// A `[name] flags` line, after its `[`. Flags are `on` or `off` and a
/// device tag, in either order.
fn header(line: usize, rest: &str) -> Result<Section<'static>, String> {
    let (name, flags) = rest
        .split_once(']')
        .ok_or_else(|| "a cheat name needs a closing \"]\"".to_owned())?;
    let mut enabled = None;
    let mut device = None;
    for word in flags.split_whitespace() {
        let on = word.eq_ignore_ascii_case("on");
        if on || word.eq_ignore_ascii_case("off") {
            if enabled.replace(on).is_some() {
                return Err("\"on\" or \"off\" can appear only once".to_owned());
            }
        } else if let Some(tag) = Device::from_tag(word) {
            if let Some(first) = device.replace(tag) {
                return Err(format!(
                    "a cheat has one device; found both {:?} and {:?}",
                    first.tag(),
                    tag.tag()
                ));
            }
        } else {
            return Err(format!(
                "expected \"on\", \"off\" or a device tag ({}), found {word:?}",
                Device::tags()
            ));
        }
    }
    Ok(Section {
        line,
        name: name.trim().to_owned(),
        enabled: enabled.unwrap_or(true),
        device,
        lines: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_names_flags_tags_and_lines() {
        let text = "# list\n02000000:01\n[Money] ARDS\n  94000130 FCFF0000 \n\n\
                    [Walls]   gsa  off\n1234\n[Plain]\n";
        let sections = sections(text).expect("valid");
        let summary: Vec<_> = sections
            .iter()
            .map(|section| (section.name.as_str(), section.enabled, section.device))
            .collect();
        assert_eq!(
            summary,
            [
                ("Unnamed", true, None),
                ("Money", true, Some(Device::ActionReplayDs)),
                ("Walls", false, Some(Device::GameSharkAdvance)),
                ("Plain", true, None),
            ]
        );
        assert_eq!(
            sections.iter().map(|s| s.line).collect::<Vec<_>>(),
            [2, 3, 6, 8]
        );
        assert_eq!(sections[0].lines, [(2, "02000000:01")]);
        assert_eq!(sections[1].lines, [(4, "94000130 FCFF0000")]);
        assert_eq!(sections[2].lines, [(7, "1234")]);
        assert!(sections[3].lines.is_empty());
    }

    #[test]
    fn bad_headers_name_their_line() {
        for (text, needle) in [
            ("[Money\n", "closing"),
            ("[Money] maybe\n", "device tag"),
            ("[Money] off on\n", "only once"),
            ("[Money] gsa par3\n", "one device"),
        ] {
            let error = sections(text).expect_err(text);
            assert_eq!(error.line, 1);
            assert!(error.to_string().contains(needle), "{error}");
        }
    }
}
