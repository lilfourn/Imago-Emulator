//! Answers from OpenRouter's chat completions, streamed as they are
//! written.
//!
//! One `curl` child per answer. The key and the request go to it on its
//! standard input as a config file, so neither shows in the process list.
//! Its output is read line by line: OpenRouter's server-sent events, then
//! the HTTP status that `--write-out` adds after the body.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use base64::Engine;
use serde_json::{Value, json};

use crate::api::Endpoints;
use crate::curl;

/// The model Guide asks: GPT-6 Luna, pinned rather than the alias that
/// moves to the next Luna without notice.
pub const MODEL: &str = "openai/gpt-6-luna";

/// The model OpenRouter falls back to when every provider of [`MODEL`]
/// fails.
pub const FALLBACK_MODEL: &str = "openai/gpt-5.6-luna";

/// How long a stream may go without a byte before it counts as failed.
/// OpenRouter sends keep-alive comments while a search runs.
const IDLE_SECONDS: u32 = 60;

/// Room for thinking and the answer together.
const MAX_TOKENS: u32 = 4000;

/// How hard the model thinks before answering.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    /// No thinking: the quickest answers.
    None,
    /// A little.
    #[default]
    Low,
    /// More.
    Medium,
    /// The most Guide offers.
    High,
}

impl Effort {
    /// OpenRouter's name for it.
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// Who said a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The player.
    User,
    /// The model.
    Assistant,
}

/// One message of a chat, as sent.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// Who said it.
    pub role: Role,
    /// What was said, Markdown for the model's answers.
    pub text: String,
    /// PNG pictures sent with it: the player's screenshots.
    pub images: Vec<Arc<[u8]>>,
    /// The model's reasoning blocks, exactly as they streamed, which go
    /// back unchanged with later questions.
    pub reasoning_details: Vec<Value>,
}

impl Message {
    /// A question from the player, with any screenshots.
    pub fn user(text: impl Into<String>, images: Vec<Arc<[u8]>>) -> Self {
        Self {
            role: Role::User,
            text: text.into(),
            images,
            reasoning_details: Vec::new(),
        }
    }

    /// An earlier answer.
    pub fn assistant(text: impl Into<String>, reasoning_details: Vec<Value>) -> Self {
        Self {
            role: Role::Assistant,
            text: text.into(),
            images: Vec::new(),
            reasoning_details,
        }
    }
}

/// One question to the model, with everything that goes with it.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// The model to ask.
    pub model: String,
    /// The system prompt.
    pub system: String,
    /// The chat so far, ending with the question.
    pub messages: Vec<Message>,
    /// How hard to think.
    pub effort: Effort,
    /// The domains web search and fetch may use, or `None` to answer
    /// without looking anything up.
    pub lookups: Option<Vec<String>>,
    /// The chat's id, which keeps it on one provider so its start hits
    /// the provider's prompt cache.
    pub session: String,
}

impl Request {
    /// The request's JSON body.
    pub fn body(&self) -> Value {
        let mut messages = vec![json!({"role": "system", "content": self.system})];
        messages.extend(self.messages.iter().map(message_json));
        let mut body = json!({
            "model": self.model,
            "stream": true,
            "max_tokens": MAX_TOKENS,
            "session_id": self.session,
            "reasoning": {"effort": self.effort.name()},
            "verbosity": "low",
            "usage": {"include": true},
            "messages": messages,
        });
        if self.model == MODEL {
            body["models"] = json!([MODEL, FALLBACK_MODEL]);
            // Bedrock goes on billing after a stop; OpenAI and Azure don't.
            body["provider"] = json!({"order": ["openai", "azure"]});
        }
        if let Some(domains) = &self.lookups {
            body["tools"] = json!([
                {"type": "openrouter:web_search", "parameters": {
                    "engine": "exa", "max_results": 5, "max_uses": 2,
                    "allowed_domains": domains}},
                {"type": "openrouter:web_fetch", "parameters": {
                    "engine": "exa", "max_uses": 3, "max_content_tokens": 6000,
                    "allowed_domains": domains}},
            ]);
        }
        body
    }
}

