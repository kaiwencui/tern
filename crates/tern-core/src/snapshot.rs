//! Copying the emulator state into a [Frame]: every color resolved to RGB,
//! only rows that changed rewritten, and no allocation once the buffers have
//! grown to size.

use tern_types::{flags, Cell, CursorDraw, CursorShape, ExtraRef, Frame, Rgb, Theme};
use tern_vt::engine::{
    CellView, Column, Color, Colors, CursorShape as EngineShape, Flags, Line,
    Point, NamedColor, Rgb as EngineRgb, SelectionRange, TermMode, Terminal, COUNT,
};

use std::sync::atomic::{AtomicU64, Ordering};

/// Source of frame seq values, shared by every session in the process so no
/// two frames are ever stamped with the same seq. That is what lets a session
/// recognize the frame it filled last, even when the app moves frames between
/// panes.
static NEXT_SEQ: AtomicU64 = AtomicU64::new(1);

const FOREGROUND: usize = NamedColor::Foreground as usize;
const BACKGROUND: usize = NamedColor::Background as usize;
const CURSOR: usize = NamedColor::Cursor as usize;

/// Everything behind the terminal lock: the emulator, the colors it starts
/// from, and what the last snapshot looked like.
pub(crate) struct TermState {
    pub(crate) term: Terminal,
    pub(crate) theme: Theme,
    snap: SnapState,
    /// The screen as it was at a moment when no synchronized update was
    /// under way, put aside by the interpreter for the app to show while
    /// one is: see [put_aside].
    aside: Frame,
    /// `aside` is newer than any frame filled from the terminal itself.
    aside_newer: bool,
}

impl TermState {
    pub(crate) fn new(term: Terminal, theme: Theme) -> TermState {
        TermState {
            term,
            theme,
            snap: SnapState::default(),
            aside: Frame::default(),
            aside_newer: false,
        }
    }

    /// The current value of a color slot (0..=255 palette, 256 foreground,
    /// 257 background, 258 cursor), as a program asking for it should see it.
    pub(crate) fn color(&self, index: usize) -> EngineRgb {
        let rgb = self.term.colors()[index]
            .map(pack)
            .unwrap_or_else(|| default_color(&self.theme, index));
        EngineRgb {
            r: (rgb >> 16) as u8,
            g: (rgb >> 8) as u8,
            b: rgb as u8,
        }
    }
}

struct SnapState {
    /// The seq of the frame this state last filled. Any other frame (or one
    /// the caller changed) gets rewritten completely.
    last_seq: Option<u64>,
    display_offset: usize,
    selection: Option<SelectionRange>,
    palette: [Rgb; COUNT],
    /// The theme settings besides the palette that change how cells look.
    looks: CellLooks,
    dirty: Vec<bool>,
    new_refs: Vec<ExtraRef>,
    new_chars: Vec<char>,
    spare_refs: Vec<ExtraRef>,
    spare_chars: Vec<char>,
}

