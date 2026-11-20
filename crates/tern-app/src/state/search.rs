//! Scrollback search bar and match highlighting for the focused pane.

use tern_core::SearchMatch;
use tern_types::{flags, Cell, CursorDraw, CursorShape, Frame, Key, Mods, PaneId, Rgb};
use unicode_width::UnicodeWidthChar;

use super::splits::mix;
use super::WindowState;
use crate::input::KeyInput;
use crate::pane::Pane;

const PASSIVE_MATCH_BG: Rgb = 0x6b531b;
const PASSIVE_MATCH_FG: Rgb = 0xfff2cc;
const ACTIVE_MATCH_BG: Rgb = 0xe5c07b;
const ACTIVE_MATCH_FG: Rgb = 0x181a1f;

#[derive(Default)]
pub(super) struct SearchState {
    pub open: bool,
    pub query: String,
    pub matches: Vec<SearchMatch>,
    pub current: usize,
    pub pane: Option<PaneId>,
    pub scanned_seq: u64,
}

impl WindowState {
    pub fn search_open(&self) -> bool {
        self.search.borrow().open
    }

    pub fn open_find(&self) {
        let Some(pane_id) = self.focused_pane_id() else {
            return;
        };
        let mut s = self.search.borrow_mut();
        s.open = true;
        s.pane = Some(pane_id);
        s.scanned_seq = 0;
        drop(s);
        self.refresh_search(true);
    }

    pub fn close_find(&self) {
        let was_open = {
            let mut s = self.search.borrow_mut();
            let was = s.open;
            s.open = false;
            was
        };
        if was_open {
            if let Some(id) = self.focused_pane_id() {
                self.pane_changed(id);
            }
            self.mark_dirty();
        }
    }

    pub fn find_step(&self, delta: isize) {
        let Some(pane_id) = self.focused_pane_id() else {
            return;
        };
        let query = self.search.borrow().query.clone();
        if query.is_empty() {
            if !self.search_open() {
                self.open_find();
            }
            return;
        }
        let matches = self
            .with_pane(pane_id, |p| p.session.search(&query))
            .unwrap_or_default();
        let mut s = self.search.borrow_mut();
        s.pane = Some(pane_id);
        s.matches = matches;
        if s.matches.is_empty() {
            drop(s);
            self.mark_dirty();
            return;
        }
        let len = s.matches.len();
        let cur = s.current.min(len - 1) as isize;
        let next = (cur + delta).rem_euclid(len as isize) as usize;
        s.current = next;
        let target = s.matches[next];
        drop(s);
        self.with_pane(pane_id, |p| p.session.focus_match(target));
        self.pane_changed(pane_id);
        self.mark_dirty();
    }

    pub fn paste_into_find(&self, text: &str) {
        let clean: String = text
            .chars()
            .take_while(|&c| c != '\n' && c != '\r')
            .filter(|c| !c.is_control())
            .collect();
        if clean.is_empty() {
            return;
        }
        self.search.borrow_mut().query.push_str(&clean);
        self.refresh_search(true);
    }

    pub fn clear_find_query(&self) {
        let changed = {
            let mut s = self.search.borrow_mut();
            let had = !s.query.is_empty();
            s.query.clear();
            s.matches.clear();
            s.current = 0;
            had
        };
        if changed {
            if let Some(id) = self.focused_pane_id() {
                self.with_pane(id, |p| p.session.selection_clear());
                self.pane_changed(id);
            }
            self.mark_dirty();
        }
    }

    /// Handles a key event while the Find bar is open. Returns true when consumed.
    pub(crate) fn handle_search_key(&self, k: &KeyInput) -> bool {
        if !self.search_open() {
            return false;
        }
        if !k.down {
            return true;
        }
        let Some(press) = k.press else {
            return true;
        };
        let mods = press.mods.held();
        match (press.key, mods) {
            (Key::Escape, _) => {
                self.close_find();
                return true;
            }
            (Key::Enter, m) if m == Mods::SHIFT => {
                self.find_step(-1);
                return true;
            }
            (Key::Enter, m) if m.is_empty() => {
                self.find_step(1);
                return true;
            }
            (Key::Backspace, m) if m.contains(Mods::SUPER) => {
                self.clear_find_query();
                return true;
            }
            (Key::Char('u'), m) if m == Mods::CTRL => {
                self.clear_find_query();
                return true;
            }
            (Key::Backspace, m) if m.contains(Mods::ALT) => {
                self.pop_find_word();
                return true;
            }
            (Key::Char('w'), m) if m == Mods::CTRL => {
                self.pop_find_word();
                return true;
            }
            (Key::Backspace, _) | (Key::Delete, _) => {
                let popped = self.search.borrow_mut().query.pop().is_some();
                if popped {
                    self.refresh_search(true);
                }
                return true;
            }
            _ => {}
        }

        let inserted = if !k.text.is_empty() {
            &k.text
        } else if !mods.contains(Mods::SUPER) && !mods.contains(Mods::CTRL) {
            &k.characters
        } else {
            ""
        };
        let mut added = false;
        {
            let mut s = self.search.borrow_mut();
            for ch in inserted.chars().filter(|c| !c.is_control()) {
                s.query.push(ch);
                added = true;
            }
        }
        if added {
            self.refresh_search(true);
        }
        true
    }

