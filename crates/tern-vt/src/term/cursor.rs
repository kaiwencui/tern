//! Moving the cursor, and the style it writes in.
//!
//! Every move ends a pending wrap: a character written after it goes where
//! the cursor is, not to the start of the next line. A move marks what it
//! passes over as changed, so the frame draws the cursor where it now is.

use crate::ansi::{Attr, Color, CursorShape, CursorStyle, Hyperlink, NamedColor};
use crate::event::{Event, EventListener};
use crate::grid::Dimensions;
use crate::index::{Column, Line, Point};
use crate::term::cell::Flags;
use crate::term::{Term, TermMode};

impl<T: EventListener> Term<T> {
    /// CUP: to `line` and `column`, counted from the top of the scrolling
    /// region in origin mode and from the top of the screen otherwise.
    #[inline]
    pub fn goto(&mut self, line: i32, column: usize) {
        let (top, bottom) = if self.mode.contains(TermMode::ORIGIN) {
            (self.scroll_region.start, self.scroll_region.end - 1)
        } else {
            (Line(0), self.bottommost_line())
        };
        let line = (Line(line) + top).min(bottom).max(Line(0));
        self.place(line, Column(column).min(self.last_column()));
    }

    /// VPA.
    #[inline]
    pub fn goto_line(&mut self, line: i32) {
        self.goto(line, self.grid.cursor.point.column.0)
    }

    /// CHA.
    #[inline]
    pub fn goto_col(&mut self, column: usize) {
        self.goto(self.grid.cursor.point.line.0, column)
    }

    /// CUU.
    #[inline]
    pub fn move_up(&mut self, lines: usize) {
        let Point { line, column } = self.grid.cursor.point;
        self.goto((line - lines).0, column.0)
    }

    /// CUD.
    #[inline]
    pub fn move_down(&mut self, lines: usize) {
        let Point { line, column } = self.grid.cursor.point;
        self.goto((line + lines).0, column.0)
    }

    /// CNL.
    #[inline]
    pub fn move_down_and_cr(&mut self, lines: usize) {
        self.goto((self.grid.cursor.point.line + lines).0, 0)
    }

    /// CPL.
    #[inline]
    pub fn move_up_and_cr(&mut self, lines: usize) {
        self.goto((self.grid.cursor.point.line - lines).0, 0)
    }

    /// CUF: right along the line, stopping at its last column.
    #[inline]
    pub fn move_forward(&mut self, columns: usize) {
        let from = self.grid.cursor.point.column;
        let to = (from + columns).min(self.last_column());
        self.slide(to);
    }

    /// CUB: left along the line, stopping at its first column.
    #[inline]
    pub fn move_backward(&mut self, columns: usize) {
        let from = self.grid.cursor.point.column;
        let to = from.saturating_sub(columns);
        self.slide(Column(to));
    }

    /// BS.
    #[inline]
    pub fn backspace(&mut self) {
        let column = self.grid.cursor.point.column.0;
        if column > 0 {
            self.slide(Column(column - 1));
        }
    }

    /// CR.
    #[inline]
    pub fn carriage_return(&mut self) {
        self.slide(Column(0));
    }

    /// LF: down a line, scrolling the region up from its bottom line. Below
    /// the region the cursor stops at the bottom of the screen.
    #[inline]
    pub fn linefeed(&mut self) {
        let next = self.grid.cursor.point.line + 1;
        if next == self.scroll_region.end {
            self.scroll_up(1);
        } else if next < self.screen_lines() {
            self.step_to(next);
        }
    }

    /// RI: up a line, scrolling the region down from its top line.
    #[inline]
    pub fn reverse_index(&mut self) {
        let line = self.grid.cursor.point.line;
        if line == self.scroll_region.start {
            self.scroll_down(1);
        } else {
            self.step_to((line - 1i32).max(Line(0)));
        }
    }

    /// DECSC: the cursor with its style and character sets.
    #[inline]
    pub fn save_cursor_position(&mut self) {
        self.grid.saved_cursor = self.grid.cursor.clone();
    }

    /// DECRC.
    #[inline]
    pub fn restore_cursor_position(&mut self) {
        self.damage_cursor();
        self.grid.cursor = self.grid.saved_cursor.clone();
        self.damage_cursor();
    }

