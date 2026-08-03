use std::collections::VecDeque;
use std::time::Duration;

/// Bytes por segundo del formato de captura: 16 kHz, mono, 16 bits.
const BYTES_PER_SECOND: f32 = 32_000.0;

/// Guarda el audio inmediatamente anterior al atajo.
///
/// El micrófono está caliente de forma permanente, pero mientras no se graba
/// cada bloque se descarta. Quien empieza a hablar y aprieta la tecla una
/// fracción tarde pierde la primera sílaba. Este búfer circular conserva ese
/// tramo para poder anteponerlo al dictado.
pub struct PreRoll {
    capacity: usize,
    buffer: VecDeque<u8>,
}

impl PreRoll {
    pub fn new(window: Duration) -> Self {
        // Se redondea a par para no partir una muestra de 16 bits por el medio.
        let mut capacity = (window.as_secs_f32() * BYTES_PER_SECOND) as usize;
        capacity -= capacity % 2;
        Self {
            capacity,
            buffer: VecDeque::with_capacity(capacity),
        }
    }

    /// Agrega audio crudo y descarta lo que ya no entra en la ventana.
    pub fn push(&mut self, pcm: &[u8]) {
        if self.capacity == 0 {
            return;
        }
        self.buffer.extend(pcm);
        if self.buffer.len() > self.capacity {
            let mut excess = self.buffer.len() - self.capacity;
            // Descartar un byte impar correría todas las muestras siguientes y
            // convertiría el audio en ruido.
            excess += excess % 2;
            self.buffer.drain(..excess.min(self.buffer.len()));
        }
    }

    /// Entrega lo guardado y deja el búfer vacío.
    pub fn take(&mut self) -> Vec<u8> {
        self.buffer.drain(..).collect()
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{PreRoll, BYTES_PER_SECOND};
    use std::time::Duration;

    fn pcm(bytes: usize) -> Vec<u8> {
        (0..bytes).map(|index| index as u8).collect()
    }

    #[test]
    fn a_zero_window_disables_it_and_keeps_nothing() {
        let mut preroll = PreRoll::new(Duration::ZERO);
        assert_eq!(preroll.capacity, 0);
        preroll.push(&pcm(1000));
        assert!(preroll.take().is_empty());
    }

    #[test]
    fn the_window_is_sized_from_the_capture_format() {
        // 300 ms a 32 000 B/s son 9 600 bytes: 4 800 muestras.
        assert_eq!(PreRoll::new(Duration::from_millis(300)).capacity, 9_600);
        assert_eq!(BYTES_PER_SECOND as usize, 32_000);
    }

    #[test]
    fn only_the_most_recent_audio_survives() {
        let mut preroll = PreRoll::new(Duration::from_millis(10)); // 320 bytes
        preroll.push(&pcm(200));
        preroll.push(&pcm(400));

        let kept = preroll.take();
        assert_eq!(kept.len(), 320);
        // La cola conservada termina en el último byte que entró.
        let ultimo = pcm(400);
        assert_eq!(kept[kept.len() - 1], *ultimo.last().unwrap());
    }

    #[test]
    fn discarding_never_breaks_sample_alignment() {
        let mut preroll = PreRoll::new(Duration::from_millis(10));
        // Bloques impares fuerzan el caso en que el excedente cae a mitad de
        // una muestra de dos bytes.
        for _ in 0..10 {
            preroll.push(&pcm(37));
        }
        assert_eq!(preroll.take().len() % 2, 0);
    }

    #[test]
    fn taking_empties_it_so_the_next_dictation_starts_clean() {
        let mut preroll = PreRoll::new(Duration::from_millis(300));
        preroll.push(&pcm(1000));
        assert!(!preroll.take().is_empty());
        assert!(preroll.take().is_empty());
    }

    #[test]
    fn clear_drops_everything_without_returning_it() {
        let mut preroll = PreRoll::new(Duration::from_millis(300));
        preroll.push(&pcm(1000));
        preroll.clear();
        assert!(preroll.take().is_empty());
    }
}
