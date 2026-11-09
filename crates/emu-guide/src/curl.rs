//! Requests through `curl`, which macOS and Windows ship and Linux systems
//! nearly always have, as the app's box art and store downloads go.
//! Anything secret goes on `curl`'s standard input, never in its
//! arguments, which any process can read.

use std::io::Write;
use std::process::{Command, Stdio};

use emu_os::fetch::Allow;
use emu_os::process::why_not_started;

/// What a server answered: its status and body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// The HTTP status, such as 200.
    pub status: u16,
    /// The body, whole.
    pub body: Vec<u8>,
}

/// Where Guide's requests may go: `https://` addresses, and in tests the
/// stand-in server on this computer.
pub const ALLOW: Allow = Allow::or_loopback_if(cfg!(test));

/// `curl` for a request of Guide's to `url`, with what every request of
/// the app's has ([`emu_os::fetch::curl`]), naming Guide as the part of
/// the app that asks.
///
/// # Errors
///
/// If `url` is not an address Guide may ask ([`ALLOW`]).
pub fn command(url: &str) -> Result<Command, emu_os::fetch::Error> {
    emu_os::fetch::curl(url, Some("Guide"), ALLOW)
}

/// Runs `curl` with `args` against `url`, writing `stdin` to it, and
/// returns what the server answered, whatever its status. Gives up after
/// `max_seconds`.
///
/// # Errors
///
/// What went wrong, in `curl`'s words, if `curl` cannot run or the
/// transfer fails: no connection, a timeout, a reply cut short; or that
/// `url` is not an address Guide may ask.
pub fn request(
    args: &[&str],
    stdin: &[u8],
    url: &str,
    max_seconds: u32,
) -> Result<Response, String> {
    let mut child = command(url)
        .map_err(|error| error.to_string())?
        .arg("--max-time")
        .arg(max_seconds.to_string())
        .args(["--output", "-", "--write-out", "\n%{http_code}"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| why_not_started("curl", &error))?;
    // Small enough to fit the pipe's buffer, so nothing waits on the
    // other end; dropping the handle closes it.
    if let Some(mut input) = child.stdin.take() {
        input
            .write_all(stdin)
            .map_err(|error| format!("curl didn't take the request: {error}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("curl didn't finish: {error}"))?;
    if !output.status.success() {
        let why = String::from_utf8_lossy(&output.stderr);
        let why = why.trim().trim_start_matches("curl: ");
        return Err(if why.is_empty() {
            "the connection failed".to_owned()
        } else {
            why.to_owned()
        });
    }
    let mut body = output.stdout;
    let split = body.iter().rposition(|&byte| byte == b'\n').unwrap_or(0);
    let status = String::from_utf8_lossy(&body[split..]).trim().parse();
    body.truncate(split);
    match status {
        Ok(status) => Ok(Response { status, body }),
        Err(_) => Err("curl gave no status".to_owned()),
    }
}

/// A `curl` config line setting `header`, quoted as its config files
/// quote: backslashes and quotes escaped. `None` if `header` holds a line
/// break or other control character, which no header may.
pub fn config_header(header: &str) -> Option<String> {
    if header.chars().any(char::is_control) {
        return None;
    }
    let quoted = header.replace('\\', "\\\\").replace('"', "\\\"");
    Some(format!("header = \"{quoted}\"\n"))
}

/// A `curl` config line sending `body`, compact JSON, as the request's
/// body, quoted as [`config_header`] quotes.
///
/// Compact JSON escapes the control characters below U+0020, but leaves
/// DEL and the C1 controls (U+007F to U+009F) as they are, as JSON allows
/// inside a string, which is the only place they can be. No such
/// character may stand in a config line, so each becomes its JSON escape,
/// `\u007f`: the server reads the same text. `None` if `body` holds one of
/// the others, a line break say, which compact JSON never does.
pub fn config_body(body: &str) -> Option<String> {
    let mut quoted = String::with_capacity(body.len());
    for c in body.chars() {
        match c {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\u{7f}'..='\u{9f}' => quoted.push_str(&format!("\\\\u{:04x}", u32::from(c))),
            c if c.is_control() => return None,
            c => quoted.push(c),
        }
    }
    Some(format!("data-binary = \"{quoted}\"\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_headers_are_quoted_and_refuse_line_breaks() {
        assert_eq!(
            config_header("Authorization: Bearer sk-or-v1-abc").as_deref(),
            Some("header = \"Authorization: Bearer sk-or-v1-abc\"\n")
        );
        assert_eq!(
            config_header(r#"X: a"b\c"#).as_deref(),
            Some("header = \"X: a\\\"b\\\\c\"\n")
        );
        assert_eq!(config_header("X: a\nY: b"), None);
    }

    #[test]
    fn config_bodies_are_quoted_the_same_way() {
        assert_eq!(
            config_body(r#"{"a":"b\"c\\n"}"#).as_deref(),
            Some("data-binary = \"{\\\"a\\\":\\\"b\\\\\\\"c\\\\\\\\n\\\"}\"\n")
        );
        assert_eq!(config_body("{\n}"), None);
    }

    #[test]
    fn a_body_s_delete_and_c1_characters_are_escaped_not_refused() {
        // As a message pasted from a terminal or an old document holds
        // them: JSON leaves these as they are inside a string.
        let message = "a\u{7f}b\u{80}c\u{85}d\u{9f}e";
        let body = serde_json::json!({ "content": message }).to_string();
        assert!(body.contains('\u{7f}'), "serde_json does not escape them");
        let config = config_body(&body).expect("it can be sent");
        assert_eq!(
            config,
            "data-binary = \"{\\\"content\\\":\\\"a\\\\u007fb\\\\u0080c\\\\u0085d\\\\u009fe\\\"}\"\n"
        );
        assert!(!config.trim_end().chars().any(char::is_control));
        // What curl makes of the line is JSON for the same message.
        let quoted = config
            .strip_prefix("data-binary = \"")
            .and_then(|rest| rest.strip_suffix("\"\n"))
            .expect("one quoted line");
        let mut sent = String::new();
        let mut chars = quoted.chars();
        while let Some(c) = chars.next() {
            sent.push(if c == '\\' {
                chars.next().expect("escaped")
            } else {
                c
            });
        }
        let parsed: serde_json::Value = serde_json::from_str(&sent).expect("JSON");
        assert_eq!(parsed["content"], message);
    }
}
