use tokio::process::Command;
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering, AtomicU16};
use tokio::sync::Mutex;
use crate::audio::gate::NoiseGate;
use crate::config::GateConfig;

/// HotMic re-diseñado para ser 100% atómico.
pub struct HotMic {
    buffer: Arc<Mutex<Vec<u8>>>,
    is_recording: Arc<AtomicBool>,
    current_volume: Arc<AtomicU16>,
    audio_multiplier: Arc<std::sync::atomic::AtomicU32>, // f32 almacenado en bits
    /// Existe siempre, incluso desactivada: apagada sólo observa, y esas
    /// mediciones son las que permiten elegir los umbrales.
    gate: Arc<Mutex<NoiseGate>>,
    _kill_tx: mpsc::Sender<()>,
}

impl HotMic {
    pub async fn start(
        initial_multiplier: f32,
        capture_device: Option<&str>,
        gate_config: &GateConfig,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut command = Command::new("parec");
        if let Some(device) = capture_device {
            command.arg(format!("--device={device}"));
        }
        let mut child = command
            .arg("--format=s16le")
            .arg("--rate=16000")
            .arg("--channels=1")
            .arg("--latency-msec=30")
            .stdout(Stdio::piped())
            // Audio capture is part of the background service, not a terminal
            // job. Keep terminal-generated signals from killing parec while
            // the global recording shortcut is being pressed.
            .process_group(0)
            .kill_on_drop(true)
            .spawn()?;

        let mut stdout = child.stdout.take().ok_or("parec no expuso stdout")?;
        let buffer = Arc::new(Mutex::new(Vec::with_capacity(32000 * 60))); // ~1 min pre-alloc
        let buffer_clone = buffer.clone();
        let is_recording = Arc::new(AtomicBool::new(false));
        let is_recording_clone = is_recording.clone();
        let current_volume = Arc::new(AtomicU16::new(0));
        let current_volume_clone = current_volume.clone();
        let audio_multiplier = Arc::new(std::sync::atomic::AtomicU32::new(initial_multiplier.to_bits()));
        let audio_multiplier_clone = audio_multiplier.clone();
        let gate = Arc::new(Mutex::new(NoiseGate::new(gate_config)));
        let gate_clone = gate.clone();
        
        let (kill_tx, mut kill_rx) = mpsc::channel::<()>(1);

        tokio::spawn(async move {
            let mut temp_buf = [0u8; 4096];
            loop {
                tokio::select! {
                    biased;
                    _ = kill_rx.recv() => { let _ = child.kill().await; break; }
                    result = stdout.read(&mut temp_buf) => {
                        match result {
                            Ok(0) => break,
                            Ok(n) => {
                                let raw = &temp_buf[..n];
                                let mult = f32::from_bits(audio_multiplier_clone.load(Ordering::Relaxed));

                                // El monitor muestra la señal ya amplificada,
                                // que es la que el usuario regula con los atajos
                                // de ganancia.
                                let peak = amplify(raw, mult).chunks_exact(2)
                                    .map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs())
                                    .max()
                                    .unwrap_or(0);
                                current_volume_clone.store(peak, Ordering::Relaxed);

                                if is_recording_clone.load(Ordering::SeqCst) {
                                    // La puerta decide sobre la señal SIN amplificar.
                                    // Si mirara la amplificada, los umbrales medidos
                                    // dejarían de valer, y peor: cambiar la ganancia
                                    // en caliente movería el umbral efectivo sin que
                                    // nadie lo pida.
                                    let kept = gate_clone.lock().await.filter(raw);
                                    if !kept.is_empty() {
                                        let mut b = buffer_clone.lock().await;
                                        b.extend_from_slice(&amplify(&kept, mult));
                                    }
                                }
                            }
                            Err(error) => {
                                tracing::error!(code="TRN-AUDIO-READ", error=%error, "Falló la lectura del capturador de audio");
                                break;
                            }
                        }
                    }
                }
            }
        });

