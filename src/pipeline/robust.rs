use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use crate::audio::capture::HotMic;
use crate::config::AppConfig;
use crate::input::Command;
use crate::{api, output};

const MAX_PCM_BYTES: usize = 24 * 1024 * 1024;

/// Pipeline simplificado: Todo va al portapapeles y se pega atómicamente.
pub async fn run(
    mut cmd_rx: mpsc::UnboundedReceiver<Command>,
    hot_mic: Arc<HotMic>,
    config: AppConfig,
) {
    let mut split_cancel: Option<oneshot::Sender<()>> = None;
    
    // Estados de sesión que pueden cambiar durante la grabación
    let mut session_add_period = config.add_period;
    let mut session_auto_enter: Option<bool> = None; // None = usa config global, Some = fuerza un valor

    while let Some(command) = cmd_rx.recv().await {
        match command {
            Command::StartRecording(_) => {
                session_add_period = config.add_period;
                session_auto_enter = None;

                if let Some(tx) = split_cancel.take() { let _ = tx.send(()); }
                hot_mic.start_recording().await;
                println!("🎤 Grabando... [Punto Final: {}]", 
                    if session_add_period { "SI" } else { "NO" }
                );

                let mic = hot_mic.clone();
                let cfg = config.clone();
                let (cancel_tx, cancel_rx) = oneshot::channel();
                split_cancel = Some(cancel_tx);

                tokio::spawn(async move {
                    auto_split_monitor(mic, cfg, cancel_rx).await;
                });
            }
            Command::ToggleFormat => {
                session_add_period = !session_add_period;
                println!("⚡ [RRC] Toggle Puntuación: {}", if session_add_period { "ACTIVADA" } else { "DESACTIVADA" });
            }
            Command::StopInvertedEnter => {
                // Invertimos la lógica global
                session_auto_enter = Some(!config.auto_enter);
                
                if let Some(tx) = split_cancel.take() { let _ = tx.send(()); }
                let wav = hot_mic.stop_recording().await;
                if wav.len() > 8044 {
                    let forced_enter = session_auto_enter.unwrap();
                    let p = if config.experimental_live { Some(config.whisper_prompt.as_str()) } else { None };
                    if config.export_audio {
                        let _ = std::fs::write("audio_exports/last_capture.wav", &wav);
                    }
                    if config.experimental_live { println!("🧪 [Experimental Live] Aplicando prompt de estilo..."); }
                    match api::groq::transcribe_audio(&config.groq_api_key, wav, &config.groq_language, p).await {
                        Ok(text) => deliver_text(&text, &config, session_add_period, forced_enter).await,
                        Err(e) => eprintln!("❌ Error Groq: {}", e),
                    }
                }
            }
            Command::StopRecording => {
                if let Some(tx) = split_cancel.take() { let _ = tx.send(()); }
                let wav = hot_mic.stop_recording().await;
                
                if wav.len() > 8044 {
                    println!("🛑 Transcribiendo...");
                    // Usamos session_auto_enter si fue seteado, sino la config global
                    let final_enter = session_auto_enter.unwrap_or(config.auto_enter);
                    let p = if config.experimental_live { Some(config.whisper_prompt.as_str()) } else { None };
                    if config.export_audio {
                        let _ = std::fs::write("audio_exports/last_capture.wav", &wav);
                    }
                    if config.experimental_live { println!("🧪 [Experimental Live] Aplicando prompt de estilo..."); }
                    match api::groq::transcribe_audio(&config.groq_api_key, wav, &config.groq_language, p).await {
                        Ok(text) => deliver_text(&text, &config, session_add_period, final_enter).await,
                        Err(e) => eprintln!("❌ Error Groq: {}", e),
                    }
                }
            }
            Command::IncreaseGain => {
                let mut new_gain = hot_mic.get_multiplier() + 0.5;
                if new_gain > 20.0 { new_gain = 20.0; } // límite razonable
                hot_mic.set_multiplier(new_gain);
                let _ = AppConfig::save_audio_multiplier("config.toml", new_gain);
            }
            Command::DecreaseGain => {
                let mut new_gain = hot_mic.get_multiplier() - 0.5;
                if new_gain < 0.5 { new_gain = 0.5; }
                hot_mic.set_multiplier(new_gain);
                let _ = AppConfig::save_audio_multiplier("config.toml", new_gain);
            }
        }
    }
}

async fn deliver_text(text: &str, config: &AppConfig, add_period: bool, auto_enter: bool) {
    let mut final_text = text.trim().to_string();

    // 0. Diccionario de reemplazos (case-insensitive)
    for (from, to) in &config.dictionary {
        let from_lower = from.to_lowercase();
        // Reemplazo case-insensitive manual (sin regex, sin dependencias extra)
        let mut result = String::with_capacity(final_text.len());
        let text_lower = final_text.to_lowercase();
        let mut last = 0;
        for (idx, _) in text_lower.match_indices(&from_lower) {
            result.push_str(&final_text[last..idx]);
            result.push_str(to);
            last = idx + from.len();
        }
        result.push_str(&final_text[last..]);
        final_text = result;
    }

    // 1. Lógica de Punto Final
    if add_period {
        if !final_text.ends_with('.') && !final_text.ends_with('?') && !final_text.ends_with('!') {
            final_text.push('.');
        }
    } else {
        if final_text.ends_with('.') {
            final_text.pop();
        }
    }

    // 2. Lógica de Espacio Final (Hardcodeada)
    final_text.push(' ');

    println!("✅ {}", final_text);

    // 3. Copiar al portapapeles (solo si está habilitado en config)
    if config.copy_to_clipboard {
        if let Err(e) = output::clipboard::set_clipboard(&final_text) {
            eprintln!("⚠️ Error portapapeles: {}", e);
            return;
        }
    }

    // 4. Pegado atómico (solo si está habilitado en config)
    if config.paste_to_input {
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        output::typer::paste_from_clipboard();
        
        if auto_enter {
            // ⚠️ ATENCIÓN MANTENEDORES: NO REMOVER ESTE SLEEP.
            // Esto previene una condición de carrera (Race Condition) documentada: 
            // Si mandamos un "Enter" virtual inmediatamente después del "Shift+Insert", 
            // la aplicación GUI o terminal no tiene tiempo suficiente para redibujarse 
            // tras leer el portapapeles. El síntoma clásico es presionar Enter mandando
            // un mensaje en blanco y pintar la transcripción *después*.
            // Ver `auto_enter_delay_ms` en `config.toml`. Valor testeado empíricamente: ~30-50ms.
            tokio::time::sleep(config.auto_enter_delay).await;
            output::typer::press_enter();
        }
    }
}

async fn auto_split_monitor(mic: Arc<HotMic>, config: AppConfig, mut cancel: oneshot::Receiver<()>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
    loop {
        tokio::select! {
            _ = &mut cancel => break,
            _ = interval.tick() => {
                if mic.buffer_len().await >= MAX_PCM_BYTES {
                    let wav = mic.flush_and_continue().await;
                    let p = if config.experimental_live { Some(config.whisper_prompt.as_str()) } else { None };
                    if config.experimental_live { println!("🧪 [Experimental Live] Aplicando prompt en auto-split..."); }
                    if let Ok(text) = api::groq::transcribe_audio(&config.groq_api_key, wav, &config.groq_language, p).await {
                        // En auto-split usamos la configuración base
                        deliver_text(&text, &config, config.add_period, config.auto_enter).await;
                    }
                }
            }
        }
    }
}
