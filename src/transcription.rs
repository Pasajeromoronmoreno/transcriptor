use crate::api::groq::{GroqClient, TranscriptionError};
use crate::config::RetryConfig;
use std::time::Duration;

pub async fn transcribe<F>(client: &GroqClient, retry: &RetryConfig, api_key: &str, wav: &[u8], language: &str, prompt: Option<&str>, mut on_retry: F) -> Result<String, TranscriptionError>
where F: FnMut(u32, u32, Duration, &TranscriptionError) {
    let max_attempts = retry.max_attempts.max(1);
    for attempt in 1..=max_attempts {
        match client.transcribe_once(api_key, wav, language, prompt).await {
            Ok(text) => return Ok(text),
            Err(error) if error.retryable() && attempt < max_attempts => {
                let multiplier = 1u32.checked_shl(attempt - 1).unwrap_or(u32::MAX);
                let base = retry.initial_delay.saturating_mul(multiplier).min(retry.max_delay);
                let cap = retry.jitter.as_millis() as u64;
                let jitter = if cap == 0 { Duration::ZERO } else { Duration::from_millis(crate::overlay::now_ms().unwrap_or(0) % (cap + 1)) };
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpListener};

    fn fast_policy() -> RetryConfig { RetryConfig { max_attempts: 3, initial_delay: Duration::ZERO, max_delay: Duration::ZERO, jitter: Duration::ZERO } }
    async fn server(statuses: Vec<u16>) -> (GroqClient, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/transcriptions", listener.local_addr().unwrap());
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        tokio::spawn(async move {
            for status in statuses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = [0u8; 65536]; let _ = socket.read(&mut buffer).await;
                seen.fetch_add(1, Ordering::SeqCst);
                let (reason, body) = match status { 200 => ("OK", r#"{"text":"prueba correcta"}"#), 401 => ("Unauthorized", "denied"), 429 => ("Too Many Requests", "busy"), _ => ("Server Error", "failed") };
                let retry = if status == 429 { "Retry-After: 0\r\n" } else { "" };
                let response = format!("HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nX-Request-Id: local-{status}\r\n{retry}Connection: close\r\n\r\n{body}", body.len());
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        (GroqClient::with_endpoint(&endpoint, Duration::from_secs(1), Duration::from_secs(2)).unwrap(), count)
    }

    #[tokio::test]
    async fn retries_500_then_succeeds() {
        let (client, count) = server(vec![500, 200]).await;
        assert_eq!(transcribe(&client, &fast_policy(), "secret", b"wav", "es", None, |_,_,_,_|{}).await.unwrap(), "prueba correcta");
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }
    #[tokio::test]
    async fn retries_429_then_succeeds() {
        let (client, count) = server(vec![429, 200]).await;
        assert_eq!(transcribe(&client, &fast_policy(), "secret", b"wav", "es", None, |_,_,_,_|{}).await.unwrap(), "prueba correcta");
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }
    #[tokio::test]
    async fn does_not_retry_401() {
        let (client, count) = server(vec![401]).await;
        assert_eq!(transcribe(&client, &fast_policy(), "secret", b"wav", "es", None, |_,_,_,_|{}).await.unwrap_err().code(), "TRN-GROQ-AUTH");
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn retries_timeout_then_succeeds() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/transcriptions", listener.local_addr().unwrap());
        let seen = Arc::new(AtomicUsize::new(0)); let count = seen.clone();
        tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap(); let mut buffer = [0u8; 8192]; let _ = first.read(&mut buffer).await; seen.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(40)).await; drop(first);
            let (mut second, _) = listener.accept().await.unwrap(); let _ = second.read(&mut buffer).await; seen.fetch_add(1, Ordering::SeqCst);
            let body = r#"{"text":"recuperado"}"#; let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()); second.write_all(response.as_bytes()).await.unwrap();
        });
        let client = GroqClient::with_endpoint(&endpoint, Duration::from_secs(1), Duration::from_millis(30)).unwrap();
        assert_eq!(transcribe(&client, &fast_policy(), "secret", b"wav", "es", None, |_,_,_,_|{}).await.unwrap(), "recuperado");
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }
}