impl Default for SnapState {
    fn default() -> SnapState {
        SnapState {
            last_seq: None,
            display_offset: 0,
            selection: None,
            palette: [0; COUNT],
            looks: CellLooks::default(),
            dirty: Vec::new(),
            new_refs: Vec::new(),
            new_chars: Vec::new(),
            spare_refs: Vec::new(),
            spare_chars: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct CellLooks {
    bold_is_bright: bool,
    selection_bg: Option<Rgb>,
    selection_fg: Option<Rgb>,
}

/// Brings the frame up to date with what there is to show. Returns true
/// when anything in it changed.
///
/// While a synchronized update is under way the terminal itself is not to
/// be shown: the program has asked for its redraw to appear all at once.
/// What there is to show then is the screen the interpreter last put aside,
/// if it has put one aside since this frame was filled, and otherwise what
/// the frame holds already. The second value returned says that an update
/// kept the terminal from being looked at.
pub(crate) fn show(state: &mut TermState, frame: &mut Frame) -> (bool, bool) {
    if !state.term.sync_update() {
        state.aside_newer = false;
        return (fill(state, frame), false);
    }
    if !std::mem::take(&mut state.aside_newer) {
        return (false, true);
    }
    (copy(&state.aside, frame), true)
}

/// Puts the screen as it is now aside, for [show] to hand out while a
/// synchronized update is under way. Called by the interpreter between two
/// updates, or where one begins: at moments the app could have looked at
/// the terminal, had it come by.
pub(crate) fn put_aside(state: &mut TermState) {
    let mut aside = std::mem::take(&mut state.aside);
    fill(state, &mut aside);
    state.aside = aside;
    state.aside_newer = true;
}

/// Makes `frame` show what `from` shows. Rows that differ count as changed,
/// and no others. Returns true when anything changed.
fn copy(from: &Frame, frame: &mut Frame) -> bool {
    let mut changed = false;
    let mut all = false;
    if frame.cols != from.cols
        || frame.rows != from.rows
        || frame.cells.len() != from.cells.len()
        || frame.row_seq.len() != from.row_seq.len()
    {
        frame.resize(from.cols, from.rows);
        all = true;
        changed = true;
    }
    let next = next_seq(frame.seq);
    let cols = from.cols as usize;
    if cols > 0 {
        let rows = from.cells.chunks_exact(cols).zip(frame.cells.chunks_exact_mut(cols));
        for (row, (src, dst)) in rows.enumerate() {
            if all || src != dst {
                dst.copy_from_slice(src);
                frame.row_seq[row] = next;
                changed = true;
            }
        }
        if frame.extra_refs != from.extra_refs || frame.extra_chars != from.extra_chars {
            // A row whose zero-width characters come, go or change is drawn
            // again, though its cells may be the same.
            for extra in frame.extra_refs.iter().chain(&from.extra_refs) {
                if let Some(seq) = frame.row_seq.get_mut(extra.cell as usize / cols) {
                    *seq = next;
                }
            }
            frame.extra_refs.clone_from(&from.extra_refs);
            frame.extra_chars.clone_from(&from.extra_chars);
            changed = true;
        }
    }
    if (frame.fg, frame.bg) != (from.fg, from.bg)
        || frame.cursor != from.cursor
        || frame.cursor_point != from.cursor_point
        || (frame.display_offset, frame.history) != (from.display_offset, from.history)
    {
        (frame.fg, frame.bg) = (from.fg, from.bg);
        frame.cursor = from.cursor;
        frame.cursor_point = from.cursor_point;
        (frame.display_offset, frame.history) = (from.display_offset, from.history);
        changed = true;
    }
    if changed {
        frame.seq = next;
    }
    changed
}

/// Brings the frame up to date with the terminal. Returns true when
/// anything in it changed.
pub(crate) fn fill(state: &mut TermState, frame: &mut Frame) -> bool {
    let TermState {
        term, theme, snap, ..
    } = state;
    let cols = term.columns();
    let rows = term.screen_lines();

    let mut palette = [0; COUNT];
    resolve_palette(term.colors(), theme, &mut palette);
    // The cursor color is not part of any cell, so it alone does not force a
    // rewrite.
    let looks = CellLooks {
        bold_is_bright: theme.bold_is_bright,
        selection_bg: theme.selection_bg,
        selection_fg: theme.selection_fg,
    };
    let colors_changed = palette[..CURSOR] != snap.palette[..CURSOR] || looks != snap.looks;
    snap.palette = palette;
    snap.looks = looks;

    let mut changed = false;
    let mut full = colors_changed || snap.last_seq != Some(frame.seq);
    if frame.fg != palette[FOREGROUND] || frame.bg != palette[BACKGROUND] {
        frame.fg = palette[FOREGROUND];
        frame.bg = palette[BACKGROUND];
        changed = true;
    }
    if frame.cols as usize != cols
        || frame.rows as usize != rows
        || frame.cells.len() != cols * rows
        || frame.row_seq.len() != rows
    {
        frame.resize(cols as u16, rows as u16);
        full = true;
        changed = true;
    }
    let next = next_seq(frame.seq);

    let display_offset = term.display_offset();
    if display_offset != snap.display_offset {
        full = true;
    }
    snap.dirty.clear();
    snap.dirty.resize(rows, full);
    term.take_damage(&mut snap.dirty);

    let selection = term.selection_range();
    if selection != snap.selection {
        mark_selection_rows(&mut snap.dirty, snap.selection, display_offset);
        mark_selection_rows(&mut snap.dirty, selection, display_offset);
    }

    let ctx = Ctx {
        palette: &palette,
        bold_is_bright: theme.bold_is_bright,
        selection: selection.as_ref(),
        selection_bg: theme.selection_bg,
        selection_fg: theme.selection_fg,
    };
    snap.new_refs.clear();
    snap.new_chars.clear();
    for row in 0..rows {
        if !snap.dirty[row] {
            continue;
        }
        let src = term.visible_row(row);
        let dst = &mut frame.cells[row * cols..(row + 1) * cols];
        let base = (row * cols) as u32;
        if write_row(
            src.cells(),
            dst,
            src.line(),
            base,
            &ctx,
            &mut snap.new_refs,
            &mut snap.new_chars,
        ) {
            frame.row_seq[row] = next;
            changed = true;
        }
    }
    changed |= merge_extras(frame, snap, next);

    let point = cursor_point(term, frame, display_offset);
    let cursor = point.and_then(|p| cursor_draw(term, theme, frame, p));
    if frame.cursor != cursor || frame.cursor_point != point {
        frame.cursor = cursor;
        frame.cursor_point = point;
        changed = true;
    }
    let history = term.history_size() as u32;
    if frame.display_offset != display_offset as u32 || frame.history != history {
        frame.display_offset = display_offset as u32;
        frame.history = history;
        changed = true;
    }

    if changed {
        frame.seq = next;
    }
    snap.last_seq = Some(frame.seq);
    snap.display_offset = display_offset;
    snap.selection = selection;
    changed
}

/// A seq larger than the frame's that no other frame has been given.
fn next_seq(current: u64) -> u64 {
    let ticket = NEXT_SEQ.fetch_add(1, Ordering::Relaxed);
    if ticket > current {
        ticket
    } else {
        // The frame came with a seq beyond any we handed out; move past it.
        NEXT_SEQ.fetch_max(current + 2, Ordering::Relaxed);
        current + 1
    }
}

fn mark_selection_rows(
    dirty: &mut [bool],
    selection: Option<SelectionRange>,
    display_offset: usize,
) {
    let Some(s) = selection else { return };
    let top = (s.start.line.0 + display_offset as i32).max(0);
    let bottom = (s.end.line.0 + display_offset as i32).min(dirty.len() as i32 - 1);
    for row in top..=bottom {
        dirty[row as usize] = true;
    }
}

struct Ctx<'a> {
    palette: &'a [Rgb; COUNT],
    bold_is_bright: bool,
    selection: Option<&'a SelectionRange>,
    selection_bg: Option<Rgb>,
    selection_fg: Option<Rgb>,
}

impl Ctx<'_> {
    #[inline]
    fn color(&self, color: Color) -> Rgb {
        match color {
            Color::Named(n) => self.palette[n as usize],
            Color::Indexed(i) => self.palette[i as usize],
            Color::Spec(rgb) => pack(rgb),
        }
    }

    #[inline]
    fn foreground(&self, color: Color, bold: bool) -> Rgb {
        if bold && self.bold_is_bright {
            match color {
                Color::Named(n) if (n as usize) < 8 => return self.palette[n as usize + 8],
                Color::Indexed(i) if i < 8 => return self.palette[i as usize + 8],
                _ => {}
            }
        }
        self.color(color)
    }

    #[inline]
    fn selected(&self, fg: Rgb, bg: Rgb) -> (Rgb, Rgb) {
        match (self.selection_bg, self.selection_fg) {
            (Some(sbg), sfg) => (sfg.unwrap_or(fg), sbg),
            (None, Some(sfg)) => (sfg, fg),
            (None, None) => (bg, fg),
        }
    }
}

/// Rewrites one row; returns true when any cell differs from what the frame
/// held. Zero-width characters go to refs/chars for [merge_extras].
fn write_row<'a>(
    src: impl Iterator<Item = CellView<'a>>,
    dst: &mut [Cell],
    line: Line,
    base: u32,
    ctx: &Ctx<'_>,
    refs: &mut Vec<ExtraRef>,
    chars: &mut Vec<char>,
) -> bool {
    let selection = ctx
        .selection
        .filter(|s| s.start.line <= line && line <= s.end.line);
    let mut changed = false;
    for (col, (cell, out)) in src.zip(dst.iter_mut()).enumerate() {
        let selected = selection.is_some_and(|s| is_selected(s, &cell, line, col));
        let mut new = resolve(&cell, ctx, selected);
        if cell.has_extra() {
            let start = chars.len();
            chars.extend(cell.zerowidth());
            if chars.len() > start {
                new.flags |= flags::HAS_EXTRA;
                refs.push(ExtraRef {
                    cell: base + col as u32,
                    start: start as u32,
                    len: (chars.len() - start) as u32,
                });
            }
        }
        if *out != new {
            *out = new;
            changed = true;
        }
    }
    changed
}

