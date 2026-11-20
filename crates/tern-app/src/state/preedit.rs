//! The input method's uncommitted text, drawn at the cursor of the focused
//! pane while the user composes it.

use tern_core::Scroll;
use tern_types::{flags, Cell, Frame, PaneId};
use unicode_width::UnicodeWidthChar;

use super::WindowState;
use crate::pane::Pane;

impl WindowState {
    /// The input method's uncommitted text changed; empty when composing
    /// ends.
    pub(crate) fn set_preedit(&self, text: String) {
        if *self.preedit.borrow() == text {
            return;
        }
        //Composing inside the Find bar keeps the viewport where the match is.
        let scrolled = !text.is_empty()
            && !self.search_open()
            && self
                .with_focused(|p| {
                    let back = p.frame.display_offset > 0;
                    if back {
                        p.session.scroll(Scroll::Bottom);
                    }
                    back
                })
                .unwrap_or(false);
        *self.preedit.borrow_mut() = text;
        if let Some(id) = self.focused_pane_id() {
            self.pane_changed(id);
        }
        if scrolled {
            self.mark_dirty();
        }
    }

    /// Rebuilds the overlay frame drawn for the focused pane while text is
    /// being composed or the Find bar is open.
    pub(super) fn update_preedit_frame(&self, panes: &mut [Pane]) {
        let text = self.preedit.borrow();
        let searching = self.search_open();
        let mut overlay = self.preedit_frame.borrow_mut();
        let focused = self.focused_pane_id();
        let active = !text.is_empty() || searching;
        let target = focused.filter(|&id| active && panes.iter().any(|p| p.id == id && p.shown));
        // A pane the overlay no longer covers goes back to its own frame.
        if let Some((id, frame)) = overlay.take_if(|(id, _)| Some(*id) != target) {
            if let Some(pane) = panes.iter_mut().find(|p| p.id == id) {
                redraw_whole(&mut pane.frame, frame.seq);
            }
            *self.spare_frame.borrow_mut() = frame;
        }
        let Some(pane) = target.and_then(|id| panes.iter().find(|p| p.id == id)) else {
            return;
        };
        if searching {
            self.sync_search_for_pane(pane);
        }
        // Above both the pane's own seq and the last overlay's, and every
        // row stamped with it: the renderer redraws the pane whole.
        let seq = overlay
            .as_ref()
            .map_or(0, |(_, f)| f.seq)
            .max(pane.frame.seq)
            + 1;
        let mut frame = overlay
            .take()
            .map(|(_, f)| f)
            .unwrap_or_else(|| std::mem::take(&mut *self.spare_frame.borrow_mut()));
        if searching {
            let s = self.search.borrow();
            super::search::overlay_search(
                &pane.frame,
                &s.query,
                &text,
                &s.matches,
                s.current,
                &mut frame,
                seq,
            );
        } else {
            overlay_preedit(&pane.frame, &text, &mut frame, seq);
        }
        *overlay = Some((pane.id, frame));
    }

    /// The text of what was last drawn for a pane, composition included.
    pub fn drawn_text(&self, id: PaneId) -> Option<String> {
        if let Some((pane, frame)) = &*self.preedit_frame.borrow()
            && *pane == id {
                return Some(frame.text());
            }
        self.with_pane(id, |p| p.frame.text())
    }
}

/// Stamps every row of a pane's own frame above the overlay that was drawn
/// in its place, so the renderer, which redraws only rows newer than what it
/// drew last, redraws all of them. The frame's seq can already be above the
/// overlay's (output arrived in the same draw that ended the composition),
/// and rows only the overlay changed would otherwise keep its text. The core
/// sees a seq it did not give and rewrites the frame whole next time.
fn redraw_whole(frame: &mut Frame, overlay_seq: u64) {
    let seq = frame.seq.max(overlay_seq) + 1;
    frame.seq = seq;
    frame.row_seq.iter_mut().for_each(|s| *s = seq);
}

