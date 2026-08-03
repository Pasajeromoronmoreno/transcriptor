use arboard::Clipboard;

pub fn set_clipboard(text: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut ctx = Clipboard::new()?;
    ctx.set_text(text)?;
    Ok(())
}

/// Lee el texto actual del portapapeles, si hay alguno.
///
/// `None` cubre tanto el portapapeles vacío como uno con contenido no textual
/// (una imagen, por ejemplo): en ningún caso hay algo que restaurar después.
pub fn get_clipboard_text() -> Option<String> {
    Clipboard::new().ok()?.get_text().ok()
}
