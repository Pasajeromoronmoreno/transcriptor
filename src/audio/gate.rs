use crate::config::GateConfig;

/// Ventana de análisis. 20 ms a 16 kHz mono de 16 bits.
///
/// Es el compromiso entre resolución y estabilidad: más corta persigue cada
/// sílaba y abre y cierra de más; más larga se come el ataque de las palabras.
const WINDOW_SAMPLES: usize = 320;
const WINDOW_BYTES: usize = WINDOW_SAMPLES * 2;
const SAMPLE_RATE: f32 = 16_000.0;

/// Puerta de ruido por amplitud.
///
/// Descarta las ventanas cuyo RMS queda por debajo del umbral, para que el
/// audio de fondo de las pausas no llegue a la transcripción. No separa el
/// fondo de la voz cuando suenan a la vez: eso viaja en las mismas muestras.
///
/// La histéresis —abrir en un nivel y cerrar en otro más bajo— evita el
/// castañeteo en el borde del umbral, y el tiempo de cierre sostiene la puerta
/// durante las pausas cortas entre palabras.
pub struct NoiseGate {
    open_level: f32,
    close_level: f32,
    release_windows: u32,
    open: bool,
    hold: u32,
    pending: Vec<u8>,
    /// Se conserva la ventana anterior para emitirla al abrir: sin eso, el
    /// ataque de la primera palabra queda del lado descartado.
    previous: Vec<u8>,
}

impl NoiseGate {
    pub fn new(config: &GateConfig) -> Self {
        let release_windows =
            (config.release.as_secs_f32() * SAMPLE_RATE / WINDOW_SAMPLES as f32).ceil() as u32;

        Self {
            open_level: dbfs_to_linear(config.open_threshold_dbfs),
            close_level: dbfs_to_linear(config.close_threshold_dbfs),
            release_windows,
            open: false,
            hold: 0,
            pending: Vec::with_capacity(WINDOW_BYTES * 2),
            previous: Vec::new(),
        }
    }

    /// Olvida el estado entre dictados: cada grabación arranca con la puerta
    /// cerrada, sin arrastrar el sostenimiento de la anterior.
    pub fn reset(&mut self) {
        self.open = false;
        self.hold = 0;
        self.pending.clear();
        self.previous.clear();
    }

    /// Consume audio y devuelve sólo lo que pasa la puerta.
    pub fn filter(&mut self, input: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(input);
        let mut output = Vec::with_capacity(self.pending.len());

        let mut consumed = 0;
        while self.pending.len() - consumed >= WINDOW_BYTES {
            let window = &self.pending[consumed..consumed + WINDOW_BYTES];
            consumed += WINDOW_BYTES;

            let level = rms(window);
            let was_open = self.open;

            if level >= self.open_level {
                self.open = true;
                self.hold = self.release_windows;
            } else if self.open {
                if level >= self.close_level {
                    // Entre los dos umbrales la puerta no cambia de estado.
                    self.hold = self.release_windows;
                } else if self.hold > 0 {
                    self.hold -= 1;
                } else {
                    self.open = false;
                }
            }

            if self.open {
                if !was_open {
                    output.append(&mut self.previous);
                }
                output.extend_from_slice(window);
            }
            self.previous.clear();
            self.previous.extend_from_slice(window);
        }

        self.pending.drain(..consumed);
        output
    }

    /// Entrega la cola que no llegó a completar una ventana, para no cortar la
    /// última consonante del dictado.
    pub fn flush(&mut self) -> Vec<u8> {
        let tail = if self.open {
            std::mem::take(&mut self.pending)
        } else {
            self.pending.clear();
            Vec::new()
        };
        self.previous.clear();
        tail
    }
}

fn dbfs_to_linear(dbfs: f32) -> f32 {
    10f32.powf(dbfs / 20.0)
}

/// RMS de la ventana, normalizado a 0..1 sobre la escala de 16 bits.
fn rms(window: &[u8]) -> f32 {
    let samples = window.chunks_exact(2);
    let count = samples.len();
    if count == 0 {
        return 0.0;
    }
    let sum: f64 = samples
        .map(|pair| {
            let sample = i16::from_le_bytes([pair[0], pair[1]]) as f64;
            sample * sample
        })
        .sum();
    ((sum / count as f64).sqrt() / 32768.0) as f32
}

#[cfg(test)]
mod tests {
    use super::{NoiseGate, WINDOW_BYTES, WINDOW_SAMPLES};
    use crate::config::GateConfig;
    use std::time::Duration;

