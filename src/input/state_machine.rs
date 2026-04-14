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
    config: AppConfig,
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
                    // --- IDLE ---
                    (InternalState::Idle, KeyEvent::Down { modifier: true }) => {
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
                                        KeyEvent::Down { .. } | KeyEvent::Up | KeyEvent::ESC => {
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
                    (_, KeyEvent::Up) => {
                        if state != InternalState::RecordingPush && !matches!(state, InternalState::RecordingTap { .. }) {
                            state = InternalState::Idle;
                        }
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
