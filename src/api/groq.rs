use reqwest::multipart;
use serde::Deserialize;
use std::time::Duration;
use lazy_static::lazy_static;

#[derive(Deserialize)]
pub struct GroqResponse {
    pub text: String,
}

lazy_static! {
    // Creamos un único cliente HTTP con un pool de conexiones persistentes
    static ref HTTP_CLIENT: reqwest::Client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .pool_max_idle_per_host(10)
        .pool_idle_timeout(Duration::from_secs(600)) // Mantiene la conexión TLS viva por 10 minutos
        .build()
        .unwrap_or_default();
}

pub async fn transcribe_audio(
    api_key: &str,
    wav_data: Vec<u8>,
    language: &str,
    prompt: Option<&str>,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let part = multipart::Part::bytes(wav_data)
        .file_name("chunk.wav")
        .mime_str("audio/wav")?;

    let mut form = multipart::Form::new()
        .part("file", part)
        // whisper-large-v3-turbo es más rápido y económico ($0.04/h)
        .text("model", "whisper-large-v3-turbo") 
        .text("language", language.to_string())
        .text("response_format", "json")
        .text("temperature", "0.0");

    if let Some(p) = prompt {
        form = form.text("prompt", p.to_string());
    }

    let res = HTTP_CLIENT.post("https://api.groq.com/openai/v1/audio/transcriptions")
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await?;

    if !res.status().is_success() {
        let err_text = res.text().await?;
        return Err(format!("Groq API Error: {}", err_text).into());
    }

    let groq_res: GroqResponse = res.json().await?;
    Ok(groq_res.text.trim().to_string())
}
