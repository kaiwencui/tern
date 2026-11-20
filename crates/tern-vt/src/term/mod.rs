use std::mem;
use std::ops::Range;

use bitflags::bitflags;

use crate::ansi::{CharsetIndex, CursorStyle, KeyboardModes};
use crate::engine::Options;
use crate::event::{Event, EventListener};
use crate::grid::{Dimensions, Grid, Scroll};
use crate::index::{Column, Line, Point};
use crate::selection::{Selection, SelectionRange, SelectionType};
use crate::term::cell::Flags;
use crate::term::color::Colors;
use crate::term::screen::TabStops;

pub mod cell;
pub mod color;
mod cursor;
mod erase;
mod handler;
mod report;
mod screen;
pub mod search;
mod text;
mod words;

pub const SEMANTIC_ESCAPE_CHARS: &str = ",│`|:\"' ()[]{}<>\t";

pub(crate) const TITLE_STACK_MAX_DEPTH: usize = 4096;
pub(crate) const KEYBOARD_MODE_STACK_MAX_DEPTH: usize = 4096;
/// Version number reported in secondary device attributes (DA2, `\e[>c`).
pub(crate) const REPORTED_VERSION: usize = 2600;

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct TermMode: u32 {
        const SHOW_CURSOR             = 1 << 0;
        const APP_CURSOR              = 1 << 1;
        const APP_KEYPAD              = 1 << 2;
        const MOUSE_REPORT_CLICK      = 1 << 3;
        const BRACKETED_PASTE         = 1 << 4;
        const SGR_MOUSE               = 1 << 5;
        const MOUSE_MOTION            = 1 << 6;
        const LINE_WRAP               = 1 << 7;
        const LINE_FEED_NEW_LINE      = 1 << 8;
        const ORIGIN                  = 1 << 9;
        const INSERT                  = 1 << 10;
        const FOCUS_IN_OUT            = 1 << 11;
        const ALT_SCREEN              = 1 << 12;
        const MOUSE_DRAG              = 1 << 13;
        const MOUSE_MODE              = Self::MOUSE_REPORT_CLICK.bits()
                                      | Self::MOUSE_MOTION.bits()
                                      | Self::MOUSE_DRAG.bits();
        const UTF8_MOUSE              = 1 << 14;
        const ALTERNATE_SCROLL        = 1 << 15;
        const URGENCY_HINTS           = 1 << 16;
        const DISAMBIGUATE_ESC_CODES  = 1 << 17;
        const REPORT_EVENT_TYPES      = 1 << 18;
        const REPORT_ALTERNATE_KEYS   = 1 << 19;
        const REPORT_ALL_KEYS_AS_ESC  = 1 << 20;
        const REPORT_ASSOCIATED_TEXT  = 1 << 21;
        const KITTY_KEYBOARD_PROTOCOL = Self::DISAMBIGUATE_ESC_CODES.bits()
                                      | Self::REPORT_EVENT_TYPES.bits()
                                      | Self::REPORT_ALTERNATE_KEYS.bits()
                                      | Self::REPORT_ALL_KEYS_AS_ESC.bits()
                                      | Self::REPORT_ASSOCIATED_TEXT.bits();
    }
}

impl Default for TermMode {
    fn default() -> TermMode {
        TermMode::SHOW_CURSOR
            | TermMode::LINE_WRAP
            | TermMode::ALTERNATE_SCROLL
            | TermMode::URGENCY_HINTS
    }
}

impl From<KeyboardModes> for TermMode {
    fn from(modes: KeyboardModes) -> TermMode {
        let mut out = TermMode::empty();
        out.set(
            TermMode::DISAMBIGUATE_ESC_CODES,
            modes.contains(KeyboardModes::DISAMBIGUATE_ESC_CODES),
        );
        out.set(
            TermMode::REPORT_EVENT_TYPES,
            modes.contains(KeyboardModes::REPORT_EVENT_TYPES),
        );
        out.set(
            TermMode::REPORT_ALTERNATE_KEYS,
            modes.contains(KeyboardModes::REPORT_ALTERNATE_KEYS),
        );
        out.set(
            TermMode::REPORT_ALL_KEYS_AS_ESC,
            modes.contains(KeyboardModes::REPORT_ALL_KEYS_AS_ESC),
        );
        out.set(
            TermMode::REPORT_ASSOCIATED_TEXT,
            modes.contains(KeyboardModes::REPORT_ASSOCIATED_TEXT),
        );
        out
    }
}

