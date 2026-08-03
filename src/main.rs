mod api;
mod audio;
mod config;
mod input;
mod observability;
mod output;
mod overlay;
mod pipeline;
mod transcription;

use config::AppConfig;
use input::{listener, state_machine};
use pipeline::replacer::Replacer;
use pipeline::robust::PipelineContext;
use std::fs::OpenOptions;
use std::io::{self, IsTerminal, Write};
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
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

    let dotenv_path = config::load_dotenv();
    let config = Arc::new(AppConfig::load_from_file("config.toml"));
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
    if let Some(path) = dotenv_path.as_deref() {
        tracing::info!(dotenv_path = %path.display(), "Variables de entorno cargadas");
    }
    for warning in &config.startup_warnings {
        tracing::warn!(code="TRN-CONFIG-PARSE", detail=%warning, "Diagnóstico de configuración");
    }
    tracing::info!(replacements=config.dictionary.len(), enabled=config.dictionary_enabled, "Diccionario configurado");

    // This is a background application controlled by global evdev shortcuts.
    // A terminal must not be able to terminate it when the physical shortcut
    // happens to map to the terminal's QUIT character for the active layout.
    ignore_terminal_quit();

    // Una sola instancia puede tener el micrófono y el teclado virtual.
    let _instance_lock = match acquire_instance_lock() {
        Ok(lock) => lock,
        Err(error) => {
            tracing::error!(code = "TRN-INSTANCE-LOCK", detail = %error, "No se puede arrancar una segunda instancia");
            eprintln!("❌ {error}");
            return;
        }
    };
    reap_orphans();

    let (overlay_hub, mut overlay_handle) = overlay::start().await;
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "Transcriptor iniciado");

    // Inicializar teclado uinput
    if let Err(error) = output::typer::init() {
        tracing::error!(code="TRN-OUTPUT-UINPUT", error=%error, "No se pudo inicializar el teclado virtual");
    }

    // Iniciar captura de audio
    let hot_mic = match audio::capture::HotMic::start(
        config.audio_multiplier,
        config.capture_device.as_deref(),
        &config.gate,
    )
    .await
    {
        Ok(mic) => {
            let m = Arc::new(mic);
            start_volume_monitor(m.clone());
            tracing::info!(
                device = config.capture_device.as_deref().unwrap_or("(fuente por defecto)"),
                gate = config.gate.enabled,
                gate_open_dbfs = config.gate.open_threshold_dbfs,
                gate_close_dbfs = config.gate.close_threshold_dbfs,
                "Audio listo"
            );
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

    // Un único cliente HTTP para todo el proceso: reconstruirlo por dictado
    // tiraba el pool de conexiones de reqwest y pagaba DNS + TCP + TLS de nuevo.
    let client = match api::groq::GroqClient::new(config.groq_connect_timeout, config.groq_request_timeout) {
        Ok(client) => Arc::new(client),
        Err(error) => {
            tracing::error!(code=error.code(), error=%error, "No se pudo crear el cliente de transcripción");
            if let Some(handle) = overlay_handle.take() {
                handle.shutdown();
            }
            return;
        }
    };

    // El diccionario se compila una vez. Si está mal formado se sigue sin
    // reemplazos, que es mejor que fallar cada entrega.
    let replacer = match Replacer::build(&config.dictionary) {
        Ok(replacer) => Arc::new(replacer),
        Err(error) => {
            tracing::error!(code="TRN-DICTIONARY-BUILD", error=%error, "No se pudo compilar el diccionario; se entregará sin reemplazos");
            Arc::new(None)
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
    tracing::info!("ESC cancela el dictado en curso sin transcribir");

    // Si la captura cae —un reinicio de PipeWire, por ejemplo— la aplicación
    // seguiría viva y sorda: overlay respondiendo, atajos andando, y nada
    // grabándose. El aviso lo hace visible mientras dura.
    watch_capture_health(hot_mic.health(), overlay_hub.clone());

    // Pipeline robusto
    let context = PipelineContext::new(config.clone(), client, replacer);

    tokio::select! {
        _ = signal::ctrl_c() => {
            tracing::info!("Señal de salida recibida; limpiando");
        }
        _ = pipeline::robust::run(cmd_rx, hot_mic.clone(), context, overlay_hub) => {
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

/// Candado de instancia única. Mientras el proceso viva mantiene el archivo
/// abierto; el kernel suelta el `flock` solo, incluso si la app muere de golpe,
/// así que no quedan candados rancios que haya que limpiar a mano.
struct InstanceLock {
    _file: std::fs::File,
}

fn instance_lock_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("transcriptor.lock")
}

/// Reemplaza la deduplicación por `pgrep`, que buscaba `target/debug` y por eso
/// nunca encontró nada cuando el binario en uso era el de release.
fn acquire_instance_lock() -> Result<InstanceLock, String> {
    let path = instance_lock_path();
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| format!("no se pudo abrir el candado {}: {error}", path.display()))?;

    // SAFETY: `file` sigue vivo y su descriptor es válido durante la llamada.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let holder = std::fs::read_to_string(&path)
            .ok()
            .map(|contents| contents.trim().to_string())
            .filter(|contents| !contents.is_empty())
            .unwrap_or_else(|| "desconocido".to_string());
        return Err(format!(
            "ya hay otra instancia de transcriptor corriendo (pid {holder}); cerrala antes de abrir otra"
        ));
    }

    // El pid es sólo diagnóstico: el candado real es el flock.
    let _ = file.set_len(0);
    let _ = (&file).write_all(format!("{}\n", std::process::id()).as_bytes());

    tracing::debug!(lock_path = %path.display(), "Candado de instancia tomado");
    Ok(InstanceLock { _file: file })
}

/// Mata procesos hijos huérfanos propios. Sólo se llama con el candado ya
/// tomado, o sea que no hay otra instancia viva y todo lo que coincida con
/// nuestras firmas exactas quedó colgado de un arranque anterior.
///
/// Ambos hijos se lanzan en su propio grupo de procesos para que las señales de
/// la terminal no los alcance, así que sobreviven si el proceso principal muere
/// sin pasar por su apagado ordenado —cerrar la ventana en vez de Ctrl+C, por
/// ejemplo—.
///
/// La versión anterior mataba por `parec --format=s16le`, que también coincide
/// con capturas de otras aplicaciones, y no contemplaba el overlay.
fn reap_orphans() {
    // Un overlay huérfano no es sólo un proceso de más: GTK usa D-Bus para
    // instancia única, así que retiene `local.transcriptor.Overlay` y hace que
    // el overlay de este arranque salga al instante, dejándonos sin indicador.
    const ORPHANS: [(&str, &str, &str); 2] = [
        (
            // El `--device` es opcional y va primero: sin contemplarlo, este
            // patrón dejó de coincidir en cuanto se pudo elegir la fuente, y
            // las capturas huérfanas dejaron de limpiarse en silencio.
            "^parec( --device=[^ ]+)? --format=s16le --rate=16000 --channels=1 --latency-msec=30$",
            "TRN-CLEANUP-PAREC",
            "Se limpió una captura de audio huérfana",
        ),
        (
            "^[^ ]*transcriptor --overlay ",
            "TRN-CLEANUP-OVERLAY",
            "Se limpió un overlay huérfano que retenía el nombre de GApplication",
        ),
    ];

    for (signature, code, message) in ORPHANS {
        match Command::new("pkill").arg("-f").arg(signature).status() {
            Ok(status) if status.success() => tracing::info!(code, "{}", message),
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(code, error=%error, "No se pudo ejecutar la limpieza de procesos huérfanos");
            }
        }
    }
}

fn watch_capture_health(
    mut health: tokio::sync::watch::Receiver<audio::capture::CaptureHealth>,
    overlay: overlay::OverlayHub,
) {
    use audio::capture::CaptureHealth;

    tokio::spawn(async move {
        while health.changed().await.is_ok() {
            let state = *health.borrow_and_update();
            match state {
                CaptureHealth::Down => {
                    // Persistente a propósito: mientras no haya micrófono, que
                    // se vea. No es un error de un dictado, es la aplicación
                    // incapaz de grabar.
                    overlay.publish(
                        overlay::AppPhase::Error,
                        None,
                        Some("Micrófono caído · reintentando".into()),
                    );
                    eprintln!("⚠️  Se perdió la captura de audio; reintentando…");
                }
                CaptureHealth::Running => {
                    overlay.publish(overlay::AppPhase::Idle, None, None);
                    eprintln!("✅ Captura de audio restablecida.");
                }
            }
        }
    });
}

fn start_volume_monitor(mic: Arc<audio::capture::HotMic>) {
    // El monitor repinta la primera línea de la terminal con secuencias ANSI.
    // Sin TTY —redirigido a un archivo, o bajo un supervisor— sólo ensuciaría
    // la salida y competiría con `tracing` por el mismo descriptor.
    if !io::stdout().is_terminal() {
        tracing::debug!("stdout no es una terminal; monitor de volumen desactivado");
        return;
    }

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
