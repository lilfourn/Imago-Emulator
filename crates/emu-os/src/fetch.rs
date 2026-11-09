//! Downloads through `curl`, which macOS and Windows ship and Linux
//! systems nearly always have: the one place that says what the app may
//! fetch, and how much of it.
//!
//! Every request goes to an `https://` address, and so does every redirect
//! it follows. The address is checked before `curl` starts, and reaches it
//! as the value of `--url`, as one address (`--globoff`), so nothing in an
//! address from a catalog can be read as an option, a list of addresses or
//! a file. A [`Download`] is also held to a size: `curl` is told it, and
//! refuses a file the server says is bigger, and the app stops one that
//! grows past it all the same, as only newer `curl`s do when the server
//! does not say.
//!
//! Tests may also fetch from a stand-in server on this computer, over
//! `http://127.0.0.1:`.

use std::fmt;
use std::fs;
use std::io::{self, Read as _};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::Duration;

use crate::process::{self, why_not_started};

/// curl: macOS's own, and elsewhere the one on `PATH`, which Windows 10
/// and later ship in System32 and Linux has as a package.
const CURL: &str = if cfg!(target_os = "macos") {
    "/usr/bin/curl"
} else {
    "curl"
};

/// curl's exit status for a file bigger than `--max-filesize`.
const TOO_BIG: i32 = 63;

/// How much may follow a download's body on curl's output: a line break
/// and the server's status.
const STATUS_ROOM: u64 = 16;

/// How often a file being downloaded is measured.
const MEASURE_EVERY: Duration = Duration::from_millis(50);

/// Where a request may go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Allow {
    /// `https://` addresses only.
    Https,
    /// Those, and a stand-in server on this computer, `http://127.0.0.1:`
    /// and a port, for tests.
    HttpsOrLoopback,
}

impl Allow {
    /// [`Allow::HttpsOrLoopback`] if `testing`, which a crate passes as
    /// `cfg!(test)`, and [`Allow::Https`] otherwise.
    #[must_use]
    pub const fn or_loopback_if(testing: bool) -> Self {
        if testing {
            Self::HttpsOrLoopback
        } else {
            Self::Https
        }
    }

    /// Whether `url` is an address the app may fetch.
    #[must_use]
    pub fn takes(self, url: &str) -> bool {
        if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return false;
        }
        let host = |scheme: &str| url.strip_prefix(scheme).filter(|rest| !rest.is_empty());
        host("https://").is_some()
            || (self == Self::HttpsOrLoopback && host("http://127.0.0.1:").is_some_and(is_port))
    }

    /// The protocols curl is held to, as `--proto` takes them.
    fn protocols(self) -> &'static str {
        match self {
            Self::Https => "=https",
            Self::HttpsOrLoopback => "=https,http",
        }
    }
}

/// Whether `rest`, what follows a host and its colon, starts with a port
/// that ends the address's host: `8080/page`, not `80@example.com`.
fn is_port(rest: &str) -> bool {
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && matches!(rest.as_bytes().get(digits), None | Some(b'/' | b'?' | b'#'))
}

/// Why a download failed.
#[derive(Debug)]
pub enum Error {
    /// The address is not one the app fetches, so curl never started.
    Refused,
    /// curl could not be started.
    NotStarted(io::Error),
    /// curl failed, and said this: the connection dropped, timed out, or
    /// led somewhere the app does not fetch.
    Failed(String),
    /// The server answered with this status, not 200.
    Status(String),
    /// The download is bigger than it may be, and was stopped.
    TooBig,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused => f.write_str("its address is not an https:// one"),
            Self::NotStarted(error) => f.write_str(&why_not_started("curl", error)),
            Self::Failed(why) => f.write_str(why),
            Self::Status(status) => write!(f, "the server answered with HTTP status {status}"),
            Self::TooBig => f.write_str("it is bigger than it may be"),
        }
    }
}

impl std::error::Error for Error {}

/// What the app calls itself to servers:
/// `ImagoEmulator/0.1.0 (+https://imagoemulator.com)`, or with a `part` of
/// the app, `ImagoEmulator/0.1.0 (Guide; +https://imagoemulator.com)`.
#[must_use]
pub fn user_agent(part: Option<&str>) -> String {
    let version = env!("CARGO_PKG_VERSION");
    match part {
        Some(part) => format!("ImagoEmulator/{version} ({part}; +https://imagoemulator.com)"),
        None => format!("ImagoEmulator/{version} (+https://imagoemulator.com)"),
    }
}

