use std::fmt;
use std::sync::Arc;

use crate::ansi::Rgb;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct WindowSize {
    pub num_lines: u16,
    pub num_cols: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Progress {
    #[default]
    Hidden,
    Set(u8),
    Error(u8),
    Indeterminate,
    Pause(u8),
}

#[derive(Clone)]
pub enum Event {
    MouseCursorDirty,
    Title(String),
    ResetTitle,
    ClipboardStore(String),
    ClipboardLoad(Arc<dyn Fn(&str) -> String + Sync + Send + 'static>),
    ColorRequest(usize, Arc<dyn Fn(Rgb) -> String + Sync + Send + 'static>),
    PtyWrite(String),
    TextAreaSizeRequest(Arc<dyn Fn(WindowSize) -> String + Sync + Send + 'static>),
    CursorBlinkingChange,
    Bell,
    Notify(Option<String>, String),
    Progress(Progress),
}

impl fmt::Debug for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Event::MouseCursorDirty => write!(f, "MouseCursorDirty"),
            Event::Title(title) => f.debug_tuple("Title").field(title).finish(),
            Event::ResetTitle => write!(f, "ResetTitle"),
            Event::ClipboardStore(text) => f.debug_tuple("ClipboardStore").field(text).finish(),
            Event::ClipboardLoad(_) => write!(f, "ClipboardLoad"),
            Event::ColorRequest(index, _) => f.debug_tuple("ColorRequest").field(index).finish(),
            Event::PtyWrite(text) => f.debug_tuple("PtyWrite").field(text).finish(),
            Event::TextAreaSizeRequest(_) => write!(f, "TextAreaSizeRequest"),
            Event::CursorBlinkingChange => write!(f, "CursorBlinkingChange"),
            Event::Bell => write!(f, "Bell"),
            Event::Notify(title, body) => {
                f.debug_tuple("Notify").field(title).field(body).finish()
            },
            Event::Progress(p) => f.debug_tuple("Progress").field(p).finish(),
        }
    }
}

pub trait EventListener {
    fn send_event(&self, _event: Event) {}
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct VoidListener;

impl EventListener for VoidListener {}
