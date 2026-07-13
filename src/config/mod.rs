use evdev::KeyCode;
use serde::Deserialize;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Deserialize, Debug)]
struct TomlConfig {
    input: Option<InputToml>,
    groq: Option<GroqToml>,
    output: Option<OutputToml>,
    pipeline: Option<PipelineToml>,
    hotkeys: Option<HotkeysToml>,
    logging: Option<LoggingToml>,
    retry: Option<RetryToml>,
}

#[derive(Deserialize, Debug)]
struct InputToml {
    push_start_threshold_ms: Option<u64>,
    tap_timeout_ms: Option<u64>,
    push_release_window_ms: Option<u64>,
    audio_multiplier: Option<f32>,
}

#[derive(Deserialize, Debug)]
struct GroqToml {
    api_key: Option<String>,
    language: Option<String>,
    whisper_prompt: Option<String>,
    experimental_live: Option<bool>,
    connect_timeout_ms: Option<u64>,
    request_timeout_ms: Option<u64>,
}

#[derive(Deserialize, Debug)]
struct RetryToml {
    max_attempts: Option<u32>,
    initial_delay_ms: Option<u64>,
    max_delay_ms: Option<u64>,
    jitter_ms: Option<u64>,
}

#[derive(Deserialize, Debug)]
struct OutputToml {
    copy_to_clipboard: Option<bool>,
    paste_to_input: Option<bool>,
    auto_enter: Option<bool>,
    auto_enter_delay_ms: Option<u64>,
    add_period: Option<bool>,
    export_audio: Option<bool>,
}

#[derive(Deserialize, Debug)]
struct PipelineToml {
    mode: Option<String>,
    chunk_window_ms: Option<u64>,
    profile_latency: Option<bool>,
}

#[derive(Deserialize, Debug)]
struct HotkeysToml {
    modifier: Option<String>,
    trigger: Option<String>,
    toggle_format: Option<String>,
    send_with_enter: Option<String>,
    increase_gain: Option<String>,
    decrease_gain: Option<String>,
}

#[derive(Deserialize, Debug)]
struct LoggingToml {
    level: Option<String>,
    file: Option<bool>,
    retention_days: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoggingConfig {
    pub level: String,
    pub file: bool,
    pub retention_days: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RetryConfig {
    pub max_attempts: u32,
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub jitter: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self { max_attempts: 3, initial_delay: Duration::from_millis(1000), max_delay: Duration::from_millis(5000), jitter: Duration::from_millis(250) }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            file: true,
            retention_days: 7,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PipelineMode {
    Robust,
    Realtime,
}

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub push_start_threshold: Duration,
    pub tap_timeout: Duration,
    pub push_release_window: Duration,
    pub groq_api_key: String,
    pub groq_language: String,
    pub whisper_prompt: String,
    pub experimental_live: bool,
    pub copy_to_clipboard: bool,
    pub paste_to_input: bool,
    pub auto_enter: bool,
    pub auto_enter_delay: Duration,
    pub add_period: bool,
    pub export_audio: bool,
    pub pipeline_mode: PipelineMode,
    pub chunk_window: Duration,
    pub profile_latency: bool,
    pub hotkey_modifier: KeyCode,
    pub hotkey_trigger: KeyCode,
    pub hotkey_toggle_format: KeyCode,
    pub hotkey_send_with_enter: KeyCode,
    pub hotkey_increase_gain: KeyCode,
    pub hotkey_decrease_gain: KeyCode,
    pub audio_multiplier: f32,
    pub dictionary: Vec<(String, String)>,
    pub dictionary_enabled: bool,
    pub config_path: Option<PathBuf>,
    pub logging: LoggingConfig,
    pub retry: RetryConfig,
    pub groq_connect_timeout: Duration,
    pub groq_request_timeout: Duration,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            push_start_threshold: Duration::from_millis(100),
            tap_timeout: Duration::from_millis(250),
            push_release_window: Duration::from_millis(150),
            groq_api_key: String::new(),
            groq_language: "es".to_string(),
            whisper_prompt: String::new(),
            experimental_live: false,
            copy_to_clipboard: true,
            paste_to_input: true,
            auto_enter: true,
            auto_enter_delay: Duration::from_millis(30),
            add_period: true,
            export_audio: false,
            pipeline_mode: PipelineMode::Robust,
            chunk_window: Duration::from_millis(800),
            profile_latency: false,
            hotkey_modifier: KeyCode::KEY_LEFTCTRL,
            hotkey_trigger: KeyCode::KEY_GRAVE,
            hotkey_toggle_format: KeyCode::KEY_LEFTALT,
            hotkey_send_with_enter: KeyCode::KEY_SPACE,
            hotkey_increase_gain: KeyCode::KEY_UP,
            hotkey_decrease_gain: KeyCode::KEY_DOWN,
            audio_multiplier: 1.0,
            dictionary: Vec::new(),
            dictionary_enabled: true,
            config_path: None,
            logging: LoggingConfig::default(),
            retry: RetryConfig::default(),
            groq_connect_timeout: Duration::from_millis(5000),
            groq_request_timeout: Duration::from_millis(30000),
        }
    }
}