    /// Genera `windows` ventanas de una onda cuadrada al nivel pedido en dBFS.
    fn tone(dbfs: f32, windows: usize) -> Vec<u8> {
        let amplitude = (10f32.powf(dbfs / 20.0) * 32767.0) as i16;
        let mut out = Vec::with_capacity(WINDOW_BYTES * windows);
        for index in 0..WINDOW_SAMPLES * windows {
            let sample = if index % 2 == 0 { amplitude } else { -amplitude };
            out.extend_from_slice(&sample.to_le_bytes());
        }
        out
    }

    fn config(open: f32, close: f32, release_ms: u64) -> GateConfig {
        GateConfig {
            enabled: true,
            open_threshold_dbfs: open,
            close_threshold_dbfs: close,
            release: Duration::from_millis(release_ms),
        }
    }

    #[test]
    fn background_below_the_threshold_is_discarded() {
        let mut gate = NoiseGate::new(&config(-27.0, -31.0, 0));
        assert!(gate.filter(&tone(-40.0, 10)).is_empty());
    }

    #[test]
    fn speech_above_the_threshold_passes() {
        let mut gate = NoiseGate::new(&config(-27.0, -31.0, 0));
        assert!(!gate.filter(&tone(-20.0, 10)).is_empty());
    }

    #[test]
    fn opening_also_emits_the_previous_window() {
        let mut gate = NoiseGate::new(&config(-27.0, -31.0, 0));
        // Una ventana de fondo y otra de voz: la de fondo se guarda como
        // contexto y sale junto con la primera que abre la puerta.
        gate.filter(&tone(-40.0, 1));
        let passed = gate.filter(&tone(-20.0, 1));
        assert_eq!(passed.len(), WINDOW_BYTES * 2);
    }

    #[test]
    fn between_both_thresholds_the_gate_keeps_its_state() {
        let mut gate = NoiseGate::new(&config(-27.0, -31.0, 0));
        gate.filter(&tone(-20.0, 1));
        // -29 dBFS está bajo el umbral de apertura pero sobre el de cierre.
        assert_eq!(gate.filter(&tone(-29.0, 4)).len(), WINDOW_BYTES * 4);
        // Y sigue cerrada si nunca se abrió.
        let mut fresh = NoiseGate::new(&config(-27.0, -31.0, 0));
        assert!(fresh.filter(&tone(-29.0, 4)).is_empty());
    }

    #[test]
    fn the_release_holds_the_gate_through_short_pauses() {
        // 100 ms de sostenimiento son cinco ventanas de 20 ms.
        let mut gate = NoiseGate::new(&config(-27.0, -31.0, 100));
        gate.filter(&tone(-20.0, 1));
        let held = gate.filter(&tone(-60.0, 5));
        assert_eq!(held.len(), WINDOW_BYTES * 5, "la pausa corta debe sostenerse");
        assert!(gate.filter(&tone(-60.0, 5)).is_empty(), "vencido el sostén, cierra");
    }

    #[test]
    fn audio_shorter_than_a_window_is_buffered_until_it_completes() {
        let mut gate = NoiseGate::new(&config(-27.0, -31.0, 0));
        let loud = tone(-20.0, 1);
        assert!(gate.filter(&loud[..WINDOW_BYTES / 2]).is_empty());
        assert_eq!(gate.filter(&loud[WINDOW_BYTES / 2..]).len(), WINDOW_BYTES);
    }

    #[test]
    fn flush_returns_the_tail_only_when_the_gate_is_open() {
        let mut gate = NoiseGate::new(&config(-27.0, -31.0, 0));
        let loud = tone(-20.0, 1);
        gate.filter(&loud);
        gate.filter(&loud[..WINDOW_BYTES / 2]);
        assert_eq!(gate.flush().len(), WINDOW_BYTES / 2);

        let mut closed = NoiseGate::new(&config(-27.0, -31.0, 0));
        closed.filter(&tone(-60.0, 1));
        closed.filter(&loud[..WINDOW_BYTES / 2]);
        assert!(closed.flush().is_empty());
    }

    #[test]
    fn reset_forgets_the_state_between_dictations() {
        let mut gate = NoiseGate::new(&config(-27.0, -31.0, 1000));
        gate.filter(&tone(-20.0, 1));
        gate.reset();
        // Sin el reset, el sostenimiento de la grabación anterior dejaría pasar
        // el fondo del arranque de la siguiente.
        assert!(gate.filter(&tone(-60.0, 1)).is_empty());
    }
}