    /// DECSCUSR; None for the style the app gives.
    #[inline]
    pub fn set_cursor_style(&mut self, style: Option<CursorStyle>) {
        self.cursor_style = style;
        self.event_proxy.send_event(Event::CursorBlinkingChange);
    }

    /// The shape alone, as iTerm2's OSC 50 gives it.
    #[inline]
    pub fn set_cursor_shape(&mut self, shape: CursorShape) {
        self.cursor_style.get_or_insert(self.options.cursor).shape = shape;
    }

    /// SGR: a change to the style of what is written next.
    #[inline]
    pub fn terminal_attribute(&mut self, attr: Attr) {
        let template = &mut self.grid.cursor.template;
        let (set, clear) = match attr {
            Attr::Foreground(color) => return template.set_fg(color),
            Attr::Background(color) => return template.set_bg(color),
            Attr::UnderlineColor(color) => return template.set_underline_color(color),
            Attr::Reset => {
                template.set_fg(Color::Named(NamedColor::Foreground));
                template.set_bg(Color::Named(NamedColor::Background));
                template.set_underline_color(None);
                (Flags::empty(), Flags::all())
            },
            Attr::Bold => (Flags::BOLD, Flags::empty()),
            Attr::Dim => (Flags::DIM, Flags::empty()),
            Attr::Italic => (Flags::ITALIC, Flags::empty()),
            Attr::Reverse => (Flags::INVERSE, Flags::empty()),
            Attr::Hidden => (Flags::HIDDEN, Flags::empty()),
            Attr::Strike => (Flags::STRIKEOUT, Flags::empty()),
            Attr::Underline => (Flags::UNDERLINE, Flags::ALL_UNDERLINES),
            Attr::DoubleUnderline => (Flags::DOUBLE_UNDERLINE, Flags::ALL_UNDERLINES),
            Attr::Undercurl => (Flags::UNDERCURL, Flags::ALL_UNDERLINES),
            Attr::DottedUnderline => (Flags::DOTTED_UNDERLINE, Flags::ALL_UNDERLINES),
            Attr::DashedUnderline => (Flags::DASHED_UNDERLINE, Flags::ALL_UNDERLINES),
            Attr::CancelBold => (Flags::empty(), Flags::BOLD),
            Attr::CancelBoldDim => (Flags::empty(), Flags::BOLD | Flags::DIM),
            Attr::CancelItalic => (Flags::empty(), Flags::ITALIC),
            Attr::CancelUnderline => (Flags::empty(), Flags::ALL_UNDERLINES),
            Attr::CancelReverse => (Flags::empty(), Flags::INVERSE),
            Attr::CancelHidden => (Flags::empty(), Flags::HIDDEN),
            Attr::CancelStrike => (Flags::empty(), Flags::STRIKEOUT),
            // Text does not blink here.
            Attr::BlinkSlow | Attr::BlinkFast | Attr::CancelBlink => return,
        };
        template.flags = (template.flags - clear) | set;
    }

    /// OSC 8: the link what is written next has, if any.
    #[inline]
    pub fn set_hyperlink(&mut self, hyperlink: Option<Hyperlink>) {
        self.grid.cursor.template.set_hyperlink(hyperlink.map(Into::into));
    }

    /// To `line` and `column`, ending a pending wrap.
    #[inline]
    fn place(&mut self, line: Line, column: Column) {
        self.damage_cursor();
        self.grid.cursor.point = Point::new(line, column);
        self.damage_cursor();
        self.grid.cursor.input_needs_wrap = false;
    }

    /// To `line` in the same column; a pending wrap stays.
    #[inline]
    fn step_to(&mut self, line: Line) {
        self.damage_cursor();
        self.grid.cursor.point.line = line;
        self.damage_cursor();
    }

    /// Along the cursor's line to `to`, marking the line changed.
    #[inline]
    fn slide(&mut self, to: Column) {
        let line = self.grid.cursor.point.line.0 as usize;
        self.damage.damage_line(line);
        self.grid.cursor.point.column = to;
        self.grid.cursor.input_needs_wrap = false;
    }
}

impl<T> Term<T> {
    /// Marks the cursor's line changed.
    #[inline]
    pub(super) fn damage_cursor(&mut self) {
        let line = self.grid.cursor.point.line.0 as usize;
        self.damage.damage_line(line);
    }
}
