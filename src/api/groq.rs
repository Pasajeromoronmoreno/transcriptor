use reqwest::multipart;
use serde::Deserialize;
use std::time::Duration;

#[derive(Deserialize)]
pub struct GroqResponse {
    pub text: String,
}

pub async fn transcribe_audio(
    api_key: &str,
    wav_data: Vec<u8>,
    language: &str,
    prompt: Option<&str>,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let client = reqwest::Client::new();
    
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

    let res = client.post("https://api.groq.com/openai/v1/audio/transcriptions")
        .bearer_auth(api_key)
        .multipart(form)
        .timeout(Duration::from_secs(10))
        .send()
        .await?;

    if !res.status().is_success() {
        let err_text = res.text().await?;
        return Err(format!("Groq API Error: {}", err_text).into());
    }

    let groq_res: GroqResponse = res.json().await?;
    Ok(groq_res.text.trim().to_string())
}
