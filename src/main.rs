mod api;
mod audio;
mod config;
mod input;
mod observability;
mod output;
mod overlay;
mod pipeline;

use config::AppConfig;
use input::{listener, state_machine};
use std::io::{self, Write};
use std::process::Command;
use std::sync::Arc;
use tokio::signal;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() {
    // El proceso GTK auxiliar no abre su propio logger: el proceso principal
    // es el dueño exclusivo del archivo persistente.
    if let Some(socket_path) = overlay_socket_arg() {
        #[cfg(feature = "overlay")]
        overlay::run_ui(&socket_path);

        #[cfg(not(feature = "overlay"))]
        let _ = socket_path;
        #[cfg(not(feature = "overlay"))]
        eprintln!("Overlay desactivado: compilá con `--features overlay`.");
        return;
    }

    let _ = dotenvy::dotenv();
    let config = AppConfig::load_from_file("config.toml");
    let observability = match observability::init(&config.logging) {
        Ok(observability) => observability,
        Err(error) => {
            eprintln!("No se pudo inicializar observabilidad: {error}");
            return;
        }
    };

    if let Some(path) = observability.effective_log_path() {
        tracing::info!(log_path = %path.display(), "Logs persistentes disponibles");
    } else {
        tracing::info!("No hay archivo persistente de logs configurado");
    }

    if let Some(path) = config.config_path.as_deref() {
        tracing::info!(config_path = %path.display(), "Configuración cargada");
    } else {
        tracing::warn!("No se encontró archivo de configuración; se usan valores por defecto");
    }
    for warning in &config.startup_warnings {
        tracing::warn!(code="TRN-CONFIG-PARSE", detail=%warning, "Diagnóstico de configuración");
    }
    tracing::info!(replacements=config.dictionary.len(), enabled=config.dictionary_enabled, "Diccionario configurado");

    // This is a background application controlled by global evdev shortcuts.
    // A terminal must not be able to terminate it when the physical shortcut
    // happens to map to the terminal's QUIT character for the active layout.
    ignore_terminal_quit();

    // Limpia instancias previas y procesos huérfanos para un inicio limpio.
    cleanup_old_processes();
    let (overlay_hub, mut overlay_handle) = overlay::start().await;
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "Transcriptor iniciado");

    // Proactive cleanup: kill any other instances or parec orphans
    // (This is a safety measure against zombies reported by user)

    // Inicializar teclado uinput
    output::typer::init();

    // Iniciar captura de audio
    let hot_mic = match audio::capture::HotMic::start(config.audio_multiplier).await {
        Ok(mic) => {
            let m = Arc::new(mic);
            start_volume_monitor(m.clone());
            tracing::info!("Audio listo");
            m
        }
        Err(e) => {
            tracing::error!(error = %e, "Error iniciando audio");
            if let Some(handle) = overlay_handle.take() {
                handle.shutdown();
            }
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

    if config.groq_api_key.is_empty() {
        tracing::warn!("No hay API key configurada; define GROQ_API_KEY o config.toml");
    }

    let mut out_info = Vec::new();
    if config.copy_to_clipboard {
        out_info.push("📋 clipboard");
    }
    if config.paste_to_input {
        out_info.push("⌨️ hardware_type");
    }
    tracing::info!(
        modifier = ?config.hotkey_modifier,
        trigger = ?config.hotkey_trigger,
        output = %out_info.join(" + "),
        "Atajo de grabación listo"
    );
    tracing::info!(
        modifier = ?config.hotkey_modifier,
        increase = ?config.hotkey_increase_gain,
        decrease = ?config.hotkey_decrease_gain,
        "Atajos de ganancia configurados"
    );

    // Pipeline robusto
    let mic_clone = hot_mic.clone();
    let cfg_final = config.clone();

    tokio::select! {
        _ = signal::ctrl_c() => {
            tracing::info!("Señal de salida recibida; limpiando");
        }
        _ = pipeline::robust::run(cmd_rx, mic_clone, cfg_final, overlay_hub) => {
            tracing::info!("Pipeline detenido");
        }
    }

    if let Some(handle) = overlay_handle {
        handle.shutdown();
    }
}

#[cfg(unix)]
fn ignore_terminal_quit() {
    tokio::spawn(async {
        let Ok(mut quit) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::quit())
        else {
            return;
        };
        while quit.recv().await.is_some() {}
    });
}

#[cfg(not(unix))]
fn ignore_terminal_quit() {}

fn overlay_socket_arg() -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--overlay" {
            return args.next();
        }
    }
    None
}

fn cleanup_old_processes() {
    let current_pid = std::process::id();

    // 1. Matar procesos parec huérfanos
    if let Err(error) = Command::new("pkill")
        .arg("-f")
        .arg("parec --format=s16le")
        .spawn() {
        tracing::warn!(code="TRN-CLEANUP-PAREC", error=%error, "No se pudo ejecutar limpieza de parec");
    }

    // 2. Matar otras instancias de transcriptor (excepto nosotros mismos)
    // Usamos pgrep para encontrar pids y los filtramos en el shell o aquí
    if let Ok(output) = Command::new("pgrep")
        .arg("-f")
        .arg("target/debug/transcriptor")
        .output()
    {
        let pids = String::from_utf8_lossy(&output.stdout);
        for pid_str in pids.lines() {
            if let Ok(pid) = pid_str.trim().parse::<u32>() {
                if pid != current_pid {
                    if let Err(error) = Command::new("kill").arg("-9").arg(pid.to_string()).spawn() {
                        tracing::warn!(code="TRN-CLEANUP-PROCESS", pid, error=%error, "No se pudo limpiar una instancia anterior");
                    }
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
            print!(
                "\x1b[s\x1b[1;1H\x1b[K\x1b[44;37m MONITOR \x1b[0m Vol: {:<5} (x{:.1}) [{}{}] \x1b[u",
                vol, mult, bar_fill, bar_empty
            );
            let _ = io::stdout().flush();
        }
    });
}
