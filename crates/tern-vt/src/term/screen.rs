//! Modes, the other screen, tab stops, character sets and keyboard modes.

use std::mem;
use std::ops::{Index, IndexMut};

use crate::ansi::{
    CharsetIndex, KeyboardModes, KeyboardModesApplyBehavior, Mode, NamedMode, NamedPrivateMode,
    PrivateMode, StandardCharset, TabulationClearMode,
};
use crate::event::{Event, EventListener};
use crate::grid::Dimensions;
use crate::index::{Column, Line};
use crate::term::{KEYBOARD_MODE_STACK_MAX_DEPTH, Term, TermMode};

/// The columns tab stops are set at to begin with: every eighth.
const INITIAL_TABSTOPS: usize = 8;

impl<T: EventListener> Term<T> {
    /// HT: to the next tab stop, or the last column, `count` times. A tab
    /// stays in the cells it crosses that are blank, for copying.
    #[inline]
    pub fn put_tab(&mut self, count: u16) {
        // A tab after the last column is the same as a line break.
        if self.grid.cursor.input_needs_wrap {
            self.wrapline();
            return;
        }
        let tab = self.grid.cursor.charsets[self.active_charset].map('\t');
        for _ in 0..count {
            let point = self.grid.cursor.point;
            let to = self.tabs.next_after(point.column);
            self.grid[point.line].write_tab(point.column.0, tab);
            self.grid.cursor.point.column = to;
            if to == point.column {
                break;
            }
        }
    }

    /// CHT: forward `count` tab stops, or to the last column.
    #[inline]
    pub fn move_forward_tabs(&mut self, count: u16) {
        let from = self.grid.cursor.point.column;
        let mut to = from;
        for _ in 0..count {
            let next = self.tabs.next_after(to);
            if next == to {
                break;
            }
            to = next;
        }
        self.tab_to(to);
    }

    /// CBT: back `count` tab stops. With none before it, the cursor stays.
    #[inline]
    pub fn move_backward_tabs(&mut self, count: u16) {
        let from = self.grid.cursor.point.column;
        let mut to = from;
        for _ in 0..count {
            match self.tabs.last_before(to) {
                Some(stop) => to = stop,
                None => break,
            }
        }
        self.tab_to(to);
    }

    /// HTS.
    #[inline]
    pub fn set_horizontal_tabstop(&mut self) {
        self.tabs[self.grid.cursor.point.column] = true;
    }

    /// TBC.
    #[inline]
    pub fn clear_tabs(&mut self, mode: TabulationClearMode) {
        match mode {
            TabulationClearMode::Current => self.tabs[self.grid.cursor.point.column] = false,
            TabulationClearMode::All => self.tabs.clear_all(),
        }
    }

    /// SM.
    #[inline]
    pub fn set_mode(&mut self, mode: Mode) {
        if let Mode::Named(mode) = mode {
            self.mode.insert(mode_flag(mode));
        }
    }

    /// RM.
    #[inline]
    pub fn unset_mode(&mut self, mode: Mode) {
        if let Mode::Named(mode) = mode {
            self.mode.remove(mode_flag(mode));
            if mode == NamedMode::Insert {
                self.mark_fully_damaged();
            }
        }
    }

    /// DECRQM for a mode set with SM.
    #[inline]
    pub fn report_mode(&mut self, mode: Mode) {
        let state = match mode {
            Mode::Named(named) => self.mode.contains(mode_flag(named)).into(),
            Mode::Unknown(_) => ModeState::NotSupported,
        };
        self.reply(format!("\x1b[{};{}$y", mode.raw(), state as u8));
    }

    /// DECSET.
    #[inline]
    pub fn set_private_mode(&mut self, mode: PrivateMode) {
        if let PrivateMode::Named(mode) = mode {
            self.private_mode(mode, true);
        }
    }

    /// DECRST.
    #[inline]
    pub fn unset_private_mode(&mut self, mode: PrivateMode) {
        if let PrivateMode::Named(mode) = mode {
            self.private_mode(mode, false);
        }
    }

    /// DECRQM for a private mode.
    #[inline]
    pub fn report_private_mode(&mut self, mode: PrivateMode) {
        let state = match mode {
            PrivateMode::Named(NamedPrivateMode::BlinkingCursor) => {
                self.cursor_style.get_or_insert(self.options.cursor).blinking.into()
            },
            PrivateMode::Named(NamedPrivateMode::SyncUpdate) => self.sync_update.into(),
            PrivateMode::Named(named) => match private_mode_flag(named) {
                Some(flag) => self.mode.contains(flag).into(),
                None => ModeState::NotSupported,
            },
            PrivateMode::Unknown(_) => ModeState::NotSupported,
        };
        self.reply(format!("\x1b[?{};{}$y", mode.raw(), state as u8));
    }

    /// DECKPAM.
    #[inline]
    pub fn set_keypad_application_mode(&mut self) {
        self.mode.insert(TermMode::APP_KEYPAD);
    }

    /// DECKPNM.
    #[inline]
    pub fn unset_keypad_application_mode(&mut self) {
        self.mode.remove(TermMode::APP_KEYPAD);
    }

