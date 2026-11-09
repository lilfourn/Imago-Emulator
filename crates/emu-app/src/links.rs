//! Opening links in the browser.
//!
//! Many of the app's links come from the network: Guide's answers quote
//! wikis that anyone can edit, and a store page's website is whatever its
//! catalog says. Handed an address, the system opens far more than web
//! pages: on Windows a program's path runs it, and on macOS an address can
//! start an app or mount a share. So every link goes through
//! [`open_web_url`], which opens web addresses and nothing else.

use gpui::App;

/// Whether `url` is a web address: `https://` or `http://`, in any case,
/// then something, with no control characters anywhere.
pub fn is_web_url(url: &str) -> bool {
    let after = |scheme: &str| {
        let start = url.get(..scheme.len())?;
        start
            .eq_ignore_ascii_case(scheme)
            .then(|| &url[scheme.len()..])
    };
    after("https://")
        .or_else(|| after("http://"))
        .is_some_and(|rest| !rest.is_empty() && !url.chars().any(char::is_control))
}

/// Opens `url` in the browser if it is a web address ([`is_web_url`]), and
/// does nothing with anything else: a `file:` or `smb:` address, another
/// app's scheme, or a path. Returns whether it was opened.
pub fn open_web_url(cx: &mut App, url: &str) -> bool {
    let web = is_web_url(url);
    if web {
        cx.open_url(url);
    }
    web
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_addresses_are_links() {
        for url in [
            "https://bulbapedia.bulbagarden.net/wiki/Pikachu_(Pok%C3%A9mon)",
            "http://example.com",
            "HTTPS://Example.com/a?b=c#d",
            "https://openrouter.ai/auth?callback_url=http://localhost:3000/callback",
            "https://ja.wikipedia.org/wiki/ポケットモンスター",
        ] {
            assert!(is_web_url(url), "{url}");
        }
        for url in [
            "",
            "https://",
            "http:/example.com",
            "file:///Applications/Calculator.app",
            "file://host/share/run.exe",
            "smb://host/share",
            "javascript:alert(1)",
            "data:text/html,<script>",
            "ms-msdt:/id PCWDiagnostic",
            "x-apple.systempreferences:",
            "C:\\Windows\\System32\\calc.exe",
            "\\\\host\\share\\run.exe",
            "/usr/bin/open",
            "-a Calculator",
            " https://example.com",
            "ftp://example.com/file",
            "mailto:someone@example.com",
            "https://example.com/\nfile:///etc/passwd",
            "htt\u{0}ps://example.com",
            // Text that only names the scheme further in.
            "see https://example.com",
            "ǆhttps://example.com",
        ] {
            assert!(!is_web_url(url), "{url:?}");
        }
    }
}
