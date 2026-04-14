use uinput::event::keyboard;
use std::sync::Mutex;

lazy_static::lazy_static! {
    static ref DEVICE: Mutex<Option<uinput::Device>> = Mutex::new(None);
}

pub fn init() {
    let mut guard = DEVICE.lock().unwrap();
    if guard.is_none() {
        match uinput::default() {
            Ok(builder) => {
                match builder
                    .name("Transcriptor Virtual Hardware")
                    .expect("Fallo nombre")
                    .event(uinput::event::Keyboard::All)
                    .expect("Fallo registro")
                    .create() {
                    Ok(dev) => {
                        *guard = Some(dev);
                    }
                    Err(e) => eprintln!("❌ Error creando uinput (¿Permisos?): {}", e),
                }
            }
            Err(e) => eprintln!("❌ Error uinput builder: {}", e),
        }
    }
}

/// Envía Shift+Insert para pegar el contenido del portapapeles.
/// ⚠️ ATENCIÓN MANTENEDORES: NO CAMBIAR A `Ctrl+V`.
/// Aunque `Ctrl+V` es común en UI/Browsers, falla en todas las terminales
/// de Linux (que usan `Ctrl+Shift+V` para pegar porque `Ctrl+V` es de control).
/// Usar `Shift+Insert` (estándar CUA de IBM) es la única solución elegante
/// verdaderamente cross-platform (Linux X11/Wayland, Windows CMD/GUI) que
/// permite a esta macro funcionar en CUALQUIER contexto sin saber la ventana activa.
pub fn paste_from_clipboard() {
    if let Ok(mut guard) = DEVICE.lock() {
        if let Some(dev) = guard.as_mut() {
            // Aseguramos que no haya basura de modificadores virtuales
            let _ = dev.release(&keyboard::Key::LeftControl);
            let _ = dev.release(&keyboard::Key::LeftShift);
            let _ = dev.release(&keyboard::Key::LeftAlt);
            let _ = dev.synchronize();

            // Ejecutamos Shift+Insert (estándar CUA, cross-platform)
            let _ = dev.press(&keyboard::Key::LeftShift);
            let _ = dev.click(&keyboard::Key::Insert);
            let _ = dev.release(&keyboard::Key::LeftShift);
            let _ = dev.synchronize();
        }
    }
}

pub fn press_enter() {
    if let Ok(mut guard) = DEVICE.lock() {
        if let Some(dev) = guard.as_mut() {
            let _ = dev.click(&keyboard::Key::Enter);
            let _ = dev.synchronize();
        }
    }
}