/// The lines of the screen changed since damage was last reset, counted
/// from the top of the view.
pub struct TermDamageIterator<'a> {
    lines: std::iter::Enumerate<std::slice::Iter<'a, bool>>,
    display_offset: usize,
}

impl<'a> TermDamageIterator<'a> {
    pub fn new(lines: &'a [bool], display_offset: usize) -> Self {
        let visible = &lines[..lines.len().saturating_sub(display_offset)];
        Self { lines: visible.iter().enumerate(), display_offset }
    }
}

impl Iterator for TermDamageIterator<'_> {
    type Item = usize;

    #[inline]
    fn next(&mut self) -> Option<usize> {
        self.lines.find(|&(_, &dirty)| dirty).map(|(line, _)| line + self.display_offset)
    }
}

pub enum TermDamage<'a> {
    Full,
    Partial(TermDamageIterator<'a>),
}

pub(super) struct TermDamageState {
    /// Whether each line of the screen changed.
    lines: Vec<bool>,
    full: bool,
    last_cursor: Point,
}

impl TermDamageState {
    fn new(num_lines: usize) -> Self {
        Self { lines: vec![false; num_lines], full: true, last_cursor: Point::default() }
    }

    fn resize(&mut self, num_lines: usize) {
        self.lines.clear();
        self.lines.resize(num_lines, false);
        self.full = true;
    }

    #[inline]
    pub(super) fn damage_line(&mut self, line: usize) {
        if let Some(dirty) = self.lines.get_mut(line) {
            *dirty = true;
        }
    }

    #[inline]
    fn reset(&mut self) {
        self.full = false;
        self.lines.fill(false);
    }
}

pub struct Term<T> {
    pub selection: Option<Selection>,
    grid: Grid,
    inactive_grid: Grid,
    active_charset: CharsetIndex,
    tabs: TabStops,
    mode: TermMode,
    scroll_region: Range<Line>,
    colors: Colors,
    cursor_style: Option<CursorStyle>,
    event_proxy: T,
    title: Option<String>,
    title_stack: Vec<Option<String>>,
    pub(crate) working_directory: Option<String>,
    keyboard_mode_stack: Vec<KeyboardModes>,
    inactive_keyboard_mode_stack: Vec<KeyboardModes>,
    options: Options,
    damage: TermDamageState,
    sync_update: bool,
    color_scheme_updates: bool,
}

impl<T> Term<T> {
    pub fn new<D: Dimensions>(options: Options, dimensions: &D, event_proxy: T) -> Term<T> {
        let num_cols = dimensions.columns();
        let num_lines = dimensions.screen_lines();

        let grid = Grid::with_storage(
            num_lines,
            num_cols,
            options.scrollback_lines,
            options.ring_rows,
            options.page_bytes,
        );
        let alt = Grid::with_storage(num_lines, num_cols, 0, options.ring_rows, options.page_bytes);

        let tabs = TabStops::new(num_cols);
        let scroll_region = Line(0)..Line(num_lines as i32);
        let damage = TermDamageState::new(num_lines);

        Term {
            selection: None,
            grid,
            inactive_grid: alt,
            active_charset: CharsetIndex::default(),
            tabs,
            mode: TermMode::default(),
            scroll_region,
            colors: Colors::default(),
            cursor_style: None,
            event_proxy,
            title: None,
            title_stack: Vec::new(),
            working_directory: None,
            keyboard_mode_stack: Vec::new(),
            inactive_keyboard_mode_stack: Vec::new(),
            options,
            damage,
            sync_update: false,
            color_scheme_updates: false,
        }
    }

    pub fn scroll_display(&mut self, scroll: Scroll)
    where
        T: EventListener,
    {
        let old_display_offset = self.grid.display_offset();
        self.grid.scroll_display(scroll);
        self.event_proxy.send_event(Event::MouseCursorDirty);

        if self.grid.display_offset() != old_display_offset {
            self.mark_fully_damaged();
        }
    }