    /// SCS: what one of G0 to G3 is.
    #[inline]
    pub fn configure_charset(&mut self, index: CharsetIndex, charset: StandardCharset) {
        self.grid.cursor.charsets[index] = charset;
    }

    /// SI, SO, LS2, LS3: which of G0 to G3 is in use.
    #[inline]
    pub fn set_active_charset(&mut self, index: CharsetIndex) {
        self.active_charset = index;
    }

    /// RIS: the terminal as it starts, but for its size, its scrollback
    /// settings and the colors set.
    #[inline]
    pub fn reset_state(&mut self) {
        if self.mode.contains(TermMode::ALT_SCREEN) {
            mem::swap(&mut self.grid, &mut self.inactive_grid);
        }
        self.grid.reset();
        self.inactive_grid.reset();
        self.active_charset = CharsetIndex::default();
        self.cursor_style = None;
        self.scroll_region = Line(0)..Line(self.screen_lines() as i32);
        self.tabs = TabStops::new(self.columns());
        self.title_stack = Vec::new();
        self.title = None;
        self.working_directory = None;
        self.selection = None;
        self.keyboard_mode_stack = Vec::new();
        self.inactive_keyboard_mode_stack = Vec::new();
        self.mode = TermMode::default();
        self.event_proxy.send_event(Event::Progress(crate::event::Progress::Hidden));
        self.event_proxy.send_event(Event::CursorBlinkingChange);
        self.mark_fully_damaged();
    }

    /// The kitty keyboard protocol's query: the modes in force.
    #[inline]
    pub fn report_keyboard_mode(&mut self) {
        let modes = self.keyboard_modes().bits();
        self.reply(format!("\x1b[?{modes}u"));
    }

    /// The kitty keyboard protocol's push. A full stack gives up its
    /// oldest.
    #[inline]
    pub fn push_keyboard_mode(&mut self, mode: KeyboardModes) {
        if self.keyboard_mode_stack.len() >= KEYBOARD_MODE_STACK_MAX_DEPTH {
            self.keyboard_mode_stack.remove(0);
        }
        self.keyboard_mode_stack.push(mode);
        self.apply_keyboard_mode(mode.into(), KeyboardModesApplyBehavior::Replace);
    }

    /// The kitty keyboard protocol's pop: the modes from before the last
    /// `count` pushes come back.
    #[inline]
    pub fn pop_keyboard_modes(&mut self, count: u16) {
        let left = self.keyboard_mode_stack.len().saturating_sub(count as usize);
        self.keyboard_mode_stack.truncate(left);
        self.apply_keyboard_mode(self.keyboard_modes().into(), KeyboardModesApplyBehavior::Replace);
    }

    /// The kitty keyboard protocol's set: in force now, but not pushed.
    #[inline]
    pub fn set_keyboard_mode(&mut self, mode: KeyboardModes, apply: KeyboardModesApplyBehavior) {
        self.apply_keyboard_mode(mode.into(), apply);
    }

    /// Between the screen of the shell and the other one programs draw on.
    /// Each has its own stack of keyboard modes. Going to the other screen
    /// blanks it and saves the cursor, which coming back restores.
    pub fn swap_alt(&mut self) {
        if !self.mode.contains(TermMode::ALT_SCREEN) {
            self.inactive_grid.cursor = self.grid.cursor.clone();
            self.grid.saved_cursor = self.grid.cursor.clone();
            self.inactive_grid.reset_region(..);
        }
        mem::swap(&mut self.keyboard_mode_stack, &mut self.inactive_keyboard_mode_stack);
        self.apply_keyboard_mode(self.keyboard_modes().into(), KeyboardModesApplyBehavior::Replace);
        mem::swap(&mut self.grid, &mut self.inactive_grid);
        self.mode ^= TermMode::ALT_SCREEN;
        self.selection = None;
        self.mark_fully_damaged();
    }

    /// DECSET or DECRST: `on` for set.
    fn private_mode(&mut self, mode: NamedPrivateMode, on: bool) {
        use NamedPrivateMode as M;
        match mode {
            M::SwapScreenAndSetRestoreCursor => {
                if on != self.mode.contains(TermMode::ALT_SCREEN) {
                    self.swap_alt();
                }
            },
            // There is no 132-column mode, but what else DECCOLM does is
            // done: the region goes and the screen is erased.
            M::ColumnMode => {
                self.set_scrolling_region(1, None);
                self.grid.reset_region(..);
                self.mark_fully_damaged();
            },
            M::BlinkingCursor => {
                self.cursor_style.get_or_insert(self.options.cursor).blinking = on;
                self.event_proxy.send_event(Event::CursorBlinkingChange);
            },
            M::SyncUpdate => self.sync_update = on,
            _ => {
                let Some(flag) = private_mode_flag(mode) else { return };
                // Of the ways of reporting the mouse, and of the encodings
                // of its reports, only one is in force at a time.
                if on && flag.intersects(TermMode::MOUSE_MODE) {
                    self.mode.remove(TermMode::MOUSE_MODE);
                } else if on && flag == TermMode::SGR_MOUSE {
                    self.mode.remove(TermMode::UTF8_MOUSE);
                } else if on && flag == TermMode::UTF8_MOUSE {
                    self.mode.remove(TermMode::SGR_MOUSE);
                }
                self.mode.set(flag, on);
                if flag.intersects(TermMode::MOUSE_MODE) {
                    self.event_proxy.send_event(Event::MouseCursorDirty);
                }
                if on && mode == M::Origin {
                    self.goto(0, 0);
                }
            },
        }
    }

