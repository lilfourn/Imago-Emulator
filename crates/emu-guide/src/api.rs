//! OpenRouter's API, the parts sign-in needs: the authorization page, the
//! exchange of a code for a key, and the key's own details.

use serde_json::Value;

use crate::{curl, url};

/// The most one of OpenRouter's answers here may be: each is a few hundred
/// bytes of JSON.
const MAX_REPLY: u64 = 1 << 20;

/// Where OpenRouter is: its site, or a stand-in for tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    site: String,
}

/// What went wrong asking OpenRouter.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ApiError {
    /// No answer: no connection, a timeout, or `curl` missing.
    #[error("couldn't reach OpenRouter ({0})")]
    Unreachable(String),
    /// OpenRouter answered with an error: its status and message.
    #[error("{message}")]
    Refused {
        /// The HTTP status, such as 401.
        status: u16,
        /// OpenRouter's own words, or the status when it gave none.
        message: String,
    },
    /// OpenRouter answered, but not with what was asked for.
    #[error("OpenRouter's reply couldn't be read")]
    Unreadable,
}

impl ApiError {
    /// Whether OpenRouter no longer accepts the key the request carried.
    pub fn is_refused_key(&self) -> bool {
        matches!(
            self,
            Self::Refused {
                status: 401 | 403,
                ..
            }
        )
    }
}

/// A key's details from `GET /api/v1/key`, in dollars.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyInfo {
    /// Spent on the key so far, if OpenRouter said.
    pub usage: Option<f64>,
    /// The key's credit limit, if it has one.
    pub limit: Option<f64>,
    /// Whether the account has never bought credits.
    pub is_free_tier: bool,
}

impl KeyInfo {
    /// The key's details as in Imago: "Free tier", or what it has spent,
    /// against its limit when it has one ("$0.42 of $5.00 used").
    pub fn summary(&self) -> Option<String> {
        if self.is_free_tier {
            return Some("Free tier".to_owned());
        }
        let usage = self.usage?;
        Some(match self.limit {
            Some(limit) if limit > 0.0 => format!("${usage:.2} of ${limit:.2} used"),
            _ => format!("${usage:.2} used"),
        })
    }

    /// The details in a `GET /api/v1/key` reply, if it is one.
    fn parse(body: &Value) -> Option<Self> {
        let data = body.get("data")?.as_object()?;
        Some(Self {
            usage: data.get("usage").and_then(Value::as_f64),
            limit: data.get("limit").and_then(Value::as_f64),
            is_free_tier: data.get("is_free_tier").and_then(Value::as_bool) == Some(true),
        })
    }
}

impl Endpoints {
    /// OpenRouter itself.
    pub fn openrouter() -> Self {
        Self::at("https://openrouter.ai")
    }

    /// A server at `site`, such as `http://127.0.0.1:4321`, answering as
    /// OpenRouter does.
    pub fn at(site: impl Into<String>) -> Self {
        Self { site: site.into() }
    }

    /// Where chat completions are asked for.
    pub(crate) fn chat_url(&self) -> String {
        format!("{}/api/v1/chat/completions", self.site)
    }

    /// The page that asks the player to let this Mac have a key, sending
    /// the browser back to `http://localhost:{port}/callback`. The key is
    /// labelled `label` in their OpenRouter key list.
    pub fn authorize_url(&self, port: u16, challenge: &str, label: &str) -> String {
        let callback = format!("http://localhost:{port}/callback");
        format!(
            "{}/auth?callback_url={}&code_challenge={}&code_challenge_method=S256&key_label={}",
            self.site,
            url::encode(&callback),
            url::encode(challenge),
            url::encode(label),
        )
    }

    /// Exchanges the `code` the browser brought back, proven by the
    /// `verifier` its challenge came from, for a key.
    ///
    /// # Errors
    ///
    /// If OpenRouter cannot be reached, refuses the code (a used or
    /// expired one, or the wrong verifier), or answers without a key.
    pub fn exchange(&self, code: &str, verifier: &str) -> Result<String, ApiError> {
        let body = serde_json::json!({
            "code": code,
            "code_verifier": verifier,
            "code_challenge_method": "S256",
        });
        let response = curl::request(
            &[
                "--header",
                "Content-Type: application/json",
                "--data-binary",
                "@-",
            ],
            body.to_string().as_bytes(),
            &format!("{}/api/v1/auth/keys", self.site),
            30,
            MAX_REPLY,
        )
        .map_err(ApiError::Unreachable)?;
        let reply = answer(&response)?;
        reply
            .get("key")
            .and_then(Value::as_str)
            .filter(|key| !key.is_empty())
            .map(str::to_owned)
            .ok_or(ApiError::Unreadable)
    }