    #[inline]
    pub fn sync_update(&self) -> bool {
        self.sync_update
    }

    #[inline]
    pub fn color_scheme_updates(&self) -> bool {
        self.color_scheme_updates
    }

    #[inline]
    pub(crate) fn set_sync_update(&mut self, sync: bool) {
        self.sync_update = sync;
    }

    #[inline]
    pub fn unsettled(&self) -> bool {
        if self.mode.contains(TermMode::ALT_SCREEN) {
            self.inactive_grid.unsettled()
        } else {
            self.grid.unsettled()
        }
    }

    pub fn settle(&mut self, lines: usize) -> bool {
        if self.mode.contains(TermMode::ALT_SCREEN) {
            self.inactive_grid.settle(lines)
        } else {
            self.grid.settle(lines)
        }
    }

    pub fn history_stats(&mut self) -> (usize, usize, usize) {
        if self.mode.contains(TermMode::ALT_SCREEN) {
            self.inactive_grid.history_stats()
        } else {
            self.grid.history_stats()
        }
    }

    #[inline]
    pub fn event_proxy(&self) -> &T {
        &self.event_proxy
    }

    #[must_use]
    pub fn damage(&mut self) -> TermDamage<'_> {
        if self.mode.contains(TermMode::INSERT) {
            self.mark_fully_damaged();
        }

        let previous_cursor = mem::replace(&mut self.damage.last_cursor, self.grid.cursor.point);

        if self.damage.full {
            return TermDamage::Full;
        }

        if self.damage.last_cursor != previous_cursor {
            self.damage.damage_line(previous_cursor.line.0 as usize);
        }

        self.damage_cursor();