fn message_json(message: &Message) -> Value {
    match message.role {
        Role::User if message.images.is_empty() => {
            json!({"role": "user", "content": message.text})
        }
        Role::User => {
            let mut content = vec![json!({"type": "text", "text": message.text})];
            content.extend(message.images.iter().map(|png| {
                let data = base64::engine::general_purpose::STANDARD.encode(png);
                json!({"type": "image_url",
                       "image_url": {"url": format!("data:image/png;base64,{data}")}})
            }));
            json!({"role": "user", "content": content})
        }
        Role::Assistant => {
            let mut value = json!({"role": "assistant", "content": message.text});
            if !message.reasoning_details.is_empty() {
                value["reasoning_details"] = Value::Array(message.reasoning_details.clone());
            }
            value
        }
    }
}

/// A page an answer drew on.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Citation {
    /// The page.
    pub url: String,
    /// Its title, if given.
    pub title: String,
    /// A little of what it says, if given.
    pub excerpt: String,
}

/// What an answer cost.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Usage {
    /// Tokens read.
    pub prompt: u64,
    /// Tokens written, thinking included.
    pub completion: u64,
    /// Of those, the thinking.
    pub reasoning: u64,
    /// In US dollars, searches included, if OpenRouter said.
    pub cost: Option<f64>,
    /// Web searches run.
    pub searches: u64,
}

/// Why an answer ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finish {
    /// It was done.
    Stop,
    /// It ran out of room, `max_tokens`.
    Length,
    /// The provider's filter stopped it.
    ContentFilter,
    /// Something else, in OpenRouter's words.
    Other(String),
}

/// What went wrong with an answer, in words for the player.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChatError {
    /// OpenRouter refused the key: revoked or deleted.
    #[error("OpenRouter no longer accepts this key — connect again")]
    KeyRefused,
    /// The account has no credits left.
    #[error("Your OpenRouter account is out of credits")]
    NoCredits,
    /// The key's own spending limit was reached.
    #[error("This key has reached its spending limit")]
    KeyLimit,
    /// The model's provider declined the question.
    #[error("The model's provider declined this question")]
    Declined,
    /// Too many requests; try again after the seconds given, if any.
    #[error("Too many requests — try again in a moment")]
    RateLimited(Option<u64>),
    /// OpenRouter couldn't be reached.
    #[error("Couldn't reach OpenRouter ({0})")]
    Unreachable(String),
    /// Nothing arrived for a minute.
    #[error("OpenRouter stopped answering")]
    Stalled,
    /// Any other error, in OpenRouter's words, with its status.
    #[error("{message}")]
    Other {
        /// The HTTP status, or 200 for an error inside the stream.
        status: u16,
        /// What OpenRouter said.
        message: String,
    },
}

impl ChatError {
    /// Whether asking again at once may work: a dropped connection, or a
    /// provider briefly down.
    fn retryable(&self) -> bool {
        match self {
            Self::Unreachable(_) => true,
            Self::Other { status, .. } => matches!(status, 502 | 503),
            _ => false,
        }
    }

    /// Whether connecting again would help.
    pub fn needs_connecting(&self) -> bool {
        *self == Self::KeyRefused
    }
}

/// One thing that happened while an answer streamed.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The model and provider answering, from the first chunk.
    Serving {
        /// The model, which differs from the one asked after a fallback.
        model: String,
        /// The provider, such as "OpenAI".
        provider: String,
    },
    /// OpenRouter is working without writing yet, as while it searches.
    Working,
    /// Readable thinking, when the model gives it.
    Reasoning(String),
    /// Reasoning blocks to send back with the next question.
    ReasoningDetails(Vec<Value>),
    /// More of the answer.
    Text(String),
    /// A page the answer drew on.
    Citation(Citation),
    /// What the answer cost.
    Usage(Usage),
    /// The answer ended.
    Finished(Finish),
    /// The answer failed; anything streamed before stays.
    Failed(ChatError),
    /// The player stopped it.
    Stopped,
}

/// Stops an answer under way: the `curl` child is killed, which closes the
/// connection, so the provider stops writing and billing.
#[derive(Debug, Clone, Default)]
pub struct Stop(Arc<StopState>);

