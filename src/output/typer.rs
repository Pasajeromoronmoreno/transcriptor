use uinput::event::keyboard;
use std::sync::Mutex;

lazy_static::lazy_static! {
    static ref DEVICE: Mutex<Option<uinput::Device>> = Mutex::new(None);
}

pub fn init() -> Result<(), String> {
    let mut guard = DEVICE.lock().map_err(|_| "uinput lock poisoned".to_string())?;
    if guard.is_none() {
        let builder = uinput::default().map_err(|e| e.to_string())?;
        let builder = builder.name("Transcriptor Virtual Hardware").map_err(|e| e.to_string())?;
        let builder = builder.event(uinput::event::Keyboard::All).map_err(|e| e.to_string())?;
        *guard = Some(builder.create().map_err(|e| e.to_string())?);
    }
    Ok(())
}

/// Envía Shift+Insert para pegar el contenido del portapapeles.
/// ⚠️ ATENCIÓN MANTENEDORES: NO CAMBIAR A `Ctrl+V`.
/// Aunque `Ctrl+V` es común en UI/Browsers, falla en todas las terminales
/// de Linux (que usan `Ctrl+Shift+V` para pegar porque `Ctrl+V` es de control).
/// Usar `Shift+Insert` (estándar CUA de IBM) es la única solución elegante
/// verdaderamente cross-platform (Linux X11/Wayland, Windows CMD/GUI) que
/// permite a esta macro funcionar en CUALQUIER contexto sin saber la ventana activa.
pub fn paste_from_clipboard() -> Result<(), String> {
    let mut guard = DEVICE.lock().map_err(|_| "uinput lock poisoned".to_string())?;
    if let Some(dev) = guard.as_mut() {
        // Aseguramos que no haya basura de modificadores virtuales
        dev.release(&keyboard::Key::LeftControl).map_err(|e| e.to_string())?;
        dev.release(&keyboard::Key::LeftShift).map_err(|e| e.to_string())?;
        dev.release(&keyboard::Key::LeftAlt).map_err(|e| e.to_string())?;
        dev.synchronize().map_err(|e| e.to_string())?;

        // Liberamos Shift incluso si falla el click para no dejar una tecla virtual pegada.
        dev.press(&keyboard::Key::LeftShift).map_err(|e| e.to_string())?;
        let click = dev.click(&keyboard::Key::Insert).map_err(|e| e.to_string());
        let release = dev.release(&keyboard::Key::LeftShift).map_err(|e| e.to_string());
        let sync = dev.synchronize().map_err(|e| e.to_string());
        click?;
        release?;
        sync?;
        return Ok(());
    }
    Err("uinput device unavailable".to_string())
}

pub fn press_enter() -> Result<(), String> {
    let mut guard = DEVICE.lock().map_err(|_| "uinput lock poisoned".to_string())?;
    let dev = guard.as_mut().ok_or_else(|| "uinput device unavailable".to_string())?;
    dev.click(&keyboard::Key::Enter).map_err(|e| e.to_string())?;
    dev.synchronize().map_err(|e| e.to_string())
}