        let display_offset = self.grid.display_offset();
        TermDamage::Partial(TermDamageIterator::new(&self.damage.lines, display_offset))
    }

    pub fn reset_damage(&mut self) {
        self.damage.reset();
    }

    #[inline]
    pub fn mark_fully_damaged(&mut self) {
        self.damage.full = true;
    }

    pub fn set_options(&mut self, options: Options)
    where
        T: EventListener,
    {
        self.options = options;

        let title_event = match &self.title {
            Some(title) => Event::Title(title.clone()),
            None => Event::ResetTitle,
        };
        self.event_proxy.send_event(title_event);

        if self.mode.contains(TermMode::ALT_SCREEN) {
            self.inactive_grid.update_history(self.options.scrollback_lines);
        } else {
            self.grid.update_history(self.options.scrollback_lines);
        }

        self.mark_fully_damaged();
    }

    pub fn selection_to_string(&self) -> Option<String> {
        let selection_range = self.selection.as_ref().and_then(|s| s.to_range(self))?;
        let SelectionRange { start, end, .. } = selection_range;

        let res = match self.selection.as_ref() {
            Some(Selection { ty: SelectionType::Block, .. }) => {
                let mut res = String::new();
                for line in (start.line.0..end.line.0).map(Line::from) {
                    res += self
                        .line_to_string(line, start.column..end.column + 1, start.column.0 != 0)
                        .trim_end();
                    res += "\n";
                }
                res += self
                    .line_to_string(end.line, start.column..end.column + 1, true)
                    .trim_end();
                res
            },
            Some(Selection { ty: SelectionType::Lines, .. }) => {
                let mut res = self.bounds_to_string(start, end);
                res.push('\n');
                res
            },
            _ => self.bounds_to_string(start, end),
        };

        Some(res)
    }

    pub fn bounds_to_string(&self, start: Point, end: Point) -> String {
        let mut res = String::new();

        for line in (start.line.0..=end.line.0).map(Line::from) {
            let start_col = if line == start.line { start.column } else { Column(0) };
            let end_col = if line == end.line { end.column + 1 } else { Column(self.columns()) };

            res += &self.line_to_string(line, start_col..end_col, line == end.line);
        }

        if res.ends_with('\n') {
            res.pop();
        }
        res
    }

    pub fn line_to_string(
        &self,
        line: Line,
        mut cols: Range<Column>,
        include_wrapped_wide: bool,
    ) -> String {
        let mut text = String::new();
        let grid_line = self.grid.line(line);
        let line_length = cols.end.min(grid_line.line_length());

        if cols.start.0 > 0
            && cols.start.0 < grid_line.len()
            && grid_line.cell(cols.start).flags().contains(Flags::WIDE_CHAR_SPACER)
        {
            cols.start -= 1;
        }

        let wide_spacers = Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER;
        let mut tab_mode = false;
        let mut last_flags = None;

        if cols.start < line_length {
            for (col, cell) in grid_line
                .cells()
                .take(line_length.0)
                .enumerate()
                .skip(cols.start.0)
            {
                last_flags = Some(cell.flags());

                if tab_mode {
                    if self.tabs[Column(col)] || cell.c() != ' ' {
                        tab_mode = false;
                    } else {
                        continue;
                    }
                }

                if cell.c() == '\t' {
                    tab_mode = true;
                }

                if !cell.flags().intersects(wide_spacers) {
                    text.push(cell.c());
                    if cell.has_extra() {
                        text.extend(cell.zerowidth());
                    }
                }
            }
        }

        let end_flags = match last_flags {
            Some(flags) => flags,
            None if line_length.0 > 0 => grid_line.cell(line_length - 1).flags(),
            None => Flags::empty(),
        };

        if line_length == self.columns()
            && line + 1 < self.screen_lines()
            && end_flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
            && include_wrapped_wide
        {
            text.push(self.grid.line(line + 1).cell(Column(0)).c());
        }

        if cols.end >= self.columns()
            && (text.is_empty() || !end_flags.contains(Flags::WRAPLINE))
        {
            text.push('\n');
        }

        text
    }

    #[inline]
    pub fn grid(&self) -> &Grid {
        &self.grid
    }

    #[inline]
    pub fn grid_mut(&mut self) -> &mut Grid {
        &mut self.grid
    }

    pub fn resize<S: Dimensions>(&mut self, size: S) {
        let old_cols = self.columns();
        let old_lines = self.screen_lines();

        let num_cols = size.columns();
        let num_lines = size.screen_lines();

        if old_cols == num_cols && old_lines == num_lines {
            return;
        }
        if num_cols == 0 || num_lines == 0 {
            return;
        }

        let history_size = self.history_size();
        let line_shift = (num_lines as i32 - old_lines as i32)
            .max((num_lines as i32 - self.grid.cursor.point.line.0 - 1).min(0));
        let delta = line_shift.min(history_size as i32);

        let is_alt = self.mode.contains(TermMode::ALT_SCREEN);
        self.grid.resize(!is_alt, num_lines, num_cols);
        self.inactive_grid.resize(is_alt, num_lines, num_cols);

        if num_cols == old_cols {
            // The anchors still refer to the screen before the resize.
            let region = Line(0)..Line(old_lines as i32);
            self.selection = self.selection.take().and_then(|s| s.rotate(self, &region, -delta));
        } else {
            self.selection = None;
            self.tabs.resize(num_cols);
        }

        self.scroll_region = Line(0)..Line(num_lines as i32);
        self.damage.resize(num_lines);
    }

    #[inline]
    pub fn mode(&self) -> &TermMode {
        &self.mode
    }

    #[inline]
    pub fn cursor_style(&self) -> CursorStyle {
        self.cursor_style.unwrap_or(self.options.cursor)
    }

    #[inline]
    pub fn colors(&self) -> &Colors {
        &self.colors
    }
}

impl<T> Dimensions for Term<T> {
    #[inline]
    fn columns(&self) -> usize {
        self.grid.columns()
    }

    #[inline]
    fn screen_lines(&self) -> usize {
        self.grid.screen_lines()
    }

    #[inline]
    fn total_lines(&self) -> usize {
        self.grid.total_lines()
    }
}

pub mod test {
    use crate::grid::Dimensions;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct TermSize {
        pub columns: usize,
        pub screen_lines: usize,
    }

    impl TermSize {
        pub fn new(columns: usize, screen_lines: usize) -> Self {
            Self { columns, screen_lines }
        }
    }

    impl Dimensions for TermSize {
        fn total_lines(&self) -> usize {
            self.screen_lines
        }

