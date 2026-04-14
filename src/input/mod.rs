pub mod listener;
pub mod state_machine;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Push,
    Tap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    StartRecording(Mode),
    StopRecording,
    ToggleFormat,
    StopInvertedEnter,
    IncreaseGain,
    DecreaseGain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEvent {
    Down { modifier: bool },
    Up,
    ESC,
    Alt { modifier: bool },
    EnterKey { modifier: bool },
    IncreaseGain,
    DecreaseGain,
}
