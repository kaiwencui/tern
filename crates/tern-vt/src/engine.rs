//! The terminal as Tern's session sees it, and all it sees of it: output goes
//! in, and out come the screen's rows, the cursor, the colors and modes, and
//! what the program asked for (replies, a title, the bell, the clipboard).
//! The view, the size and the selection are changed from here as well.
//!
//! `tern-core` uses this module and nothing else of the crate, so that what
//! lies behind it can change.

use std::cell::{Cell as Flag, RefCell};
use std::time::Instant;

use crate::event::EventListener;
use crate::feed::Feeder;
use crate::grid::{Cells, Dimensions, LineRef, Scroll as GridScroll};
use crate::index::Side as TermSide;
use crate::selection::{Selection, SelectionType};
use crate::term::{Term, TermDamage};
use crate::ansi::ClearMode;
use crate::parser::Parser;

pub use crate::event::{Event, Progress, WindowSize};
pub use crate::grid::PAGE_BYTES;
pub use crate::grid::CellRef as CellView;
pub use crate::feed::{BEGIN_SYNC_LEN, Turn};
pub use crate::index::{Column, Line, Point};
pub use crate::selection::SelectionRange;
pub use crate::term::cell::Flags;
pub use crate::term::color::{COUNT, Colors};
pub use crate::term::search::SearchMatch;
pub use crate::term::TermMode;
pub use crate::ansi::{Color, CursorShape, CursorStyle, NamedColor, Rgb};

/// What the terminal reported since it was last asked: events in the order
/// they came, and whether anything asked for no more than a redraw.
#[derive(Default)]
pub(crate) struct Queue {
    events: RefCell<Vec<Event>>,
    wake: Flag<bool>,
}

impl EventListener for Queue {
    fn send_event(&self, event: Event) {
        match event {
            // These need no ordering, so they are a flag rather than entries.
            Event::MouseCursorDirty | Event::CursorBlinkingChange => self.wake.set(true),
            other => self.events.borrow_mut().push(other),
        }
    }
}

/// How a terminal starts, and what it goes back to when a program resets it.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Lines kept in the scrollback.
    pub scrollback_lines: usize,
    /// The cursor until a program asks for another (DECSCUSR).
    pub cursor: CursorStyle,
    /// How many of the newest lines of the scrollback are kept as cells
    /// before they are packed, and the size in bytes of a page of the packed
    /// ones. Tests make both small, so that a short scrollback reaches
    /// across pages.
    #[doc(hidden)]
    pub ring_rows: usize,
    #[doc(hidden)]
    pub page_bytes: usize,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            scrollback_lines: 10_000,
            cursor: CursorStyle::default(),
            ring_rows: usize::MAX,
            page_bytes: crate::grid::PAGE_BYTES,
        }
    }
}

/// How to move the view through the scrollback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewScroll {
    /// Positive scrolls back into the history.
    Lines(i32),
    /// Positive scrolls back into the history.
    Pages(i32),
    Top,
    Bottom,
}

/// Which half of a cell the pointer is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionKind {
    /// Character by character.
    Simple,
    /// A rectangle.
    Block,
    /// Whole words.
    Word,
    /// Whole lines.
    Line,
}

/// One terminal: its state and the interpreter of its output.
pub struct Terminal {
    term: Term<Queue>,
    feeder: Feeder,
}

impl Terminal {
    /// A terminal of `cols` by `rows` cells (at least 2 by 1).
    pub fn new(options: &Options, cols: usize, rows: usize) -> Terminal {
        Terminal {
            term: Term::new(*options, &Size(cols.max(2), rows.max(1)), Queue::default()),
            feeder: Feeder::new(),
        }
    }

    pub fn set_options(&mut self, options: &Options) {
        self.term.set_options(*options);
    }

    // ---- Output

    /// Interprets output. With `watch`, only as far as the next beginning or
    /// end of a synchronized update: returns how many bytes were interpreted
    /// and which of the two stopped it. Without it, all of the output is
    /// interpreted.
    pub fn advance_to_turn(&mut self, bytes: &[u8], watch: bool) -> (usize, Turn) {
        self.feeder.advance_to_turn(&mut self.term, bytes, watch)
    }

    /// Interprets all of the output.
    pub fn advance(&mut self, bytes: &[u8]) {
        let mut at = 0;
        while at < bytes.len() {
            at += self.advance_to_turn(&bytes[at..], false).0;
        }
    }