    /// What OpenRouter says of `key`: what it has spent and its limit.
    ///
    /// # Errors
    ///
    /// If OpenRouter cannot be reached or refuses the key
    /// ([`ApiError::is_refused_key`]), or the key could not be sent.
    pub fn key_info(&self, key: &str) -> Result<KeyInfo, ApiError> {
        let config = curl::config_header(&format!("Authorization: Bearer {key}"))
            .ok_or(ApiError::Unreadable)?;
        let response = curl::request(
            &["--config", "-"],
            config.as_bytes(),
            &format!("{}/api/v1/key", self.site),
            15,
            MAX_REPLY,
        )
        .map_err(ApiError::Unreachable)?;
        KeyInfo::parse(&answer(&response)?).ok_or(ApiError::Unreadable)
    }
}

/// The JSON of a successful `response`, or the error it carries: OpenRouter
/// says what went wrong in `{"error": {"message": …}}`.
fn answer(response: &curl::Response) -> Result<Value, ApiError> {
    let body: Option<Value> = serde_json::from_slice(&response.body).ok();
    if (200..300).contains(&response.status) {
        return body.ok_or(ApiError::Unreadable);
    }
    let message = body
        .as_ref()
        .and_then(|body| body.pointer("/error/message"))
        .and_then(Value::as_str)
        .map_or_else(|| format!("HTTP {}", response.status), str::to_owned);
    Err(ApiError::Refused {
        status: response.status,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake;
    use serde_json::json;

    #[test]
    fn the_authorize_url_names_the_loopback_the_challenge_and_the_label() {
        let url = Endpoints::openrouter().authorize_url(4321, "abc_-", "Imago Emulator · Mac");
        assert_eq!(
            url,
            "https://openrouter.ai/auth?callback_url=http%3A%2F%2Flocalhost%3A4321%2Fcallback\
             &code_challenge=abc_-&code_challenge_method=S256\
             &key_label=Imago%20Emulator%20%C2%B7%20Mac"
        );
    }

    #[test]
    fn errors_carry_openrouters_message_or_the_status() {
        let refused = curl::Response {
            status: 401,
            body: br#"{"error":{"code":401,"message":"User not found."}}"#.to_vec(),
        };
        let error = answer(&refused).unwrap_err();
        assert_eq!(error.to_string(), "User not found.");
        assert!(error.is_refused_key());
        let bare = curl::Response {
            status: 502,
            body: b"<html>".to_vec(),
        };
        assert_eq!(answer(&bare).unwrap_err().to_string(), "HTTP 502");
        assert!(!answer(&bare).unwrap_err().is_refused_key());
        let garbled = curl::Response {
            status: 200,
            body: b"not json".to_vec(),
        };
        assert_eq!(answer(&garbled), Err(ApiError::Unreadable));
    }

    #[test]
    fn a_keys_details_read_as_imago_shows_them() {
        let info = |body: Value| KeyInfo::parse(&body).unwrap().summary();
        assert_eq!(
            info(json!({"data": {"usage": 0.4213, "limit": null, "is_free_tier": false}})),
            Some("$0.42 used".to_owned())
        );
        assert_eq!(
            info(json!({"data": {"usage": 2.5, "limit": 10.0, "is_free_tier": false}})),
            Some("$2.50 of $10.00 used".to_owned())
        );
        assert_eq!(
            info(json!({"data": {"usage": 0.0, "is_free_tier": true}})),
            Some("Free tier".to_owned())
        );
        assert_eq!(info(json!({"data": {}})), None);
        assert_eq!(KeyInfo::parse(&json!({"error": {}})), None);
    }

    #[test]
    fn the_key_goes_in_a_header_and_its_details_come_back() {
        let endpoints = fake::openrouter(|request| {
            assert_eq!(request.line, "GET /api/v1/key HTTP/1.1");
            match request.header("authorization") {
                Some("Bearer sk-or-v1-good") => (
                    200,
                    r#"{"data":{"label":"x","usage":1.5,"limit":null,"is_free_tier":false}}"#
                        .into(),
                ),
                _ => (
                    401,
                    r#"{"error":{"code":401,"message":"User not found."}}"#.into(),
                ),
            }
        });
        let info = endpoints.key_info("sk-or-v1-good").unwrap();
        assert_eq!(info.summary().as_deref(), Some("$1.50 used"));
        assert!(
            endpoints
                .key_info("sk-or-v1-gone")
                .unwrap_err()
                .is_refused_key()
        );
        assert_eq!(endpoints.key_info("line\nbreak"), Err(ApiError::Unreadable));
    }

    #[test]
    fn an_unreachable_openrouter_says_so() {
        // Port 9 (discard) has no listener on a loopback.
        let error = Endpoints::at("http://127.0.0.1:9")
            .exchange("code", "verifier")
            .unwrap_err();
        assert!(matches!(error, ApiError::Unreachable(_)), "{error:?}");
    }
}