impl AppConfig {
    pub fn load_from_file(filename: &str) -> Self {
        let mut config = Self::default();

        let binary_dir = env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()));
        let xdg_config = env::var("XDG_CONFIG_HOME")
            .ok()
            .or_else(|| env::var("HOME").ok().map(|h| format!("{}/.config", h)))
            .map(|d| PathBuf::from(d).join("transcriptor"));
        let cwd = env::current_dir().ok();

        let search_locations = [
            binary_dir.as_ref().map(|p| p.join(filename)),
            xdg_config.as_ref().map(|p| p.join(filename)),
            cwd.as_ref().map(|p| p.join(filename)),
        ];

        let mut loaded_path: Option<PathBuf> = None;

        for opt_path in &search_locations {
            if let Some(path) = opt_path {
                if let Ok(contents) = fs::read_to_string(path) {
                    println!("🔍 Configuración cargada desde: {:?}", path);
                    loaded_path = Some(path.clone());

                    Self::parse_toml(&contents, &mut config);
                    break;
                }
            }
        }

        if loaded_path.is_none() {
            eprintln!("⚠️ No se encontró archivo de configuración. Usando valores por defecto.");
        }

        config.config_path = loaded_path;

        if let Ok(api_key) = env::var("GROQ_API_KEY") {
            if !api_key.trim().is_empty() {
                config.groq_api_key = api_key;
            }
        }

        config
    }

    fn parse_toml(contents: &str, config: &mut AppConfig) {
        match toml::from_str::<TomlConfig>(contents) {
            Ok(t) => {
                if let Some(input) = t.input {
                    if let Some(v) = input.push_start_threshold_ms {
                        config.push_start_threshold = Duration::from_millis(v);
                    }
                    if let Some(v) = input.tap_timeout_ms {
                        config.tap_timeout = Duration::from_millis(v);
                    }
                    if let Some(v) = input.push_release_window_ms {
                        config.push_release_window = Duration::from_millis(v);
                    }
                    if let Some(v) = input.audio_multiplier {
                        config.audio_multiplier = v;
                    }
                }
                if let Some(groq) = t.groq {
                    if let Some(v) = groq.api_key {
                        config.groq_api_key = v;
                    }
                    if let Some(v) = groq.language {
                        config.groq_language = v;
                    }
                    if let Some(v) = groq.whisper_prompt {
                        config.whisper_prompt = v;
                    }
                    if let Some(v) = groq.experimental_live {
                        config.experimental_live = v;
                    }
                    if let Some(v) = groq.connect_timeout_ms { config.groq_connect_timeout = Duration::from_millis(v); }
                    if let Some(v) = groq.request_timeout_ms { config.groq_request_timeout = Duration::from_millis(v); }
                }
                if let Some(out) = t.output {
                    if let Some(v) = out.copy_to_clipboard {
                        config.copy_to_clipboard = v;
                    }
                    if let Some(v) = out.paste_to_input {
                        config.paste_to_input = v;
                    }
                    if let Some(v) = out.auto_enter {
                        config.auto_enter = v;
                    }
                    if let Some(v) = out.auto_enter_delay_ms {
                        config.auto_enter_delay = Duration::from_millis(v);
                    }
                    if let Some(v) = out.add_period {
                        config.add_period = v;
                    }
                    if let Some(v) = out.export_audio {
                        config.export_audio = v;
                    }
                }
                if let Some(pipe) = t.pipeline {
                    if let Some(v) = pipe.mode {
                        config.pipeline_mode = match v.as_str() {
                            "realtime" => PipelineMode::Realtime,
                            _ => PipelineMode::Robust,
                        };
                    }
                    if let Some(v) = pipe.chunk_window_ms {
                        config.chunk_window = Duration::from_millis(v);
                    }
                    if let Some(v) = pipe.profile_latency {
                        config.profile_latency = v;
                    }
                }
                if let Some(hot) = t.hotkeys {
                    if let Some(v) = hot.modifier {
                        if let Some(k) = parse_keycode(&v) {
                            config.hotkey_modifier = k;
                        }
                    }
                    if let Some(v) = hot.trigger {
                        if let Some(k) = parse_keycode(&v) {
                            config.hotkey_trigger = k;
                        }
                    }
                    if let Some(v) = hot.toggle_format {
                        if let Some(k) = parse_keycode(&v) {
                            config.hotkey_toggle_format = k;
                        }
                    }
                    if let Some(v) = hot.send_with_enter {
                        if let Some(k) = parse_keycode(&v) {
                            config.hotkey_send_with_enter = k;
                        }
                    }
                    if let Some(v) = hot.increase_gain {
                        if let Some(k) = parse_keycode(&v) {
                            config.hotkey_increase_gain = k;
                        }
                    }
                    if let Some(v) = hot.decrease_gain {
                        if let Some(k) = parse_keycode(&v) {
                            config.hotkey_decrease_gain = k;
                        }
                    }
                } else {
                    eprintln!("⚠️ No se encontró sección [hotkeys] en el archivo.");
                }
                if let Some(logging) = t.logging {
                    if let Some(v) = logging.level {
                        config.logging.level = v;
                    }
                    if let Some(v) = logging.file {
                        config.logging.file = v;
                    }
                    if let Some(v) = logging.retention_days {
                        config.logging.retention_days = v;
                    }
                }
                if let Some(retry) = t.retry {
                    if let Some(v) = retry.max_attempts { config.retry.max_attempts = v.max(1); }
                    if let Some(v) = retry.initial_delay_ms { config.retry.initial_delay = Duration::from_millis(v); }
                    if let Some(v) = retry.max_delay_ms { config.retry.max_delay = Duration::from_millis(v); }
                    if let Some(v) = retry.jitter_ms { config.retry.jitter = Duration::from_millis(v); }
                }

                // Cargar diccionario de reemplazos
                config.dictionary_enabled = true;
                if let Some(table) = toml::from_str::<toml::Value>(contents)
                    .ok()
                    .and_then(|v| v.get("dictionary").cloned())
                    .and_then(|v| v.as_table().cloned())
                {
                    if let Some(enabled) = table.get("enabled").and_then(|v| v.as_bool()) {
                        config.dictionary_enabled = enabled;
                    }

                    let mut dict: Vec<(String, String)> = table
                        .into_iter()
                        .filter_map(|(k, v)| {
                            if k == "enabled" {
                                return None;
                            }
                            v.as_str().map(|s| (k, s.to_string()))
                        })
                        .collect();
                    dict.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
                    if !dict.is_empty() {
                        println!("📖 Diccionario cargado: {} reemplazos (Habilitado: {})", dict.len(), config.dictionary_enabled);
                    }
                    config.dictionary = dict;
                }
            }
            Err(e) => {
                eprintln!("❌ Error parseando TOML: {e}");
            }
        }
    }

    pub fn save_audio_multiplier(
        config_path: Option<&std::path::Path>,
        gain: f32,
    ) -> Result<(), std::io::Error> {
        let full_path = match config_path {
            Some(p) => p.to_path_buf(),
            None => env::current_dir().unwrap_or_default().join("config.toml"),
        };

        let contents = fs::read_to_string(&full_path)?;
            let mut new_contents = String::new();
            let mut in_input = false;
            let mut replaced = false;

            for line in contents.lines() {
                if line.starts_with("[input]") {
                    in_input = true;
                    new_contents.push_str(line);
                    new_contents.push('\n');
                } else if line.starts_with('[') && in_input {
                    in_input = false;
                    new_contents.push_str(line);
                    new_contents.push('\n');
                } else if in_input && line.trim().starts_with("audio_multiplier") {
                    new_contents.push_str(&format!("audio_multiplier = {:.1}\n", gain));
                    replaced = true;
                } else {
                    new_contents.push_str(line);
                    new_contents.push('\n');
                }
            }
            if in_input && !replaced {
                // If EOF was reached while in [input]
                new_contents.push_str(&format!("audio_multiplier = {:.1}\n", gain));
            }
            fs::write(full_path, new_contents)?;
        Ok(())
    }
}