    /// Writes text as if the program had printed it, through an interpreter
    /// of its own: whatever the program's output was in the middle of, it is
    /// not disturbed, and the text cannot leave anything unfinished.
    pub fn show_message(&mut self, bytes: &[u8]) {
        Parser::new().advance(&mut self.term, bytes);
    }

    /// Whether a synchronized update (private mode 2026) is under way.
    pub fn sync_update(&self) -> bool {
        self.term.sync_update()
    }

    /// Whether color-scheme update notifications (private mode 2031) are enabled.
    pub fn color_scheme_updates(&self) -> bool {
        self.term.color_scheme_updates()
    }

    /// When the synchronized update under way is to be ended for lasting too
    /// long, as of the last output interpreted.
    pub fn sync_deadline(&self) -> Option<Instant> {
        self.feeder.sync_deadline()
    }

    /// Ends the synchronized update under way, if there is one.
    pub fn stop_sync(&mut self) {
        self.feeder.stop_sync(&mut self.term);
    }

    /// Whether a synchronized update has ended since this was last asked.
    pub fn take_ended(&mut self) -> bool {
        self.feeder.take_ended()
    }

    // ---- What it reported

    /// Moves the events reported since the last call into `into` (which must
    /// be empty), keeping both vectors' capacity.
    pub fn take_events(&mut self, into: &mut Vec<Event>) {
        std::mem::swap(&mut *self.term.event_proxy().events.borrow_mut(), into);
    }

    /// Whether anything asked for a redraw since this was last asked.
    pub fn take_wake(&mut self) -> bool {
        self.term.event_proxy().wake.replace(false)
    }

    // ---- State

    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    pub fn columns(&self) -> usize {
        self.term.columns()
    }

    pub fn screen_lines(&self) -> usize {
        self.term.screen_lines()
    }

    /// How many lines the view is scrolled back.
    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    pub fn history_size(&self) -> usize {
        self.term.history_size()
    }

    /// The colors programs have set, by slot; None where the theme's apply.
    pub fn colors(&self) -> &Colors {
        self.term.colors()
    }

    pub fn cursor_style(&self) -> CursorStyle {
        self.term.cursor_style()
    }

    /// Sets the cursor a program asked for, or with None the default one.
    pub fn set_cursor_style(&mut self, style: Option<CursorStyle>) {
        self.term.set_cursor_style(style);
    }

    /// Where the cursor is, in lines of the screen (not of the view), and
    /// whether it is on the second half of a wide character.
    pub fn cursor(&self) -> (Point, bool) {
        let point = self.term.grid().cursor.point;
        let spacer = self.term.grid()[point].flags.contains(Flags::WIDE_CHAR_SPACER);
        (point, spacer)
    }

    /// Marks in `dirty` (a flag per row of the view) the rows changed since
    /// the last call, and forgets the changes.
    pub fn take_damage(&mut self, dirty: &mut [bool]) {
        match self.term.damage() {
            TermDamage::Full => dirty.fill(true),
            TermDamage::Partial(lines) => {
                for line in lines {
                    if let Some(d) = dirty.get_mut(line) {
                        *d = true;
                    }
                }
            },
        }
        self.term.reset_damage();
    }

