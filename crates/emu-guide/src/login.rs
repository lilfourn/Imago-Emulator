//! Connect OpenRouter, as Imago does: OpenRouter's OAuth PKCE flow mints a
//! key for this computer. The app listens on a loopback port, the browser opens
//! OpenRouter's authorization page, and the code the browser brings back
//! is exchanged for the key, which is saved.
//!
//! The browser's return trip is held open until the key is saved, so the
//! page it lands on says how connecting actually went. Declining on
//! OpenRouter returns to the callback with no code, which ends the flow at
//! once. Anything else the browser asks for, such as a favicon, gets a 404
//! and the wait goes on.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::api::{ApiError, Endpoints};
use crate::key::KeyStore;
use crate::{pkce, url};

/// How long the browser has to come back: as long as OpenRouter's codes
/// last.
pub const TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// How often the wait looks for the browser and for being cancelled.
const POLL: Duration = Duration::from_millis(50);

/// How long a connection may take to send its request. A browser sends a
/// real one at once; a connection it opened ahead of time and left idle
/// holds the next one up this long at most.
const REQUEST_WAIT: Duration = Duration::from_secs(3);

/// The most of a request's head that is read.
const HEAD_LIMIT: usize = 16 * 1024;

/// How far connecting has got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// Open this page in the browser, where the player lets this Mac have
    /// a key.
    Open(String),
    /// The browser came back with a code, which is being exchanged for a
    /// key.
    Returned,
}

/// Why connecting ended without a key.
#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    /// No port to listen on, or no randomness for the verifier.
    #[error("Couldn't start connecting: {0}")]
    Setup(io::Error),
    /// The player declined on OpenRouter.
    #[error("Access was declined on OpenRouter.")]
    Declined,
    /// The browser never came back.
    #[error("OpenRouter didn't send the browser back in time.")]
    TimedOut,
    /// The app stopped waiting.
    #[error("Connecting was cancelled.")]
    Cancelled,
    /// OpenRouter would not give a key for the code.
    #[error("OpenRouter didn't issue a key: {0}")]
    Exchange(ApiError),
    /// The key came but could not be kept.
    #[error("The key couldn't be saved: {0}")]
    Save(io::Error),
}

/// The name a key minted on `device` carries in the player's OpenRouter key
/// list, so keys from different computers stay apart.
pub fn label(device: &str) -> String {
    match device.trim() {
        "" => "DS Emulator".to_owned(),
        device => format!("DS Emulator \u{b7} {device}"),
    }
}