    fn pop_find_word(&self) {
        let changed = {
            let mut s = self.search.borrow_mut();
            let before = s.query.len();
            let trimmed = s.query.trim_end().len();
            s.query.truncate(trimmed);
            while let Some(c) = s.query.chars().last() {
                if c.is_whitespace() {
                    break;
                }
                s.query.pop();
            }
            s.query.len() != before
        };
        if changed {
            self.refresh_search(true);
        }
    }

    pub(super) fn refresh_search(&self, jump_nearest: bool) {
        let Some(pane_id) = self.focused_pane_id() else {
            return;
        };
        let query = self.search.borrow().query.clone();
        if query.is_empty() {
            let mut s = self.search.borrow_mut();
            s.matches.clear();
            s.current = 0;
            s.pane = Some(pane_id);
            drop(s);
            self.with_pane(pane_id, |p| p.session.selection_clear());
            self.pane_changed(pane_id);
            self.mark_dirty();
            return;
        }
        let Some((matches, seq, view_bottom)) = self.with_pane(pane_id, |p| {
            let offset = p.frame.display_offset as i32;
            let bottom = p.frame.rows as i32 - 1 - offset;
            (p.session.search(&query), p.frame.seq, bottom)
        }) else {
            return;
        };
        let mut s = self.search.borrow_mut();
        s.pane = Some(pane_id);
        s.scanned_seq = seq;
        let focus = if matches.is_empty() {
            s.current = 0;
            None
        } else {
            if jump_nearest {
                // Pick the newest match at or above the bottom of the current view, or wrap to last.
                s.current = matches
                    .iter()
                    .rposition(|m| m.start_line <= view_bottom)
                    .unwrap_or(matches.len() - 1);
            } else if s.current >= matches.len() {
                s.current = matches.len() - 1;
            }
            Some(matches[s.current])
        };
        s.matches = matches;
        drop(s);
        if let Some(target) = focus {
            self.with_pane(pane_id, |p| p.session.focus_match(target));
        } else {
            self.with_pane(pane_id, |p| p.session.selection_clear());
        }
        self.pane_changed(pane_id);
        self.mark_dirty();
    }

    /// Syncs live search matches if the focused pane received new output while
    /// the Find bar is open.
    pub(super) fn sync_search_for_pane(&self, pane: &Pane) {
        let needs_rescan = {
            let s = self.search.borrow();
            s.open && !s.query.is_empty() && (s.pane != Some(pane.id) || s.scanned_seq != pane.frame.seq)
        };
        if needs_rescan {
            let query = self.search.borrow().query.clone();
            let matches = pane.session.search(&query);
            let mut s = self.search.borrow_mut();
            s.pane = Some(pane.id);
            s.scanned_seq = pane.frame.seq;
            if !matches.is_empty() && s.current >= matches.len() {
                s.current = matches.len() - 1;
            }
            s.matches = matches;
        }
    }
}

