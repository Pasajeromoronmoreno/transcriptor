use arboard::Clipboard;

pub fn set_clipboard(text: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut ctx = Clipboard::new()?;
    ctx.set_text(text)?;
    Ok(())
}
