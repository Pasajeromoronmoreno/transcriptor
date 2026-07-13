use crate::config::LoggingConfig;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use std::error::Error;
use tracing::Subscriber;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::{Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

const LOG_FILE_PREFIX: &str = "transcriptor.log";
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

pub fn error_chain(error: &dyn Error) -> String {
    let mut messages = vec![error.to_string()];
    let mut source = error.source();
    while let Some(cause) = source {
        messages.push(cause.to_string());
        source = cause.source();
    }
    messages.join(" -> ")
}

/// Owns the non-blocking logging worker for the lifetime of the application.
pub struct Observability {
    log_directory: Option<PathBuf>,
    _worker_guard: Option<WorkerGuard>,
}

impl Observability {
    /// Returns the directory containing the daily log files, if file logging is enabled.
    pub fn effective_log_path(&self) -> Option<&Path> {
        self.log_directory.as_deref()
    }
}

/// Initializes the console subscriber and, when enabled, the persistent daily file sink.
pub fn init(config: &LoggingConfig) -> Result<Observability, String> {
    let (filter, filter_warning) = build_filter(&config.level);
    let mut startup_warnings = Vec::new();
    if let Some(warning) = filter_warning {
        startup_warnings.push(warning);
    }

    let mut log_directory = None;
    let mut worker_guard = None;

    if config.file {
        match prepare_log_directory(config.retention_days) {
            Ok((directory, warnings)) => {
                log_directory = Some(directory.clone());
                startup_warnings.extend(warnings);

                let appender = tracing_appender::rolling::daily(&directory, LOG_FILE_PREFIX);
                let (non_blocking, guard) = tracing_appender::non_blocking(appender);
                worker_guard = Some(guard);

                let console_layer = console_layer();
                let file_layer = fmt::layer()
                    .json()
                    .with_ansi(false)
                    .with_target(true)
                    .with_current_span(true)
                    .with_span_list(true)
                    .with_writer(non_blocking);

                if let Err(error) = tracing_subscriber::registry()
                    .with(filter)
                    .with(console_layer)
                    .with(file_layer)
                    .try_init()
                {
                    return Err(error.to_string());
                }
            }
            Err(error) => {
                startup_warnings.push(format!(
                    "No se pudo crear ningún directorio de logs; se usa sólo consola: {error}"
                ));
                if let Err(error) = tracing_subscriber::registry()
                    .with(filter)
                    .with(console_layer())
                    .try_init()
                {
                    return Err(error.to_string());
                }
            }
        }
    } else if let Err(error) = tracing_subscriber::registry()
        .with(filter)
        .with(console_layer())
        .try_init()
    {
        return Err(error.to_string());
    }

    for warning in startup_warnings {
        tracing::warn!(message = %warning, "Configuración de observabilidad");
    }
    Ok(Observability {
        log_directory,
        _worker_guard: worker_guard,
    })
}

fn console_layer<S>() -> impl Layer<S>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    fmt::layer()
        .compact()
        .with_target(false)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
}

fn build_filter(level: &str) -> (EnvFilter, Option<String>) {
    if let Ok(value) = env::var("RUST_LOG") {
        if !value.trim().is_empty() {
            match EnvFilter::try_new(value.clone()) {
                Ok(filter) => return (filter, None),
                Err(_) => {
                    return (
                        configured_filter(level),
                        Some("RUST_LOG inválido; se usa logging.level".to_string()),
                    );
                }
            }
        }
    }
    (configured_filter(level), None)
}

fn configured_filter(level: &str) -> EnvFilter {
    EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"))
}

fn prepare_log_directory(retention_days: u64) -> Result<(PathBuf, Vec<String>), String> {
    let mut candidates = Vec::new();
    if let Some(state_home) = non_empty_env_path("XDG_STATE_HOME") {
        candidates.push((state_home.join("transcriptor/logs"), "XDG_STATE_HOME"));
    }
    if let Some(home) = non_empty_env_path("HOME") {
        candidates.push((home.join(".local/state/transcriptor/logs"), "HOME"));
    }
    candidates.push((env::temp_dir().join("transcriptor/logs"), "TMPDIR"));

    let mut errors = Vec::new();
    for (directory, source) in candidates {
        match fs::create_dir_all(&directory) {
            Ok(()) => {
                let mut warnings = Vec::new();
                if !errors.is_empty() {
                    warnings.push(format!(
                        "Se usa {source} para logs tras fallos en rutas anteriores: {}",
                        errors.join("; ")
                    ));
                }
                if let Err(error) = cleanup_old_logs(&directory, retention_days) {
                    warnings.push(format!(
                        "No se pudo limpiar logs antiguos en {}: {error}",
                        directory.display()
                    ));
                }
                return Ok((directory, warnings));
            }
            Err(error) => errors.push(format!("{} ({error})", directory.display())),
        }
    }

    Err(errors.join("; "))
}

fn non_empty_env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn cleanup_old_logs(directory: &Path, retention_days: u64) -> io::Result<usize> {
    let now = SystemTime::now();
    let max_age = Duration::from_secs(retention_days.saturating_mul(SECONDS_PER_DAY));
    let mut removed = 0;

    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let file_name = entry.file_name();
        let is_log = file_type.is_file()
            && file_name
                .to_str()
                .is_some_and(|name| name.starts_with(LOG_FILE_PREFIX));
        if !is_log {
            continue;
        }

        let modified = entry.metadata()?.modified()?;
        let age = now.duration_since(modified).unwrap_or_default();
        if age > max_age {
            fs::remove_file(entry.path())?;
            removed += 1;
        }
    }

    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::{LOG_FILE_PREFIX, cleanup_old_logs, configured_filter};
    use std::fs;
    use std::time::Duration;

    #[test]
    fn configured_filter_falls_back_to_info() {
        let filter = configured_filter("level=definitely-not-a-level");
        assert!(filter.to_string().contains("info"));
    }

    #[test]
    fn cleanup_keeps_recent_logs_and_ignores_other_files() {
        let directory = std::env::temp_dir().join(format!(
            "transcriptor-observability-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("test log directory");
        fs::write(
            directory.join(format!("{LOG_FILE_PREFIX}.today")),
            b"recent",
        )
        .expect("recent log");
        fs::write(directory.join("keep.txt"), b"keep").expect("unrelated file");

        cleanup_old_logs(&directory, 7).expect("cleanup");

        assert!(directory.join(format!("{LOG_FILE_PREFIX}.today")).exists());
        assert!(directory.join("keep.txt").exists());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn retention_duration_is_day_based() {
        assert_eq!(Duration::from_secs(7 * 24 * 60 * 60).as_secs(), 604_800);
    }

    #[test]
    fn zero_retention_removes_matching_old_file_only() {
        let directory = std::env::temp_dir().join(format!("transcriptor-retention-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory); fs::create_dir_all(&directory).unwrap();
        let log = directory.join(format!("{LOG_FILE_PREFIX}.old"));
        fs::write(&log, b"old").unwrap();
        std::thread::sleep(Duration::from_millis(2));
        assert_eq!(cleanup_old_logs(&directory, 0).unwrap(), 1);
        assert!(!log.exists());
        let _ = fs::remove_dir_all(directory);
    }
}