    /// Row `row` of the view, counted from its top.
    #[inline]
    pub fn visible_row(&self, row: usize) -> RowView<'_> {
        let line = Line(row as i32 - self.display_offset() as i32);
        RowView { cells: self.term.grid().line(line), line }
    }

    /// The visible screen as text, one line per row, trailing blanks removed.
    pub fn screen_text(&self) -> String {
        let mut out = String::new();
        for row in 0..self.screen_lines() {
            let start = out.len();
            for cell in self.visible_row(row).cells() {
                if cell.flags().intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                out.push(if cell.c() == '\t' { ' ' } else { cell.c() });
                if cell.has_extra() {
                    out.extend(cell.zerowidth());
                }
            }
            let trimmed = out[start..].trim_end_matches(' ').len();
            out.truncate(start + trimmed);
            out.push('\n');
        }
        out
    }

    /// The OSC 8 link target at a cell of the view.
    pub fn hyperlink_at(&self, row: usize, col: usize) -> Option<String> {
        if row >= self.screen_lines() || col >= self.columns() {
            return None;
        }
        let line = Line(row as i32 - self.display_offset() as i32);
        let cell = self.term.grid().line(line).cell(Column(col));
        if cell.has_extra() { cell.hyperlink_uri() } else { None }
    }

    /// The working directory path reported by the shell via OSC 7, if any.
    pub fn working_directory(&self) -> Option<&str> {
        self.term.working_directory.as_deref()
    }

    // ---- Changes from the app's side

    /// Changes the size, rewrapping the lines.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.term.resize(Size(cols.max(2), rows.max(1)));
    }

    pub fn scroll(&mut self, scroll: ViewScroll) {
        // The grid adds a delta to its offset without checking for overflow;
        // no useful delta is longer than the history.
        let limit = self.term.history_size().min(i32::MAX as usize) as i32;
        let scroll = match scroll {
            ViewScroll::Lines(n) => GridScroll::Delta(n.clamp(-limit, limit)),
            ViewScroll::Pages(n) => GridScroll::Delta(
                n.saturating_mul(self.term.screen_lines() as i32).clamp(-limit, limit),
            ),
            ViewScroll::Top => GridScroll::Top,
            ViewScroll::Bottom => GridScroll::Bottom,
        };
        self.term.scroll_display(scroll);
    }

    /// Starts a selection at a cell of the view. Rows outside the view are
    /// clamped to its edge.
    pub fn selection_start(&mut self, row: i32, col: usize, side: Side, kind: SelectionKind) {
        let ty = match kind {
            SelectionKind::Simple => SelectionType::Simple,
            SelectionKind::Block => SelectionType::Block,
            SelectionKind::Word => SelectionType::Semantic,
            SelectionKind::Line => SelectionType::Lines,
        };
        let (point, side) = self.view_point(row, col, side);
        self.term.selection = Some(Selection::new(ty, point, side));
    }

    /// Moves the selection's moving end. Rows outside the view are clamped
    /// to its edge.
    pub fn selection_update(&mut self, row: i32, col: usize, side: Side) {
        let (point, side) = self.view_point(row, col, side);
        if let Some(selection) = self.term.selection.as_mut() {
            selection.update(point, side);
        }
    }

    pub fn selection_clear(&mut self) {
        self.term.selection = None;
    }

    /// Selects the whole scrollback and screen.
    pub fn select_all(&mut self) {
        let start = Point::new(self.term.topmost_line(), Column(0));
        let end = Point::new(self.term.bottommost_line(), self.term.last_column());
        let mut selection = Selection::new(SelectionType::Simple, start, TermSide::Left);
        selection.update(end, TermSide::Right);
        self.term.selection = Some(selection);
    }

    pub fn has_selection(&self) -> bool {
        self.term.selection.as_ref().is_some_and(|s| !s.is_empty())
    }

    /// The selected text; None for none.
    pub fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string().filter(|s| !s.is_empty())
    }

    /// The cells selected, in lines of the screen.
    pub fn selection_range(&self) -> Option<SelectionRange> {
        self.term.selection.as_ref().and_then(|s| s.to_range(&self.term))
    }

    /// Case-insensitive substring matches from oldest scrollback to bottom of
    /// screen, spanning soft-wrapped lines.
    pub fn search(&self, query: &str) -> Vec<SearchMatch> {
        self.term.search(query)
    }

    /// Scrolls the viewport minimally so `start..=end` is visible, centering it
    /// when outside the current view, and selects `start..=end`.
    pub fn focus_match(&mut self, m: SearchMatch) {
        let rows = self.term.screen_lines() as i32;
        let offset = self.term.grid().display_offset() as i32;
        let view_top = -offset;
        let view_bottom = rows - 1 - offset;
        if m.start.line.0 < view_top || m.end.line.0 > view_bottom {
            let max_hist = self.term.history_size() as i32;
            let wanted = ((rows / 2) - m.start.line.0).clamp(0, max_hist);
            let delta = wanted - offset;
            if delta != 0 {
                self.term.scroll_display(GridScroll::Delta(delta));
            }
        }
        let mut selection = Selection::new(SelectionType::Simple, m.start, TermSide::Left);
        selection.update(m.end, TermSide::Right);
        self.term.selection = Some(selection);
    }

    /// Scrolls to the previous (`delta < 0`) or next (`delta > 0`) OSC 133
    /// prompt line relative to the top of the current viewport.
    pub fn jump_prompt(&mut self, delta: i32) -> bool {
        use crate::grid::row::MARK_PROMPT;
        let offset = self.term.grid().display_offset() as i32;
        let view_top = -offset;
        let top = self.term.topmost_line().0;
        let bottom = self.term.bottommost_line().0;
        let max_hist = self.term.history_size() as i32;

        if delta < 0 {
            let mut raw = view_top - 1;
            while raw >= top {
                if self.term.grid().line(Line(raw)).mark() & MARK_PROMPT != 0 {
                    let wanted = (-raw).clamp(0, max_hist);
                    if wanted != offset {
                        self.term.scroll_display(GridScroll::Delta(wanted - offset));
                        return true;
                    }
                }
                raw -= 1;
            }
            false
        } else {
            let mut raw = view_top + 1;
            while raw <= bottom {
                if self.term.grid().line(Line(raw)).mark() & MARK_PROMPT != 0 {
                    let wanted = (-raw).clamp(0, max_hist);
                    if wanted != offset {
                        self.term.scroll_display(GridScroll::Delta(wanted - offset));
                        return true;
                    }
                }
                raw += 1;
            }
            if offset > 0 {
                self.term.scroll_display(GridScroll::Bottom);
                return true;
            }
            false
        }
    }

    /// Selects the output produced by the most recent OSC 133;C command and
    /// scrolls its start into view.
    pub fn select_last_output(&mut self) -> bool {
        use crate::grid::row::{MARK_OUTPUT, MARK_PROMPT};
        let top = self.term.topmost_line().0;
        let bottom = self.term.bottommost_line().0;
        let mut cmd_line = None;
        let mut raw = bottom;
        while raw >= top {
            if self.term.grid().line(Line(raw)).mark() & MARK_OUTPUT != 0 {
                cmd_line = Some(raw);
                break;
            }
            raw -= 1;
        }
        let Some(cmd_raw) = cmd_line else {
            return false;
        };
        let cmd_mark = self.term.grid().line(Line(cmd_raw)).mark();
        let start_raw = if cmd_mark & MARK_PROMPT != 0 {
            cmd_raw + 1
        } else {
            cmd_raw
        };
        if start_raw > bottom {
            return false;
        }
        let mut end_raw = start_raw;
        let mut r = start_raw;
        while r <= bottom {
            if r > cmd_raw && self.term.grid().line(Line(r)).mark() & MARK_PROMPT != 0 {
                break;
            }
            end_raw = r;
            r += 1;
        }
        while end_raw > start_raw && self.term.grid().line(Line(end_raw)).line_length().0 == 0 {
            end_raw -= 1;
        }
        if start_raw == end_raw && self.term.grid().line(Line(start_raw)).line_length().0 == 0 {
            return false;
        }
        let m = SearchMatch {
            start: Point::new(Line(start_raw), Column(0)),
            end: Point::new(Line(end_raw), self.term.last_column()),
        };
        self.focus_match(m);
        true
    }

    /// Tells the terminal whether it has the keyboard focus. Returns the
    /// report to send the program, if it asked for them.
    pub fn set_focused(&mut self, focused: bool) -> Option<&'static [u8]> {
        self.term
            .mode()
            .contains(TermMode::FOCUS_IN_OUT)
            .then_some(if focused { b"\x1b[I" } else { b"\x1b[O" })
    }

    /// Clears the screen and drops the scrollback, the cursor going to the
    /// top left. Returns false, having done nothing, on the alternate
    /// screen, which belongs to the program.
    pub fn clear_all(&mut self) -> bool {
        if self.term.mode().contains(TermMode::ALT_SCREEN) {
            return false;
        }
        self.term.clear_screen(ClearMode::All);
        self.term.clear_screen(ClearMode::Saved);
        self.term.goto(0, 0);
        true
    }

    // ---- The scrollback

    /// Whether lines of the scrollback that output left as cells wait to be
    /// packed: see [Terminal::settle].
    #[inline]
    pub fn unsettled(&self) -> bool {
        self.term.unsettled()
    }

    /// Packs up to `lines` of the lines of the scrollback that output left
    /// as cells, all but the few dozen newest, and once they are packed gives
    /// back the memory they took. Returns whether there is more to do. For
    /// once output has stopped: while it pours in, lines that leave the
    /// scrollback before it stops are never packed.
    pub fn settle(&mut self, lines: usize) -> bool {
        self.term.settle(lines)
    }

    /// How many lines the scrollback has, how many of them are packed, and
    /// how many bytes the packed ones take.
    #[doc(hidden)]
    pub fn history_stats(&mut self) -> (usize, usize, usize) {
        self.term.history_stats()
    }

    fn view_point(&self, row: i32, col: usize, side: Side) -> (Point, TermSide) {
        let rows = self.term.screen_lines() as i32;
        let last_col = self.term.last_column();
        let side = match side {
            Side::Left => TermSide::Left,
            Side::Right => TermSide::Right,
        };
        let (row, col, side) = if row < 0 {
            (0, Column(0), TermSide::Left)
        } else if row >= rows {
            (rows - 1, last_col, TermSide::Right)
        } else {
            (row, Column(col).min(last_col), side)
        };
        (Point::new(Line(row - self.display_offset() as i32), col), side)
    }
}