fn parse_keycode(name: &str) -> Option<KeyCode> {
    use evdev::KeyCode;
    match name.to_uppercase().as_str() {
        "KEY_Q" => Some(KeyCode::KEY_Q),
        "KEY_A" => Some(KeyCode::KEY_A),
        "KEY_B" => Some(KeyCode::KEY_B),
        "KEY_C" => Some(KeyCode::KEY_C),
        "KEY_D" => Some(KeyCode::KEY_D),
        "KEY_E" => Some(KeyCode::KEY_E),
        "KEY_F" => Some(KeyCode::KEY_F),
        "KEY_G" => Some(KeyCode::KEY_G),
        "KEY_H" => Some(KeyCode::KEY_H),
        "KEY_I" => Some(KeyCode::KEY_I),
        "KEY_J" => Some(KeyCode::KEY_J),
        "KEY_K" => Some(KeyCode::KEY_K),
        "KEY_L" => Some(KeyCode::KEY_L),
        "KEY_M" => Some(KeyCode::KEY_M),
        "KEY_N" => Some(KeyCode::KEY_N),
        "KEY_O" => Some(KeyCode::KEY_O),
        "KEY_P" => Some(KeyCode::KEY_P),
        "KEY_R" => Some(KeyCode::KEY_R),
        "KEY_S" => Some(KeyCode::KEY_S),
        "KEY_T" => Some(KeyCode::KEY_T),
        "KEY_U" => Some(KeyCode::KEY_U),
        "KEY_V" => Some(KeyCode::KEY_V),
        "KEY_W" => Some(KeyCode::KEY_W),
        "KEY_X" => Some(KeyCode::KEY_X),
        "KEY_Y" => Some(KeyCode::KEY_Y),
        "KEY_Z" => Some(KeyCode::KEY_Z),
        "KEY_SPACE" => Some(KeyCode::KEY_SPACE),
        "KEY_UP" => Some(KeyCode::KEY_UP),
        "KEY_DOWN" => Some(KeyCode::KEY_DOWN),
        "KEY_LEFTALT" => Some(KeyCode::KEY_LEFTALT),
        "KEY_RIGHTALT" => Some(KeyCode::KEY_RIGHTALT),
        "KEY_F1" => Some(KeyCode::KEY_F1),
        "KEY_F2" => Some(KeyCode::KEY_F2),
        "KEY_F3" => Some(KeyCode::KEY_F3),
        "KEY_F4" => Some(KeyCode::KEY_F4),
        "KEY_F5" => Some(KeyCode::KEY_F5),
        "KEY_F6" => Some(KeyCode::KEY_F6),
        "KEY_F7" => Some(KeyCode::KEY_F7),
        "KEY_F8" => Some(KeyCode::KEY_F8),
        "KEY_F9" => Some(KeyCode::KEY_F9),
        "KEY_F10" => Some(KeyCode::KEY_F10),
        "KEY_F11" => Some(KeyCode::KEY_F11),
        "KEY_F12" => Some(KeyCode::KEY_F12),
        "KEY_PAUSE" => Some(KeyCode::KEY_PAUSE),
        "KEY_SCROLLLOCK" => Some(KeyCode::KEY_SCROLLLOCK),
        "KEY_GRAVE" => Some(KeyCode::KEY_GRAVE),
        "KEY_TAB" => Some(KeyCode::KEY_TAB),
        "KEY_CAPSLOCK" => Some(KeyCode::KEY_CAPSLOCK),
        "KEY_ESC" => Some(KeyCode::KEY_ESC),
        "KEY_ENTER" => Some(KeyCode::KEY_ENTER),
        "KEY_LEFTSHIFT" => Some(KeyCode::KEY_LEFTSHIFT),
        "KEY_RIGHTSHIFT" => Some(KeyCode::KEY_RIGHTSHIFT),
        "KEY_LEFTCTRL" => Some(KeyCode::KEY_LEFTCTRL),
        "KEY_RIGHTCTRL" => Some(KeyCode::KEY_RIGHTCTRL),
        _ => {
            eprintln!("⚠️ Tecla no reconocida: {name}. Usando default.");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AppConfig, LoggingConfig};

    #[test]
    fn logging_defaults_are_stable() {
        assert_eq!(AppConfig::default().logging, LoggingConfig::default());
        assert_eq!(AppConfig::default().logging.level, "info");
        assert!(AppConfig::default().logging.file);
        assert_eq!(AppConfig::default().logging.retention_days, 7);
    }

    #[test]
    fn logging_section_is_optional_and_overridable() {
        let mut config = AppConfig::default();
        AppConfig::parse_toml(
            r#"
                [logging]
                level = "debug,transcriptor=trace"
                file = false
                retention_days = 21
            "#,
            &mut config,
        );

        assert_eq!(config.logging.level, "debug,transcriptor=trace");
        assert!(!config.logging.file);
        assert_eq!(config.logging.retention_days, 21);
    }

    #[test]
    fn retry_and_timeouts_are_configurable_and_safe() {
        let mut config = AppConfig::default();
        AppConfig::parse_toml(r#"
            [retry]
            max_attempts = 0
            initial_delay_ms = 25
            max_delay_ms = 100
            jitter_ms = 5
            [groq]
            connect_timeout_ms = 200
            request_timeout_ms = 900
        "#, &mut config);
        assert_eq!(config.retry.max_attempts, 1);
        assert_eq!(config.retry.initial_delay, std::time::Duration::from_millis(25));
        assert_eq!(config.groq_connect_timeout, std::time::Duration::from_millis(200));
        assert_eq!(config.groq_request_timeout, std::time::Duration::from_millis(900));
    }
}
