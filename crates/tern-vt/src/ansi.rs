//! The values the control functions of a terminal are about: colors and
//! styles, modes, the cursor's look, character sets, what to erase. The
//! interpreter of output reads them from it, and the terminal acts on them.

use bitflags::bitflags;

/// A color given as red, green and blue.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// The colors a terminal names: the sixteen of the palette and the ones it
/// keeps for itself. The palette's are numbered as its slots, the others
/// from 256, as the slots of the colors a program can set and ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NamedColor {
    Black = 0,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
    Foreground = 256,
    Background,
    Cursor,
}

/// A color as a cell has it: one the terminal names, one of the 256 of the
/// palette, or one given outright.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Color {
    Named(NamedColor),
    Spec(Rgb),
    Indexed(u8),
}

/// A change of the style text is written in (SGR).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attr {
    Reset,
    Bold,
    Dim,
    Italic,
    Underline,
    DoubleUnderline,
    Undercurl,
    DottedUnderline,
    DashedUnderline,
    BlinkSlow,
    BlinkFast,
    Reverse,
    Hidden,
    Strike,
    CancelBold,
    CancelBoldDim,
    CancelItalic,
    CancelUnderline,
    CancelBlink,
    CancelReverse,
    CancelHidden,
    CancelStrike,
    Foreground(Color),
    Background(Color),
    /// None for the color of the text.
    UnderlineColor(Option<Color>),
}

/// What the cursor looks like.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Beam,
    HollowBlock,
}

/// What the cursor looks like, and whether it blinks.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CursorStyle {
    pub shape: CursorShape,
    pub blinking: bool,
}

/// A mode set and reset with SM and RM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Named(NamedMode),
    Unknown(u16),
}

impl Mode {
    /// The mode numbered `number`.
    pub fn from_raw(number: u16) -> Mode {
        match number {
            4 => Mode::Named(NamedMode::Insert),
            20 => Mode::Named(NamedMode::LineFeedNewLine),
            _ => Mode::Unknown(number),
        }
    }

    /// The mode's number.
    pub fn raw(self) -> u16 {
        match self {
            Mode::Named(named) => named as u16,
            Mode::Unknown(number) => number,
        }
    }
}

/// The modes set with SM that the terminal knows.
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedMode {
    Insert = 4,
    LineFeedNewLine = 20,
}

/// A mode set and reset with DECSET and DECRST (`CSI ? … h` and `l`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivateMode {
    Named(NamedPrivateMode),
    Unknown(u16),
}

impl PrivateMode {
    /// The mode numbered `number`.
    pub fn from_raw(number: u16) -> PrivateMode {
        use NamedPrivateMode as Named;
        let named = match number {
            1 => Named::CursorKeys,
            3 => Named::ColumnMode,
            6 => Named::Origin,
            7 => Named::LineWrap,
            12 => Named::BlinkingCursor,
            25 => Named::ShowCursor,
            1000 => Named::ReportMouseClicks,
            1002 => Named::ReportCellMouseMotion,
            1003 => Named::ReportAllMouseMotion,
            1004 => Named::ReportFocusInOut,
            1005 => Named::Utf8Mouse,
            1006 => Named::SgrMouse,
            1007 => Named::AlternateScroll,
            1042 => Named::UrgencyHints,
            1049 => Named::SwapScreenAndSetRestoreCursor,
            2004 => Named::BracketedPaste,
            2026 => Named::SyncUpdate,
            _ => return PrivateMode::Unknown(number),
        };
        PrivateMode::Named(named)
    }

    /// The mode's number.
    pub fn raw(self) -> u16 {
        match self {
            PrivateMode::Named(named) => named as u16,
            PrivateMode::Unknown(number) => number,
        }
    }
}

impl From<NamedPrivateMode> for PrivateMode {
    fn from(named: NamedPrivateMode) -> PrivateMode {
        PrivateMode::Named(named)
    }
}

/// The private modes the terminal knows.
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedPrivateMode {
    CursorKeys = 1,
    ColumnMode = 3,
    Origin = 6,
    LineWrap = 7,
    BlinkingCursor = 12,
    ShowCursor = 25,
    ReportMouseClicks = 1000,
    ReportCellMouseMotion = 1002,
    ReportAllMouseMotion = 1003,
    ReportFocusInOut = 1004,
    Utf8Mouse = 1005,
    SgrMouse = 1006,
    AlternateScroll = 1007,
    UrgencyHints = 1042,
    SwapScreenAndSetRestoreCursor = 1049,
    BracketedPaste = 2004,
    SyncUpdate = 2026,
}

/// What of the cursor's line to erase (EL).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineClearMode {
    Right,
    Left,
    All,
}

/// What of the screen to erase (ED).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearMode {
    Below,
    Above,
    All,
    /// The scrollback.
    Saved,
}

/// Which tab stops to clear (TBC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabulationClearMode {
    Current,
    All,
}

/// One of the four character sets a program names, G0 to G3.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CharsetIndex {
    #[default]
    G0,
    G1,
    G2,
    G3,
}

/// What a character set can be.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum StandardCharset {
    #[default]
    Ascii,
    /// DEC's special characters and line drawing.
    SpecialCharacterAndLineDrawing,
}

impl StandardCharset {
    /// The character `c` stands for in this set.
    #[inline]
    pub fn map(self, c: char) -> char {
        match self {
            StandardCharset::Ascii => c,
            StandardCharset::SpecialCharacterAndLineDrawing => match c {
                '_' => ' ',
                '`' => '\u{25c6}',
                'a' => '\u{2592}',
                'b' => '\u{2409}',
                'c' => '\u{240c}',
                'd' => '\u{240d}',
                'e' => '\u{240a}',
                'f' => '\u{b0}',
                'g' => '\u{b1}',
                'h' => '\u{2424}',
                'i' => '\u{240b}',
                'j' => '\u{2518}',
                'k' => '\u{2510}',
                'l' => '\u{250c}',
                'm' => '\u{2514}',
                'n' => '\u{253c}',
                'o' => '\u{23ba}',
                'p' => '\u{23bb}',
                'q' => '\u{2500}',
                'r' => '\u{23bc}',
                's' => '\u{23bd}',
                't' => '\u{251c}',
                'u' => '\u{2524}',
                'v' => '\u{2534}',
                'w' => '\u{252c}',
                'x' => '\u{2502}',
                'y' => '\u{2264}',
                'z' => '\u{2265}',
                '{' => '\u{3c0}',
                '|' => '\u{2260}',
                '}' => '\u{a3}',
                '~' => '\u{b7}',
                _ => c,
            },
        }
    }
}

bitflags! {
    /// The enhancements of the kitty keyboard protocol a program asks for.
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct KeyboardModes: u8 {
        const NO_MODE                 = 0b0000_0000;
        const DISAMBIGUATE_ESC_CODES  = 0b0000_0001;
        const REPORT_EVENT_TYPES      = 0b0000_0010;
        const REPORT_ALTERNATE_KEYS   = 0b0000_0100;
        const REPORT_ALL_KEYS_AS_ESC  = 0b0000_1000;
        const REPORT_ASSOCIATED_TEXT  = 0b0001_0000;
    }
}

/// How keyboard modes a program gives go with those in force.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardModesApplyBehavior {
    #[default]
    Replace,
    Union,
    Difference,
}

/// A link a program gives text (OSC 8).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hyperlink {
    /// What links the cells of one link together, if the program gave it.
    pub id: Option<String>,
    pub uri: String,
}
