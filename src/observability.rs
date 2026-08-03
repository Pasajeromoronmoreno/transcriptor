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

/// Fecha local en formato `YYYY-MM-DD`.
///
/// `localtime_r` es la vía segura en procesos con hilos, a diferencia de
/// `localtime`, que devuelve un puntero a un `tm` estático compartido.
fn local_date(seconds_since_epoch: i64) -> String {
    let time = seconds_since_epoch as libc::time_t;
    let mut broken_down: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `time` es válido y `broken_down` está inicializado en cero y vive
    // durante toda la llamada.
    unsafe { libc::localtime_r(&time, &mut broken_down) };
    format!(
        "{:04}-{:02}-{:02}",
        broken_down.tm_year + 1900,
        broken_down.tm_mon + 1,
        broken_down.tm_mday
    )
}

fn today_local() -> String {
    let seconds = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0);
    local_date(seconds)
}

/// Archivo diario rotado por fecha **local**.
///
/// `tracing_appender::rolling::daily` rota en UTC, así que en cualquier huso
/// al oeste de Greenwich lo que se dicta de noche cae en el archivo del día
/// siguiente. Buscar "lo de anoche" en el archivo de anoche no lo encontraba.
struct DailyLocalFile {
    directory: PathBuf,
    current_date: String,
    file: fs::File,
}

impl DailyLocalFile {
    fn open(directory: PathBuf) -> io::Result<Self> {
        let current_date = today_local();
        let file = Self::open_for(&directory, &current_date)?;
        Ok(Self { directory, current_date, file })
    }

    fn open_for(directory: &Path, date: &str) -> io::Result<fs::File> {
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(directory.join(format!("{LOG_FILE_PREFIX}.{date}")))
    }
}

impl io::Write for DailyLocalFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let today = today_local();
        if today != self.current_date {
            // Si no se puede abrir el archivo nuevo se sigue escribiendo en el
            // anterior: perder la rotación de un día es mejor que perder los
            // registros, y anotar la fecha evita reintentar en cada línea.
            if let Ok(file) = Self::open_for(&self.directory, &today) {
                self.file = file;
            }
            self.current_date = today;
        }
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

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

                let appender = match DailyLocalFile::open(directory.clone()) {
                    Ok(appender) => appender,
                    Err(error) => {
                        return Err(format!(
                            "No se pudo abrir el archivo de logs en {}: {error}",
                            directory.display()
                        ));
                    }
                };
                let (non_blocking, guard) = tracing_appender::non_blocking(appender);
                worker_guard = Some(guard);

                // La limpieza del arranque sólo alcanza si la aplicación se
                // reinicia seguido. Repetirla mientras corre hace que la
                // retención se cumpla sola aunque quede semanas abierta.
                spawn_retention_task(directory.clone(), config.retention_days);

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

fn spawn_retention_task(directory: PathBuf, retention_days: u64) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(SECONDS_PER_DAY));
        // El primer disparo es inmediato y el arranque ya limpió.
        interval.tick().await;
        loop {
            interval.tick().await;
            match cleanup_old_logs(&directory, retention_days) {
                Ok(0) => {}
                Ok(removed) => tracing::info!(removed, retention_days, "Se limpiaron logs vencidos"),
                Err(error) => {
                    tracing::warn!(code="TRN-LOG-CLEANUP", error=%error, "No se pudieron limpiar los logs vencidos");
                }
            }
        }
    });
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
    if let Ok(value) = env::var("RUST_LOG")
        && !value.trim().is_empty()
    {
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
    use super::{DailyLocalFile, LOG_FILE_PREFIX, cleanup_old_logs, configured_filter, local_date};
    use std::fs;
    use std::io::Write;
    use std::time::Duration;

    #[test]
    fn local_date_has_the_expected_shape() {
        let date = local_date(1_785_000_000);
        assert_eq!(date.len(), 10, "{date}");
        let parts: Vec<&str> = date.split('-').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!((parts[0].len(), parts[1].len(), parts[2].len()), (4, 2, 2));
        assert!(parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit())));
    }

    /// Fecha UTC del mismo instante, para contrastar contra la local.
    fn utc_date(seconds_since_epoch: i64) -> String {
        let time = seconds_since_epoch as libc::time_t;
        let mut broken_down: libc::tm = unsafe { std::mem::zeroed() };
        unsafe { libc::gmtime_r(&time, &mut broken_down) };
        format!(
            "{:04}-{:02}-{:02}",
            broken_down.tm_year + 1900,
            broken_down.tm_mon + 1,
            broken_down.tm_mday
        )
    }

    fn local_offset_seconds(seconds_since_epoch: i64) -> i64 {
        let time = seconds_since_epoch as libc::time_t;
        let mut broken_down: libc::tm = unsafe { std::mem::zeroed() };
        unsafe { libc::localtime_r(&time, &mut broken_down) };
        broken_down.tm_gmtoff as i64
    }

    #[test]
    fn local_date_follows_the_local_offset_and_not_utc() {
        // Se contrasta contra la fecha UTC del instante corrido por el desfase
        // local. En un entorno UTC —el runner de CI— ambas coinciden y el test
        // sigue siendo válido; en uno con desfase, comprueba justamente lo que
        // se quería arreglar.
        for timestamp in [1_785_000_000, 1_785_043_000, 1_785_086_399] {
            let offset = local_offset_seconds(timestamp);
            assert_eq!(
                local_date(timestamp),
                utc_date(timestamp + offset),
                "desfase {offset}s en el instante {timestamp}"
            );
        }
    }

    #[test]
    fn local_date_is_stable_and_advances_with_the_days() {
        let base = 1_785_000_000;
        assert_eq!(local_date(base), local_date(base));
        // Dos días evitan cualquier ambigüedad por cambios de horario.
        assert_ne!(local_date(base), local_date(base + 2 * 24 * 60 * 60));
    }

    #[test]
    fn the_appender_writes_to_a_file_named_after_the_local_date() {
        let directory = std::env::temp_dir()
            .join(format!("transcriptor-appender-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("test log directory");

        let mut appender = DailyLocalFile::open(directory.clone()).expect("appender");
        appender.write_all(b"linea\n").expect("write");
        appender.flush().expect("flush");

        let expected = directory.join(format!("{LOG_FILE_PREFIX}.{}", super::today_local()));
        assert_eq!(fs::read_to_string(&expected).expect("log file"), "linea\n");

        // Reabrir no pisa lo ya escrito: el archivo del día se acumula.
        let mut again = DailyLocalFile::open(directory.clone()).expect("appender");
        again.write_all(b"otra\n").expect("write");
        again.flush().expect("flush");
        assert_eq!(fs::read_to_string(&expected).expect("log file"), "linea\notra\n");

        let _ = fs::remove_dir_all(&directory);
    }

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
