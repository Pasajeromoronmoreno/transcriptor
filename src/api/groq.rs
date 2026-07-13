use reqwest::{multipart, StatusCode};
use serde::Deserialize;
use std::time::Duration;
use thiserror::Error;

const ENDPOINT: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
const MAX_ATTEMPTS: u32 = 3;

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
    #[error("no se pudo construir el multipart: {0}")]
    Request(String),
}

impl TranscriptionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Transport { kind: "timeout", .. } => "TRN-NET-TIMEOUT",
            Self::Transport { .. } => "TRN-NET-CONNECT",
            Self::Http { status, .. } if *status == StatusCode::UNAUTHORIZED || *status == StatusCode::FORBIDDEN => "TRN-GROQ-AUTH",
            Self::Http { status, .. } if *status == StatusCode::TOO_MANY_REQUESTS => "TRN-GROQ-RATE",
            Self::Http { status, .. } if status.is_server_error() => "TRN-GROQ-5XX",
            Self::Http { .. } => "TRN-GROQ-4XX",
            Self::Decode(_) => "TRN-GROQ-DECODE",
            Self::Request(_) => "TRN-GROQ-REQUEST",
        }
    }

    pub fn retryable(&self) -> bool {
        match self {
            Self::Transport { .. } => true,
            Self::Http { status, .. } => matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504),
            _ => false,
        }
    }

    fn retry_after(&self) -> Option<Duration> {
        match self { Self::Http { retry_after, .. } => *retry_after, _ => None }
    }
}

pub async fn transcribe_audio<F>(
    api_key: &str,
    wav_data: &[u8],
    language: &str,
    prompt: Option<&str>,
    mut on_retry: F,
) -> Result<String, TranscriptionError>
where F: FnMut(u32, u32, Duration, &TranscriptionError) {
    for attempt in 1..=MAX_ATTEMPTS {
        match send_once(api_key, wav_data, language, prompt).await {
            Ok(text) => return Ok(text),
            Err(error) if error.retryable() && attempt < MAX_ATTEMPTS => {
                let delay = error.retry_after().unwrap_or_else(|| Duration::from_secs(1 << (attempt - 1)));
                tracing::warn!(code=error.code(), attempt, max_attempts=MAX_ATTEMPTS, delay_ms=delay.as_millis() as u64, error=%error, error_chain=?error, "Fallo transitorio de transcripción; se reintentará");
                on_retry(attempt + 1, MAX_ATTEMPTS, delay, &error);
                tokio::time::sleep(delay).await;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!()
}

async fn send_once(api_key: &str, wav_data: &[u8], language: &str, prompt: Option<&str>) -> Result<String, TranscriptionError> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(30))
        .build().map_err(|e| TranscriptionError::Request(e.to_string()))?;
    let part = multipart::Part::bytes(wav_data.to_vec()).file_name("chunk.wav").mime_str("audio/wav")
        .map_err(|e| TranscriptionError::Request(e.to_string()))?;
    let mut form = multipart::Form::new().part("file", part)
        .text("model", "whisper-large-v3-turbo").text("language", language.to_owned())
        .text("response_format", "json").text("temperature", "0.0");
    if let Some(prompt) = prompt { form = form.text("prompt", prompt.to_owned()); }
    let response = client.post(ENDPOINT).bearer_auth(api_key).multipart(form).send().await
        .map_err(|source| TranscriptionError::Transport { kind: if source.is_timeout() { "timeout" } else { "connection" }, source })?;
    let status = response.status();
    let request_id = response.headers().get("x-request-id").and_then(|v| v.to_str().ok()).map(str::to_owned);
    let retry_after = response.headers().get(reqwest::header::RETRY_AFTER).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()).map(Duration::from_secs);
    if !status.is_success() {
        let mut body = response.text().await.unwrap_or_else(|e| format!("<body unreadable: {e}>"));
        body.truncate(1024);
        return Err(TranscriptionError::Http { status, request_id, body, retry_after });
    }
    response.json::<GroqResponse>().await.map(|r| r.text.trim().to_owned()).map_err(TranscriptionError::Decode)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn retry_policy_is_selective() {
        let rate = TranscriptionError::Http { status: StatusCode::TOO_MANY_REQUESTS, request_id: None, body: String::new(), retry_after: None };
        let auth = TranscriptionError::Http { status: StatusCode::UNAUTHORIZED, request_id: None, body: String::new(), retry_after: None };
        assert!(rate.retryable()); assert_eq!(rate.code(), "TRN-GROQ-RATE");
        assert!(!auth.retryable()); assert_eq!(auth.code(), "TRN-GROQ-AUTH");
    }
}