        fn screen_lines(&self) -> usize {
            self.screen_lines
        }

        fn columns(&self) -> usize {
            self.columns
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use crate::ansi::{CursorShape, KeyboardModes, Mode, NamedMode};
    use crate::engine::Options;
    use crate::event::{Event, EventListener, VoidListener};
    use crate::grid::Scroll;
    use crate::index::{Column, Line, Point, Side};
    use crate::parser::Parser;
    use crate::selection::{Selection, SelectionType};
    use crate::term::test::TermSize;
    use crate::term::{KEYBOARD_MODE_STACK_MAX_DEPTH, TITLE_STACK_MAX_DEPTH, Term, TermDamage, TermMode};

    #[derive(Default)]
    struct RecordedEvents(RefCell<Vec<Event>>);

    impl EventListener for RecordedEvents {
        fn send_event(&self, event: Event) {
            self.0.borrow_mut().push(event);
        }
    }

    #[test]
    fn damage_tracking_covers_partial_insert_and_scrollback_offset() {
        let options = Options { scrollback_lines: 20, ..Options::default() };
        let mut term = Term::new(options, &TermSize::new(10, 4), VoidListener);

        assert!(matches!(term.damage(), TermDamage::Full));
        term.reset_damage();

        Parser::new().advance(&mut term, b"\x1b[2;4Hx");
        let damaged: Vec<usize> = match term.damage() {
            TermDamage::Full => panic!("expected partial damage"),
            TermDamage::Partial(iter) => iter.collect(),
        };
        assert_eq!(damaged, vec![0, 1]);
        term.reset_damage();

        // Insert mode forces full damage and keeps the configured cursor style.
        term.set_mode(Mode::Named(NamedMode::Insert));
        assert_eq!(term.cursor_style().shape, CursorShape::Block);
        assert!(matches!(term.damage(), TermDamage::Full));
        term.unset_mode(Mode::Named(NamedMode::Insert));
        assert_eq!(term.cursor_style().shape, CursorShape::Block);
        term.reset_damage();

        // Scroll lines into history and verify viewport offset shifts partial damage.
        Parser::new().advance(&mut term, b"\r\n1\r\n2\r\n3\r\n4\r\n5");
        let _ = term.damage();
        term.reset_damage();

        term.scroll_display(Scroll::Delta(2));
        assert!(matches!(term.damage(), TermDamage::Full));
        term.reset_damage();

        // Screen line 0 is visible at viewport row 2 when display_offset == 2;
        // screen line 3 is scrolled off the bottom of the 4-line viewport.
        term.goto(0, 1);
        let shifted: Vec<usize> = match term.damage() {
            TermDamage::Full => panic!("expected partial damage while scrolled"),
            TermDamage::Partial(iter) => iter.collect(),
        };
        assert_eq!(shifted, vec![2]);
    }

    #[test]
    fn selection_extraction_handles_tabs_wide_chars_lines_and_blocks() {
        let mut term = Term::new(Options::default(), &TermSize::new(12, 4), VoidListener);
        Parser::new().advance(&mut term, b"a\tb\r\ncd  ef\r\ngh  ij");

        let mut simple = Selection::new(SelectionType::Simple, Point::new(Line(0), Column(0)), Side::Left);
        simple.update(Point::new(Line(0), Column(8)), Side::Right);
        term.selection = Some(simple);
        assert_eq!(term.selection_to_string().as_deref(), Some("a\tb"));

        let mut lines = Selection::new(SelectionType::Lines, Point::new(Line(1), Column(2)), Side::Left);
        lines.update(Point::new(Line(1), Column(2)), Side::Right);
        term.selection = Some(lines);
        assert_eq!(term.selection_to_string().as_deref(), Some("cd  ef\n"));

        let mut block = Selection::new(SelectionType::Block, Point::new(Line(1), Column(0)), Side::Left);
        block.update(Point::new(Line(2), Column(3)), Side::Right);
        term.selection = Some(block);
        assert_eq!(term.selection_to_string().as_deref(), Some("cd\ngh"));

        // Wrapped wide char at column 4 of a 5-column terminal.
        let mut wide = Term::new(Options::default(), &TermSize::new(5, 3), VoidListener);
        Parser::new().advance(&mut wide, "abcd\u{4e2d}".as_bytes());
        let mut sel = Selection::new(SelectionType::Simple, Point::new(Line(0), Column(0)), Side::Left);
        sel.update(Point::new(Line(0), Column(4)), Side::Right);
        wide.selection = Some(sel);
        assert_eq!(wide.selection_to_string().as_deref(), Some("abcd\u{4e2d}"));
    }

    #[test]
    fn title_and_keyboard_stacks_and_set_options_behave_consistently() {
        let options = Options { scrollback_lines: 20, ..Options::default() };
        let mut term = Term::new(options, &TermSize::new(10, 3), RecordedEvents::default());

        for i in 0..(TITLE_STACK_MAX_DEPTH + 2) {
            term.set_title(Some(format!("t{i}")));
            term.push_title();
        }
        assert_eq!(term.title_stack.len(), TITLE_STACK_MAX_DEPTH);
        term.set_title(Some("top".to_owned()));
        term.pop_title();
        assert_eq!(term.title.as_deref(), Some(&*format!("t{}", TITLE_STACK_MAX_DEPTH + 1)));

        for _ in 0..(KEYBOARD_MODE_STACK_MAX_DEPTH + 2) {
            term.push_keyboard_mode(KeyboardModes::DISAMBIGUATE_ESC_CODES);
        }
        assert!(term.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES));

        term.event_proxy().0.borrow_mut().clear();
        term.set_options(options);
        assert!(term.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES));
        assert!(
            term.event_proxy()
                .0
                .borrow()
                .iter()
                .any(|e| matches!(e, Event::Title(_)))
        );
    }

