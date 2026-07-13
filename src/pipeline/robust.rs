use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use aho_corasick::{AhoCorasick, MatchKind};
use crate::audio::capture::HotMic;
use crate::config::AppConfig;
use crate::input::Command;
use crate::overlay::{AppPhase, OverlayHub, RecordingMode};
use crate::{api, output, transcription};
use crate::pipeline::profile::PipelineProfiler;
use thiserror::Error;
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_PCM_BYTES: usize = 24 * 1024 * 1024;
static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Pipeline simplificado: Todo va al portapapeles y se pega atómicamente.
pub async fn run(
    mut cmd_rx: mpsc::UnboundedReceiver<Command>,
    hot_mic: Arc<HotMic>,
    config: AppConfig,
    overlay: OverlayHub,
) {
    let mut split_cancel: Option<oneshot::Sender<()>> = None;
    
    // Estados de sesión que pueden cambiar durante la grabación
    let mut session_add_period = config.add_period;
    let mut session_auto_enter: Option<bool> = None; // None = usa config global, Some = fuerza un valor
    let mut session_mode: Option<RecordingMode> = None;
    let mut session_id = 0u64;

    while let Some(command) = cmd_rx.recv().await {
        match command {
            Command::BeginArming => {
                overlay.publish(AppPhase::Arming, None, None);
            }
            Command::StartRecording(mode) => {
                session_id = ((std::process::id() as u64) << 32) | SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
                session_add_period = config.add_period;
                session_auto_enter = None;
                session_mode = Some(mode.into());

                if let Some(tx) = split_cancel.take() { let _ = tx.send(()); }
                hot_mic.start_recording().await;
                overlay.publish(AppPhase::Recording, session_mode, None);
                tracing::info!(session_id, mode=?session_mode, "Grabación iniciada");
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
            Command::WaitForLatch => {
                overlay.publish(AppPhase::WaitingForLatch, session_mode, None);
            }
            Command::LatchRecording => {
                session_mode = Some(RecordingMode::Tap);
                overlay.publish(AppPhase::Recording, session_mode, None);
            }
            Command::ToggleFormat => {
                session_add_period = !session_add_period;
                println!("⚡ [RRC] Toggle Puntuación: {}", if session_add_period { "ACTIVADA" } else { "DESACTIVADA" });
            }
            Command::StopInvertedEnter => {
                if let Some(tx) = split_cancel.take() { let _ = tx.send(()); }
                session_auto_enter = Some(!config.auto_enter);
                finish_recording(session_id, &hot_mic, &config, &overlay, session_mode, session_add_period, !config.auto_enter).await;
            }
            Command::StopRecording => {
                if let Some(tx) = split_cancel.take() { let _ = tx.send(()); }
                let final_enter = session_auto_enter.unwrap_or(config.auto_enter);
                finish_recording(session_id, &hot_mic, &config, &overlay, session_mode, session_add_period, final_enter).await;
            }
            Command::IncreaseGain => {
                let mut new_gain = hot_mic.get_multiplier() + 0.5;
                if new_gain > 20.0 { new_gain = 20.0; } // límite razonable
                hot_mic.set_multiplier(new_gain);
                if let Err(error) = AppConfig::save_audio_multiplier(config.config_path.as_deref(), new_gain) {
                    tracing::warn!(code="TRN-CONFIG-WRITE", error=%error, "No se pudo persistir la ganancia");
                }
            }
            Command::DecreaseGain => {
                let mut new_gain = hot_mic.get_multiplier() - 0.5;
                if new_gain < 0.5 { new_gain = 0.5; }
                hot_mic.set_multiplier(new_gain);
                if let Err(error) = AppConfig::save_audio_multiplier(config.config_path.as_deref(), new_gain) {
                    tracing::warn!(code="TRN-CONFIG-WRITE", error=%error, "No se pudo persistir la ganancia");
                }
            }
        }
    }
}

#[tracing::instrument(skip_all, fields(session_id = session_id, mode=?session_mode))]
async fn finish_recording(session_id: u64, hot_mic: &HotMic, config: &AppConfig, overlay: &OverlayHub, session_mode: Option<RecordingMode>, add_period: bool, auto_enter: bool) {
    let mut profiler = PipelineProfiler::new(config.profile_latency);
    profiler.start();
    let wav = hot_mic.stop_recording().await;
    profiler.stamp("Frenar Grabación (Obtención de Audio)");
    if wav.len() <= 8044 {
        tracing::warn!(code="TRN-AUDIO-EMPTY", wav_bytes=wav.len(), "La captura no contiene audio suficiente");
        overlay.publish(AppPhase::Error, None, Some("Sin audio · TRN-AUDIO-EMPTY".into()));
        profiler.finish();
        return;
    }
    overlay.publish(AppPhase::Transcribing, session_mode, None);
    tracing::info!(wav_bytes=wav.len(), mode=?session_mode, "Iniciando transcripción");
    if config.export_audio {
        if let Err(error) = std::fs::write("audio_exports/last_capture.wav", &wav) {
            tracing::warn!(code="TRN-AUDIO-EXPORT", error=%error, "No se pudo exportar la captura");
        } else { profiler.stamp("Exportar Audio a WAV"); }
    }
    let prompt = config.experimental_live.then_some(config.whisper_prompt.as_str());
    profiler.stamp("Inferencia API Groq (Iniciar Petición)");
    let retry_overlay = overlay.clone();
    let client = match api::groq::GroqClient::new(config.groq_connect_timeout, config.groq_request_timeout) {
        Ok(client) => client,
        Err(error) => {
            tracing::error!(code=error.code(), error=%error, "No se pudo crear el cliente de transcripción");
            overlay.publish(AppPhase::Error, None, Some(format!("Error de transcripción · {}", error.code())));
            return;
        }
    };
    match transcription::transcribe(&client, &config.retry, &config.groq_api_key, &wav, &config.groq_language, prompt, move |attempt, max, _, error| {
        let reason = if error.code() == "TRN-GROQ-RATE" { "Groq ocupado" } else { "Problema de red" };
        retry_overlay.publish(AppPhase::Retrying, session_mode, Some(format!("{reason} · reintentando {attempt}/{max}…")));
    }).await {
        Ok(text) => {
            profiler.stamp("API Groq (Respuesta Recibida)");
            overlay.publish(AppPhase::Delivering, session_mode, None);
            match deliver_text(&text, config, add_period, auto_enter, &mut profiler).await {
                Ok(()) => overlay.publish(AppPhase::Idle, None, None),
                Err(error) => {
                    tracing::error!(code=error.code(), error=%error, "Fallo entregando transcripción");
                    overlay.publish(AppPhase::Error, None, Some(format!("Error de salida · {}", error.code())));
                }
            }
        }
        Err(error) => {
            tracing::error!(code=error.code(), status=error.status(), request_id=error.request_id(), error=%error, error_chain=%crate::observability::error_chain(&error), "Fallo definitivo de transcripción");
            eprintln!("❌ Error Groq [{}]: {error:?}", error.code());
            overlay.publish(AppPhase::Error, None, Some(format!("Error de transcripción · {}", error.code())));
            profiler.finish();
        }
    }
}

#[derive(Debug, Error)]
enum DeliveryError {
    #[error("clipboard: {0}")] Clipboard(String),
    #[error("uinput: {0}")] Uinput(String),
    #[error("diccionario: {0}")] Dictionary(String),
}
impl DeliveryError { fn code(&self) -> &'static str { match self { Self::Clipboard(_) => "TRN-OUTPUT-CLIPBOARD", Self::Uinput(_) => "TRN-OUTPUT-UINPUT", Self::Dictionary(_) => "TRN-DICTIONARY-BUILD" } } }

async fn deliver_text(text: &str, config: &AppConfig, add_period: bool, auto_enter: bool, profiler: &mut PipelineProfiler) -> Result<(), DeliveryError> {
    let mut final_text = text.trim().to_string();

    // 0. Diccionario de reemplazos (case-insensitive) — single-pass con Aho-Corasick
    profiler.stamp("Diccionario (Inicio)");
    if config.dictionary_enabled && !config.dictionary.is_empty() {
        let patterns: Vec<String> = config.dictionary.iter()
            .map(|(k, _)| k.to_lowercase())
            .collect();
        let ac = AhoCorasick::builder()
            .match_kind(MatchKind::LeftmostLongest)
            .build(&patterns)
            .map_err(|error| DeliveryError::Dictionary(error.to_string()))?;

        let lower_input = final_text.to_lowercase();
        let mut result = String::with_capacity(final_text.len());
        let mut last_end = 0;
        for m in ac.find_iter(&lower_input) {
            result.push_str(&final_text[last_end..m.start()]);
            result.push_str(&config.dictionary[m.pattern().as_usize()].1);
            last_end = m.end();
        }
        result.push_str(&final_text[last_end..]);
        final_text = result;
    }
    profiler.stamp("Diccionario (Fin / Reemplazo completado)");

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
            return Err(DeliveryError::Clipboard(e.to_string()));
        }
        profiler.stamp("Copiar al portapapeles");
    }

    // 4. Pegado atómico (solo si está habilitado en config)
    if config.paste_to_input {
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        output::typer::paste_from_clipboard().map_err(DeliveryError::Uinput)?;
        profiler.stamp("Simular pegar desde portapapeles (Shift+Insert)");
        
        if auto_enter {
            // ⚠️ ATENCIÓN MANTENEDORES: NO REMOVER ESTE SLEEP.
            // Esto previene una condición de carrera (Race Condition) documentada: 
            // Si mandamos un "Enter" virtual inmediatamente después del "Shift+Insert", 
            // la aplicación GUI o terminal no tiene tiempo suficiente para redibujarse 
            // tras leer el portapapeles. El síntoma clásico es presionar Enter mandando
            // un mensaje en blanco y pintar la transcripción *después*.
            // Ver `auto_enter_delay_ms` en `config.toml`. Valor testeado empíricamente: ~30-50ms.
            tokio::time::sleep(config.auto_enter_delay).await;
            output::typer::press_enter().map_err(DeliveryError::Uinput)?;
            profiler.stamp("Simular Enter");
        }
    }

    profiler.finish();
    Ok(())
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
                    let client = match api::groq::GroqClient::new(config.groq_connect_timeout, config.groq_request_timeout) {
                        Ok(client) => client,
                        Err(error) => { tracing::error!(code=error.code(), error=%error, "No se pudo crear cliente Groq en auto-split"); continue; }
                    };
                    match transcription::transcribe(&client, &config.retry, &config.groq_api_key, &wav, &config.groq_language, p, |_, _, _, _| {}).await {
                      Ok(text) => {
                        // En auto-split usamos la configuración base con profiler inactivo
                        let mut dummy_profiler = PipelineProfiler::new(false);
                        if let Err(error) = deliver_text(&text, &config, config.add_period, config.auto_enter, &mut dummy_profiler).await {
                            tracing::error!(code=error.code(), error=%error, "Fallo de salida en auto-split");
                        }
                      }
                      Err(error) => tracing::error!(code=error.code(), status=error.status(), request_id=error.request_id(), error=%error, error_chain=%crate::observability::error_chain(&error), "Fallo de transcripción en auto-split"),
                    }
                }
            }
        }
    }
}
