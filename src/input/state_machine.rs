use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{sleep_until, Duration, Instant};
use crate::config::AppConfig;
use crate::input::{Command, KeyEvent, Mode};

#[derive(Debug, Clone, Copy, PartialEq)]
enum InternalState {
    Idle,
    Evaluating { start: Instant },
    RecordingPush,
    RecordingTap { started_at: Instant },
    WaitingForLatch { latch_deadline: Instant },
}

pub async fn run_state_machine(
    mut rx: mpsc::UnboundedReceiver<KeyEvent>,
    cmd_tx: mpsc::UnboundedSender<Command>,
    config: Arc<AppConfig>,
) {
    let mut state = InternalState::Idle;
    let fallback_instant = Instant::now() + Duration::from_secs(86400 * 365);
    
    let push_threshold = config.push_start_threshold; 
    let latch_window = config.push_release_window;
    let tap_safety_window = config.tap_timeout;

    loop {
        let deadline = match state {
            InternalState::Evaluating { start } => start + push_threshold,
            InternalState::WaitingForLatch { latch_deadline } => latch_deadline,
            _ => fallback_instant,
        };

        tokio::select! {
            Some(event) = rx.recv() => {
                let now = Instant::now();
                
                match (state, event) {
                    // --- CANCELAR ---
                    // ESC aborta desde cualquier estado. Va antes que el resto
                    // de los brazos porque `RecordingTap` captura todo evento.
                    (_, KeyEvent::Escape) => {
                        let _ = cmd_tx.send(Command::CancelRecording);
                        state = InternalState::Idle;
                    }

                    // --- IDLE ---
                    (InternalState::Idle, KeyEvent::Down { modifier: true }) => {
                        let _ = cmd_tx.send(Command::BeginArming);
                        state = InternalState::Evaluating { start: now };
                    }

                    // --- EVALUATING ---
                    (InternalState::Evaluating { .. }, KeyEvent::Up) => {
                        println!("🎤 [MODO TAP] Detectado.");
                        let _ = cmd_tx.send(Command::StartRecording(Mode::Tap));
                        state = InternalState::RecordingTap { started_at: now };
                    }
                    
                    // --- MODO PUSH (Mantenido) ---
                    (InternalState::RecordingPush, KeyEvent::Up) => {
                        let _ = cmd_tx.send(Command::WaitForLatch);
                        state = InternalState::WaitingForLatch { 
                            latch_deadline: now + latch_window 
                        };
                    }
                    (InternalState::RecordingPush, KeyEvent::Alt { .. }) => {
                        let _ = cmd_tx.send(Command::ToggleFormat);
                    }
                    (InternalState::RecordingPush, KeyEvent::EnterKey { .. }) => {
                        let _ = cmd_tx.send(Command::StopInvertedEnter);
                        state = InternalState::Idle;
                    }

                    // --- WAITING FOR LATCH ---
                    (InternalState::WaitingForLatch { .. }, KeyEvent::Down { .. }) => {
                        println!("🔁 Recording anclada (LATCH).");
                        let _ = cmd_tx.send(Command::LatchRecording);
                        state = InternalState::RecordingTap { started_at: now };
                    }

                    // --- MODO TAP (Anclado) ---
                    (InternalState::RecordingTap { started_at, .. }, event) => {
                        match event {
                            KeyEvent::Alt { .. } => {
                                let _ = cmd_tx.send(Command::ToggleFormat);
                            }
                            KeyEvent::EnterKey { .. } => {
                                let _ = cmd_tx.send(Command::StopInvertedEnter);
                                state = InternalState::Idle;
                            }
                            _ => {
                                if now.duration_since(started_at) > tap_safety_window {
                                    match event {
                                        KeyEvent::Down { .. } | KeyEvent::Up => {
                                            println!("🛑 [STOP] Finalizado.");
                                            let _ = cmd_tx.send(Command::StopRecording);
                                            state = InternalState::Idle;
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }

                    (_, KeyEvent::IncreaseGain) => {
                        let _ = cmd_tx.send(Command::IncreaseGain);
                    }
                    (_, KeyEvent::DecreaseGain) => {
                        let _ = cmd_tx.send(Command::DecreaseGain);
                    }
                    // Acá sólo llegan Idle y WaitingForLatch: RecordingPush y
                    // RecordingTap consumen su propio Up en los brazos de
                    // arriba, así que la guarda que había era siempre cierta.
                    (_, KeyEvent::Up) => {
                        state = InternalState::Idle;
                    }
                    
                    _ => {}
                }
            }
            
            _ = sleep_until(deadline) => {
                match state {
                    InternalState::Evaluating { .. } => {
                        println!("🎤 [MODO PUSH] Activo.");
                        let _ = cmd_tx.send(Command::StartRecording(Mode::Push));
                        state = InternalState::RecordingPush;
                    }
                    InternalState::WaitingForLatch { .. } => {
                        println!("🛑 [STOP] Push terminado.");
                        let _ = cmd_tx.send(Command::StopRecording);
                        state = InternalState::Idle;
                    }
                    _ => {},
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::run_state_machine;
    use crate::config::AppConfig;
    use crate::input::{Command, KeyEvent, Mode};
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tokio::time::{advance, Duration};

    /// Maneja la máquina de estados como una caja negra: se le empujan eventos
    /// de teclado y se leen los comandos que emite.
    struct Harness {
        keys: mpsc::UnboundedSender<KeyEvent>,
        commands: mpsc::UnboundedReceiver<Command>,
    }

    impl Harness {
        fn start() -> Self {
            let (keys, key_rx) = mpsc::unbounded_channel();
            let (cmd_tx, commands) = mpsc::unbounded_channel();
            tokio::spawn(run_state_machine(
                key_rx,
                cmd_tx,
                Arc::new(AppConfig::default()),
            ));
            Self { keys, commands }
        }

        fn press(&self, event: KeyEvent) {
            self.keys.send(event).expect("la máquina de estados sigue viva");
        }

        /// El reloj está pausado, así que un `recv` sin nada pendiente avanza el
        /// tiempo hasta el próximo temporizador en vez de colgarse.
        async fn next(&mut self) -> Command {
            self.commands.recv().await.expect("se esperaba un comando")
        }

        /// Arranca en modo Tap: pulsación corta del trigger con modificador.
        async fn start_tap(&mut self) {
            self.press(KeyEvent::Down { modifier: true });
            self.press(KeyEvent::Up);
            assert_eq!(self.next().await, Command::BeginArming);
            assert_eq!(self.next().await, Command::StartRecording(Mode::Tap));
        }

        /// Arranca en modo Push: se mantiene el trigger más allá del umbral.
        async fn start_push(&mut self) {
            self.press(KeyEvent::Down { modifier: true });
            assert_eq!(self.next().await, Command::BeginArming);
            assert_eq!(self.next().await, Command::StartRecording(Mode::Push));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn releasing_the_trigger_quickly_starts_tap_mode() {
        let mut harness = Harness::start();
        harness.start_tap().await;
    }

    #[tokio::test(start_paused = true)]
    async fn holding_the_trigger_past_the_threshold_starts_push_mode() {
        let mut harness = Harness::start();
        harness.start_push().await;
    }

    #[tokio::test(start_paused = true)]
    async fn releasing_push_waits_for_a_latch_before_stopping() {
        let mut harness = Harness::start();
        harness.start_push().await;

        harness.press(KeyEvent::Up);
        assert_eq!(harness.next().await, Command::WaitForLatch);
        // Nadie vuelve a pulsar: al vencer la ventana de latch, se corta.
        assert_eq!(harness.next().await, Command::StopRecording);
    }

    #[tokio::test(start_paused = true)]
    async fn pressing_again_inside_the_latch_window_anchors_the_recording() {
        let mut harness = Harness::start();
        harness.start_push().await;

        harness.press(KeyEvent::Up);
        assert_eq!(harness.next().await, Command::WaitForLatch);
        harness.press(KeyEvent::Down { modifier: false });
        assert_eq!(harness.next().await, Command::LatchRecording);
    }

    #[tokio::test(start_paused = true)]
    async fn esc_cancels_an_active_push_recording() {
        let mut harness = Harness::start();
        harness.start_push().await;

        harness.press(KeyEvent::Escape);
        assert_eq!(harness.next().await, Command::CancelRecording);
    }

    #[tokio::test(start_paused = true)]
    async fn esc_cancels_an_anchored_tap_recording() {
        let mut harness = Harness::start();
        harness.start_tap().await;

        harness.press(KeyEvent::Escape);
        assert_eq!(harness.next().await, Command::CancelRecording);
    }

    #[tokio::test(start_paused = true)]
    async fn esc_after_cancelling_leaves_the_machine_idle() {
        let mut harness = Harness::start();
        harness.start_tap().await;
        harness.press(KeyEvent::Escape);
        assert_eq!(harness.next().await, Command::CancelRecording);

        // Tras cancelar se vuelve a Idle: un nuevo dictado arranca de cero.
        harness.start_tap().await;
    }

    #[tokio::test(start_paused = true)]
    async fn tap_ignores_the_release_of_its_own_trigger() {
        let mut harness = Harness::start();
        harness.start_tap().await;

        // El Up inmediato es el rebote de la propia pulsación que abrió el Tap:
        // dentro de la ventana de seguridad no debe cortar la grabación.
        harness.press(KeyEvent::Up);
        harness.press(KeyEvent::EnterKey { modifier: false });
        assert_eq!(harness.next().await, Command::StopInvertedEnter);
    }

    #[tokio::test(start_paused = true)]
    async fn tap_stops_on_a_new_trigger_once_the_safety_window_passed() {
        let mut harness = Harness::start();
        harness.start_tap().await;

        advance(AppConfig::default().tap_timeout + Duration::from_millis(50)).await;
        harness.press(KeyEvent::Down { modifier: false });
        assert_eq!(harness.next().await, Command::StopRecording);
    }

    #[tokio::test(start_paused = true)]
    async fn enter_finishes_a_push_recording_inverting_the_newline() {
        let mut harness = Harness::start();
        harness.start_push().await;

        harness.press(KeyEvent::EnterKey { modifier: false });
        assert_eq!(harness.next().await, Command::StopInvertedEnter);
    }

    #[tokio::test(start_paused = true)]
    async fn alt_toggles_the_final_period_while_recording() {
        let mut harness = Harness::start();
        harness.start_push().await;

        harness.press(KeyEvent::Alt { modifier: false });
        assert_eq!(harness.next().await, Command::ToggleFormat);
    }

    #[tokio::test(start_paused = true)]
    async fn gain_shortcuts_work_without_an_active_recording() {
        let mut harness = Harness::start();

        harness.press(KeyEvent::IncreaseGain);
        assert_eq!(harness.next().await, Command::IncreaseGain);
        harness.press(KeyEvent::DecreaseGain);
        assert_eq!(harness.next().await, Command::DecreaseGain);
    }

    #[tokio::test(start_paused = true)]
    async fn the_trigger_without_the_modifier_does_not_start_anything() {
        let mut harness = Harness::start();

        // Sin modificador no hay dictado; el siguiente comando debe venir de la
        // pulsación válida que le sigue, no de esta.
        harness.press(KeyEvent::Down { modifier: false });
        harness.press(KeyEvent::IncreaseGain);
        assert_eq!(harness.next().await, Command::IncreaseGain);
    }
}