    fn row_text(term: &Term<VoidListener>, line: i32) -> String {
        term.grid()[Line(line)][..].iter().map(|cell| cell.c()).collect()
    }

    fn has_wide_half(term: &Term<VoidListener>, line: i32) -> bool {
        use crate::term::cell::Flags;
        term.grid()[Line(line)][..]
            .iter()
            .any(|cell| cell.flags.intersects(Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER))
    }

    #[test]
    fn a_cell_keeps_only_so_many_characters_of_no_width() {
        use crate::term::cell::MAX_ZEROWIDTH;
        let mut term = Term::new(Options::default(), &TermSize::new(10, 3), VoidListener);
        let mut out = String::from("a");
        out.extend(std::iter::repeat_n('\u{301}', MAX_ZEROWIDTH * 4));
        Parser::new().advance(&mut term, out.as_bytes());
        let joined = term.grid()[Line(0)][Column(0)].zerowidth().map(<[char]>::len);
        assert_eq!(joined, Some(MAX_ZEROWIDTH));
    }

    #[test]
    fn a_scrolling_region_past_the_screen_is_cut_to_it_or_ignored() {
        let mut term = Term::new(Options::default(), &TermSize::new(10, 4), VoidListener);
        // Entirely below the screen: ignored, as in xterm.
        Parser::new().advance(&mut term, b"\x1b[30;40r");
        assert_eq!(term.scroll_region, Line(0)..Line(4));
        // One line once cut to the screen: ignored too.
        Parser::new().advance(&mut term, b"\x1b[4;99r");
        assert_eq!(term.scroll_region, Line(0)..Line(4));
        // A bottom past the screen is the bottom of the screen.
        Parser::new().advance(&mut term, b"\x1b[2;99r");
        assert_eq!(term.scroll_region, Line(1)..Line(4));

        // Output at the bottom still scrolls.
        let mut term = Term::new(Options::default(), &TermSize::new(10, 4), VoidListener);
        Parser::new().advance(&mut term, b"\x1b[30;40r\x1b[4Ha\r\nb\r\nc");
        assert_eq!(row_text(&term, 1).trim_end(), "a");
        assert_eq!(row_text(&term, 2).trim_end(), "b");
        assert_eq!(row_text(&term, 3).trim_end(), "c");
    }