/// `curl`, asked for `url`, with what every request of the app's has: it
/// is quiet but for errors, gives a server ten seconds to connect, names
/// the app (and the `part` of it asking, see [`user_agent`]), and goes
/// only where `allow` lets it, redirects included. The caller adds what
/// its own request needs.
///
/// # Errors
///
/// [`Error::Refused`] if `url` is not an address `allow` takes.
pub fn curl(url: &str, part: Option<&str>, allow: Allow) -> Result<Command, Error> {
    if !allow.takes(url) {
        return Err(Error::Refused);
    }
    let mut command = process::command(CURL);
    command
        .args(["--silent", "--show-error", "--connect-timeout", "10"])
        .arg("--user-agent")
        .arg(user_agent(part))
        .args(["--proto", allow.protocols()])
        .args(["--proto-redir", allow.protocols()])
        // One address: `[1-9]` and `{a,b}` in it are not lists of them.
        .arg("--globoff")
        .args(["--url", url]);
    Ok(command)
}

/// A file to download.
#[derive(Debug, Clone, Copy)]
pub struct Download<'a> {
    /// Its address.
    pub url: &'a str,
    /// Where it may come from.
    pub allow: Allow,
    /// The longest it may take in all, in seconds.
    pub seconds: u32,
    /// The most bytes it may be.
    pub max_bytes: u64,
    /// Whether it is a big file to wait for, as an installer's is: then
    /// one stalled below 1 KB/s for a minute is given up, and one that
    /// fails in passing is tried twice more. Otherwise the server may
    /// send it compressed.
    pub patient: bool,
}

impl Download<'_> {
    /// Downloads it whole into memory.
    ///
    /// # Errors
    ///
    /// Why it did not come ([`Error`]).
    pub fn bytes(&self) -> Result<Vec<u8>, Error> {
        let mut command = self.command()?;
        command.args(["--output", "-", "--write-out", "\n%{http_code}"]);
        let mut child = command.spawn().map_err(Error::NotStarted)?;
        let most = self.max_bytes.saturating_add(STATUS_ROOM);
        let mut body = Vec::new();
        if let Some(output) = child.stdout.take() {
            let read = output.take(most.saturating_add(1)).read_to_end(&mut body);
            if read.is_err() || body.len() as u64 > most {
                stop(child);
                return Err(match read {
                    Err(error) => Error::Failed(error.to_string()),
                    Ok(_) => Error::TooBig,
                });
            }
        }
        let output = child
            .wait_with_output()
            .map_err(|error| Error::Failed(error.to_string()))?;
        ended_well(output.status, &output.stderr)?;
        let split = body.iter().rposition(|&byte| byte == b'\n').unwrap_or(0);
        answered_200(&body[split..])?;
        body.truncate(split);
        if body.len() as u64 > self.max_bytes {
            return Err(Error::TooBig);
        }
        Ok(body)
    }

    /// Downloads it into the file `to`, written as it comes, replacing
    /// any file there. A download that fails leaves no file.
    ///
    /// # Errors
    ///
    /// Why it did not come ([`Error`]).
    pub fn to_file(&self, to: &Path) -> Result<(), Error> {
        // Gone first: a bigger file there would count as the download's.
        match fs::remove_file(to) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                return Err(Error::Failed(error.to_string()));
            }
            _ => {}
        }
        let downloaded = self.write_to(to);
        if downloaded.is_err() {
            fs::remove_file(to).ok();
        }
        downloaded
    }

    fn write_to(&self, to: &Path) -> Result<(), Error> {
        let mut command = self.command()?;
        command
            .arg("--output")
            .arg(to)
            .args(["--write-out", "%{http_code}"]);
        let mut child = command.spawn().map_err(Error::NotStarted)?;
        let size = || fs::metadata(to).map_or(0, |file| file.len());
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if size() > self.max_bytes => {
                    stop(child);
                    return Err(Error::TooBig);
                }
                Ok(None) => thread::sleep(MEASURE_EVERY),
                Err(error) => {
                    stop(child);
                    return Err(Error::Failed(error.to_string()));
                }
            }
        }
        // What it printed is short: the status, and why it failed if it did.
        let output = child
            .wait_with_output()
            .map_err(|error| Error::Failed(error.to_string()))?;
        ended_well(output.status, &output.stderr)?;
        answered_200(&output.stdout)?;
        if size() > self.max_bytes {
            return Err(Error::TooBig);
        }
        Ok(())
    }

    /// curl for this download, but for where it goes.
    fn command(&self) -> Result<Command, Error> {
        let mut command = curl(self.url, None, self.allow)?;
        command
            .arg("--location")
            .arg("--max-time")
            .arg(self.seconds.to_string())
            .arg("--max-filesize")
            .arg(self.max_bytes.to_string());
        if self.patient {
            command.args([
                "--speed-limit",
                "1024",
                "--speed-time",
                "60",
                "--retry",
                "2",
            ]);
        } else {
            command.arg("--compressed");
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Ok(command)
    }
}