        Ok(HotMic { buffer, is_recording, current_volume, audio_multiplier, gate, _kill_tx: kill_tx })
    }

    pub fn get_volume(&self) -> u16 {
        self.current_volume.load(Ordering::Relaxed)
    }

    pub fn get_multiplier(&self) -> f32 {
        f32::from_bits(self.audio_multiplier.load(Ordering::Relaxed))
    }

    pub fn set_multiplier(&self, mult: f32) {
        self.audio_multiplier.store(mult.to_bits(), Ordering::Relaxed);
    }

    pub async fn start_recording(&self) {
        // La puerta se reinicia antes de habilitar la grabación: si arrancara
        // con el sostenimiento del dictado anterior, dejaría entrar el fondo.
        self.gate.lock().await.reset();
        let mut b = self.buffer.lock().await;
        b.clear(); // Limpiar rastro anterior obligatoriamente
        self.is_recording.store(true, Ordering::SeqCst);
    }

    pub async fn stop_recording(&self) -> Vec<u8> {
        self.is_recording.store(false, Ordering::SeqCst);

        // La cola que no completó una ventana entra igual, para no cortar la
        // última consonante. Se amplifica acá porque la puerta trabaja sobre la
        // señal cruda.
        let (tail, stats) = {
            let mut gate = self.gate.lock().await;
            (gate.flush(), gate.take_stats())
        };
        if !tail.is_empty() {
            let amplified = amplify(&tail, self.get_multiplier());
            self.buffer.lock().await.extend_from_slice(&amplified);
        }
        if let Some(stats) = stats {
            // Estos son los niveles que ve la puerta, no los de una medición
            // hecha por fuera: son los que hay que mirar para calibrarla.
            tracing::info!(
                windows = stats.windows,
                kept = stats.kept,
                kept_pct = (100 * stats.kept) / stats.windows.max(1),
                level_p50_dbfs = stats.p50_dbfs,
                level_p95_dbfs = stats.p95_dbfs,
                peak_dbfs = stats.peak_dbfs,
                "Niveles de audio del dictado"
            );
        }

        let mut b = self.buffer.lock().await;
        let pcm: Vec<u8> = b.drain(..).collect(); // Drenar hasta el último byte
        drop(b);

        if pcm.is_empty() { return Vec::new(); }

        let mut wav = create_wav_header(pcm.len() as u32);
        wav.extend_from_slice(&pcm);
        wav
    }


    /// Corta la grabación tirando el audio: no se arma WAV ni se llama a la API.
    ///
    /// Devuelve los bytes descartados, o `None` si no había grabación activa,
    /// para que quien cancela sepa si había algo que cancelar.
    pub async fn discard_recording(&self) -> Option<usize> {
        if !self.is_recording.swap(false, Ordering::SeqCst) {
            return None;
        }

        // Los niveles se reportan igual que en un dictado normal. Así se puede
        // medir el ambiente —grabar, no hablar, cancelar— sin pagar la llamada
        // a la API ni recibir una transcripción de basura.
        let stats = {
            let mut gate = self.gate.lock().await;
            let stats = gate.take_stats();
            gate.reset();
            stats
        };
        if let Some(stats) = stats {
            tracing::info!(
                windows = stats.windows,
                kept = stats.kept,
                kept_pct = (100 * stats.kept) / stats.windows.max(1),
                level_p50_dbfs = stats.p50_dbfs,
                level_p95_dbfs = stats.p95_dbfs,
                peak_dbfs = stats.peak_dbfs,
                "Niveles de audio del dictado cancelado"
            );
        }

        let mut b = self.buffer.lock().await;
        let discarded = b.len();
        b.clear();
        Some(discarded)
    }

    pub async fn buffer_len(&self) -> usize {
        self.buffer.lock().await.len()
    }

    pub async fn flush_and_continue(&self) -> Vec<u8> {
        let mut b = self.buffer.lock().await;
        let pcm: Vec<u8> = b.drain(..).collect();
        drop(b);
        let mut wav = create_wav_header(pcm.len() as u32);
        wav.extend_from_slice(&pcm);
        wav
    }
}

/// Aplica la ganancia digital, saturando en vez de dar la vuelta.
fn amplify(pcm: &[u8], multiplier: f32) -> Vec<u8> {
    if multiplier == 1.0 {
        return pcm.to_vec();
    }
    let mut out = Vec::with_capacity(pcm.len());
    for pair in pcm.chunks_exact(2) {
        let sample = i16::from_le_bytes([pair[0], pair[1]]);
        let scaled = (sample as f32 * multiplier).clamp(-32768.0, 32767.0) as i16;
        out.extend_from_slice(&scaled.to_le_bytes());
    }
    out
}

pub fn create_wav_header(pcm_data_len: u32) -> Vec<u8> {
    let mut header = Vec::with_capacity(44);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&(36 + pcm_data_len).to_le_bytes());
    header.extend_from_slice(b"WAVE");
    header.extend_from_slice(b"fmt ");
    header.extend_from_slice(&16u32.to_le_bytes());
    header.extend_from_slice(&1u16.to_le_bytes()); // PCM
    header.extend_from_slice(&1u16.to_le_bytes()); // Mono
    header.extend_from_slice(&16000u32.to_le_bytes()); // 16kHz
    header.extend_from_slice(&32000u32.to_le_bytes()); // ByteRate
    header.extend_from_slice(&2u16.to_le_bytes()); // BlockAlign
    header.extend_from_slice(&16u16.to_le_bytes()); // 16-bit
    header.extend_from_slice(b"data");
    header.extend_from_slice(&pcm_data_len.to_le_bytes());
    header
}

#[cfg(test)]
mod tests {
    use super::amplify;

    #[test]
    fn unity_gain_returns_the_input_untouched() {
        let pcm = [0x10, 0x27, 0xf0, 0xd8];
        assert_eq!(amplify(&pcm, 1.0), pcm);
    }

    #[test]
    fn gain_scales_each_sample() {
        let pcm = 1000i16.to_le_bytes();
        assert_eq!(amplify(&pcm, 2.0), 2000i16.to_le_bytes());
    }

    #[test]
    fn loud_input_saturates_instead_of_wrapping() {
        // Sin la saturación, 30000 * 2 daría la vuelta a negativo y el dictado
        // sonaría destrozado en vez de simplemente recortado.
        assert_eq!(amplify(&30000i16.to_le_bytes(), 2.0), 32767i16.to_le_bytes());
        assert_eq!(amplify(&(-30000i16).to_le_bytes(), 2.0), (-32768i16).to_le_bytes());
    }
}
