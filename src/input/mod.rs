pub mod listener;
pub mod state_machine;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Push,
    Tap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    BeginArming,
    StartRecording(Mode),
    WaitForLatch,
    LatchRecording,
    StopRecording,
    /// Aborta el dictado sin transcribir: descarta el audio y, si la llamada a
    /// la API ya está en vuelo, la cancela.
    CancelRecording,
    ToggleFormat,
    StopInvertedEnter,
    IncreaseGain,
    DecreaseGain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEvent {
    Down { modifier: bool },
    Up,
    Escape,
    Alt { modifier: bool },
    EnterKey { modifier: bool },
    IncreaseGain,
    DecreaseGain,
}