#[derive(Debug, Default)]
struct StopState {
    stopped: AtomicBool,
    child: Mutex<Option<Child>>,
}

impl Stop {
    /// A stop not yet pressed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stops the answer.
    pub fn stop(&self) {
        self.0.stopped.store(true, Ordering::SeqCst);
        let mut child = self.0.child.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(child) = child.as_mut() {
            child.kill().ok();
        }
    }

    /// Whether [`Stop::stop`] was called.
    pub fn is_stopped(&self) -> bool {
        self.0.stopped.load(Ordering::SeqCst)
    }
}

/// Asks the model and passes on what streams back to `on_event`, until the
/// answer finishes, fails or is stopped; the last event is always
/// [`Event::Finished`], [`Event::Failed`] or [`Event::Stopped`]. Blocks
/// the calling thread for as long.
///
/// A connection error, 502 or 503 before anything streamed is retried once.
pub fn ask(
    endpoints: &Endpoints,
    key: &str,
    request: &Request,
    stop: &Stop,
    on_event: &mut dyn FnMut(Event),
) {
    let body = request.body().to_string();
    let (Some(header), Some(data)) = (
        curl::config_header(&format!("Authorization: Bearer {key}")),
        curl::config_body(&body),
    ) else {
        on_event(Event::Failed(ChatError::Other {
            status: 0,
            message: "The question couldn't be sent".to_owned(),
        }));
        return;
    };
    let config = format!("{header}{data}");
    for attempt in 0..2 {
        let mut streamed = false;
        let outcome = attempt_once(endpoints, &config, stop, &mut |event| {
            streamed |= matches!(
                event,
                Event::Text(_) | Event::Reasoning(_) | Event::Citation(_)
            );
            on_event(event);
        });
        match outcome {
            Outcome::Done => return,
            Outcome::Failed(error) => {
                if stop.is_stopped() {
                    on_event(Event::Stopped);
                    return;
                }
                if attempt == 0 && !streamed && error.retryable() {
                    thread::sleep(Duration::from_millis(800));
                    if !stop.is_stopped() {
                        continue;
                    }
                    on_event(Event::Stopped);
                    return;
                }
                on_event(Event::Failed(error));
                return;
            }
        }
    }
}

enum Outcome {
    /// The last event has been sent.
    Done,
    /// Failed before finishing, maybe worth another try.
    Failed(ChatError),
}

fn attempt_once(
    endpoints: &Endpoints,
    config: &str,
    stop: &Stop,
    on_event: &mut dyn FnMut(Event),
) -> Outcome {
    let spawned = Command::new("curl")
        .args(["--silent", "--show-error", "--no-buffer"])
        .args(["--connect-timeout", "10"])
        // Fails a stream that sends nothing, keep-alives included, for a
        // minute.
        .args(["--speed-limit", "1", "--speed-time"])
        .arg(IDLE_SECONDS.to_string())
        .args([
            "--user-agent",
            concat!(
                "ImagoEmulator/",
                env!("CARGO_PKG_VERSION"),
                " (Guide; +https://imagoemulator.com)"
            ),
        ])
        .args(["--header", "Content-Type: application/json"])
        .args(["--header", "HTTP-Referer: https://imagoemulator.com"])
        .args(["--header", "X-OpenRouter-Title: Imago Emulator"])
        .args(["--write-out", "\n%{http_code}\n", "--config", "-"])
        .arg(endpoints.chat_url())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => return Outcome::Failed(ChatError::Unreachable(error.to_string())),
    };
    let input = child.stdin.take();
    let output = child.stdout.take();
    let errors = child.stderr.take();
    {
        let mut slot = stop.0.child.lock().unwrap_or_else(PoisonError::into_inner);
        if stop.is_stopped() {
            child.kill().ok();
        }
        *slot = Some(child);
    }
    // The config may be larger than the pipe holds with screenshots, and
    // curl reads it before it writes anything, so a thread writes it.
    let config = config.to_owned();
    let writer = thread::spawn(move || {
        if let Some(mut input) = input {
            input.write_all(config.as_bytes()).ok();
        }
    });
    let mut reader = Reader::default();
    if let Some(output) = output {
        for line in BufReader::new(output).split(b'\n') {
            let Ok(line) = line else { break };
            for event in reader.line(&String::from_utf8_lossy(&line)) {
                on_event(event);
            }
        }
    }
    writer.join().ok();
    let mut stderr = String::new();
    if let Some(mut errors) = errors {
        std::io::Read::read_to_string(&mut errors, &mut stderr).ok();
    }
    let status = stop
        .0
        .child
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
        .and_then(|mut child| child.wait().ok());
    if stop.is_stopped() {
        on_event(Event::Stopped);
        return Outcome::Done;
    }
    let curl_failed = status.is_some_and(|status| !status.success());
    match reader.finish(curl_failed.then_some(stderr.trim())) {
        Ok(events) => {
            for event in events {
                on_event(event);
            }
            Outcome::Done
        }
        Err(error) => Outcome::Failed(error),
    }
}

