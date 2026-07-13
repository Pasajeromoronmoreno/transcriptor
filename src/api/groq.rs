use reqwest::{multipart, StatusCode};
use serde::Deserialize;
use std::time::Duration;
use thiserror::Error;

const ENDPOINT: &str = "https://api.groq.com/openai/v1/audio/transcriptions";

#[derive(Deserialize)]
struct GroqResponse { text: String }

#[derive(Debug, Error)]
pub enum TranscriptionError {
    #[error("{kind}: {source}")]
    Transport { kind: &'static str, #[source] source: reqwest::Error },
    #[error("Groq HTTP {status}; request_id={request_id:?}; body={body}")]
    Http { status: StatusCode, request_id: Option<String>, body: String, retry_after: Option<Duration> },
    #[error("respuesta inválida de Groq: {0}")]
    Decode(#[source] reqwest::Error),
    #[error("no se pudo construir el request: {0}")]
    Request(String),
}

impl TranscriptionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Transport { kind: "timeout", .. } => "TRN-NET-TIMEOUT",
            Self::Transport { .. } => "TRN-NET-CONNECT",
            Self::Http { status, .. } if matches!(status.as_u16(), 401 | 403) => "TRN-GROQ-AUTH",
            Self::Http { status, .. } if *status == StatusCode::TOO_MANY_REQUESTS => "TRN-GROQ-RATE",
            Self::Http { status, .. } if status.is_server_error() => "TRN-GROQ-5XX",
            Self::Http { .. } => "TRN-GROQ-4XX",
            Self::Decode(_) => "TRN-GROQ-DECODE",
            Self::Request(_) => "TRN-GROQ-REQUEST",
        }
    }
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Transport { .. }) || matches!(self, Self::Http { status, .. } if matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504))
    }
    pub fn retry_after(&self) -> Option<Duration> {
        match self { Self::Http { retry_after, .. } => *retry_after, _ => None }
    }
    pub fn status(&self) -> Option<u16> { match self { Self::Http { status, .. } => Some(status.as_u16()), _ => None } }
    pub fn request_id(&self) -> Option<&str> { match self { Self::Http { request_id, .. } => request_id.as_deref(), _ => None } }
}

pub struct GroqClient { client: reqwest::Client, endpoint: String }

fn redact_body(mut body: String, api_key: &str, prompt: Option<&str>) -> String {
    if !api_key.is_empty() { body = body.replace(api_key, "[REDACTED]"); }
    if let Some(prompt) = prompt.filter(|value| !value.is_empty()) { body = body.replace(prompt, "[REDACTED_PROMPT]"); }
    body
}

fn truncate_body(mut body: String, limit: usize) -> String {
    if body.len() > limit {
        let mut boundary = limit;
        while !body.is_char_boundary(boundary) { boundary -= 1; }
        body.truncate(boundary);
    }
    body
}

impl GroqClient {
    pub fn new(connect_timeout: Duration, request_timeout: Duration) -> Result<Self, TranscriptionError> {
        Self::with_endpoint(ENDPOINT, connect_timeout, request_timeout)
    }
    pub(crate) fn with_endpoint(endpoint: &str, connect_timeout: Duration, request_timeout: Duration) -> Result<Self, TranscriptionError> {
        let client = reqwest::Client::builder().connect_timeout(connect_timeout).timeout(request_timeout).build()
            .map_err(|error| TranscriptionError::Request(error.to_string()))?;
        Ok(Self { client, endpoint: endpoint.to_owned() })
    }
    pub async fn transcribe_once(&self, api_key: &str, wav_data: &[u8], language: &str, prompt: Option<&str>) -> Result<String, TranscriptionError> {
        let part = multipart::Part::bytes(wav_data.to_vec()).file_name("chunk.wav").mime_str("audio/wav")
            .map_err(|error| TranscriptionError::Request(error.to_string()))?;
        let mut form = multipart::Form::new().part("file", part).text("model", "whisper-large-v3-turbo")
            .text("language", language.to_owned()).text("response_format", "json").text("temperature", "0.0");
        if let Some(prompt) = prompt { form = form.text("prompt", prompt.to_owned()); }
        let response = self.client.post(&self.endpoint).bearer_auth(api_key).multipart(form).send().await
            .map_err(|source| TranscriptionError::Transport { kind: if source.is_timeout() { "timeout" } else { "connection" }, source })?;
        let status = response.status();
        let request_id = response.headers().get("x-request-id").and_then(|v| v.to_str().ok()).map(str::to_owned);
        let retry_after = response.headers().get(reqwest::header::RETRY_AFTER).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()).map(Duration::from_secs);
        if !status.is_success() {
            let mut body = response.text().await.unwrap_or_else(|error| format!("<body unreadable: {error}>"));
            body = truncate_body(body, 1024);
            body = redact_body(body, api_key, prompt);
            return Err(TranscriptionError::Http { status, request_id, body, retry_after });
        }
        response.json::<GroqResponse>().await.map(|r| r.text.trim().to_owned()).map_err(TranscriptionError::Decode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retry_classification_is_selective() {
        let rate = TranscriptionError::Http { status: StatusCode::TOO_MANY_REQUESTS, request_id: None, body: String::new(), retry_after: None };
        let auth = TranscriptionError::Http { status: StatusCode::UNAUTHORIZED, request_id: None, body: String::new(), retry_after: None };
        assert!(rate.retryable()); assert_eq!(rate.code(), "TRN-GROQ-RATE");
        assert!(!auth.retryable()); assert_eq!(auth.code(), "TRN-GROQ-AUTH");
    }
    #[test]
    fn provider_body_redacts_secrets() {
        let body = redact_body("key=secret prompt=private".into(), "secret", Some("private"));
        assert!(!body.contains("secret")); assert!(!body.contains("private"));
    }
    #[test]
    fn provider_body_truncates_unicode_safely() {
        let body = truncate_body("á".repeat(600), 1024);
        assert!(body.len() <= 1024);
        assert!(body.is_char_boundary(body.len()));
    }
}
