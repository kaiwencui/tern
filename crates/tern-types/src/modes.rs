/// Which mouse events the program in the terminal asked to receive.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MouseTracking {
    #[default]
    Off,
    /// Presses and releases (mode 1000).
    Click,
    /// Also motion while a button is held (mode 1002).
    Drag,
    /// All motion (mode 1003).
    Motion,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MouseEncoding {
    /// The original encoding, limited to 223 columns and rows.
    #[default]
    Default,
    /// Mode 1005.
    Utf8,
    /// Mode 1006.
    Sgr,
}

/// The terminal modes that change how input is encoded. The core publishes
/// them; the input encoder and the app read them.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Modes {
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub bracketed_paste: bool,
    pub focus_events: bool,
    pub alt_screen: bool,
    /// In the alternate screen, the wheel sends arrow keys (mode 1007).
    pub alternate_scroll: bool,
    pub mouse: MouseTracking,
    pub mouse_encoding: MouseEncoding,
    /// The kitty keyboard protocol's current flags (0 when it is off):
    /// 1 disambiguate, 2 report event types, 4 report alternate keys,
    /// 8 report all keys as escape codes, 16 report associated text.
    pub kitty_flags: u8,
    /// LNM: Enter sends CR LF.
    pub line_feed_new_line: bool,
}