/// Turns `curl`'s output, line by line, into events.
#[derive(Debug, Default)]
struct Reader {
    /// Lines that weren't events: an error's JSON body, then the status.
    other: Vec<String>,
    /// Whether the first chunk has been seen.
    served: bool,
    /// The first finish reason given; the usage chunk repeats it.
    finish: Option<Finish>,
    /// An error inside the stream.
    failure: Option<ChatError>,
    /// Whether `data: [DONE]` came.
    done: bool,
}

impl Reader {
    fn line(&mut self, line: &str) -> Vec<Event> {
        let line = line.trim_end_matches('\r');
        if line.starts_with(':') {
            return vec![Event::Working];
        }
        let Some(data) = line.strip_prefix("data:") else {
            if !line.trim().is_empty() {
                self.other.push(line.to_owned());
            }
            return Vec::new();
        };
        let data = data.trim();
        if data == "[DONE]" {
            self.done = true;
            return Vec::new();
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            return Vec::new();
        };
        self.chunk(&chunk)
    }

    fn chunk(&mut self, chunk: &Value) -> Vec<Event> {
        let mut events = Vec::new();
        if let Some(error) = chunk.get("error") {
            self.failure = Some(error_from(200, error, None));
            return events;
        }
        if !self.served {
            self.served = true;
            events.push(Event::Serving {
                model: text_at(chunk, "/model"),
                provider: text_at(chunk, "/provider"),
            });
        }
        let choice = chunk.pointer("/choices/0");
        if let Some(delta) = choice.and_then(|choice| choice.get("delta")) {
            if let Some(text) = delta.get("reasoning").and_then(Value::as_str)
                && !text.is_empty()
            {
                events.push(Event::Reasoning(text.to_owned()));
            }
            if let Some(details) = delta.get("reasoning_details").and_then(Value::as_array)
                && !details.is_empty()
            {
                events.extend(
                    details
                        .iter()
                        .filter_map(|detail| detail.get("text").or_else(|| detail.get("summary")))
                        .filter_map(Value::as_str)
                        .filter(|text| !text.is_empty())
                        .map(|text| Event::Reasoning(text.to_owned())),
                );
                events.push(Event::ReasoningDetails(details.clone()));
            }
            if let Some(annotations) = delta.get("annotations").and_then(Value::as_array) {
                events.extend(annotations.iter().filter_map(citation).map(Event::Citation));
            }
            if let Some(text) = delta.get("content").and_then(Value::as_str)
                && !text.is_empty()
            {
                events.push(Event::Text(text.to_owned()));
            }
        }
        if self.finish.is_none()
            && let Some(reason) = choice
                .and_then(|choice| choice.get("finish_reason"))
                .and_then(Value::as_str)
        {
            self.finish = Some(match reason {
                "stop" | "tool_calls" => Finish::Stop,
                "length" => Finish::Length,
                "content_filter" => Finish::ContentFilter,
                other => Finish::Other(other.to_owned()),
            });
        }
        if let Some(usage) = chunk.get("usage") {
            let count = |pointer: &str| usage.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
            events.push(Event::Usage(Usage {
                prompt: count("/prompt_tokens"),
                completion: count("/completion_tokens"),
                reasoning: count("/completion_tokens_details/reasoning_tokens"),
                cost: usage.get("cost").and_then(Value::as_f64),
                searches: count("/server_tool_use_details/web_search_requests"),
            }));
        }
        events
    }