#[inline]
fn is_selected(s: &SelectionRange, cell: &CellView<'_>, line: Line, col: usize) -> bool {
    // Both halves of a wide character count as selected when either is.
    s.contains(Point::new(line, Column(col)))
        || (cell.flags().contains(Flags::WIDE_CHAR) && s.contains(Point::new(line, Column(col + 1))))
        || (cell.flags().contains(Flags::WIDE_CHAR_SPACER)
            && col > 0
            && s.contains(Point::new(line, Column(col - 1))))
}

#[inline]
fn resolve(cell: &CellView<'_>, ctx: &Ctx<'_>, selected: bool) -> Cell {
    let f = cell.flags();
    let mut out = map_flags(f);
    let mut fg = ctx.foreground(cell.fg(), f.contains(Flags::BOLD));
    let mut bg = ctx.color(cell.bg());
    if f.contains(Flags::INVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }
    if f.contains(Flags::DIM) {
        fg = dim(fg, bg);
    }
    let mut ul = None;
    if cell.has_extra() {
        ul = cell.underline_color().map(|c| ctx.color(c));
        if cell.has_hyperlink() {
            out |= flags::HYPERLINK;
        }
    }
    if selected {
        (fg, bg) = ctx.selected(fg, bg);
        out |= flags::SELECTED;
    }
    if f.contains(Flags::HIDDEN) {
        fg = bg;
        ul = Some(bg);
    }
    // Spacers draw as blanks; a tab is stored in its cell only so copying
    // can reproduce it.
    let ch = if f.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        || cell.c() == '\t'
    {
        ' '
    } else {
        cell.c()
    };
    Cell {
        ch: ch as u32,
        fg,
        bg,
        ul: ul.unwrap_or(fg),
        flags: out,
        _reserved: 0,
    }
}

#[inline]
fn map_flags(f: Flags) -> u16 {
    const MAP: [(Flags, u16); 12] = [
        (Flags::BOLD, flags::BOLD),
        (Flags::ITALIC, flags::ITALIC),
        (Flags::DIM, flags::DIM),
        (Flags::UNDERLINE, flags::UNDERLINE),
        (Flags::DOUBLE_UNDERLINE, flags::DOUBLE_UNDERLINE),
        (Flags::UNDERCURL, flags::UNDERCURL),
        (Flags::DOTTED_UNDERLINE, flags::DOTTED_UNDERLINE),
        (Flags::DASHED_UNDERLINE, flags::DASHED_UNDERLINE),
        (Flags::STRIKEOUT, flags::STRIKEOUT),
        (Flags::WIDE_CHAR, flags::WIDE),
        (Flags::WIDE_CHAR_SPACER, flags::WIDE_SPACER),
        (Flags::HIDDEN, flags::HIDDEN),
    ];
    if f.is_empty() {
        return 0;
    }
    MAP.iter()
        .filter(|(a, _)| f.contains(*a))
        .fold(0, |acc, (_, b)| acc | b)
}

