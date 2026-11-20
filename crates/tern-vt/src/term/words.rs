use crate::grid::{Dimensions, LineRef};
use crate::index::{Column, Line, Point};
use crate::term::cell::Flags;
use crate::term::{SEMANTIC_ESCAPE_CHARS, Term};

impl<T> Term<T> {
    /// Finds the matching bracket for the character at `point`, scanning across
    /// the entire scrollback and screen while tracking nesting depth.
    pub fn bracket_search(&self, point: Point) -> Option<Point> {
        let topmost = self.topmost_line();
        let bottommost = self.bottommost_line();
        if point.line < topmost || point.line > bottommost || point.column.0 >= self.columns() {
            return None;
        }

        let start_char = self.grid.line(point.line).cell(point.column).c();
        let (forward, end_char) = match start_char {
            '(' => (true, ')'),
            '[' => (true, ']'),
            '{' => (true, '}'),
            '<' => (true, '>'),
            ')' => (false, '('),
            ']' => (false, '['),
            '}' => (false, '{'),
            '>' => (false, '<'),
            _ => return None,
        };

        let mut depth = 0usize;
        if forward {
            for (col, cell) in self
                .grid
                .line(point.line)
                .cells()
                .enumerate()
                .skip(point.column.0 + 1)
            {
                let c = cell.c();
                if c == end_char {
                    if depth == 0 {
                        return Some(Point::new(point.line, Column(col)));
                    }
                    depth -= 1;
                } else if c == start_char {
                    depth += 1;
                }
            }

            for raw_line in (point.line.0 + 1)..=bottommost.0 {
                let line = Line(raw_line);
                for (col, cell) in self.grid.line(line).cells().enumerate() {
                    let c = cell.c();
                    if c == end_char {
                        if depth == 0 {
                            return Some(Point::new(line, Column(col)));
                        }
                        depth -= 1;
                    } else if c == start_char {
                        depth += 1;
                    }
                }
            }
        } else {
            if let Some(col) =
                scan_brackets_backward(self.grid.line(point.line), point.column.0, start_char, end_char, &mut depth)
            {
                return Some(Point::new(point.line, Column(col)));
            }

            let cols = self.columns();
            for raw_line in (topmost.0..point.line.0).rev() {
                let line = Line(raw_line);
                if let Some(col) =
                    scan_brackets_backward(self.grid.line(line), cols, start_char, end_char, &mut depth)
                {
                    return Some(Point::new(line, Column(col)));
                }
            }
        }

        None
    }

    /// Finds the start of the semantic word containing `point`, stopping at
    /// hard line breaks or characters in [`SEMANTIC_ESCAPE_CHARS`].
    pub fn semantic_search_left(&self, mut point: Point) -> Point {
        let wide_spacers = Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER;
        let topmost = self.topmost_line();
        let bottommost = self.bottommost_line();
        let last_col = self.last_column();

        point.line = point.line.clamp(topmost, bottommost);
        point.column = point.column.min(last_col);

        let mut last_reached = point;
        if let Some(delim_col) = self.find_delimiter_left(
            point.line,
            point.column.0,
            wide_spacers,
            &mut last_reached,
        ) {
            return self.first_non_spacer_after(Point::new(point.line, Column(delim_col)), bottommost, wide_spacers);
        }

        let cols = self.columns();
        for raw_line in (topmost.0..point.line.0).rev() {
            let line = Line(raw_line);
            if !self.grid.line(line).wrapped() {
                return last_reached;
            }
            if let Some(delim_col) = self.find_delimiter_left(line, cols, wide_spacers, &mut last_reached) {
                return self.first_non_spacer_after(Point::new(line, Column(delim_col)), bottommost, wide_spacers);
            }
        }

        last_reached
    }

    /// Finds the end of the semantic word containing `point`, stopping at
    /// hard line breaks or characters in [`SEMANTIC_ESCAPE_CHARS`].
    pub fn semantic_search_right(&self, mut point: Point) -> Point {
        let wide_spacers = Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER;
        let topmost = self.topmost_line();
        let bottommost = self.bottommost_line();
        let last_col = self.last_column();

        point.line = point.line.clamp(topmost, bottommost);
        point.column = point.column.min(last_col);

        let first_line = self.grid.line(point.line);
        if point.column == last_col && !first_line.wrapped() {
            return point;
        }

        let escape_chars = SEMANTIC_ESCAPE_CHARS;
        let mut last_reached = point;

        for (col, cell) in first_line.cells().enumerate().skip(point.column.0 + 1) {
            if !cell.flags().intersects(wide_spacers) && escape_chars.contains(cell.c()) {
                return last_reached;
            }
            last_reached = Point::new(point.line, Column(col));
        }

        if !first_line.wrapped() {
            return last_reached;
        }

        for raw_line in (point.line.0 + 1)..=bottommost.0 {
            let line = Line(raw_line);
            let line_ref = self.grid.line(line);
            for (col, cell) in line_ref.cells().enumerate() {
                if !cell.flags().intersects(wide_spacers) && escape_chars.contains(cell.c()) {
                    return last_reached;
                }
                last_reached = Point::new(line, Column(col));
            }
            if !line_ref.wrapped() {
                return last_reached;
            }
        }

        last_reached
    }

