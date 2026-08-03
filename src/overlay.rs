use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

#[cfg(feature = "overlay")]
use std::path::PathBuf;
#[cfg(feature = "overlay")]
use std::process::{Child, Command, Stdio};
#[cfg(feature = "overlay")]
use std::os::unix::process::CommandExt;
#[cfg(feature = "overlay")]
use tokio::io::AsyncWriteExt;
#[cfg(feature = "overlay")]
use tokio::net::UnixListener;
#[cfg(feature = "overlay")]
use tokio::task::JoinHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingMode {
    Push,
    Tap,
}

impl From<crate::input::Mode> for RecordingMode {
    fn from(mode: crate::input::Mode) -> Self {
        match mode {
            crate::input::Mode::Push => Self::Push,
            crate::input::Mode::Tap => Self::Tap,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppPhase {
    Idle,
    Arming,
    Recording,
    WaitingForLatch,
    Transcribing,
    Retrying,
    Delivering,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayStatus {
    pub phase: AppPhase,
    pub mode: Option<RecordingMode>,
    pub started_at_ms: Option<u64>,
    pub message: Option<String>,
}

impl OverlayStatus {
    pub fn idle() -> Self {
        Self {
            phase: AppPhase::Idle,
            mode: None,
            started_at_ms: None,
            message: None,
        }
    }
}

#[derive(Clone)]
pub struct OverlayHub {
    tx: watch::Sender<OverlayStatus>,
}

impl OverlayHub {
    pub fn new() -> Self {
        let (tx, _) = watch::channel(OverlayStatus::idle());
        Self { tx }
    }

    pub fn publish(&self, phase: AppPhase, mode: Option<RecordingMode>, message: Option<String>) {
        let started_at_ms = match phase {
            AppPhase::Recording | AppPhase::WaitingForLatch => {
                self.tx.borrow().started_at_ms.or_else(now_ms)
            }
            _ => self.tx.borrow().started_at_ms,
        };

        let started_at_ms = if matches!(phase, AppPhase::Idle | AppPhase::Arming | AppPhase::Error) {
            None
        } else {
            started_at_ms
        };

        let _ = self.tx.send(OverlayStatus {
            phase,
            mode,
            started_at_ms,
            message,
        });
    }

    #[cfg(any(feature = "overlay", test))]
    fn subscribe(&self) -> watch::Receiver<OverlayStatus> {
        self.tx.subscribe()
    }
}

pub struct OverlayHandle {
    #[cfg(feature = "overlay")]
    child: Option<Child>,
    #[cfg(feature = "overlay")]
    socket_path: PathBuf,
    #[cfg(feature = "overlay")]
    server_task: JoinHandle<()>,
}

impl OverlayHandle {
    #[cfg(not(feature = "overlay"))]
    pub fn shutdown(self) {
        let _ = self;
    }

    #[cfg(feature = "overlay")]
    pub fn shutdown(self) {
        self.server_task.abort();
        if let Some(mut child) = self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(not(feature = "overlay"))]
pub async fn start() -> (OverlayHub, Option<OverlayHandle>) {
    eprintln!("ℹ️ Overlay desactivado: compilá con `--features overlay` para habilitarlo.");
    (OverlayHub::new(), None)
}

#[cfg(feature = "overlay")]
pub async fn start() -> (OverlayHub, Option<OverlayHandle>) {
    let hub = OverlayHub::new();

    match start_enabled(hub.clone()).await {
        Ok(handle) => (hub, Some(handle)),
        Err(error) => {
            tracing::warn!(code="TRN-OVERLAY-START", error=%error, "No se pudo iniciar el overlay");
            (hub, None)
        }
    }
}

#[cfg(feature = "overlay")]
async fn start_enabled(hub: OverlayHub) -> Result<OverlayHandle, Box<dyn std::error::Error>> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let socket_path = runtime_dir.join(format!("transcriptor-overlay-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path)?;

    let executable = std::env::current_exe()?;
    let mut command = Command::new(executable);
    command
        .arg("--overlay")
        .arg(&socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // The overlay is a desktop surface, not another terminal job. Giving it
    // its own process group prevents terminal-generated signals from reaching
    // both the recorder and the GTK process at once.
    command.process_group(0);
    let child = command.spawn()?;

    let server_task = tokio::spawn(serve(listener, hub.subscribe()));
    Ok(OverlayHandle {
        child: Some(child),
        socket_path,
        server_task,
    })
}

#[cfg(feature = "overlay")]
async fn serve(listener: UnixListener, rx: watch::Receiver<OverlayStatus>) {
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            break;
        };
        let mut client_rx = rx.clone();
        tokio::spawn(async move {
            loop {
                let status = client_rx.borrow().clone();
                let Ok(mut payload) = serde_json::to_vec(&status) else {
                    break;
                };
                payload.push(b'\n');
                if stream.write_all(&payload).await.is_err() {
                    break;
                }
                if client_rx.changed().await.is_err() {
                    break;
                }
            }
        });
    }
}

pub fn now_ms() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
}

#[cfg(feature = "overlay")]
pub fn run_ui(socket_path: &str) {
    ui::run(socket_path);
}

#[cfg(feature = "overlay")]
mod ui {
    use super::OverlayStatus;
    use gtk4::prelude::*;
    use gtk4::{glib, Application, ApplicationWindow, CssProvider, Label};
    use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixStream;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    pub fn run(socket_path: &str) {
        let (status_tx, status_rx) = mpsc::channel::<OverlayStatus>();
        let socket_path = socket_path.to_owned();
        thread::spawn(move || {
            let Ok(stream) = UnixStream::connect(socket_path) else {
                return;
            };
            // Se corta ante el primer error de lectura: `flatten()` los saltea
            // y giraría para siempre si el socket queda en estado de error.
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                if let Ok(status) = serde_json::from_str::<OverlayStatus>(&line)
                    && status_tx.send(status).is_err()
                {
                    break;
                }
            }
        });

        let status_rx = Rc::new(RefCell::new(status_rx));
        let app = Application::builder()
            .application_id("local.transcriptor.Overlay")
            .build();
        app.connect_activate(move |app| build_window(app, status_rx.clone()));
        // The child process is launched with the private `--overlay` flag.
        // Do not let GTK parse the parent application's arguments, otherwise
        // GTK rejects the flag before the overlay window can start.
        let args: [&str; 0] = [];
        app.run_with_args(&args);
    }

    fn build_window(app: &Application, status_rx: Rc<RefCell<mpsc::Receiver<OverlayStatus>>>) {
        let window = ApplicationWindow::builder()
            .application(app)
            .decorated(false)
            .default_width(360)
            .default_height(58)
            .build();
        window.add_css_class("overlay-window");
        let label = Label::builder().label("Transcriptor").build();
        label.set_margin_top(12);
        label.set_margin_bottom(12);
        label.set_margin_start(18);
        label.set_margin_end(18);
        window.set_child(Some(&label));

        let css = CssProvider::new();
        css.load_from_data(
            ".overlay-window { background: transparent; }\n.overlay { background: rgba(25, 25, 30, 0.92); border-radius: 14px; color: white; font-size: 16px; }\n.recording { color: #ff6b6b; }\n.processing { color: #8ab4f8; }\n.success { color: #81c995; }\n.error { color: #ff8a80; }",
        );
        label.add_css_class("overlay");
        if let Some(display) = gtk4::gdk::Display::default() {
            gtk4::style_context_add_provider_for_display(
                &display,
                &css,
                gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }

        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_keyboard_mode(KeyboardMode::None);
        window.set_anchor(Edge::Bottom, true);
        window.set_margin(Edge::Bottom, 28);
        window.set_exclusive_zone(0);
        window.set_visible(false);

        glib::timeout_add_local(Duration::from_millis(80), move || {
            while let Ok(status) = status_rx.borrow().try_recv() {
                apply_status(&window, &label, &status);
            }
            glib::ControlFlow::Continue
        });
    }

    fn apply_status(window: &ApplicationWindow, label: &Label, status: &OverlayStatus) {
        use super::{AppPhase, RecordingMode};
        label.remove_css_class("recording");
        label.remove_css_class("processing");
        label.remove_css_class("success");
        label.remove_css_class("error");

        let (text, class) = match status.phase {
            AppPhase::Idle => {
                window.set_visible(false);
                return;
            }
            AppPhase::Arming => ("🎙️ Preparando…", "recording"),
            AppPhase::Recording => match status.mode {
                Some(RecordingMode::Push) => ("🔴 Grabando · Push", "recording"),
                Some(RecordingMode::Tap) => ("🔴 Grabando · Tap", "recording"),
                None => ("🔴 Grabando", "recording"),
            },
            AppPhase::WaitingForLatch => ("🔴 Grabando · esperando latch", "recording"),
            AppPhase::Transcribing => ("🔵 Transcribiendo…", "processing"),
            AppPhase::Retrying => ("🟠 Problema de red · reintentando…", "processing"),
            AppPhase::Delivering => ("🟢 Pegando resultado…", "success"),
            AppPhase::Error => ("⚠️ Error", "error"),
        };

        label.set_text(status.message.as_deref().unwrap_or(text));
        label.add_css_class(class);
        window.set_visible(true);
        window.present();
    }
}

#[cfg(test)]
mod tests {
    use super::{AppPhase, OverlayHub, RecordingMode};

    #[test]
    fn publishes_serializable_recording_state() {
        let hub = OverlayHub::new();
        let mut rx = hub.subscribe();

        hub.publish(AppPhase::Recording, Some(RecordingMode::Push), None);

        let status = rx.borrow_and_update().clone();
        assert_eq!(status.phase, AppPhase::Recording);
        assert_eq!(status.mode, Some(RecordingMode::Push));
        assert!(status.started_at_ms.is_some());
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"phase\":\"recording\""));
        assert!(json.contains("\"mode\":\"push\""));
    }

    #[test]
    fn idle_clears_recording_timestamp() {
        let hub = OverlayHub::new();
        let mut rx = hub.subscribe();

        hub.publish(AppPhase::Recording, Some(RecordingMode::Tap), None);
        assert!(rx.borrow_and_update().started_at_ms.is_some());
        hub.publish(AppPhase::Idle, None, None);

        let status = rx.borrow_and_update().clone();
        assert_eq!(status.phase, AppPhase::Idle);
        assert_eq!(status.mode, None);
        assert_eq!(status.started_at_ms, None);
    }

    #[test]
    fn retry_state_exposes_attempt_to_ui() {
        let hub = OverlayHub::new();
        let mut rx = hub.subscribe();
        hub.publish(AppPhase::Retrying, Some(RecordingMode::Tap), Some("Problema de red · reintentando 2/3…".into()));
        let status = rx.borrow_and_update().clone();
        assert_eq!(status.phase, AppPhase::Retrying);
        assert_eq!(status.message.as_deref(), Some("Problema de red · reintentando 2/3…"));
        assert!(serde_json::to_string(&status).unwrap().contains("retrying"));
    }
}