    #[test]
    fn inserting_blanks_pushes_a_wide_character_off_whole() {
        let mut term = Term::new(Options::default(), &TermSize::new(10, 3), VoidListener);
        // The wide character ends the line; one blank at the start pushes
        // its spacer off the end. Writing in the last column after must not
        // touch the 8 beside it.
        Parser::new().advance(&mut term, "12345678\u{4e2d}\r\x1b[@\x1b[10Gx".as_bytes());
        assert_eq!(row_text(&term, 0), " 12345678x");
        assert!(!has_wide_half(&term, 0));

        // In insert mode, the same.
        let mut term = Term::new(Options::default(), &TermSize::new(10, 3), VoidListener);
        Parser::new().advance(&mut term, "12345678\u{4e2d}\r\x1b[4hy\x1b[4l\x1b[10Gx".as_bytes());
        assert_eq!(row_text(&term, 0), "y12345678x");
        assert!(!has_wide_half(&term, 0));
    }

    #[test]
    fn deleting_or_erasing_half_a_wide_character_clears_both_halves() {
        for (keys, after) in [
            // DCH on the spacer.
            ("\u{4e2d}ab\x1b[1;2H\x1b[P", " ab"),
            // DCH of the cell before it, and of half of it.
            ("a\u{4e2d}b\x1b[1;1H\x1b[2P", " b"),
            // ECH on the spacer.
            ("\u{4e2d}ab\x1b[1;2H\x1b[X", "  ab"),
            // EL to the left, ending on the wide character.
            ("\u{4e2d}ab\x1b[1;1H\x1b[1K", "  ab"),
            // ED below, from the spacer.
            ("\u{4e2d}ab\x1b[1;2H\x1b[J", ""),
        ] {
            let mut term = Term::new(Options::default(), &TermSize::new(10, 3), VoidListener);
            Parser::new().advance(&mut term, keys.as_bytes());
            assert_eq!(row_text(&term, 0).trim_end(), after, "{keys:?}");
            assert!(!has_wide_half(&term, 0), "{keys:?}");
        }
    }

    #[test]
    fn resizing_the_alternate_screen_never_leaves_half_a_wide_character() {
        let mut term = Term::new(Options::default(), &TermSize::new(10, 3), VoidListener);
        Parser::new().advance(&mut term, "\x1b[?1049h12345678\u{4e2d}".as_bytes());
        term.resize(TermSize::new(9, 3));
        assert_eq!(row_text(&term, 0), "12345678 ");
        assert!(!has_wide_half(&term, 0));
    }

    #[test]
    fn scrolling_a_region_leaves_a_view_of_the_history_where_it_was() {
        let options = Options { scrollback_lines: 20, ..Options::default() };
        let mut term = Term::new(options, &TermSize::new(10, 4), VoidListener);
        Parser::new().advance(&mut term, b"1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7\r\n8");
        term.scroll_display(Scroll::Delta(2));
        assert_eq!(term.grid().display_offset(), 2);
        // A region below the top: nothing goes into the history.
        Parser::new().advance(&mut term, b"\x1b[2;4r\x1b[4H\n\n\n");
        assert_eq!(term.grid().display_offset(), 2);
        // A region at the top of the screen does add to the history, and
        // the view moves with what it showed.
        Parser::new().advance(&mut term, b"\x1b[1;4r\x1b[4H\n");
        assert_eq!(term.grid().display_offset(), 3);
    }

    #[test]
    fn a_link_longer_than_a_page_survives_the_scrollback() {
        use crate::grid::Dimensions;
        let options = Options { scrollback_lines: 200, ring_rows: 2, page_bytes: 128, ..Options::default() };
        let mut term = Term::new(options, &TermSize::new(10, 3), VoidListener);
        let uri = format!("https://e.com/{}", "a".repeat(200_000));
        let mut parser = Parser::new();
        parser.advance(&mut term, format!("\x1b]8;;{uri}\x1b\\L\x1b]8;;\x1b\\\r\n").as_bytes());
        for _ in 0..20 {
            parser.advance(&mut term, b"x\r\n");
        }
        while term.settle(64) {}
        let history = term.grid().history_size() as i32;
        let found = (1..=history)
            .map(|back| term.grid().line(Line(-back)))
            .find_map(|line| line.cells().next().filter(|cell| cell.c() == 'L').map(|cell| cell.hyperlink_uri()));
        assert_eq!(found, Some(Some(uri)));
    }
}