    /// The events that end the stream, or why it failed. `curl_error` is
    /// what `curl` said if it failed.
    fn finish(mut self, curl_error: Option<&str>) -> Result<Vec<Event>, ChatError> {
        if let Some(failure) = self.failure.take() {
            return Err(failure);
        }
        let status: Option<u16> = self.other.last().and_then(|line| line.trim().parse().ok());
        if let Some(why) = curl_error {
            // curl's own words: "(28) Operation too slow" for the idle limit.
            if why.contains("too slow") || (why.contains("(28)") && self.served) {
                return Err(ChatError::Stalled);
            }
            if !self.served {
                let why = why.trim_start_matches("curl: ");
                return Err(ChatError::Unreachable(if why.is_empty() {
                    "the connection failed".to_owned()
                } else {
                    why.to_owned()
                }));
            }
        }
        match status {
            Some(200) | None if self.served => {
                let finish = self.finish.take().unwrap_or(if self.done {
                    Finish::Stop
                } else {
                    Finish::Other("the stream was cut short".to_owned())
                });
                Ok(vec![Event::Finished(finish)])
            }
            Some(status) => {
                let body = self.other[..self.other.len() - 1].join("\n");
                let value: Value = serde_json::from_str(&body).unwrap_or_default();
                let error = value.get("error").cloned().unwrap_or_default();
                Err(error_from(status, &error, Some(&body)))
            }
            None => Err(ChatError::Unreachable("no answer".to_owned())),
        }
    }
}