    /// The kitty keyboard modes `mode` gives, with those in force.
    #[inline]
    pub(super) fn apply_keyboard_mode(&mut self, mode: TermMode, apply: KeyboardModesApplyBehavior) {
        let active = self.mode & TermMode::KITTY_KEYBOARD_PROTOCOL;
        let modes = match apply {
            KeyboardModesApplyBehavior::Replace => mode,
            KeyboardModesApplyBehavior::Union => active | mode,
            KeyboardModesApplyBehavior::Difference => active - mode,
        };
        self.mode = (self.mode - TermMode::KITTY_KEYBOARD_PROTOCOL) | modes;
    }

    /// The keyboard modes last pushed.
    #[inline]
    fn keyboard_modes(&self) -> KeyboardModes {
        self.keyboard_mode_stack.last().copied().unwrap_or(KeyboardModes::NO_MODE)
    }

    /// Along the cursor's line to `to` by tabs, marking the line changed.
    /// A pending wrap stays.
    #[inline]
    fn tab_to(&mut self, to: Column) {
        self.grid.cursor.point.column = to;
        let line = self.grid.cursor.point.line.0 as usize;
        self.damage.damage_line(line);
    }
}

/// The flag of a mode set with SM.
fn mode_flag(mode: NamedMode) -> TermMode {
    match mode {
        NamedMode::Insert => TermMode::INSERT,
        NamedMode::LineFeedNewLine => TermMode::LINE_FEED_NEW_LINE,
    }
}

/// The flag of a private mode, for those that are one.
fn private_mode_flag(mode: NamedPrivateMode) -> Option<TermMode> {
    use NamedPrivateMode as M;
    Some(match mode {
        M::CursorKeys => TermMode::APP_CURSOR,
        M::Origin => TermMode::ORIGIN,
        M::LineWrap => TermMode::LINE_WRAP,
        M::ShowCursor => TermMode::SHOW_CURSOR,
        M::ReportMouseClicks => TermMode::MOUSE_REPORT_CLICK,
        M::ReportCellMouseMotion => TermMode::MOUSE_DRAG,
        M::ReportAllMouseMotion => TermMode::MOUSE_MOTION,
        M::ReportFocusInOut => TermMode::FOCUS_IN_OUT,
        M::Utf8Mouse => TermMode::UTF8_MOUSE,
        M::SgrMouse => TermMode::SGR_MOUSE,
        M::AlternateScroll => TermMode::ALTERNATE_SCROLL,
        M::UrgencyHints => TermMode::URGENCY_HINTS,
        M::SwapScreenAndSetRestoreCursor => TermMode::ALT_SCREEN,
        M::BracketedPaste => TermMode::BRACKETED_PASTE,
        M::ColumnMode | M::BlinkingCursor | M::SyncUpdate => return None,
    })
}

/// What DECRQM answers about a mode.
#[derive(Clone, Copy)]
enum ModeState {
    NotSupported = 0,
    Set = 1,
    Reset = 2,
}

impl From<bool> for ModeState {
    fn from(set: bool) -> ModeState {
        if set { ModeState::Set } else { ModeState::Reset }
    }
}

/// Which columns have a tab stop.
pub(super) struct TabStops {
    tabs: Vec<bool>,
}

impl TabStops {
    pub(super) fn new(columns: usize) -> TabStops {
        let mut tabs = TabStops { tabs: Vec::new() };
        tabs.resize(columns);
        tabs
    }

    fn clear_all(&mut self) {
        self.tabs.fill(false);
    }

    /// For `columns`: the stops kept, and those of new columns at the
    /// columns they were at to begin with.
    pub(super) fn resize(&mut self, columns: usize) {
        let mut column = self.tabs.len();
        self.tabs.resize_with(columns, || {
            column += 1;
            (column - 1).is_multiple_of(INITIAL_TABSTOPS)
        });
    }

    /// The first stop after `column`, or the last column.
    fn next_after(&self, column: Column) -> Column {
        let last = self.tabs.len() - 1;
        let next = self.tabs[(column.0 + 1).min(last)..].iter().position(|&stop| stop);
        Column(next.map_or(last, |at| (column.0 + 1).min(last) + at))
    }

    /// The last stop before `column`, if there is one.
    fn last_before(&self, column: Column) -> Option<Column> {
        self.tabs[..column.0].iter().rposition(|&stop| stop).map(Column)
    }
}

impl Index<Column> for TabStops {
    type Output = bool;

    fn index(&self, column: Column) -> &bool {
        &self.tabs[column.0]
    }
}

impl IndexMut<Column> for TabStops {
    fn index_mut(&mut self, column: Column) -> &mut bool {
        &mut self.tabs[column.0]
    }
}