    /// Finds the start of the logical (soft-wrapped) line containing `point`.
    pub fn line_search_left(&self, mut point: Point) -> Point {
        let topmost = self.topmost_line();
        let bottommost = self.bottommost_line();
        point.line = point.line.clamp(topmost, bottommost);

        while point.line > topmost && self.grid.line(point.line - 1i32).wrapped() {
            point.line -= 1;
        }
        point.column = Column(0);
        point
    }

    /// Finds the end of the logical (soft-wrapped) line containing `point`.
    pub fn line_search_right(&self, mut point: Point) -> Point {
        let topmost = self.topmost_line();
        let bottommost = self.bottommost_line();
        point.line = point.line.clamp(topmost, bottommost);

        while point.line + 1 < self.screen_lines() && self.grid.line(point.line).wrapped() {
            point.line += 1;
        }
        point.column = self.last_column();
        point
    }

    fn find_delimiter_left(
        &self,
        line: Line,
        end_col: usize,
        wide_spacers: Flags,
        last_reached: &mut Point,
    ) -> Option<usize> {
        let escape_chars = SEMANTIC_ESCAPE_CHARS;
        match self.grid.line(line) {
            LineRef::Cells(row) => {
                for col in (0..end_col).rev() {
                    *last_reached = Point::new(line, Column(col));
                    let cell = &row[Column(col)];
                    if !cell.flags.intersects(wide_spacers) && escape_chars.contains(cell.c()) {
                        return Some(col);
                    }
                }
            },
            line_ref @ LineRef::Packed(..) => {
                let cells: Vec<(char, Flags)> =
                    line_ref.cells().take(end_col).map(|c| (c.c(), c.flags())).collect();
                for (col, (c, flags)) in cells.into_iter().enumerate().rev() {
                    *last_reached = Point::new(line, Column(col));
                    if !flags.intersects(wide_spacers) && escape_chars.contains(c) {
                        return Some(col);
                    }
                }
            },
        }
        None
    }

    fn first_non_spacer_after(&self, delim: Point, bottommost: Line, wide_spacers: Flags) -> Point {
        for (col, cell) in self
            .grid
            .line(delim.line)
            .cells()
            .enumerate()
            .skip(delim.column.0 + 1)
        {
            if !cell.flags().intersects(wide_spacers) {
                return Point::new(delim.line, Column(col));
            }
        }

        for raw_line in (delim.line.0 + 1)..=bottommost.0 {
            let line = Line(raw_line);
            for (col, cell) in self.grid.line(line).cells().enumerate() {
                if !cell.flags().intersects(wide_spacers) {
                    return Point::new(line, Column(col));
                }
            }
        }

        delim
    }
}

fn scan_brackets_backward(
    line_ref: LineRef<'_>,
    end_col: usize,
    start_char: char,
    end_char: char,
    depth: &mut usize,
) -> Option<usize> {
    match line_ref {
        LineRef::Cells(row) => {
            for col in (0..end_col).rev() {
                let c = row[Column(col)].c();
                if c == end_char {
                    if *depth == 0 {
                        return Some(col);
                    }
                    *depth -= 1;
                } else if c == start_char {
                    *depth += 1;
                }
            }
        },
        LineRef::Packed(..) => {
            let chars: Vec<char> = line_ref.cells().take(end_col).map(|c| c.c()).collect();
            for (col, c) in chars.into_iter().enumerate().rev() {
                if c == end_char {
                    if *depth == 0 {
                        return Some(col);
                    }
                    *depth -= 1;
                } else if c == start_char {
                    *depth += 1;
                }
            }
        },
    }
    None
}

#[cfg(test)]
mod tests {
    use crate::event::VoidListener;
    use crate::index::{Column, Line, Point};
    use crate::parser::Parser;
    use crate::term::test::TermSize;
    use crate::engine::Options;
    use crate::term::Term;

