//! HTTP plumbing shared by the provider adapters.
//!
//! The adapters never own a client. They take a [`Transport`], which keeps two
//! things true at once:
//!
//! * the shipped app can run the real network stack (WinHTTP lives in
//!   `oly-platform-win`, where `unsafe` is already confined), and
//! * the parity tests replay the recorded byte streams from
//!   `native/tests/fixtures/http/recorded.json` through the **same** request
//!   shapes the live code sends, so a changed header, URL or body fails a test.
//!
//! Timeout and status semantics are the reference's: a request that exceeds its
//! budget is reported as `请求超时` (not as a generic transport failure), and a
//! non-2xx response is `HTTP {status}` so the LRCLIB retry can recognise `5xx`.

use std::fmt;

use serde_json::Value;

/// The browser UA the reference sends with every request.
pub const DEFAULT_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
     Chrome/126.0.0.0 Safari/537.36";

/// One outgoing request.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// Already-encoded body bytes (form or JSON).
    pub body: Option<Vec<u8>>,
    pub timeout_ms: u64,
}

impl HttpRequest {
    pub fn get(url: impl Into<String>, timeout_ms: u64) -> Self {
        Self {
            method: "GET".to_string(),
            url: url.into(),
            headers: Vec::new(),
            body: None,
            timeout_ms,
        }
    }

    pub fn post(url: impl Into<String>, body: impl Into<Vec<u8>>, timeout_ms: u64) -> Self {
        Self {
            method: "POST".to_string(),
            url: url.into(),
            headers: Vec::new(),
            body: Some(body.into()),
            timeout_ms,
        }
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// True when this request already names the header (case-insensitive).
    pub fn has_header(&self, name: &str) -> bool {
        self.headers
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case(name))
    }
}

/// A response as the transport hands it back. The body stays bytes because two
/// providers return KRC/QRC ciphertext that is not UTF-8 JSON.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Why a request produced no usable body.
#[derive(Debug)]
pub enum HttpError {
    /// The budget expired: reported as `请求超时` so a caller can distinguish a
    /// slow network from a refused connection.
    Timeout,
    /// Non-2xx. The LRCLIB adapter retries exactly the 5xx family.
    Status(u16),
    /// Connection, DNS, TLS or header failure.
    Transport(String),
    /// `200` with a body that is not JSON, or JSON of the wrong shape.
    Json(String),
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => write!(f, "请求超时"),
            Self::Status(status) => write!(f, "HTTP {status}"),
            Self::Transport(reason) => write!(f, "{reason}"),
            Self::Json(reason) => write!(f, "{reason}"),
        }
    }
}

impl std::error::Error for HttpError {}

impl HttpError {
    /// `HTTP 5xx` — the one condition LRCLIB retries.
    pub fn is_server_error(&self) -> bool {
        matches!(self, Self::Status(status) if (500..600).contains(status))
    }
}

/// A blocking HTTP client. Implementations must be safe to call from several
/// threads at once: the search service fans out across providers.
pub trait Transport: Send + Sync {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, HttpError>;
}

/// Request JSON. `headers` are merged under the default UA.
pub fn get_json(
    transport: &dyn Transport,
    url: &str,
    headers: &[(&str, &str)],
    timeout_ms: u64,
) -> Result<Value, HttpError> {
    let text = exchange(transport, HttpRequest::get(url, timeout_ms), headers)?;
    parse_json(&text)
}

/// Request a POST with a JSON body and parse the JSON reply.
pub fn post_json(
    transport: &dyn Transport,
    url: &str,
    body: &str,
    headers: &[(&str, &str)],
    timeout_ms: u64,
) -> Result<Value, HttpError> {
    let request = HttpRequest::post(url, body.as_bytes().to_vec(), timeout_ms)
        .header("Content-Type", "application/json");
    let text = exchange(transport, request, headers)?;
    parse_json(&text)
}

/// Request a POST with an `application/x-www-form-urlencoded` body.
pub fn post_form(
    transport: &dyn Transport,
    url: &str,
    body: &str,
    headers: &[(&str, &str)],
    timeout_ms: u64,
) -> Result<String, HttpError> {
    let request = HttpRequest::post(url, body.as_bytes().to_vec(), timeout_ms)
        .header("Content-Type", "application/x-www-form-urlencoded");
    exchange(transport, request, headers)
}

/// Request plain text (an XML document, a lyric body, a TTML file).
pub fn get_text(
    transport: &dyn Transport,
    url: &str,
    headers: &[(&str, &str)],
    timeout_ms: u64,
) -> Result<String, HttpError> {
    exchange(transport, HttpRequest::get(url, timeout_ms), headers)
}

/// Send one already-built request and return the body text of a 2xx reply.
///
/// This is the lowest-level entry point: adapters that need a method, headers
/// and timeout the helpers do not cover (an AI `POST` with a bearer token, a
/// form POST) build the [`HttpRequest`] themselves and call this.
pub fn exchange_text(
    transport: &dyn Transport,
    request: HttpRequest,
) -> Result<String, HttpError> {
    exchange(transport, request, &[])
}

fn exchange(
    transport: &dyn Transport,
    mut request: HttpRequest,
    headers: &[(&str, &str)],
) -> Result<String, HttpError> {
    // The caller's headers win over the UA only when it sets one explicitly;
    // otherwise the browser UA is prepended exactly like the reference.
    if !request.has_header("User-Agent") {
        request
            .headers
            .insert(0, ("User-Agent".to_string(), DEFAULT_UA.to_string()));
    }
    for (name, value) in headers {
        if !request.has_header(name) {
            request.headers.push(((*name).to_string(), (*value).to_string()));
        }
    }
    let response = transport.send(&request)?;
    if !(200..300).contains(&response.status) {
        return Err(HttpError::Status(response.status));
    }
    Ok(response.text())
}

fn parse_json(text: &str) -> Result<Value, HttpError> {
    serde_json::from_str(text).map_err(|_| HttpError::Json("响应内容不是合法 JSON".to_string()))
}

/// Percent-encode one query value the way `encodeURIComponent` does for the
/// characters these URLs carry.
pub fn encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(*byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_matches_encode_uri_component_for_the_used_alphabet() {
        assert_eq!(encode_component("稻香 周杰伦"), "%E7%A8%BB%E9%A6%99%20%E5%91%A8%E6%9D%B0%E4%BC%A6");
        assert_eq!(encode_component("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(encode_component("a/b&c"), "a%2Fb%26c");
    }

    #[test]
    fn server_errors_are_the_retry_signal() {
        assert!(HttpError::Status(503).is_server_error());
        assert!(!HttpError::Status(404).is_server_error());
        assert_eq!(HttpError::Timeout.to_string(), "请求超时");
    }
}