/// This computer's name, or nothing if it can't be read: on macOS the
/// name in Sharing settings, on Windows its computer name, and on Linux the
/// "pretty" name the desktop's settings give it, or else its host name.
pub fn device_name() -> String {
    if cfg!(target_os = "macos") {
        emu_os::process::command("scutil")
            .args(["--get", "ComputerName"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .unwrap_or_default()
    } else if cfg!(windows) {
        std::env::var("COMPUTERNAME").unwrap_or_default()
    } else {
        let read = |path: &str| std::fs::read_to_string(path).ok();
        linux_device_name(
            read("/etc/machine-info").as_deref(),
            read("/proc/sys/kernel/hostname").as_deref(),
        )
    }
}

/// A Linux computer's name: the `PRETTY_HOSTNAME` in `machine_info`, the
/// text of `/etc/machine-info` (which `hostnamectl` and the desktop's
/// settings write), or else `hostname`, its host name.
fn linux_device_name(machine_info: Option<&str>, hostname: Option<&str>) -> String {
    let pretty = machine_info.and_then(|info| {
        info.lines().find_map(|line| {
            let value = line.trim().strip_prefix("PRETTY_HOSTNAME=")?;
            let value = value.trim();
            let unquoted = ['"', '\'']
                .iter()
                .find_map(|quote| value.strip_prefix(*quote)?.strip_suffix(*quote));
            Some(
                unquoted
                    .unwrap_or(value)
                    .replace("\\", "")
                    .trim()
                    .to_owned(),
            )
        })
    });
    pretty
        .filter(|name| !name.is_empty())
        .or_else(|| hostname.map(|name| name.trim().to_owned()))
        .unwrap_or_default()
}

/// Connects an OpenRouter account at `endpoints` and saves the key it mints
/// in `store`, labelled `label`. Says where to send the browser, and when
/// it is back, through `progress`. Waits for the browser up to `timeout`,
/// and stops waiting once `cancel` is set.
///
/// Blocks until connecting ends: run it on a thread of its own.
///
/// # Errors
///
/// Why no key was saved ([`LoginError`]).
pub fn connect(
    endpoints: &Endpoints,
    store: &KeyStore,
    label: &str,
    timeout: Duration,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> Result<(), LoginError> {
    let callback = Callback::bind().map_err(LoginError::Setup)?;
    let verifier = pkce::verifier().map_err(LoginError::Setup)?;
    progress(Progress::Open(endpoints.authorize_url(
        callback.port,
        &pkce::challenge(&verifier),
        label,
    )));
    let (code, browser) = match callback.wait(timeout, cancel)? {
        Reply::Granted { code, browser } => (code, browser),
        Reply::Denied(browser) => {
            respond(browser, "200 OK", &Page::Denied);
            return Err(LoginError::Declined);
        }
    };
    progress(Progress::Returned);
    let connected = endpoints
        .exchange(&code, &verifier)
        .map_err(LoginError::Exchange)
        .and_then(|key| store.save(&key).map_err(LoginError::Save));
    let page = match &connected {
        Ok(()) => Page::Connected,
        Err(error) => Page::Failed(error.to_string()),
    };
    respond(browser, "200 OK", &page);
    connected
}

/// The loopback port OpenRouter sends the browser back to. `localhost` may
/// resolve to either address family, so both loopbacks listen on one port
/// when the IPv6 one is free.
struct Callback {
    port: u16,
    listeners: Vec<TcpListener>,
}

/// How the browser came back to the callback.
enum Reply {
    /// Access granted: the code, and the browser's request, held open
    /// until its page can say how connecting went.
    Granted { code: String, browser: TcpStream },
    /// Access declined: the browser came back with no code.
    Denied(TcpStream),
}

impl Callback {
    fn bind() -> io::Result<Self> {
        let v4 = TcpListener::bind(("127.0.0.1", 0))?;
        let port = v4.local_addr()?.port();
        let mut listeners = vec![v4];
        listeners.extend(TcpListener::bind(("::1", port)).ok());
        for listener in &listeners {
            listener.set_nonblocking(true)?;
        }
        Ok(Self { port, listeners })
    }

    /// Answers requests until the browser comes back to `/callback`, for
    /// up to `timeout` or until `cancel` is set.
    fn wait(self, timeout: Duration, cancel: &AtomicBool) -> Result<Reply, LoginError> {
        let deadline = Instant::now() + timeout;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(LoginError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(LoginError::TimedOut);
            }
            let accepted = self
                .listeners
                .iter()
                .find_map(|listener| listener.accept().ok());
            let Some((mut stream, _)) = accepted else {
                thread::sleep(POLL);
                continue;
            };
            let Ok(head) = read_head(&mut stream) else {
                continue;
            };
            match callback(&head) {
                Some(Some(code)) => {
                    return Ok(Reply::Granted {
                        code,
                        browser: stream,
                    });
                }
                Some(None) => return Ok(Reply::Denied(stream)),
                None => respond(stream, "404 Not Found", &Page::NotFound),
            }
        }
    }
}

/// A request's head, as much of it as [`HEAD_LIMIT`] allows.
fn read_head(stream: &mut TcpStream) -> io::Result<String> {
    // A connection takes its listener's non-blocking mode on macOS.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(REQUEST_WAIT))?;
    stream.set_write_timeout(Some(REQUEST_WAIT))?;
    let mut head = Vec::with_capacity(2048);
    let mut chunk = [0u8; 1024];
    while !head.windows(4).any(|end| end == b"\r\n\r\n") && head.len() <= HEAD_LIMIT {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        head.extend_from_slice(&chunk[..read]);
    }
    Ok(String::from_utf8_lossy(&head).into_owned())
}

/// `None` unless the request is `GET /callback`; then its `code`, `None`
/// when it has none.
fn callback(head: &str) -> Option<Option<String>> {
    let mut request = head.lines().next()?.split_whitespace();
    if request.next()? != "GET" {
        return None;
    }
    let target = request.next()?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    (path == "/callback").then(|| url::query_value(query, "code").filter(|code| !code.is_empty()))
}

/// A page the callback answers the browser with.
enum Page {
    Connected,
    Denied,
    Failed(String),
    NotFound,
}

impl Page {
    /// `(title, heading, message)`.
    fn text(&self) -> (&'static str, &'static str, String) {
        let not_connected = ("OpenRouter not connected", "OpenRouter wasn't connected");
        match self {
            Self::Connected => (
                "OpenRouter connected",
                "OpenRouter is connected",
                "DS Emulator can now ask OpenRouter's models about your games. \
                 You can close this tab."
                    .to_owned(),
            ),
            Self::Denied => (
                not_connected.0,
                not_connected.1,
                "You declined access. To connect later, choose Connect OpenRouter \
                 in DS Emulator's settings."
                    .to_owned(),
            ),
            Self::Failed(reason) => {
                let stop = if reason.ends_with('.') { "" } else { "." };
                (
                    not_connected.0,
                    not_connected.1,
                    format!("{reason}{stop} Try again from DS Emulator's settings."),
                )
            }
            Self::NotFound => ("Not found", "Not found", String::new()),
        }
    }

    /// The page as HTML: plain, in the app's white and ink (or the
    /// system's dark scheme), with the one-time code taken out of the
    /// address bar.
    fn html(&self) -> String {
        let (title, heading, message) = self.text();
        format!(
            "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title>\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<style>:root{{color-scheme:light dark}}body{{margin:0;min-height:100vh;display:grid;place-items:center;\
font:15px/1.5 -apple-system,BlinkMacSystemFont,sans-serif;background:#fff;color:#232327}}\
@media (prefers-color-scheme:dark){{body{{background:#0b0b0d;color:#ededf0}}}}\
main{{max-width:400px;padding:32px;text-align:center}}h1{{font-size:19px;font-weight:600;margin:0 0 8px}}\
p{{margin:0;opacity:.72}}</style></head><body><main><h1>{heading}</h1><p>{}</p></main>\
<script>history.replaceState(null,\"\",location.pathname)</script></body></html>",
            escape(&message)
        )
    }
}

/// `text` safe inside HTML.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Answers the browser with `page` and closes its connection. A browser
/// that already went away is no failure of the flow.
fn respond(mut stream: TcpStream, status: &str, page: &Page) {
    let body = page.html();
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\n\
         cache-control: no-store\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    if stream.write_all(response.as_bytes()).is_ok() {
        stream.shutdown(Shutdown::Write).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake;
    use std::path::PathBuf;
    use std::sync::{Arc, mpsc};

    #[test]
    fn the_callback_tells_a_grant_from_a_denial() {
        assert_eq!(
            callback("GET /callback?code=abc%2B1 HTTP/1.1\r\nHost: localhost\r\n\r\n"),
            Some(Some("abc+1".into()))
        );
        assert_eq!(callback("GET /callback HTTP/1.1\r\n\r\n"), Some(None));
        assert_eq!(callback("GET /callback?code= HTTP/1.1\r\n\r\n"), Some(None));
        assert_eq!(callback("GET /favicon.ico HTTP/1.1\r\n\r\n"), None);
        assert_eq!(callback("GET /other?code=abc HTTP/1.1\r\n\r\n"), None);
        assert_eq!(callback("POST /callback?code=abc HTTP/1.1\r\n\r\n"), None);
        assert_eq!(callback(""), None);
    }

    #[test]
    fn a_key_is_labelled_with_its_mac() {
        assert_eq!(label(" Luke's Mac \n"), "DS Emulator · Luke's Mac");
        assert_eq!(label(""), "DS Emulator");
    }

    #[test]
    fn a_linux_computer_is_named_as_its_settings_name_it() {
        let info = "ICON_NAME=computer-laptop\nPRETTY_HOSTNAME=\"Luke's Laptop\"\nCHASSIS=laptop\n";
        assert_eq!(
            linux_device_name(Some(info), Some("thinkpad\n")),
            "Luke's Laptop"
        );
        assert_eq!(
            linux_device_name(Some("PRETTY_HOSTNAME=Desk\n"), Some("desk")),
            "Desk"
        );
        // Without a pretty name, the host name.
        assert_eq!(
            linux_device_name(Some("CHASSIS=desktop\n"), Some("thinkpad\n")),
            "thinkpad"
        );
        assert_eq!(
            linux_device_name(Some("PRETTY_HOSTNAME=\n"), Some("thinkpad")),
            "thinkpad"
        );
        assert_eq!(linux_device_name(None, Some("thinkpad")), "thinkpad");
        assert_eq!(linux_device_name(None, None), "");
    }

    #[test]
    fn a_failure_page_escapes_its_reason() {
        let html = Page::Failed("<bad> & \"worse\"".into()).html();
        assert!(html.contains("&lt;bad&gt; &amp; &quot;worse&quot;. Try again"));
        assert!(!html.contains("<bad>"));
        assert!(Page::Connected.html().contains("history.replaceState"));
    }

    /// What a browser gets asking `path` of the callback at `port`.
    fn browse(port: u16, path: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n"
        )
        .unwrap();
        let mut page = String::new();
        stream.read_to_string(&mut page).unwrap();
        page
    }

    /// The port and challenge in an authorization URL.
    fn port_and_challenge(url: &str) -> (u16, String) {
        let query = url.split_once('?').unwrap().1;
        let callback = url::query_value(query, "callback_url").unwrap();
        let port = callback
            .trim_start_matches("http://localhost:")
            .trim_end_matches("/callback")
            .parse()
            .unwrap();
        (port, url::query_value(query, "code_challenge").unwrap())
    }

    /// A key store in a folder of its own for one test, emptied first.
    fn store(name: &str) -> KeyStore {
        let folder: PathBuf = std::env::temp_dir()
            .join(format!("emu-guide-login-{}", std::process::id()))
            .join(name);
        std::fs::remove_dir_all(&folder).ok();
        KeyStore::new(folder)
    }

    /// What a [`connect`] run on a thread of its own ends with: its
    /// outcome and the progress it reported.
    type Run = thread::JoinHandle<(Result<(), LoginError>, Vec<Progress>)>;

    /// Runs [`connect`] against `endpoints` on a thread of its own: the
    /// authorization URL it opens, the cancel flag, and the run.
    fn start(
        endpoints: Endpoints,
        store: KeyStore,
        timeout: Duration,
    ) -> (String, Arc<AtomicBool>, Run) {
        let cancel = Arc::new(AtomicBool::new(false));
        let (opened, url) = mpsc::channel();
        let flag = cancel.clone();
        let flow = thread::spawn(move || {
            let mut seen = Vec::new();
            let outcome = connect(
                &endpoints,
                &store,
                "DS Emulator · Test",
                timeout,
                &flag,
                |p| {
                    if let Progress::Open(url) = &p {
                        opened.send(url.clone()).unwrap();
                    }
                    seen.push(p);
                },
            );
            (outcome, seen)
        });
        (url.recv().unwrap(), cancel, flow)
    }

    #[test]
    fn a_grant_is_exchanged_for_a_key_that_is_saved_before_the_page_says_so() {
        // The stand-in mints a key that shows what it was sent.
        let endpoints = fake::openrouter(|request| {
            assert_eq!(request.line, "POST /api/v1/auth/keys HTTP/1.1");
            assert_eq!(request.header("content-type"), Some("application/json"));
            let body = request.json();
            let field = |name| body[name].as_str().unwrap_or("").to_owned();
            let key = format!(
                "sk-or-v1-{}-{}-{}",
                field("code"),
                field("code_challenge_method"),
                field("code_verifier")
            );
            (200, serde_json::json!({ "key": key }).to_string())
        });
        let store = store("granted");
        let (url, _, flow) = start(endpoints, store.clone(), TIMEOUT);
        assert!(url.contains("key_label=DS%20Emulator%20%C2%B7%20Test"));
        let (port, challenge) = port_and_challenge(&url);

        assert!(browse(port, "/favicon.ico").starts_with("HTTP/1.1 404"));
        let page = browse(port, "/callback?code=minted");
        assert!(page.starts_with("HTTP/1.1 200"), "{page}");
        assert!(page.contains("OpenRouter is connected"));

        let (outcome, progress) = flow.join().unwrap();
        outcome.unwrap();
        assert_eq!(progress.len(), 2);
        assert_eq!(progress[1], Progress::Returned);
        let key = store.load_with(None).unwrap();
        let verifier = key
            .secret()
            .strip_prefix("sk-or-v1-minted-S256-")
            .expect("the code and method were sent");
        assert_eq!(
            pkce::challenge(verifier),
            challenge,
            "the verifier fits the challenge"
        );
    }

    #[test]
    fn a_refused_code_leaves_no_key_and_the_page_says_why() {
        let endpoints = fake::openrouter(|_| {
            (
                403,
                r#"{"error":{"code":403,"message":"Invalid code"}}"#.to_owned(),
            )
        });
        let store = store("refused");
        let (url, _, flow) = start(endpoints, store.clone(), TIMEOUT);
        let (port, _) = port_and_challenge(&url);
        let page = browse(port, "/callback?code=stale");
        assert!(page.contains("OpenRouter wasn't connected"));
        assert!(page.contains("OpenRouter didn't issue a key: Invalid code. Try again"));
        let (outcome, _) = flow.join().unwrap();
        assert!(matches!(
            outcome,
            Err(LoginError::Exchange(ApiError::Refused { status: 403, .. }))
        ));
        assert_eq!(store.load_with(None), None);
    }

    #[test]
    fn declining_ends_the_flow_at_once() {
        let store = store("declined");
        let (url, _, flow) = start(Endpoints::at("http://127.0.0.1:9"), store.clone(), TIMEOUT);
        let (port, _) = port_and_challenge(&url);
        assert!(browse(port, "/callback").contains("You declined access."));
        assert!(matches!(flow.join().unwrap().0, Err(LoginError::Declined)));
        assert_eq!(store.load_with(None), None);
    }

    #[test]
    fn cancelling_or_waiting_too_long_stops_the_wait() {
        let (_, cancel, flow) = start(Endpoints::openrouter(), store("cancelled"), TIMEOUT);
        cancel.store(true, Ordering::Relaxed);
        assert!(matches!(flow.join().unwrap().0, Err(LoginError::Cancelled)));

        let (_, _, flow) = start(
            Endpoints::openrouter(),
            store("timed-out"),
            Duration::from_millis(100),
        );
        assert!(matches!(flow.join().unwrap().0, Err(LoginError::TimedOut)));
    }
}
