//! What the user can see and hear: the tray icon state and audio cues.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Loading,
    Ready,
    Recording,
    Transcribing,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    Start,
    Stop,
    Lock,
    Cancel,
    Error,
}