/// Replaces the zero-width characters of rewritten rows with the new ones and
/// keeps the others, leaving the lists sorted. Returns true when a row's
/// zero-width characters changed (raising that row's seq).
fn merge_extras(frame: &mut Frame, snap: &mut SnapState, next: u64) -> bool {
    if frame.extra_refs.is_empty() && snap.new_refs.is_empty() {
        return false;
    }
    let cols = frame.cols as u32;
    let (old_refs, old_chars) = (&frame.extra_refs, &frame.extra_chars);
    let (new_refs, new_chars) = (&snap.new_refs, &snap.new_chars);
    let out_refs = &mut snap.spare_refs;
    let out_chars = &mut snap.spare_chars;
    out_refs.clear();
    out_chars.clear();
    let mut changed = false;
    let (mut i, mut j) = (0, 0);
    for row in 0..frame.rows as usize {
        let end = (row as u32 + 1) * cols;
        let old_start = i;
        while i < old_refs.len() && old_refs[i].cell < end {
            i += 1;
        }
        let new_start = j;
        while j < new_refs.len() && new_refs[j].cell < end {
            j += 1;
        }
        let (refs, chars) = if snap.dirty[row] {
            let old = &old_refs[old_start..i];
            let new = &new_refs[new_start..j];
            if !same_extras(old, old_chars, new, new_chars) {
                frame.row_seq[row] = next;
                changed = true;
            }
            (new, new_chars)
        } else {
            (&old_refs[old_start..i], old_chars)
        };
        for r in refs {
            out_refs.push(ExtraRef {
                cell: r.cell,
                start: out_chars.len() as u32,
                len: r.len,
            });
            out_chars.extend_from_slice(&chars[r.start as usize..(r.start + r.len) as usize]);
        }
    }
    std::mem::swap(&mut frame.extra_refs, &mut snap.spare_refs);
    std::mem::swap(&mut frame.extra_chars, &mut snap.spare_chars);
    changed
}

fn same_extras(a: &[ExtraRef], a_chars: &[char], b: &[ExtraRef], b_chars: &[char]) -> bool {
    fn chars<'a>(r: &ExtraRef, all: &'a [char]) -> &'a [char] {
        &all[r.start as usize..(r.start + r.len) as usize]
    }
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| x.cell == y.cell && chars(x, a_chars) == chars(y, b_chars))
}

/// The cursor's cell on screen, shown or hidden; None when it is scrolled
/// out of view. On the spacer half of a wide character it is the
/// character's first cell.
fn cursor_point(term: &Terminal, frame: &Frame, display_offset: usize) -> Option<(u16, u16)> {
    let (point, on_spacer) = term.cursor();
    let row = point.line.0 + display_offset as i32;
    if row < 0 || row >= frame.rows as i32 || frame.cols == 0 {
        return None;
    }
    let mut col = point.column.0.min(frame.cols as usize - 1);
    if col > 0 && on_spacer {
        col -= 1;
    }
    Some((row as u16, col as u16))
}

/// How the cursor at (row, col) is drawn; None while the program hides it.
fn cursor_draw(
    term: &Terminal,
    theme: &Theme,
    frame: &Frame,
    (row, col): (u16, u16),
) -> Option<CursorDraw> {
    if !term.mode().contains(TermMode::SHOW_CURSOR) {
        return None;
    }
    let style = term.cursor_style();
    let shape = match style.shape {
        EngineShape::Block => CursorShape::Block,
        EngineShape::Beam => CursorShape::Beam,
        EngineShape::Underline => CursorShape::Underline,
        EngineShape::HollowBlock => CursorShape::HollowBlock,
    };
    let cell = *frame.cell(row, col);
    let color = term.colors()[CURSOR]
        .map(pack)
        .or(theme.cursor)
        .unwrap_or(cell.fg);
    Some(CursorDraw {
        row,
        col,
        shape,
        color,
        text_color: theme.cursor_text.unwrap_or(cell.bg),
        wide: cell.has(flags::WIDE),
        blinking: style.blinking,
    })
}

fn resolve_palette(overrides: &Colors, theme: &Theme, out: &mut [Rgb; COUNT]) {
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = overrides[i]
            .map(pack)
            .unwrap_or_else(|| default_color(theme, i));
    }
}

/// The theme's value for a color slot before any program overrides it.
fn default_color(theme: &Theme, index: usize) -> Rgb {
    match index {
        0..=255 => theme.palette[index],
        FOREGROUND => theme.fg,
        BACKGROUND => theme.bg,
        CURSOR => theme.cursor.unwrap_or(theme.fg),
        // There are no slots past the cursor's.
        _ => theme.fg,
    }
}

#[inline]
fn pack(rgb: EngineRgb) -> Rgb {
    (rgb.r as u32) << 16 | (rgb.g as u32) << 8 | rgb.b as u32
}

