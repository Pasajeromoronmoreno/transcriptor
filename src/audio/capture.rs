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
    /// `None` cuando la puerta está desactivada: el audio pasa entero.
    gate: Option<Arc<Mutex<NoiseGate>>>,
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
        let gate = gate_config
            .enabled
            .then(|| Arc::new(Mutex::new(NoiseGate::new(gate_config))));
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
                                let mult = f32::from_bits(audio_multiplier_clone.load(Ordering::Relaxed));
                                
                                let processed_buf: Vec<u8> = if mult != 1.0 {
                                    let mut buf = Vec::with_capacity(n);
                                    for chunk in temp_buf[..n].chunks_exact(2) {
                                        let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
                                        let new_sample = (sample as f32 * mult).clamp(-32768.0, 32767.0) as i16;
                                        buf.extend_from_slice(&new_sample.to_le_bytes());
                                    }
                                    buf
                                } else {
                                    temp_buf[..n].to_vec()
                                };

                                // Cálculo de volumen Peak (0-32767)
                                let peak = processed_buf.chunks_exact(2)
                                    .map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs())
                                    .max()
                                    .unwrap_or(0);
                                current_volume_clone.store(peak, Ordering::Relaxed);

                                if is_recording_clone.load(Ordering::SeqCst) {
                                    // El volumen se mide antes de la puerta: el
                                    // monitor tiene que mostrar lo que entra,
                                    // que es lo que se usa para calibrarla.
                                    let kept = match gate_clone.as_ref() {
                                        Some(gate) => gate.lock().await.filter(&processed_buf),
                                        None => processed_buf,
                                    };
                                    if !kept.is_empty() {
                                        let mut b = buffer_clone.lock().await;
                                        b.extend_from_slice(&kept);
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
        if let Some(gate) = self.gate.as_ref() {
            gate.lock().await.reset();
        }
        let mut b = self.buffer.lock().await;
        b.clear(); // Limpiar rastro anterior obligatoriamente
        self.is_recording.store(true, Ordering::SeqCst);
    }

    pub async fn stop_recording(&self) -> Vec<u8> {
        self.is_recording.store(false, Ordering::SeqCst);

        // La cola que no completó una ventana entra igual, para no cortar la
        // última consonante.
        if let Some(gate) = self.gate.as_ref() {
            let tail = gate.lock().await.flush();
            if !tail.is_empty() {
                self.buffer.lock().await.extend_from_slice(&tail);
            }
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
        if let Some(gate) = self.gate.as_ref() {
            gate.lock().await.reset();
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
