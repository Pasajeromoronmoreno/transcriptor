use reqwest::{multipart, StatusCode};
use serde::Deserialize;
use std::time::Duration;
use thiserror::Error;

const ENDPOINT: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
use crate::config::RetryConfig;

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
    retry: &RetryConfig,
    connect_timeout: Duration,
    request_timeout: Duration,
    on_retry: F,
) -> Result<String, TranscriptionError>
where F: FnMut(u32, u32, Duration, &TranscriptionError) {
    transcribe_audio_at(ENDPOINT, api_key, wav_data, language, prompt, retry, connect_timeout, request_timeout, on_retry).await
}

async fn transcribe_audio_at<F>(endpoint: &str, api_key: &str, wav_data: &[u8], language: &str, prompt: Option<&str>, retry: &RetryConfig, connect_timeout: Duration, request_timeout: Duration, mut on_retry: F) -> Result<String, TranscriptionError>
where F: FnMut(u32, u32, Duration, &TranscriptionError) {
    let max_attempts = retry.max_attempts.max(1);
    for attempt in 1..=max_attempts {
        match send_once(endpoint, api_key, wav_data, language, prompt, connect_timeout, request_timeout).await {
            Ok(text) => return Ok(text),
            Err(error) if error.retryable() && attempt < max_attempts => {
                let multiplier = 1u32.checked_shl(attempt - 1).unwrap_or(u32::MAX);
                let base = retry.initial_delay.saturating_mul(multiplier).min(retry.max_delay);
                let jitter_cap = retry.jitter.as_millis() as u64;
                let jitter = if jitter_cap == 0 { Duration::ZERO } else { Duration::from_millis((crate::overlay::now_ms().unwrap_or(0) % (jitter_cap + 1)).min(jitter_cap)) };
                let delay = error.retry_after().unwrap_or(base.saturating_add(jitter));
                tracing::warn!(code=error.code(), attempt, max_attempts, delay_ms=delay.as_millis() as u64, error=%error, error_chain=?error, "Fallo transitorio de transcripción; se reintentará");
                on_retry(attempt + 1, max_attempts, delay, &error);
                tokio::time::sleep(delay).await;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!()
}

async fn send_once(endpoint: &str, api_key: &str, wav_data: &[u8], language: &str, prompt: Option<&str>, connect_timeout: Duration, request_timeout: Duration) -> Result<String, TranscriptionError> {
    let client = reqwest::Client::builder()
        .connect_timeout(connect_timeout)
        .timeout(request_timeout)
        .build().map_err(|e| TranscriptionError::Request(e.to_string()))?;
    let part = multipart::Part::bytes(wav_data.to_vec()).file_name("chunk.wav").mime_str("audio/wav")
        .map_err(|e| TranscriptionError::Request(e.to_string()))?;
    let mut form = multipart::Form::new().part("file", part)
        .text("model", "whisper-large-v3-turbo").text("language", language.to_owned())
        .text("response_format", "json").text("temperature", "0.0");
    if let Some(prompt) = prompt { form = form.text("prompt", prompt.to_owned()); }
    let response = client.post(endpoint).bearer_auth(api_key).multipart(form).send().await
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
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpListener};
    #[test] fn retry_policy_is_selective() {
        let rate = TranscriptionError::Http { status: StatusCode::TOO_MANY_REQUESTS, request_id: None, body: String::new(), retry_after: None };
        let auth = TranscriptionError::Http { status: StatusCode::UNAUTHORIZED, request_id: None, body: String::new(), retry_after: None };
        assert!(rate.retryable()); assert_eq!(rate.code(), "TRN-GROQ-RATE");
        assert!(!auth.retryable()); assert_eq!(auth.code(), "TRN-GROQ-AUTH");
    }

    async fn server(statuses: Vec<u16>) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        tokio::spawn(async move {
            for status in statuses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = vec![0u8; 65536];
                let _ = socket.read(&mut buffer).await;
                seen.fetch_add(1, Ordering::SeqCst);
                let (reason, body) = match status { 200 => ("OK", r#"{"text":"prueba correcta"}"#), 401 => ("Unauthorized", "denied"), 429 => ("Too Many Requests", "busy"), _ => ("Server Error", "failed") };
                let retry = if status == 429 { "Retry-After: 0\r\n" } else { "" };
                let response = format!("HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nX-Request-Id: local-{status}\r\n{retry}Connection: close\r\n\r\n{body}", body.len());
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        (format!("http://{address}/transcriptions"), count)
    }

    fn fast_policy() -> RetryConfig { RetryConfig { max_attempts: 3, initial_delay: Duration::ZERO, max_delay: Duration::ZERO, jitter: Duration::ZERO } }

    #[tokio::test]
    async fn retries_server_error_then_succeeds() {
        let (endpoint, count) = server(vec![500, 200]).await;
        let result = transcribe_audio_at(&endpoint, "secret", b"wav", "es", None, &fast_policy(), Duration::from_secs(1), Duration::from_secs(2), |_,_,_,_|{}).await.unwrap();
        assert_eq!(result, "prueba correcta");
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn authorization_error_is_not_retried() {
        let (endpoint, count) = server(vec![401]).await;
        let error = transcribe_audio_at(&endpoint, "secret", b"wav", "es", None, &fast_policy(), Duration::from_secs(1), Duration::from_secs(2), |_,_,_,_|{}).await.unwrap_err();
        assert_eq!(error.code(), "TRN-GROQ-AUTH");
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn respects_retry_after_for_rate_limit() {
        let (endpoint, count) = server(vec![429, 200]).await;
        let result = transcribe_audio_at(&endpoint, "secret", b"wav", "es", None, &fast_policy(), Duration::from_secs(1), Duration::from_secs(2), |_,_,_,_|{}).await.unwrap();
        assert_eq!(result, "prueba correcta");
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn retries_timeout_then_succeeds() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/transcriptions", listener.local_addr().unwrap());
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 8192];
            let _ = first.read(&mut buffer).await;
            seen.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(40)).await;
            drop(first);
            let (mut second, _) = listener.accept().await.unwrap();
            let _ = second.read(&mut buffer).await;
            seen.fetch_add(1, Ordering::SeqCst);
            let body = r#"{"text":"recuperado"}"#;
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            second.write_all(response.as_bytes()).await.unwrap();
        });
        let result = transcribe_audio_at(&endpoint, "secret", b"wav", "es", None, &fast_policy(), Duration::from_secs(1), Duration::from_millis(30), |_,_,_,_|{}).await.unwrap();
        assert_eq!(result, "recuperado");
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }
}