/// Moves a color a third of the way toward the background.
#[inline]
fn dim(fg: Rgb, bg: Rgb) -> Rgb {
    let mix = |shift: u32| {
        let f = (fg >> shift) & 0xff;
        let b = (bg >> shift) & 0xff;
        ((f * 2 + b) / 3) << shift
    };
    mix(16) | mix(8) | mix(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tern_vt::engine::{CursorStyle, Options, SelectionKind, Side, ViewScroll};

    struct Fixture {
        state: TermState,
        frame: Frame,
    }

    impl Fixture {
        fn new(cols: usize, rows: usize) -> Fixture {
            Fixture::with_theme(cols, rows, Theme::default())
        }

        fn with_theme(cols: usize, rows: usize, theme: Theme) -> Fixture {
            let options = Options {
                scrollback_lines: 10_000,
                cursor: CursorStyle::default(),
                ..Options::default()
            };
            let term = Terminal::new(&options, cols, rows);
            Fixture {
                state: TermState::new(term, theme),
                frame: Frame::default(),
            }
        }

        fn feed(&mut self, bytes: &[u8]) {
            self.state.term.advance(bytes);
        }

        /// Selects from one cell of a row of the view to another.
        fn select(&mut self, row: i32, from: usize, to: usize) {
            let term = &mut self.state.term;
            term.selection_start(row, from, Side::Left, SelectionKind::Simple);
            term.selection_update(row, to, Side::Right);
        }

        fn snap(&mut self) -> bool {
            fill(&mut self.state, &mut self.frame)
        }

        fn theme(&self) -> &Theme {
            &self.state.theme
        }
    }

    #[test]
    fn first_snapshot_fills_everything() {
        let mut f = Fixture::new(10, 3);
        f.feed(b"hi");
        assert!(f.snap());
        assert_eq!((f.frame.cols, f.frame.rows), (10, 3));
        assert_eq!(f.frame.row_text(0), "hi");
        assert_eq!(f.frame.fg, f.theme().fg);
        assert_eq!(f.frame.bg, f.theme().bg);
        let c = f.frame.cell(0, 0);
        assert_eq!(
            (c.fg, c.bg, c.ul),
            (f.theme().fg, f.theme().bg, f.theme().fg)
        );
        let cursor = f.frame.cursor.expect("cursor");
        assert_eq!((cursor.row, cursor.col), (0, 2));
        assert_eq!(cursor.shape, CursorShape::Block);
    }

    #[test]
    fn idle_snapshot_reports_no_change() {
        let mut f = Fixture::new(10, 3);
        f.feed(b"hi");
        assert!(f.snap());
        let seq = f.frame.seq;
        assert!(!f.snap());
        assert_eq!(f.frame.seq, seq);
    }

    #[test]
    fn only_changed_rows_get_a_new_seq() {
        let mut f = Fixture::new(10, 4);
        f.feed(b"a\r\nb\r\nc");
        f.snap();
        let before = f.frame.row_seq.clone();
        // Rewrites row 1 with the same text: damaged but not changed.
        f.feed(b"\x1b[2;1Hb\x1b[4;1Hd");
        assert!(f.snap());
        assert_eq!(f.frame.row_seq[0], before[0]);
        assert_eq!(f.frame.row_seq[1], before[1]);
        assert_eq!(f.frame.row_seq[3], f.frame.seq);
        assert_eq!(f.frame.row_text(3), "d");
    }

    #[test]
    fn a_foreign_frame_is_rewritten() {
        let mut f = Fixture::new(10, 2);
        f.feed(b"x");
        f.snap();
        let mut other = Frame::default();
        assert!(fill(&mut f.state, &mut other));
        assert_eq!(other.row_text(0), "x");
        // The original frame is now out of step and must be rewritten too.
        f.frame.cells[0].ch = 'z' as u32;
        f.frame.seq += 5;
        f.snap();
        assert_eq!(f.frame.row_text(0), "x");
    }

    #[test]
    fn frames_taken_in_turn_are_each_rewritten() {
        let mut f = Fixture::new(10, 2);
        f.feed(b"one");
        let mut a = Frame::default();
        let mut b = Frame::default();
        assert!(fill(&mut f.state, &mut a));
        assert!(fill(&mut f.state, &mut b));
        assert_ne!(a.seq, b.seq);
        // a went stale while b was filled; it must not be patched as if it
        // were b.
        f.feed(b"\r\ntwo");
        assert!(fill(&mut f.state, &mut a));
        assert_eq!(a.row_text(1), "two");
        a.cells[0].ch = 'X' as u32;
        assert!(fill(&mut f.state, &mut b));
        assert!(fill(&mut f.state, &mut a));
        assert_eq!(a.row_text(0), "one");
    }

    #[test]
    fn a_frame_moved_between_sessions_is_rewritten() {
        let mut x = Fixture::new(10, 1);
        let mut y = Fixture::new(10, 1);
        x.feed(b"xxx");
        y.feed(b"yyy");
        let mut frame = Frame::default();
        fill(&mut x.state, &mut frame);
        fill(&mut y.state, &mut y.frame);
        fill(&mut y.state, &mut frame);
        assert_eq!(frame.row_text(0), "yyy");
    }

    #[test]
    fn sgr_colors_and_attributes() {
        let mut f = Fixture::new(20, 2);
        f.feed(b"\x1b[1;31mA\x1b[0;7mB\x1b[0;38;5;196mC\x1b[38;2;1;2;3;48;2;4;5;6mD\x1b[0;4:3;58;2;9;9;9mE\x1b[0;2mF\x1b[0;8mG\x1b[0;9;3mH");
        f.snap();
        let t = f.theme().clone();
        let row = f.frame.row(0);
        assert_eq!(row[0].fg, t.palette[1]);
        assert!(row[0].has(flags::BOLD));
        assert_eq!((row[1].fg, row[1].bg), (t.bg, t.fg), "inverse swaps");
        assert_eq!(row[2].fg, 0xff0000);
        assert_eq!((row[3].fg, row[3].bg), (0x010203, 0x040506));
        assert!(row[4].has(flags::UNDERCURL));
        assert_eq!(row[4].ul, 0x090909);
        assert_eq!(row[5].fg, dim(t.fg, t.bg));
        assert!(row[5].has(flags::DIM));
        assert!(row[6].has(flags::HIDDEN));
        assert_eq!(row[6].fg, t.bg);
        assert!(row[7].has(flags::STRIKEOUT) && row[7].has(flags::ITALIC));
    }

    #[test]
    fn bold_is_bright_follows_the_theme() {
        let theme = Theme {
            bold_is_bright: true,
            ..Theme::default()
        };
        let mut f = Fixture::with_theme(10, 1, theme.clone());
        f.feed(b"\x1b[1;32mA\x1b[0;1;38;5;2mB\x1b[0;1;38;5;100mC");
        f.snap();
        assert_eq!(f.frame.cell(0, 0).fg, theme.palette[10]);
        assert_eq!(f.frame.cell(0, 1).fg, theme.palette[10]);
        assert_eq!(f.frame.cell(0, 2).fg, theme.palette[100]);
    }

    #[test]
    fn color_overrides_apply_and_force_a_rewrite() {
        let mut f = Fixture::new(10, 2);
        f.feed(b"\x1b[31mR");
        f.snap();
        // OSC 4 changes palette entry 1 and OSC 11 the background.
        f.feed(b"\x1b]4;1;rgb:12/34/56\x07\x1b]11;rgb:00/00/01\x07");
        assert!(f.snap());
        assert_eq!(f.frame.cell(0, 0).fg, 0x123456);
        assert_eq!(f.frame.bg, 0x000001);
        assert_eq!(f.frame.cell(1, 5).bg, 0x000001);
        assert_eq!(
            f.state.color(1),
            EngineRgb {
                r: 0x12,
                g: 0x34,
                b: 0x56
            }
        );
        // A theme change also reaches cells that were not damaged.
        f.state.theme.palette[1] = 0xabcdef;
        f.feed(b"\x1b]104;1\x07");
        assert!(f.snap());
        assert_eq!(f.frame.cell(0, 0).fg, 0xabcdef);
    }

    #[test]
    fn wide_and_zero_width_characters() {
        let mut f = Fixture::new(10, 2);
        f.feed("日e\u{301}x".as_bytes());
        f.snap();
        assert!(f.frame.cell(0, 0).has(flags::WIDE));
        assert_eq!(f.frame.cell(0, 0).char(), '日');
        assert!(f.frame.cell(0, 1).has(flags::WIDE_SPACER));
        assert!(f.frame.cell(0, 2).has(flags::HAS_EXTRA));
        assert_eq!(f.frame.extras(2), &['\u{301}']);
        assert_eq!(f.frame.row_text(0), "日e\u{301}x");

        // Rewriting another row keeps row 0's extras; clearing row 0 drops them.
        f.feed("\x1b[2;1Ha\u{308}".as_bytes());
        f.snap();
        assert_eq!(f.frame.extras(2), &['\u{301}']);
        assert_eq!(f.frame.extras(10), &['\u{308}']);
        let seq0 = f.frame.row_seq[0];
        f.feed(b"\x1b[1;1H\x1b[2K");
        f.snap();
        assert!(f.frame.extras(2).is_empty());
        assert_eq!(f.frame.extras(10), &['\u{308}']);
        assert!(f.frame.row_seq[0] > seq0);
        assert!(f.frame.extra_refs.windows(2).all(|w| w[0].cell < w[1].cell));
    }

    #[test]
    fn changing_only_a_combining_mark_raises_the_row() {
        let mut f = Fixture::new(10, 1);
        f.feed("e\u{301}".as_bytes());
        f.snap();
        let seq = f.frame.row_seq[0];
        f.feed("\x1b[1;1He\u{300}".as_bytes());
        assert!(f.snap());
        assert!(f.frame.row_seq[0] > seq);
        assert_eq!(f.frame.extras(0), &['\u{300}']);
    }

    #[test]
    fn selection_colors_and_flag() {
        let mut f = Fixture::new(10, 3);
        f.feed(b"hello\r\nworld");
        f.snap();
        f.select(0, 1, 2);
        assert!(f.snap());
        let t = f.theme().clone();
        let row = f.frame.row(0);
        assert!(!row[0].has(flags::SELECTED));
        assert!(row[1].has(flags::SELECTED) && row[2].has(flags::SELECTED));
        assert_eq!(row[1].bg, t.selection_bg.expect("theme has selection bg"));
        assert_eq!(row[1].fg, t.fg);
        f.state.term.selection_clear();
        assert!(f.snap());
        assert!(!f.frame.cell(0, 1).has(flags::SELECTED));
    }

    #[test]
    fn selection_without_theme_colors_swaps() {
        let theme = Theme {
            selection_bg: None,
            selection_fg: None,
            ..Theme::default()
        };
        let mut f = Fixture::with_theme(10, 1, theme.clone());
        f.feed(b"abc");
        f.select(0, 0, 0);
        f.snap();
        assert_eq!(
            (f.frame.cell(0, 0).fg, f.frame.cell(0, 0).bg),
            (theme.bg, theme.fg)
        );
    }

    #[test]
    fn during_an_update_the_screen_put_aside_is_what_is_shown() {
        let mut f = Fixture::new(10, 4);
        f.feed(b"one\r\ntwo");
        assert_eq!(show(&mut f.state, &mut f.frame), (true, false));
        let before = f.frame.row_seq.clone();

        // More output, then an update begins: the interpreter puts the
        // screen aside there and goes on into the update.
        f.feed(b"\r\nthree");
        put_aside(&mut f.state);
        f.feed(b"\x1b[?2026h");
        f.feed(b"\x1b[Hhalf\r\n\x1b[Kredrawn");

        // What was put aside is shown, and only the rows in which it
        // differs from the frame count as changed.
        assert_eq!(show(&mut f.state, &mut f.frame), (true, true));
        let rows = |frame: &Frame| [0, 1, 2].map(|row| frame.row_text(row));
        assert_eq!(rows(&f.frame), ["one", "two", "three"]);
        assert_eq!(f.frame.row_seq[..2], before[..2]);
        assert_eq!(f.frame.row_seq[2], f.frame.seq);
        assert_eq!(f.frame.cursor_point, Some((2, 5)));
        // Once: after that the frame has it.
        let shown = f.frame.clone();
        assert_eq!(show(&mut f.state, &mut f.frame), (false, true));
        assert_eq!(
            (f.frame.text(), f.frame.seq, &f.frame.row_seq),
            (shown.text(), shown.seq, &shown.row_seq)
        );

        // Between two updates the interpreter puts the screen aside again.
        f.feed(b"\x1b[?2026l");
        put_aside(&mut f.state);
        f.feed(b"\x1b[?2026h");
        f.feed(b"\x1b[3;1Hnext");
        assert_eq!(show(&mut f.state, &mut f.frame), (true, true));
        assert_eq!(rows(&f.frame), ["half", "redrawn", "three"]);
        assert_eq!(f.frame.row_seq[2], shown.row_seq[2]);

        // The update over, the terminal itself is shown again, and a row
        // that is as the frame has it keeps its seq.
        f.feed(b"\x1b[?2026l");
        let seqs = f.frame.row_seq.clone();
        assert_eq!(show(&mut f.state, &mut f.frame), (true, false));
        assert_eq!(rows(&f.frame), ["half", "redrawn", "nexte"]);
        assert_eq!(f.frame.row_seq[..2], seqs[..2]);
        assert!(f.frame.row_seq[2] > seqs[2]);
        assert_eq!(show(&mut f.state, &mut f.frame), (false, false));
    }

    #[test]
    fn an_update_with_nothing_put_aside_leaves_the_frame_as_it_is() {
        let mut f = Fixture::new(10, 3);
        f.feed(b"shown");
        show(&mut f.state, &mut f.frame);
        let shown = f.frame.clone();
        f.feed(b"\x1b[?2026h");
        f.feed(b"\r\nnot");
        assert_eq!(show(&mut f.state, &mut f.frame), (false, true));
        assert_eq!(
            (f.frame.text(), f.frame.seq, &f.frame.row_seq),
            (shown.text(), shown.seq, &shown.row_seq)
        );
        // A screen put aside before a frame was filled from the terminal
        // is older than the frame, and stays out of it.
        f.feed(b"\x1b[?2026l");
        put_aside(&mut f.state);
        f.feed(b" yet");
        assert_eq!(show(&mut f.state, &mut f.frame), (true, false));
        f.feed(b"\x1b[?2026h");
        f.feed(b"\x1b[2J");
        assert_eq!(show(&mut f.state, &mut f.frame), (false, true));
        assert_eq!([f.frame.row_text(0), f.frame.row_text(1)], ["shown", "not yet"]);
    }

    #[test]
    fn a_screen_put_aside_is_copied_whatever_it_holds() {
        let mut f = Fixture::new(10, 3);
        f.feed("e\u{301}x\r\n\u{65e5}\u{672c}".as_bytes());
        show(&mut f.state, &mut f.frame);
        let before = f.frame.row_seq.clone();

        // A combining mark changes, in a row whose cells stay the same: the
        // row counts as changed. So does a row that loses or gains one.
        f.feed("\x1b[He\u{308}\x1b[3;1Ha\u{323}".as_bytes());
        put_aside(&mut f.state);
        f.feed(b"\x1b[?2026h");
        assert_eq!(show(&mut f.state, &mut f.frame), (true, true));
        assert_eq!(f.frame.extras(0), &['\u{308}']);
        assert_eq!(f.frame.extras(20), &['\u{323}']);
        assert!(f.frame.row_seq[0] > before[0] && f.frame.row_seq[2] > before[2]);
        assert_eq!(f.frame.row_seq[1], before[1]);
        assert!(f.frame.cell(1, 0).has(flags::WIDE) && f.frame.cell(1, 1).has(flags::WIDE_SPACER));

        // Another size, colors and a scrolled view come over too.
        f.feed(b"\x1b[?2026l");
        f.state.term.resize(6, 2);
        f.feed(b"\x1b]11;#102030\x07\x1b[2J\x1b[Hsmall");
        put_aside(&mut f.state);
        f.feed(b"\x1b[?2026h");
        f.feed(b"\x1b[2J");
        assert_eq!(show(&mut f.state, &mut f.frame), (true, true));
        assert_eq!((f.frame.cols, f.frame.rows), (6, 2));
        assert_eq!(f.frame.cells.len(), 12);
        assert_eq!(f.frame.row_seq, [f.frame.seq; 2]);
        assert_eq!(f.frame.bg, 0x102030);
        assert_eq!(f.frame.row_text(0), "small");
        assert!(f.frame.extra_refs.is_empty());
    }

    #[test]
    fn scrolling_rewrites_and_reports_offset() {
        let mut f = Fixture::new(10, 3);
        for i in 0..10 {
            f.feed(format!("{i}\r\n").as_bytes());
        }
        f.snap();
        assert_eq!(f.frame.history, 8);
        f.state.term.scroll(ViewScroll::Lines(2));
        assert!(f.snap());
        assert_eq!(f.frame.display_offset, 2);
        assert_eq!(f.frame.row_text(0), "6");
        // The cursor sits on the bottom line, now scrolled out of view.
        assert_eq!(f.frame.cursor, None);
        f.state.term.scroll(ViewScroll::Bottom);
        f.snap();
        assert_eq!(f.frame.row_text(0), "8");
        assert!(f.frame.cursor.is_some());
    }

    #[test]
    fn resize_is_picked_up() {
        let mut f = Fixture::new(10, 3);
        f.feed(b"abc");
        f.snap();
        f.state.term.resize(20, 5);
        assert!(f.snap());
        assert_eq!((f.frame.cols, f.frame.rows), (20, 5));
        assert_eq!(f.frame.cells.len(), 100);
        assert_eq!(f.frame.row_text(0), "abc");
    }

    #[test]
    fn cursor_shape_color_and_visibility() {
        let theme = Theme {
            cursor: None,
            cursor_text: None,
            ..Theme::default()
        };
        let mut f = Fixture::with_theme(10, 2, theme.clone());
        f.feed(b"\x1b[5 q\x1b[32m");
        f.snap();
        let c = f.frame.cursor.expect("cursor");
        assert_eq!(c.shape, CursorShape::Beam);
        assert!(c.blinking);
        assert_eq!(c.color, theme.fg);
        assert_eq!(c.text_color, theme.bg);
        f.feed(b"\x1b]12;rgb:ff/00/00\x07");
        f.snap();
        assert_eq!(f.frame.cursor.expect("cursor").color, 0xff0000);
        f.feed(b"\x1b[?25l");
        assert!(f.snap());
        assert_eq!(f.frame.cursor, None);
    }

    #[test]
    fn a_hidden_cursor_still_has_a_place() {
        let mut f = Fixture::new(10, 3);
        f.feed(b"\x1b[?25lab\r\ncd");
        f.snap();
        assert_eq!(f.frame.cursor, None);
        assert_eq!(f.frame.cursor_point, Some((1, 2)));
        // Moving it while hidden changes the frame.
        f.feed(b"\x1b[3;5H");
        assert!(f.snap());
        assert_eq!(f.frame.cursor_point, Some((2, 4)));
    }

    #[test]
    fn cursor_on_wide_character() {
        let mut f = Fixture::new(10, 2);
        f.feed("日".as_bytes());
        f.feed(b"\x1b[1;1H");
        f.snap();
        let c = f.frame.cursor.expect("cursor");
        assert_eq!(c.col, 0);
        assert!(c.wide);
    }

    #[test]
    fn hyperlinks_are_flagged() {
        let mut f = Fixture::new(10, 1);
        f.feed(b"\x1b]8;;https://example.com\x1b\\ab\x1b]8;;\x1b\\c");
        f.snap();
        assert!(f.frame.cell(0, 0).has(flags::HYPERLINK));
        assert!(f.frame.cell(0, 1).has(flags::HYPERLINK));
        assert!(!f.frame.cell(0, 2).has(flags::HYPERLINK));
    }

    #[test]
    fn screen_text_matches_frame_text() {
        let mut f = Fixture::new(10, 3);
        f.feed("a\tb\r\n日本\r\ne\u{301}".as_bytes());
        f.snap();
        assert_eq!(f.state.term.screen_text(), f.frame.text());
        assert_eq!(f.frame.row_text(0), "a       b");
    }

    #[test]
    fn theme_changes_outside_the_palette_reach_every_cell() {
        // The cursor ends on row 2, so rows 0 and 1 are not damaged later.
        let mut f = Fixture::new(10, 3);
        f.feed(b"\x1b[1;32mbold\x1b[0m\r\nplain\r\n");
        f.select(1, 0, 3);
        f.snap();
        let palette = f.theme().palette;
        assert_eq!(f.frame.cell(0, 0).fg, palette[2]);

        // Nothing the program did changed, only the theme.
        f.state.theme.bold_is_bright = true;
        f.state.theme.selection_bg = Some(0x123456);
        f.state.theme.selection_fg = Some(0x654321);
        assert!(f.snap());
        assert_eq!(f.frame.cell(0, 0).fg, palette[10]);
        let selected = f.frame.cell(1, 0);
        assert_eq!((selected.fg, selected.bg), (0x654321, 0x123456));
    }

    #[test]
    fn a_frame_with_inconsistent_buffers_is_rebuilt() {
        let mut f = Fixture::new(10, 3);
        f.feed(b"abc");
        f.snap();
        // A caller that cleared row_seq (or built the frame by hand) must not
        // make the next snapshot index out of bounds.
        f.frame.row_seq.clear();
        f.feed(b"\x1b[2;1Hx");
        assert!(f.snap());
        assert_eq!(f.frame.row_seq.len(), 3);
        assert_eq!(f.frame.row_text(0), "abc");
    }

    #[test]
    fn dim_moves_toward_background() {
        assert_eq!(dim(0xffffff, 0x000000), 0xaaaaaa);
        assert_eq!(dim(0x000000, 0x000000), 0x000000);
        assert_eq!(dim(0x303030, 0x303030), 0x303030);
    }
}