/// One row of the view.
#[derive(Clone, Copy)]
pub struct RowView<'a> {
    cells: LineRef<'a>,
    line: Line,
}

impl<'a> RowView<'a> {
    /// The row's line, as selections count them.
    #[inline]
    pub fn line(&self) -> Line {
        self.line
    }

    /// The row's cells, from the first column.
    #[inline]
    pub fn cells(&self) -> Cells<'a> {
        self.cells.cells()
    }
}

/// How many cells with extras there are in every terminal of the process,
/// for tests that count them.
#[doc(hidden)]
pub fn live_extras() -> usize {
    crate::term::cell::live_extras()
}

struct Size(usize, usize);

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.1
    }

    fn screen_lines(&self) -> usize {
        self.1
    }

    fn columns(&self) -> usize {
        self.0
    }
}


#[cfg(test)]
mod tests {
    use super::{Options, Terminal};
    use crate::grid::Dimensions;
    use crate::index::Line;
    use crate::term::cell::Flags;

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    /// Every wide character on the screen has its spacer after it, and
    /// every spacer its wide character before it.
    fn wide_characters_whole(terminal: &Terminal) -> Result<(), String> {
        let term = &terminal.term;
        let columns = term.columns();
        for line in 0..term.screen_lines() {
            let row = &term.grid()[Line(line as i32)][..];
            for (at, cell) in row.iter().enumerate() {
                let wide = cell.flags.contains(Flags::WIDE_CHAR);
                let spacer = cell.flags.contains(Flags::WIDE_CHAR_SPACER);
                if wide && !(at + 1 < columns && row[at + 1].flags.contains(Flags::WIDE_CHAR_SPACER)) {
                    return Err(format!("line {line}, column {at}: a wide character without its spacer"));
                }
                if spacer && !(at > 0 && row[at - 1].flags.contains(Flags::WIDE_CHAR)) {
                    return Err(format!("line {line}, column {at}: a spacer without its wide character"));
                }
            }
        }
        Ok(())
    }

