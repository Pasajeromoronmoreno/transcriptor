mod config;
mod input;
mod audio;
mod api;
mod output;
mod pipeline;

use config::AppConfig;
use input::{listener, state_machine};
use tokio::sync::mpsc;
use std::sync::Arc;
use tokio::signal;
use std::process::Command;
use std::io::{self, Write};

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();

    // Limpia instancias previas y procesos huérfanos para un inicio limpio.
    cleanup_old_processes();
    let config = AppConfig::load_from_file("config.toml");
    println!("🎙️ Transcriptor v0.6 — Robust Cimientos");
    println!("------------------------------------------------------------");

    // Proactive cleanup: kill any other instances or parec orphans
    // (This is a safety measure against zombies reported by user)
    
    // Inicializar teclado uinput
    output::typer::init();

    // Iniciar captura de audio
    let hot_mic = match audio::capture::HotMic::start(config.audio_multiplier).await {
        Ok(mic) => {
            let m = Arc::new(mic);
            start_volume_monitor(m.clone());
            println!("🔊 Audio listo.");
            m
        }
        Err(e) => {
            eprintln!("❌ Error de audio: {}", e);
            return;
        }
    };

    // Canal de eventos evdev -> state machine -> pipeline
    let (key_tx, key_rx) = mpsc::unbounded_channel();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

    // Listener de teclado con filtrado de dispositivo virtual
    listener::start_keyboard_listener(key_tx, config.clone());

    let cfg_clone = config.clone();
    tokio::spawn(async move {
        state_machine::run_state_machine(key_rx, cmd_tx, cfg_clone).await;
    });

    println!("------------------------------------------------------------");
    if config.groq_api_key.is_empty() {
        println!("⚠️ Configura tu API key en .env (GROQ_API_KEY) o en config.toml");
    }

    let mut out_info = Vec::new();
    if config.copy_to_clipboard { out_info.push("📋 clipboard"); }
    if config.paste_to_input { out_info.push("⌨️ hardware_type"); }
    println!("👉 {:?} + {:?} para grabar. Salida: {}", config.hotkey_modifier, config.hotkey_trigger, out_info.join(" + "));
    println!("🔊 Ganancia: {:?} + {:?} (Subir) / {:?} (Bajar)", config.hotkey_modifier, config.hotkey_increase_gain, config.hotkey_decrease_gain);

    // Pipeline robusto
    let mic_clone = hot_mic.clone();
    let cfg_final = config.clone();
    
    tokio::select! {
        _ = signal::ctrl_c() => {
            println!("\n👋 Saliendo y limpiando...");
        }
        _ = pipeline::robust::run(cmd_rx, mic_clone, cfg_final) => {
            println!("\n🛑 Pipeline detenido.");
        }
    }
}

fn cleanup_old_processes() {
    let current_pid = std::process::id();
    
    // 1. Matar procesos parec huérfanos
    let _ = Command::new("pkill").arg("-f").arg("parec --format=s16le").spawn();

    // 2. Matar otras instancias de transcriptor (excepto nosotros mismos)
    // Usamos pgrep para encontrar pids y los filtramos en el shell o aquí
    if let Ok(output) = Command::new("pgrep").arg("-f").arg("target/debug/transcriptor").output() {
        let pids = String::from_utf8_lossy(&output.stdout);
        for pid_str in pids.lines() {
            if let Ok(pid) = pid_str.trim().parse::<u32>() {
                if pid != current_pid {
                    let _ = Command::new("kill").arg("-9").arg(pid.to_string()).spawn();
                }
            }
        }
    }
}

fn start_volume_monitor(mic: Arc<audio::capture::HotMic>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
        loop {
            interval.tick().await;
            let vol = mic.get_volume();
            let mult = mic.get_multiplier();
            
            // Escalar de 0-32767 a 20 niveles
            let bars = (vol as f32 / 32768.0 * 20.0).ceil() as usize;
            let bars_safe = bars.min(20);
            let bar_fill = "█".repeat(bars_safe);
            let bar_empty = "░".repeat(20 - bars_safe);

            // Código ANSI: 
            // \x1b[s  - Guardar posición cursor
            // \x1b[1;1H - Ir a línea 1, columna 1
            // \x1b[K  - Borrar línea actual
            // \x1b[u  - Restaurar cursor
            print!("\x1b[s\x1b[1;1H\x1b[K\x1b[44;37m MONITOR \x1b[0m Vol: {:<5} (x{:.1}) [{}{}] \x1b[u", vol, mult, bar_fill, bar_empty);
            let _ = io::stdout().flush();
        }
    });
}