/// Overlays visible match highlights and the top-right Find bar onto `out`.
pub(super) fn overlay_search(
    base: &Frame,
    query: &str,
    preedit: &str,
    matches: &[SearchMatch],
    current: usize,
    out: &mut Frame,
    seq: u64,
) {
    out.clone_from(base);
    out.seq = seq;
    out.row_seq.iter_mut().for_each(|s| *s = seq);

    let rows = out.rows;
    let cols = out.cols;
    if rows == 0 || cols == 0 {
        return;
    }

    let offset = out.display_offset as i32;
    let view_top = -offset;
    let view_bottom = rows as i32 - 1 - offset;

    for (idx, m) in matches.iter().enumerate() {
        if m.end_line < view_top || m.start_line > view_bottom {
            continue;
        }
        let active = idx == current;
        let (bg, fg) = if active {
            (ACTIVE_MATCH_BG, ACTIVE_MATCH_FG)
        } else {
            (PASSIVE_MATCH_BG, PASSIVE_MATCH_FG)
        };
        let r_start = (m.start_line + offset).max(0) as u16;
        let r_end = (m.end_line + offset).min(rows as i32 - 1) as u16;
        for r in r_start..=r_end {
            let line = r as i32 - offset;
            let c_start = if line == m.start_line { m.start_col } else { 0 };
            let c_end = if line == m.end_line {
                m.end_col.min(cols - 1)
            } else {
                cols - 1
            };
            let row_slice = out.row_mut(r);
            for c in c_start..=c_end {
                if let Some(cell) = row_slice.get_mut(c as usize) {
                    cell.bg = bg;
                    cell.fg = fg;
                    cell.ul = fg;
                }
            }
        }
    }

    // Draw the Find bar at top-right of row 0.
    let bar_bg = mix(base.bg, base.fg, 0.14);
    let count_label = if query.is_empty() {
        String::new()
    } else if matches.is_empty() {
        " 0/0 ".to_string()
    } else {
        format!(" {}/{} ", current.min(matches.len() - 1) + 1, matches.len())
    };
    let prefix = " Find: ";
    let full_query = if preedit.is_empty() {
        query.to_string()
    } else {
        format!("{query}{preedit}")
    };

    let bar_width = (cols as usize).min(42).max(cols.min(16) as usize);
    let bar_start = (cols as usize).saturating_sub(bar_width);
    let row0 = out.row_mut(0);
    for cell in &mut row0[bar_start..cols as usize] {
        *cell = Cell {
            ch: u32::from(' '),
            fg: base.fg,
            bg: bar_bg,
            ul: base.fg,
            flags: 0,
            _reserved: 0,
        };
    }

    let count_cols = count_label.chars().count().min(bar_width);
    let count_start = cols as usize - count_cols;
    let count_fg = if !query.is_empty() && matches.is_empty() {
        0xe06c75
    } else {
        mix(base.fg, base.bg, 0.35)
    };
    for (i, ch) in count_label.chars().take(count_cols).enumerate() {
        row0[count_start + i] = Cell {
            ch: u32::from(ch),
            fg: count_fg,
            bg: bar_bg,
            ul: count_fg,
            flags: 0,
            _reserved: 0,
        };
    }

    let mut col = bar_start;
    let text_limit = count_start.saturating_sub(1);
    for ch in prefix.chars() {
        if col >= text_limit {
            break;
        }
        row0[col].ch = u32::from(ch);
        row0[col].fg = mix(base.fg, base.bg, 0.30);
        row0[col].flags = flags::BOLD;
        col += 1;
    }

    // Show the tail of long queries so the typing cursor stays visible.
    let avail = text_limit.saturating_sub(col);
    let qchars: Vec<char> = full_query.chars().collect();
    let mut widths: Vec<usize> = qchars.iter().map(|c| c.width().unwrap_or(0)).collect();
    let mut start_idx = qchars.len();
    let mut used = 0;
    while start_idx > 0 {
        let w = widths[start_idx - 1];
        if used + w > avail {
            break;
        }
        used += w;
        start_idx -= 1;
    }
    let _ = &mut widths;
    for &ch in &qchars[start_idx..] {
        let w = ch.width().unwrap_or(0);
        if w == 0 || col + w > text_limit {
            continue;
        }
        row0[col].ch = u32::from(ch);
        row0[col].fg = base.fg;
        if w == 2 {
            row0[col].flags = flags::WIDE;
            row0[col + 1].ch = u32::from(' ');
            row0[col + 1].flags = flags::WIDE_SPACER;
        }
        col += w;
    }

    let cursor_col = (col as u16).min(cols.saturating_sub(1));
    out.cursor_point = Some((0, cursor_col));
    out.cursor = Some(CursorDraw {
        row: 0,
        col: cursor_col,
        shape: CursorShape::Beam,
        color: base.fg,
        text_color: bar_bg,
        wide: false,
        blinking: false,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_matches_and_renders_find_bar_counter() {
        let mut base = Frame {
            fg: 0xdcdfe4,
            bg: 0x181a1f,
            display_offset: 5,
            history: 10,
            ..Frame::default()
        };
        base.resize(50, 4);
        for (i, ch) in "hello world".chars().enumerate() {
            base.row_mut(2)[i].ch = u32::from(ch);
        }
        // Row 2 with display_offset 5 corresponds to grid line 2 - 5 = -3.
        let matches = vec![
            SearchMatch {
                start_line: -8,
                start_col: 0,
                end_line: -8,
                end_col: 4,
            },
            SearchMatch {
                start_line: -3,
                start_col: 6,
                end_line: -3,
                end_col: 10,
            },
        ];
        let mut out = Frame::default();
        overlay_search(&base, "world", "", &matches, 1, &mut out, 10);

        assert_eq!(out.cell(2, 6).bg, ACTIVE_MATCH_BG);
        assert_eq!(out.cell(2, 10).bg, ACTIVE_MATCH_BG);
        assert_eq!(out.cell(2, 5).bg, base.bg);
        assert!(out.row_text(0).contains("Find: world"));
        assert!(out.row_text(0).contains("2/2"));
        assert_eq!(out.cursor.map(|c| c.row), Some(0));
    }
}