    fn make_term(cols: usize, rows: usize, text: &[u8]) -> Term<VoidListener> {
        let options = Options { scrollback_lines: 32, ring_rows: 2, page_bytes: 64, ..Options::default() };
        let mut term = Term::new(options, &TermSize::new(cols, rows), VoidListener);
        Parser::new().advance(&mut term, text);
        term
    }

    #[test]
    fn semantic_boundaries_respect_delimiters_and_hard_breaks() {
        let term = make_term(8, 3, b"test abc\r\ndef test");

        assert_eq!(term.semantic_search_left(Point::new(Line(0), Column(0))), Point::new(Line(0), Column(0)));
        assert_eq!(term.semantic_search_right(Point::new(Line(0), Column(0))), Point::new(Line(0), Column(3)));

        assert_eq!(term.semantic_search_left(Point::new(Line(0), Column(7))), Point::new(Line(0), Column(5)));
        assert_eq!(term.semantic_search_right(Point::new(Line(0), Column(7))), Point::new(Line(0), Column(7)));

        assert_eq!(term.semantic_search_left(Point::new(Line(1), Column(0))), Point::new(Line(1), Column(0)));
        assert_eq!(term.semantic_search_right(Point::new(Line(1), Column(0))), Point::new(Line(1), Column(2)));
    }

    #[test]
    fn semantic_boundaries_cross_soft_wraps_and_wide_spacers() {
        let term = make_term(5, 4, "abc \u{4e2d}\u{6587} x".as_bytes());

        // "abc " takes cols 0..4; '\u{4e2d}' cannot fit in col 4 alone so col 4 gets
        // LEADING_WIDE_CHAR_SPACER + WRAPLINE, and line 1 holds "\u{4e2d}\u{6587} ".
        assert_eq!(term.semantic_search_left(Point::new(Line(1), Column(2))), Point::new(Line(1), Column(0)));
        assert_eq!(term.semantic_search_right(Point::new(Line(1), Column(0))), Point::new(Line(1), Column(3)));

        let wrapped = make_term(6, 3, b"hello_world next");
        assert_eq!(wrapped.semantic_search_left(Point::new(Line(1), Column(2))), Point::new(Line(0), Column(0)));
        assert_eq!(wrapped.semantic_search_right(Point::new(Line(0), Column(2))), Point::new(Line(1), Column(4)));
    }

    #[test]
    fn line_search_follows_soft_wrapped_lines() {
        let term = make_term(5, 4, b"abcdefghijkl\r\nmnop");

        assert_eq!(term.line_search_left(Point::new(Line(2), Column(1))), Point::new(Line(0), Column(0)));
        assert_eq!(term.line_search_right(Point::new(Line(0), Column(2))), Point::new(Line(2), Column(4)));

        assert_eq!(term.line_search_left(Point::new(Line(3), Column(2))), Point::new(Line(3), Column(0)));
        assert_eq!(term.line_search_right(Point::new(Line(3), Column(0))), Point::new(Line(3), Column(4)));
    }

    #[test]
    fn bracket_search_matches_nested_and_multiline_pairs() {
        let mut term = make_term(12, 3, b"fn({[<a>]})\r\n(unclosed\r\n{\r\n  [ok]\r\n}");
        while term.settle(16) {}

        // First line scrolled into packed scrollback at Line(-2).
        assert_eq!(term.bracket_search(Point::new(Line(-2), Column(2))), Some(Point::new(Line(-2), Column(10))));
        assert_eq!(term.bracket_search(Point::new(Line(-2), Column(10))), Some(Point::new(Line(-2), Column(2))));
        assert_eq!(term.bracket_search(Point::new(Line(-2), Column(5))), Some(Point::new(Line(-2), Column(7))));
        assert_eq!(term.bracket_search(Point::new(Line(-2), Column(7))), Some(Point::new(Line(-2), Column(5))));

        // Unmatched '(' at Line(-1), Column(0).
        assert_eq!(term.bracket_search(Point::new(Line(-1), Column(0))), None);

        // Multiline '{' at Line(0), Column(0) matches '}' at Line(2), Column(0).
        assert_eq!(term.bracket_search(Point::new(Line(0), Column(0))), Some(Point::new(Line(2), Column(0))));
        assert_eq!(term.bracket_search(Point::new(Line(2), Column(0))), Some(Point::new(Line(0), Column(0))));

        // Non-bracket character returns None.
        assert_eq!(term.bracket_search(Point::new(Line(1), Column(3))), None);
    }
}
