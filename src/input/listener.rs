use evdev::{EventType, KeyCode};
use std::thread;
use tokio::sync::mpsc;
use crate::input::KeyEvent;
use crate::config::AppConfig;

pub fn start_keyboard_listener(tx: mpsc::UnboundedSender<KeyEvent>, config: AppConfig) {
    let devices = evdev::enumerate().into_iter().filter(|(_, dev)| {
        let name = dev.name().unwrap_or("");
        if name.contains("Transcriptor Virtual") { return false; }
        dev.supported_keys().map_or(false, |keys| keys.contains(KeyCode::KEY_ENTER))
    })
    .map(|(_, dev)| dev)
    .collect::<Vec<_>>();

    if devices.is_empty() {
        eprintln!("❌ No se encontró ningún teclado real en /dev/input.");
        return;
    }

    // Cache local de teclas para evitar leer config mil veces en el loop
    let trigger = config.hotkey_trigger;
    let toggle = config.hotkey_toggle_format;
    let send = config.hotkey_send_with_enter;
    let increase_gain = config.hotkey_increase_gain;
    let decrease_gain = config.hotkey_decrease_gain;
    let mod_key = config.hotkey_modifier;

    for mut dev in devices {
        let tx = tx.clone();
        
        thread::spawn(move || {

            // Estado inicial del modificador (evitamos syscalls dentro del loop para ganar velocidad)
            let mut is_mod_down_local = dev.get_key_state()
                .map_or(false, |k| k.contains(mod_key));

            loop {
                let events: Vec<_> = match dev.fetch_events() {
                    Ok(e) => e.collect(),
                    Err(_) => break,
                };

                for ev in events {
                    if ev.event_type() == EventType::KEY {
                        let code = KeyCode(ev.code());
                        
                        // Actualización reactiva del estado del modificador (0: UP, 1: DOWN, 2: REPEAT)
                        if code == mod_key {
                            is_mod_down_local = ev.value() != 0;
                        }

                        if code == trigger {
                            if ev.value() == 0 { // UP

                                let _ = tx.send(KeyEvent::Up);
                            } 
                            else if ev.value() == 1 { // DOWN
                                if is_mod_down_local {

                                }
                                let _ = tx.send(KeyEvent::Down { modifier: is_mod_down_local });
                            }
                        }
                        else if code == toggle {
                            if ev.value() == 1 {
                                let _ = tx.send(KeyEvent::Alt { modifier: is_mod_down_local });
                            }
                        }
                        else if code == send {
                            if ev.value() == 1 {
                                // La tecla de Envío/Invertir (físicamente Enter ahora)
                                let _ = tx.send(KeyEvent::EnterKey { modifier: is_mod_down_local });
                            }
                        }
                        else if code == increase_gain {
                            if ev.value() == 1 && is_mod_down_local {
                                let _ = tx.send(KeyEvent::IncreaseGain);
                            }
                        }
                        else if code == decrease_gain {
                            if ev.value() == 1 && is_mod_down_local {
                                let _ = tx.send(KeyEvent::DecreaseGain);
                            }
                        }
                        else if code == KeyCode::KEY_ESC {
                            if ev.value() == 1 {

                                let _ = tx.send(KeyEvent::ESC);
                            }
                        }
                    }
                }
                thread::sleep(std::time::Duration::from_millis(5));
            }
        });
    }
}