    /// Wide characters cut by every kind of edit and resize, through the
    /// way the session gives output: no half of one is ever left alone.
    #[test]
    fn edits_and_resizes_never_leave_half_a_wide_character() {
        const PIECES: [&str; 22] = [
            "\u{4e2d}", "\u{4e2d}\u{6587}", "\u{1f600}", "ab", "x", "\u{301}", " ",
            "\x1b[@", "\x1b[2@", "\x1b[P", "\x1b[3P", "\x1b[X", "\x1b[2X",
            "\x1b[K", "\x1b[1K", "\x1b[J", "\x1b[1J", "\x1b[4h", "\x1b[4l", "\r\n", "\x1b[?1049h", "\x1b[?1049l",
        ];
        let options = Options { scrollback_lines: 20, ..Options::default() };
        for seed in 1..=300u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
            let (mut cols, mut rows) = (rng.below(12) + 2, rng.below(5) + 1);
            let mut terminal = Terminal::new(&options, cols, rows);
            let mut log = Vec::new();
            for _ in 0..200 {
                let step = match rng.below(8) {
                    0 => format!("\x1b[{};{}H", rng.below(rows) + 1, rng.below(cols) + 1),
                    1 if rng.below(4) == 0 => {
                        (cols, rows) = (rng.below(12) + 2, rng.below(5) + 1);
                        terminal.resize(cols, rows);
                        log.push(format!("resize {cols}x{rows}"));
                        continue;
                    },
                    _ => PIECES[rng.below(PIECES.len())].to_owned(),
                };
                terminal.advance(step.as_bytes());
                log.push(format!("{step:?}"));
                if let Err(what) = wide_characters_whole(&terminal) {
                    panic!("seed {seed}: {what}, after {}", log.join(" "));
                }
            }
        }
    }
}