/// Stops `curl`, which is still downloading, and waits for it to end.
fn stop(mut curl: Child) {
    curl.kill().ok();
    curl.wait().ok();
}

/// Whether curl ended well, by its own `status`: a transfer cut short can
/// still say 200. If not, why, from what it printed as `errors`.
fn ended_well(status: ExitStatus, errors: &[u8]) -> Result<(), Error> {
    if status.success() {
        return Ok(());
    }
    if status.code() == Some(TOO_BIG) {
        return Err(Error::TooBig);
    }
    let errors = String::from_utf8_lossy(errors);
    let said = errors.lines().map(str::trim).rfind(|line| !line.is_empty());
    Err(Error::Failed(
        said.map_or_else(|| status.to_string(), str::to_owned),
    ))
}

/// Whether the server's status, as curl wrote it out, is 200.
fn answered_200(written: &[u8]) -> Result<(), Error> {
    match String::from_utf8_lossy(written).trim() {
        "200" => Ok(()),
        status => Err(Error::Status(status.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::net::{TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A megabyte of a body that never says how long it is.
    const ENDLESS: usize = 1 << 20;

    /// A stand-in server on this computer: where it is, and how many
    /// requests it has had.
    struct Server {
        site: String,
        requests: Arc<AtomicUsize>,
    }

    /// Serves on a loopback port, answering each request by its path.
    fn serve() -> Server {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a port");
        let site = format!("http://{}", listener.local_addr().expect("address"));
        let requests = Arc::new(AtomicUsize::new(0));
        let (counted, own) = (Arc::clone(&requests), site.clone());
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                counted.fetch_add(1, Ordering::SeqCst);
                let own = own.clone();
                thread::spawn(move || answer(stream, &own));
            }
        });
        Server { site, requests }
    }

    /// Answers the request on `stream`, at the server `site`.
    fn answer(mut stream: TcpStream, site: &str) {
        let mut head = Vec::new();
        let mut chunk = [0; 1024];
        while !head.windows(4).any(|end| end == b"\r\n\r\n") {
            match stream.read(&mut chunk) {
                Ok(read) if read > 0 => head.extend_from_slice(&chunk[..read]),
                _ => return,
            }
        }
        let head = String::from_utf8_lossy(&head).into_owned();
        let path = head.split_whitespace().nth(1).unwrap_or_default();
        let with_length = |status: &str, body: &[u8]| {
            let mut reply = format!(
                "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            )
            .into_bytes();
            reply.extend_from_slice(body);
            reply
        };
        let redirect = |to: &str| {
            format!("HTTP/1.1 302 Found\r\nlocation: {to}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                .into_bytes()
        };
        let reply = match path {
            "/game.gb" => with_length("200 OK", b"a game"),
            "/agent" => {
                let agent = head
                    .lines()
                    .find_map(|line| line.strip_prefix("User-Agent: "))
                    .unwrap_or_default();
                with_length("200 OK", agent.as_bytes())
            }
            "/moved" => redirect(&format!("{site}/game.gb")),
            "/to-a-file" => redirect("file:///etc/hosts"),
            "/to-ftp" => redirect("ftp://127.0.0.1:1/game.gb"),
            "/declared" => with_length("200 OK", &vec![b'x'; ENDLESS]),
            // No length: the body ends when the connection closes.
            "/endless" => {
                let mut reply = b"HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n".to_vec();
                reply.extend(vec![b'x'; ENDLESS]);
                reply
            }
            _ => with_length("404 Not Found", b"<html>Not Found</html>"),
        };
        // curl hangs up on a download it stops.
        stream.write_all(&reply).ok();
    }

    fn download(url: &str, max_bytes: u64) -> Download<'_> {
        Download {
            url,
            allow: Allow::HttpsOrLoopback,
            seconds: 20,
            max_bytes,
            patient: false,
        }
    }

    /// A fresh, empty folder for `test`.
    fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("emu-os-fetch-{test}-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn only_https_addresses_are_taken() {
        for url in [
            "https://example.com/game.zip",
            "https://example.com/a%20b.gb?x=[1]",
        ] {
            assert!(Allow::Https.takes(url), "{url}");
            assert!(Allow::HttpsOrLoopback.takes(url), "{url}");
        }
        for url in [
            "http://example.com/game.zip",
            "ftp://example.com/game.zip",
            "file:///etc/passwd",
            "/etc/passwd",
            "example.com/game.zip",
            "HTTPS://example.com/",
            "https://",
            "",
            // What curl would read as an option, or as more than an address.
            "-V",
            "--output=/tmp/x",
            "-o/tmp/x https://example.com/",
            " https://example.com/",
            "https://example.com/a b",
            "https://example.com/\r\nX-Header: 1",
        ] {
            assert!(!Allow::Https.takes(url), "{url:?}");
            assert!(!Allow::HttpsOrLoopback.takes(url), "{url:?}");
        }
        // A test's stand-in server, and nothing that only looks like one.
        for url in ["http://127.0.0.1:8080/api?x=1", "http://127.0.0.1:9"] {
            assert!(Allow::HttpsOrLoopback.takes(url), "{url}");
            assert!(!Allow::Https.takes(url), "{url}");
        }
        for url in [
            "http://127.0.0.1/",
            "http://127.0.0.1:/",
            "http://127.0.0.1:80@example.com/",
            "http://127.0.0.1:80.example.com/",
            "http://127.0.0.1.example.com:80/",
            "http://localhost:8080/",
        ] {
            assert!(!Allow::HttpsOrLoopback.takes(url), "{url}");
        }
        assert_eq!(Allow::or_loopback_if(false), Allow::Https);
        assert_eq!(Allow::or_loopback_if(true), Allow::HttpsOrLoopback);
    }

    #[test]
    fn every_request_is_held_to_https_and_one_address() {
        let args = |command: &Command| -> Vec<String> {
            command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect()
        };
        let url = "https://example.com/{a,b}/[1-9].zip";
        let command = curl(url, None, Allow::Https).expect("an address it takes");
        let program = command.get_program().to_string_lossy().into_owned();
        if cfg!(target_os = "macos") {
            assert_eq!(program, "/usr/bin/curl");
        } else {
            assert_eq!(program, "curl");
        }
        let agent = format!(
            "ImagoEmulator/{} (+https://imagoemulator.com)",
            env!("CARGO_PKG_VERSION")
        );
        assert_eq!(
            args(&command),
            [
                "--silent",
                "--show-error",
                "--connect-timeout",
                "10",
                "--user-agent",
                agent.as_str(),
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--globoff",
                "--url",
                url,
            ]
        );
        assert_eq!(
            user_agent(Some("Guide")),
            format!(
                "ImagoEmulator/{} (Guide; +https://imagoemulator.com)",
                env!("CARGO_PKG_VERSION")
            )
        );
        // An address the app does not fetch never reaches curl.
        for url in ["-V", "http://example.com/", "file:///etc/passwd"] {
            assert!(matches!(curl(url, None, Allow::Https), Err(Error::Refused)));
        }

        let pinned = Download {
            url,
            allow: Allow::Https,
            seconds: 1800,
            max_bytes: 60_000_000,
            patient: true,
        };
        let command = pinned.command().expect("an address it takes");
        assert_eq!(
            args(&command)[13..],
            [
                "--location",
                "--max-time",
                "1800",
                "--max-filesize",
                "60000000",
                "--speed-limit",
                "1024",
                "--speed-time",
                "60",
                "--retry",
                "2",
            ]
        );
        let quick = Download {
            patient: false,
            seconds: 60,
            ..pinned
        };
        let command = quick.command().expect("an address it takes");
        assert_eq!(
            args(&command)[13..],
            [
                "--location",
                "--max-time",
                "60",
                "--max-filesize",
                "60000000",
                "--compressed",
            ]
        );
    }

    #[test]
    fn a_download_comes_whole_into_memory_or_a_file() {
        let server = serve();
        let dir = temp_dir("whole");
        let url = format!("{}/game.gb", server.site);
        assert_eq!(download(&url, 6).bytes().expect("downloaded"), b"a game");
        let to = dir.join("game.gb");
        fs::write(&to, b"what was there before, and longer").expect("write");
        download(&url, 6).to_file(&to).expect("downloaded");
        assert_eq!(fs::read(&to).expect("read"), b"a game");
        // A redirect is followed to an address the app fetches.
        let moved = format!("{}/moved", server.site);
        assert_eq!(download(&moved, 6).bytes().expect("followed"), b"a game");
        // The app says who it is.
        let agent = format!("{}/agent", server.site);
        assert_eq!(
            download(&agent, 100).bytes().expect("downloaded"),
            user_agent(None).as_bytes()
        );
        // A patient download, as an installer's, comes the same way.
        let patient = Download {
            patient: true,
            ..download(&url, 6)
        };
        patient.to_file(&to).expect("downloaded");
        assert_eq!(fs::read(&to).expect("read"), b"a game");
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_file_that_is_not_there_says_the_servers_status_and_leaves_nothing() {
        let server = serve();
        let dir = temp_dir("missing");
        let url = format!("{}/gone.gb", server.site);
        let error = download(&url, 1000).bytes().expect_err("gone");
        assert!(
            matches!(&error, Error::Status(status) if status == "404"),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            "the server answered with HTTP status 404"
        );
        let to = dir.join("gone.gb");
        let error = download(&url, 1000).to_file(&to).expect_err("gone");
        assert!(
            matches!(&error, Error::Status(status) if status == "404"),
            "{error:?}"
        );
        assert!(!to.exists(), "the server's page is not kept as the file");
        // Nothing listens at port 9 of the loopback.
        let error = download("http://127.0.0.1:9/game.gb", 1000)
            .to_file(&to)
            .expect_err("no server");
        assert!(matches!(error, Error::Failed(_)), "{error:?}");
        assert!(error.to_string().starts_with("curl: (7) "), "{error}");
        assert!(!to.exists());
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_download_past_its_size_is_stopped() {
        let server = serve();
        let dir = temp_dir("too-big");
        let to = dir.join("game.gb");
        // Whether or not the server says how long it is.
        for path in ["/declared", "/endless"] {
            let url = format!("{}{path}", server.site);
            let cap = (ENDLESS / 4) as u64;
            let error = download(&url, cap).bytes().expect_err("too big");
            assert!(matches!(error, Error::TooBig), "{path}: {error:?}");
            let error = download(&url, cap).to_file(&to).expect_err("too big");
            assert!(matches!(error, Error::TooBig), "{path}: {error:?}");
            assert!(!to.exists(), "{path}: what came of it is removed");
            // Up to its size, it comes.
            let whole = download(&url, ENDLESS as u64).bytes().expect("fits");
            assert_eq!(whole.len(), ENDLESS, "{path}");
            download(&url, ENDLESS as u64).to_file(&to).expect("fits");
            assert_eq!(fs::metadata(&to).expect("file").len(), ENDLESS as u64);
            fs::remove_file(&to).expect("remove");
        }
        fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn nothing_but_an_address_the_app_fetches_reaches_curl() {
        let server = serve();
        let dir = temp_dir("refused");
        let to = dir.join("game.gb");
        let plain = format!("{}/game.gb", server.site);
        let refused = [
            // As the store's catalogs could say.
            "-V",
            "--output=/tmp/x",
            "file:///etc/hosts",
            "http://example.com/game.gb",
            plain.as_str(),
        ];
        for url in refused {
            let download = Download {
                allow: Allow::Https,
                ..download(url, 1000)
            };
            assert!(matches!(download.bytes(), Err(Error::Refused)), "{url}");
            assert!(
                matches!(download.to_file(&to), Err(Error::Refused)),
                "{url}"
            );
            assert!(!to.exists());
        }
        assert_eq!(
            server.requests.load(Ordering::SeqCst),
            0,
            "the server on this computer was never asked"
        );
        // Nor does a redirect lead anywhere else.
        for path in ["/to-a-file", "/to-ftp"] {
            let url = format!("{}{path}", server.site);
            let error = download(&url, 1000).bytes().expect_err("refused");
            assert!(matches!(error, Error::Failed(_)), "{path}: {error:?}");
            assert!(error.to_string().contains("disabled"), "{path}: {error}");
            assert!(download(&url, 1000).to_file(&to).is_err(), "{path}");
            assert!(!to.exists());
        }
        fs::remove_dir_all(&dir).expect("clean up");
    }
}