fn text_at(value: &Value, pointer: &str) -> String {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// A citation from an annotation, in either the Chat Completions shape
/// (`{type, url_citation: {…}}`) or the flat one.
fn citation(annotation: &Value) -> Option<Citation> {
    let inner = annotation.get("url_citation").unwrap_or(annotation);
    let url = inner.get("url").and_then(Value::as_str)?.to_owned();
    let excerpt: String = text_at(inner, "/content")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(280)
        .collect();
    Some(Citation {
        url,
        title: text_at(inner, "/title"),
        excerpt,
    })
}

/// The error OpenRouter described in `error` (`{code, message, metadata}`)
/// with HTTP `status`.
fn error_from(status: u16, error: &Value, body: Option<&str>) -> ChatError {
    let code = error
        .get("code")
        .and_then(Value::as_u64)
        .and_then(|code| u16::try_from(code).ok())
        .filter(|_| status == 200)
        .unwrap_or(status);
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            body.map(str::trim)
                .filter(|body| !body.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| format!("OpenRouter answered with HTTP {code}"));
    let metadata = error.get("metadata");
    let says = |what: &str| {
        message.to_lowercase().contains(what)
            || metadata.is_some_and(|metadata| metadata.to_string().to_lowercase().contains(what))
    };
    match code {
        401 => ChatError::KeyRefused,
        402 => ChatError::NoCredits,
        403 if says("moderation") || says("flagged") => ChatError::Declined,
        403 if says("limit") => ChatError::KeyLimit,
        403 if says("credit") => ChatError::NoCredits,
        429 => ChatError::RateLimited(
            metadata
                .and_then(|metadata| metadata.get("retry_after"))
                .and_then(Value::as_u64),
        ),
        _ => ChatError::Other {
            status: code,
            message,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake;

    /// A stream recorded from GPT-6 Luna on 2026-11-04 with search on,
    /// trimmed: a citation, text in pieces, the finish and the usage.
    fn recorded() -> Vec<String> {
        [
            ": OPENROUTER PROCESSING\n\n",
            r#"data: {"id":"gen-1","model":"openai/gpt-6-luna","provider":"OpenAI","choices":[{"index":0,"delta":{"content":"","role":"assistant","annotations":[{"type":"url_citation","url_citation":{"url":"https://www.serebii.net/pokearth/hoenn/route102.shtml","title":"Pokéarth - Hoenn - Route 102","start_index":0,"end_index":0,"content":"Route 102 is another forest\n\nsurrounded route"}}]},"finish_reason":null}]}"#,
            "\n\n",
            r#"data: {"id":"gen-1","model":"openai/gpt-6-luna","provider":"OpenAI","choices":[{"index":0,"delta":{"content":"You can catch **Ralts","role":"assistant"},"finish_reason":null}]}"#,
            "\n\n",
            r#"data: {"id":"gen-1","model":"openai/gpt-6-luna","provider":"OpenAI","choices":[{"index":0,"delta":{"content":" on Route 102**.","role":"assistant"},"finish_reason":null}]}"#,
            "\n\n",
            r#"data: {"id":"gen-1","model":"openai/gpt-6-luna","provider":"OpenAI","choices":[{"index":0,"delta":{"content":"","role":"assistant","reasoning":null,"reasoning_details":[{"type":"reasoning.encrypted","data":"gAAAA","id":"rs_1","format":"openai-responses-v1","index":0}]},"finish_reason":"stop"}]}"#,
            "\n\n",
            r#"data: {"id":"gen-1","model":"openai/gpt-6-luna","provider":"OpenAI","choices":[{"index":0,"delta":{"content":"","role":"assistant"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2590,"completion_tokens":113,"cost":0.0073696,"completion_tokens_details":{"reasoning_tokens":0},"server_tool_use_details":{"web_search_requests":1}}}"#,
            "\n\n",
            "data: [DONE]\n\n",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }

    fn request(lookups: Option<Vec<String>>) -> Request {
        Request {
            model: MODEL.to_owned(),
            system: "You are Guide.".to_owned(),
            messages: vec![Message::user("Where is Ralts?", Vec::new())],
            effort: Effort::Low,
            lookups,
            session: "chat-1".to_owned(),
        }
    }

    fn collect(endpoints: &Endpoints, request: &Request, stop: &Stop) -> Vec<Event> {
        let mut events = Vec::new();
        ask(endpoints, "sk-or-test", request, stop, &mut |event| {
            events.push(event);
        });
        events.retain(|event| *event != Event::Working);
        events
    }

    fn text(events: &[Event]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                Event::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_body_asks_luna_with_its_fallback_tools_and_pictures() {
        let mut request = request(Some(vec!["serebii.net".to_owned()]));
        request.messages.insert(
            0,
            Message::assistant("Earlier.", vec![json!({"type": "reasoning.encrypted"})]),
        );
        request.messages[1].images.push(Arc::from(&b"PNG"[..]));
        let body = request.body();
        assert_eq!(body["model"], MODEL);
        assert_eq!(body["models"], json!([MODEL, FALLBACK_MODEL]));
        assert_eq!(body["stream"], true);
        assert_eq!(body["reasoning"]["effort"], "low");
        assert_eq!(body["session_id"], "chat-1");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(
            body["messages"][1]["reasoning_details"][0]["type"],
            "reasoning.encrypted"
        );
        assert_eq!(body["messages"][2]["content"][0]["text"], "Where is Ralts?");
        assert_eq!(
            body["messages"][2]["content"][1]["image_url"]["url"],
            "data:image/png;base64,UE5H"
        );
        assert_eq!(body["tools"][0]["type"], "openrouter:web_search");
        assert_eq!(body["tools"][0]["parameters"]["engine"], "exa");
        assert_eq!(
            body["tools"][1]["parameters"]["allowed_domains"],
            json!(["serebii.net"])
        );
        assert!(self::request(None).body().get("tools").is_none());
    }

    #[test]
    fn a_recorded_stream_becomes_events() {
        let endpoints = fake::stream(|_| (200, recorded()), Duration::ZERO);
        let events = collect(&endpoints, &request(None), &Stop::new());
        assert_eq!(
            events[0],
            Event::Serving {
                model: MODEL.to_owned(),
                provider: "OpenAI".to_owned()
            }
        );
        assert_eq!(text(&events), "You can catch **Ralts on Route 102**.");
        assert!(events.contains(&Event::Citation(Citation {
            url: "https://www.serebii.net/pokearth/hoenn/route102.shtml".to_owned(),
            title: "Pokéarth - Hoenn - Route 102".to_owned(),
            excerpt: "Route 102 is another forest surrounded route".to_owned(),
        })));
        assert!(events.iter().any(|event| matches!(event,
            Event::ReasoningDetails(details) if details[0]["data"] == "gAAAA")));
        assert!(events.iter().any(|event| matches!(event,
            Event::Usage(usage) if usage.searches == 1 && usage.cost == Some(0.007_369_6))));
        assert_eq!(events.last(), Some(&Event::Finished(Finish::Stop)));
    }

    #[test]
    fn the_key_and_body_never_reach_the_command_line() {
        let endpoints = fake::stream(
            |request| {
                let fine = request.header("authorization") == Some("Bearer sk-or-test")
                    && request.json()["messages"][1]["content"] == "Where is Ralts?"
                    && request.header("x-openrouter-title") == Some("Imago Emulator");
                (if fine { 200 } else { 400 }, recorded())
            },
            Duration::ZERO,
        );
        let events = collect(&endpoints, &request(None), &Stop::new());
        assert_eq!(events.last(), Some(&Event::Finished(Finish::Stop)));
    }

    type Reply = fn(&fake::Request) -> (u16, Vec<String>);

    #[test]
    fn errors_say_what_to_do() {
        fn reply(body: &str) -> Vec<String> {
            vec![body.to_owned()]
        }
        let cases: [(Reply, ChatError); 4] = [
            (
                |_| (401, reply(r#"{"error":{"code":401,"message":"No auth"}}"#)),
                ChatError::KeyRefused,
            ),
            (
                |_| {
                    (
                        402,
                        reply(r#"{"error":{"code":402,"message":"Insufficient credits"}}"#),
                    )
                },
                ChatError::NoCredits,
            ),
            (
                |_| {
                    (
                        403,
                        reply(
                            r#"{"error":{"code":403,"message":"Key limit exceeded","metadata":{"limit_source":"key"}}}"#,
                        ),
                    )
                },
                ChatError::KeyLimit,
            ),
            (
                |_| {
                    (
                        400,
                        reply(r#"{"error":{"code":400,"message":"Bad model"}}"#),
                    )
                },
                ChatError::Other {
                    status: 400,
                    message: "Bad model".to_owned(),
                },
            ),
        ];
        for (reply, error) in cases {
            let endpoints = fake::stream(reply, Duration::ZERO);
            let events = collect(&endpoints, &request(None), &Stop::new());
            assert_eq!(events, vec![Event::Failed(error)]);
        }
    }

    #[test]
    fn an_error_inside_the_stream_keeps_what_came_before() {
        let endpoints = fake::stream(
            |_| {
                let mut pieces = recorded();
                pieces.truncate(5);
                pieces.push(
                    r#"data: {"error":{"code":502,"message":"Provider disconnected"},"choices":[{"index":0,"delta":{"content":""},"finish_reason":"error"}]}"#
                        .to_owned()
                        + "\n\n",
                );
                (200, pieces)
            },
            Duration::ZERO,
        );
        let events = collect(&endpoints, &request(None), &Stop::new());
        assert_eq!(text(&events), "You can catch **Ralts");
        assert_eq!(
            events.last(),
            Some(&Event::Failed(ChatError::Other {
                status: 502,
                message: "Provider disconnected".to_owned()
            }))
        );
    }

    #[test]
    fn a_dropped_connection_is_tried_once_more() {
        // Nothing listens on the port: both tries fail to connect.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let endpoints = Endpoints::at(format!("http://127.0.0.1:{port}"));
        let started = std::time::Instant::now();
        let events = collect(&endpoints, &request(None), &Stop::new());
        assert!(matches!(
            events.as_slice(),
            [Event::Failed(ChatError::Unreachable(_))]
        ));
        assert!(started.elapsed() >= Duration::from_millis(800));
    }

    #[test]
    fn stopping_ends_the_answer_at_once() {
        let endpoints = fake::stream(|_| (200, recorded()), Duration::from_secs(2));
        let stop = Stop::new();
        let stopper = stop.clone();
        let started = std::time::Instant::now();
        let mut events = Vec::new();
        ask(
            &endpoints,
            "sk-or-test",
            &request(None),
            &stop,
            &mut |event| {
                if event == Event::Working {
                    stopper.stop();
                }
                events.push(event);
            },
        );
        assert_eq!(events.last(), Some(&Event::Stopped));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
