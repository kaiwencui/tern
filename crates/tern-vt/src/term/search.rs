use memchr::memchr2;

use crate::grid::{Dimensions, LineRef};
use crate::index::{Column, Line, Point};
use crate::term::cell::Flags;
use crate::term::Term;

/// Inclusive grid span of one search match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchMatch {
    pub start: Point,
    pub end: Point,
}

impl<T> Term<T> {
    /// Finds all non-overlapping case-insensitive occurrences of `query` from
    /// the oldest scrollback line to the bottom of the screen, spanning
    /// soft-wrapped lines.
    pub fn search(&self, query: &str) -> Vec<SearchMatch> {
        let needle: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
        if needle.is_empty() {
            return Vec::new();
        }

        let first_ascii = query
            .chars()
            .next()
            .filter(|c| c.is_ascii() && *c != ' ')
            .map(|c| (c.to_ascii_lowercase() as u8, c.to_ascii_uppercase() as u8));

        let top = self.topmost_line().0;
        let bottom = self.bottommost_line().0;
        let cols = self.columns();
        let mut out = Vec::new();
        let mut hay: Vec<(char, Point, bool)> = Vec::with_capacity(cols * 2);

        let mut raw = top;
        while raw <= bottom {
            let mut end_raw = raw;
            while end_raw < bottom && self.grid.line(Line(end_raw)).wrapped() {
                end_raw += 1;
            }

            if raw == end_raw
                && let Some((lo, hi)) = first_ascii
                && let LineRef::Packed(record, _) = self.grid.line(Line(raw))
                && memchr2(lo, hi, record.text()).is_none()
            {
                raw = end_raw + 1;
                continue;
            }

            hay.clear();
            for r in raw..=end_raw {
                let row = self.grid.line(Line(r));
                let limit = if r == end_raw && !row.wrapped() {
                    row.line_length().0
                } else {
                    cols
                };
                for (col, cell) in row.cells().enumerate().take(limit) {
                    if cell
                        .flags()
                        .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                    {
                        continue;
                    }
                    let pt = Point::new(Line(r), Column(col));
                    let wide = cell.flags().contains(Flags::WIDE_CHAR) && col + 1 < cols;
                    let ch = if cell.c() == '\t' { ' ' } else { cell.c() };
                    for lc in ch.to_lowercase() {
                        hay.push((lc, pt, wide));
                    }
                    if cell.has_extra() {
                        for zw in cell.zerowidth() {
                            for lc in zw.to_lowercase() {
                                hay.push((lc, pt, wide));
                            }
                        }
                    }
                }
            }

            let nlen = needle.len();
            let mut i = 0;
            while i + nlen <= hay.len() {
                if hay[i].0 == needle[0]
                    && hay[i + 1..i + nlen]
                        .iter()
                        .zip(&needle[1..])
                        .all(|((hc, _, _), nc)| hc == nc)
                {
                    let start = hay[i].1;
                    let (_, mut end, wide) = hay[i + nlen - 1];
                    if wide {
                        end.column += 1;
                    }
                    out.push(SearchMatch { start, end });
                    i += nlen;
                } else {
                    i += 1;
                }
            }

            raw = end_raw + 1;
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::VoidListener;
    use crate::parser::Parser;
    use crate::term::test::TermSize;
    use crate::engine::Options;

    #[test]
    fn searches_packed_scrollback_soft_wraps_and_wide_chars() {
        let options = Options {
            scrollback_lines: 200,
            ring_rows: 2,
            page_bytes: 128,
            ..Options::default()
        };
        let mut term = Term::new(options, &TermSize::new(8, 3), VoidListener);
        let mut parser = Parser::new();

        for i in 0..25 {
            parser.advance(&mut term, format!("row{i:02}\r\n").as_bytes());
        }
        // Soft-wrapped match across two 8-col rows ("abcdERR" + "ORxyz"), then CJK wide match.
        parser.advance(&mut term, "abcdERRORxyz\r\nhi日本語ok".as_bytes());
        while term.settle(64) {}

        let row_hits = term.search("ROW03");
        assert_eq!(row_hits.len(), 1);
        assert_eq!(row_hits[0].start.column, Column(0));
        assert_eq!(row_hits[0].end.column, Column(4));

        let wrap_hits = term.search("error");
        assert_eq!(wrap_hits.len(), 1);
        assert_eq!(wrap_hits[0].start.line + 1, wrap_hits[0].end.line);
        assert_eq!(wrap_hits[0].start.column, Column(4));
        assert_eq!(wrap_hits[0].end.column, Column(0));

        let wide_hits = term.search("日本");
        assert_eq!(wide_hits.len(), 1);
        assert_eq!(wide_hits[0].start.column, Column(2));
        // Covers both 2-column wide cells (columns 2..=5).
        assert_eq!(wide_hits[0].end.column, Column(5));
    }
}
