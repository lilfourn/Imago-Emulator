//! A stand-in for OpenRouter in tests: a loopback server that answers each
//! request as a test says, so `curl` really runs, offline.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use crate::api::Endpoints;

/// A request the stand-in received.
pub struct Request {
    /// Its request line, such as `GET /api/v1/key HTTP/1.1`.
    pub line: String,
    /// Its headers, names lowercased, in order.
    pub headers: Vec<(String, String)>,
    /// Its body, by its `content-length`.
    pub body: Vec<u8>,
}

impl Request {
    /// The value of header `name` (lowercase), if sent.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The body as JSON, or `null`.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or_default()
    }
}

/// Serves on a loopback port, answering each request with the status and
/// JSON `reply` makes of it, and returns where it is.
pub fn openrouter(reply: fn(&Request) -> (u16, String)) -> Endpoints {
    Endpoints::at(serve(reply))
}

/// Serves on a loopback port as [`openrouter`] does, and returns its
/// address, such as `http://127.0.0.1:50123`.
pub fn serve(reply: fn(&Request) -> (u16, String)) -> String {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let Some(request) = read(&mut stream) else {
                continue;
            };
            let (status, json) = reply(&request);
            let response = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{json}",
                json.len()
            );
            stream.write_all(response.as_bytes()).ok();
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// The request on `stream`, whole.
fn read(stream: &mut TcpStream) -> Option<Request> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..read]);
        let Some(end) = bytes.windows(4).position(|end| end == b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&bytes[..end]).into_owned();
        let mut lines = head.lines();
        let line = lines.next()?.to_owned();
        let headers: Vec<(String, String)> = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_lowercase(), value.trim().to_owned()))
            .collect();
        let length = headers
            .iter()
            .find(|(name, _)| name == "content-length")
            .and_then(|(_, length)| length.parse().ok())
            .unwrap_or(0);
        if bytes.len() >= end + 4 + length {
            let body = bytes[end + 4..end + 4 + length].to_vec();
            return Some(Request {
                line,
                headers,
                body,
            });
        }
    }
}

/// Serves on a loopback port as [`openrouter`] does, but answers with a
/// stream: the status `reply` gives, then each of its pieces in turn,
/// `pause` apart, as server-sent events arrive, and then closes.
pub fn stream(reply: fn(&Request) -> (u16, Vec<String>), pause: std::time::Duration) -> Endpoints {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let Some(request) = read(&mut stream) else {
                continue;
            };
            let (status, pieces) = reply(&request);
            thread::spawn(move || {
                let head = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: text/event-stream\r\n\
                     connection: close\r\n\r\n"
                );
                if stream.write_all(head.as_bytes()).is_err() {
                    return;
                }
                for piece in pieces {
                    if stream.write_all(piece.as_bytes()).is_err() {
                        return;
                    }
                    stream.flush().ok();
                    thread::sleep(pause);
                }
            });
        }
    });
    Endpoints::at(format!("http://127.0.0.1:{port}"))
}
