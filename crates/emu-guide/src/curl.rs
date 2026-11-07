//! Requests through `curl`, which macOS and Windows ship and Linux systems
//! nearly always have, as the app's box art and store downloads go.
//! Anything secret goes on `curl`'s standard input, never in its
//! arguments, which any process can read.

use std::io::Write;
use std::process::Stdio;

use emu_os::process::why_not_started;

/// What a server answered: its status and body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// The HTTP status, such as 200.
    pub status: u16,
    /// The body, whole.
    pub body: Vec<u8>,
}

/// Runs `curl` with `args` against `url`, writing `stdin` to it, and
/// returns what the server answered, whatever its status. Gives up after
/// `max_seconds`.
///
/// # Errors
///
/// What went wrong, in `curl`'s words, if `curl` cannot run or the
/// transfer fails: no connection, a timeout, a reply cut short.
pub fn request(
    args: &[&str],
    stdin: &[u8],
    url: &str,
    max_seconds: u32,
) -> Result<Response, String> {
    let mut child = emu_os::process::command("curl")
        .args(["--silent", "--show-error", "--connect-timeout", "10"])
        .args([
            "--user-agent",
            concat!(
                "ImagoEmulator/",
                env!("CARGO_PKG_VERSION"),
                " (Guide; +https://imagoemulator.com)"
            ),
        ])
        .arg("--max-time")
        .arg(max_seconds.to_string())
        .args(["--output", "-", "--write-out", "\n%{http_code}"])
        .args(args)
        .arg(url)
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

/// A `curl` config line sending `body` as the request's body, quoted as
/// [`config_header`] quotes. `None` if `body` holds a control character,
/// which compact JSON never does.
pub fn config_body(body: &str) -> Option<String> {
    if body.chars().any(char::is_control) {
        return None;
    }
    let quoted = body.replace('\\', "\\\\").replace('"', "\\\"");
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
}