/// Copies frame into out with text written over it from the cursor on,
/// underlined in the default colors, and the cursor after it. Text that
/// does not fit runs onto the next rows and stops at the bottom.
fn overlay_preedit(frame: &Frame, text: &str, out: &mut Frame, seq: u64) {
    out.clone_from(frame);
    out.seq = seq;
    out.row_seq.iter_mut().for_each(|s| *s = seq);
    // A program that hides the cursor still takes typed text where it is.
    let Some((mut row, mut col)) = frame.cursor_point else {
        return;
    };
    let (cols, rows) = (frame.cols, frame.rows);
    let style = |ch: char, extra: u16| Cell {
        ch: u32::from(ch),
        fg: frame.fg,
        bg: frame.bg,
        ul: frame.fg,
        flags: flags::UNDERLINE | extra,
        _reserved: 0,
    };
    for ch in text.chars() {
        let width = ch.width().unwrap_or(0) as u16;
        if width == 0 {
            continue;
        }
        if col + width > cols {
            (row, col) = (row + 1, 0);
        }
        if row >= rows || width > cols {
            break;
        }
        let at = usize::from(row) * usize::from(cols) + usize::from(col);
        if width == 2 {
            out.cells[at] = style(ch, flags::WIDE);
            out.cells[at + 1] = style(' ', flags::WIDE_SPACER);
        } else {
            out.cells[at] = style(ch, 0);
        }
        col += width;
    }
    // The cursor follows the text, unless the program hides it.
    out.cursor_point = (row < rows && col < cols).then_some((row, col));
    out.cursor = frame
        .cursor
        .zip(out.cursor_point)
        .map(|(cursor, (row, col))| tern_types::CursorDraw {
            row,
            col,
            wide: false,
            ..cursor
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tern_types::{CursorDraw, CursorShape};

    fn frame(cols: u16, rows: u16, cursor: (u16, u16)) -> Frame {
        let mut f = Frame {
            fg: 0xdddddd,
            bg: 0x111111,
            ..Frame::default()
        };
        f.resize(cols, rows);
        f.cursor_point = Some(cursor);
        f.cursor = Some(CursorDraw {
            row: cursor.0,
            col: cursor.1,
            shape: CursorShape::Block,
            color: 0xffffff,
            text_color: 0,
            wide: false,
            blinking: false,
        });
        f
    }

    #[test]
    fn composed_text_sits_at_the_cursor_underlined() {
        let f = frame(6, 2, (0, 2));
        let mut out = Frame::default();
        overlay_preedit(&f, "日a", &mut out, f.seq + 1);
        assert_eq!(out.row_text(0), "  日a");
        let c = out.cell(0, 2);
        assert!(c.has(flags::UNDERLINE | flags::WIDE));
        assert!(out.cell(0, 3).has(flags::WIDE_SPACER));
        assert!(out.cell(0, 4).has(flags::UNDERLINE));
        let cursor = out.cursor.expect("cursor");
        assert_eq!((cursor.row, cursor.col), (0, 5));
        assert!(out.seq > f.seq && out.row_seq.iter().all(|&s| s == out.seq));
        // The pane's own frame is untouched.
        assert_eq!(f.row_text(0), "");
    }

    #[test]
    fn the_pane_frame_after_an_overlay_is_redrawn_whole() {
        // Output arrived in the same draw: the pane's frame is already newer
        // than the overlay, but only its first row changed.
        let mut f = frame(4, 3, (0, 0));
        f.seq = 20;
        f.row_seq = vec![20, 5, 5];
        redraw_whole(&mut f, 12);
        assert!(f.seq > 20);
        assert!(f.row_seq.iter().all(|&s| s == f.seq));
        // And when the overlay is the newer one.
        redraw_whole(&mut f, 40);
        assert!(f.seq > 40 && f.row_seq.iter().all(|&s| s == f.seq));
    }

    #[test]
    fn composed_text_shows_where_a_hidden_cursor_is() {
        let mut f = frame(6, 2, (1, 1));
        f.cursor = None;
        let mut out = Frame::default();
        overlay_preedit(&f, "か", &mut out, 9);
        assert_eq!(out.row_text(1), " か");
        // The program hid the cursor; it stays hidden.
        assert_eq!(out.cursor, None);
        assert_eq!(out.cursor_point, Some((1, 3)));
    }

    #[test]
    fn long_text_wraps_and_stops_at_the_bottom() {
        let f = frame(4, 2, (1, 2));
        let mut out = Frame::default();
        // The wide character does not fit in the last column; it goes to
        // the next row, which is past the bottom.
        overlay_preedit(&f, "ab日cd", &mut out, 9);
        assert_eq!(out.row_text(1), "  ab");
        assert_eq!(out.cursor, None);
        // Combining marks take no cell of their own.
        let mut out = Frame::default();
        overlay_preedit(&frame(4, 1, (0, 0)), "e\u{301}x", &mut out, 9);
        assert_eq!(out.row_text(0), "ex");
    }
}
