use super::*;

use crate::term::cell::{Cell, Flags};

/// Numbers that look random and are the same every time.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

/// Scrolling up moves rows about in several ways (a region by moving
/// their numbers, the lines that go off the top into the ring of the newest
/// lines of the scrollback, and from it, packed, into its pages, of which
/// the oldest go as the scrollback reaches its length), and all of them have
/// to come to what scrolling up is: the lines of the region move up, the top
/// ones go into the scrollback if the region starts at the top and are lost
/// otherwise, empty lines come in at the bottom, and no other line changes.
#[test]
fn scrolling_up_is_the_same_whichever_way_it_is_done() {
    let mut rng = Rng(0x243f_6a88_85a3_08d3);
    for round in 0..400 {
        let lines = [1, 2, 3, 7, 12, 24][rng.below(6)];
        let columns = [1, 4, 20][rng.below(3)];
        let history = [0, 0, 5, 40, 3000][rng.below(5)];
        let ring = [1, 2, 32, 40, usize::MAX][rng.below(5)];
        let page = [64, 256, PAGE_BYTES][rng.below(3)];
        let mut grid = Grid::with_storage(lines, columns, history, ring, page);

        // What the screen and the scrollback should hold, a string a line, the oldest first.
        let mut scrollback: Vec<String> = Vec::new();
        let mut screen: Vec<String> = vec![String::new(); lines];
        let mut next = 0u32;

        for step in 0..rng.below(300) {
            // Something written to a line or two, each in one of the ways lines are written.
            for _ in 0..rng.below(3) {
                let line = rng.below(lines);
                let text: String = (0..1 + rng.below(columns))
                    .map(|_| {
                        next += 1;
                        char::from(b'a' + (next % 26) as u8)
                    })
                    .collect();
                let row = &mut grid[Line(line as i32)];
                if rng.below(2) == 0 {
                    row.write_ascii(0, text.as_bytes(), &Cell::default());
                    row.erase_from(text.len(), &Cell::default());
                } else {
                    row.reset(&Cell::default());
                    for (at, c) in text.chars().enumerate() {
                        row[Column(at)].set_c(c);
                    }
                }
                screen[line] = text;
            }

            // A scroll of some region by some number of lines, most often the usual one.
            let (start, end) = match rng.below(3) {
                0 => {
                    let start = rng.below(lines);
                    (start, start + 1 + rng.below(lines - start))
                },
                1 => (0, 1 + rng.below(lines)),
                _ => (0, lines),
            };
            let positions = match rng.below(4) {
                0 => 1 + rng.below(end - start),
                _ => 1,
            };
            if rng.below(8) == 0 {
                grid.scroll_display(Scroll::Delta(rng.below(9) as i32 - 4));
            }
            grid.scroll_up(&(Line(start as i32)..Line(end as i32)), positions);

            // Now and then, as once output has stopped, lines of the ring are packed.
            if rng.below(16) == 0 {
                grid.settle(1 + rng.below(50));
            }

            // A region of one line or fewer than it scrolls by, not at the top, is cleared.
            if end - start <= positions && start != 0 {
                for line in &mut screen[start..end] {
                    line.clear();
                }
            } else {
                let gone: Vec<String> = screen.drain(start..start + positions).collect();
                if start == 0 {
                    scrollback.extend(gone);
                    let over = scrollback.len().saturating_sub(history);
                    scrollback.drain(..over);
                }
                for _ in 0..positions {
                    screen.insert(end - positions, String::new());
                }
            }

            // Every line of the grid is the line it should be.
            assert_eq!(grid.history_size(), scrollback.len(), "round {round}, step {step}");
            assert!(grid.display_offset() <= grid.history_size(), "round {round}, step {step}");
            let all = scrollback.iter().chain(&screen);
            for (line, want) in (grid.topmost_line().0..).zip(all) {
                let got: String = grid.line(Line(line)).cells().map(|cell| cell.c()).collect();
                assert_eq!(
                    got.trim_end(),
                    want,
                    "round {round}, step {step}: line {line} after scrolling {start}..{end} by \
                     {positions} ({lines} lines, {history} of scrollback, a ring of {ring}, \
                     pages of {page})"
                );
            }
        }
    }
}

/// Lines past the ring are packed, and read as they were written, styles,
/// wide characters and extras and all.
#[test]
fn the_scrollback_past_the_ring_is_packed_and_reads_back() {
    let mut grid = Grid::with_storage(1, 6, 100, 4, 256);
    let mut written = Vec::new();
    for i in 0..60u32 {
        let row = &mut grid[Line(0)];
        row.reset(&Cell::default());
        let mut first = Cell::new(char::from(b'a' + (i % 26) as u8), crate::ansi::Color::Indexed(i as u8), crate::ansi::Color::Named(crate::ansi::NamedColor::Background), Flags::BOLD);
        if i % 3 == 0 {
            first.push_zerowidth('\u{301}');
        }
        row[Column(0)] = first;
        row[Column(1)] = Cell::new('\u{4e2d}', crate::ansi::Color::Named(crate::ansi::NamedColor::Foreground), crate::ansi::Color::Named(crate::ansi::NamedColor::Background), Flags::WIDE_CHAR);
        row[Column(2)] = Cell::new(' ', crate::ansi::Color::Named(crate::ansi::NamedColor::Foreground), crate::ansi::Color::Named(crate::ansi::NamedColor::Background), Flags::WIDE_CHAR_SPACER);
        if i % 2 == 0 {
            row[Column(5)].flags.insert(Flags::WRAPLINE);
        }
        written.push(row.cells().to_vec());
        grid.scroll_up(&(Line(0)..Line(1)), 1);
    }
    let (history, packed, _) = grid.history_stats();
    assert_eq!(history, 60);
    assert_eq!(packed, 56);
    // The newest 60 lines of the scrollback are the ones written, the oldest first.
    for (back, want) in written.iter().rev().enumerate() {
        let line = Line(-(back as i32) - 1);
        assert_eq!(cells(&grid, line.0), *want, "line {line}");
        assert_eq!(grid.line(line).wrapped(), back % 2 == 1, "line {line}");
    }
}

/// While output pours in, the ring grows into chunks of rows instead of
/// packing lines, and the lines that go out of the scrollback are never
/// packed; settling packs all but the few dozen newest and gives the chunks
/// back, and the lines read the same before and after.
#[test]
fn the_ring_grows_with_output_and_settles() {
    let mut grid = Grid::with_storage(2, 4, 1000, usize::MAX, PAGE_BYTES);
    let text = |i: usize| format!("{:04}", i % 10_000);
    for i in 0..3000 {
        let row = &mut grid[Line(1)];
        row.reset(&Cell::default());
        row.write_ascii(0, text(i).as_bytes(), &Cell::default());
        grid.scroll_up(&(Line(0)..Line(2)), 1);
    }
    let (history, packed, _) = grid.history_stats();
    assert_eq!((history, packed), (1000, 0));
    assert!(grid.slab.rows() > 1000);
    let lines = |grid: &Grid| -> Vec<String> {
        (grid.topmost_line().0..2).map(|line| grid.line(Line(line)).cells().map(|cell| cell.c()).collect()).collect()
    };
    let before = lines(&grid);
    assert_eq!(before[0], text(1999));
    while grid.settle(300) {}
    let (history, packed, _) = grid.history_stats();
    assert_eq!((history, packed), (1000, 1000 - RING_ROWS));
    assert_eq!(grid.slab.rows(), grid.slab.base_rows());
    assert_eq!(lines(&grid), before);
    // Output after settling goes into the ring again, and the oldest packed lines go.
    for i in 3000..3100 {
        let row = &mut grid[Line(1)];
        row.reset(&Cell::default());
        row.write_ascii(0, text(i).as_bytes(), &Cell::default());
        grid.scroll_up(&(Line(0)..Line(2)), 1);
    }
    assert_eq!(grid.history_size(), 1000);
    assert_eq!(lines(&grid)[0], text(2099));
    assert_eq!(grid.history_stats().1, 1000 - RING_ROWS - 100);
}

/// Scrolls `count` numbered lines through the last line of the screen.
fn flood(grid: &mut Grid, from: usize, count: usize) {
    let bottom = grid.screen_lines() - 1;
    for i in from..from + count {
        let row = &mut grid[Line(bottom as i32)];
        row.reset(&Cell::default());
        row.write_ascii(0, format!("{:05}", i % 100_000).as_bytes(), &Cell::default());
        grid.scroll_up(&(Line(0)..Line(bottom as i32 + 1)), 1);
    }
}

/// The ring may grow as far as the size of the moment lets it: a window
/// made wide, where its rows cost more, and narrow again, gets back the
/// ring it had, and the whole scrollback fits in it again.
#[test]
fn the_ring_grows_as_far_again_once_the_window_is_narrow_again() {
    let mut grid = Grid::with_storage(2, 40, 10_000, usize::MAX, PAGE_BYTES);
    assert_eq!(grid.ring_rows, 10_000);
    grid.resize(true, 2, 400);
    let wide = ring_limit(2, 400, 10_000, usize::MAX);
    assert!(wide < 10_000, "a ring of {wide} rows of 400 columns");
    assert_eq!(grid.ring_rows, wide);
    grid.resize(true, 2, 40);
    assert_eq!(grid.ring_rows, 10_000);
    while grid.settle(usize::MAX) {}
    let packed = grid.history_stats().1;
    flood(&mut grid, 0, 12_000);
    let (history, now_packed, _) = grid.history_stats();
    assert_eq!(history, 10_000);
    // The lines packed before are gone, and none of the flood was packed.
    assert_eq!(now_packed, 0, "{packed} packed before the flood");
}

/// Making the scrollback longer lets the ring grow as far, and making it
/// longer than none at all gives the ring room for the newest line again.
#[test]
fn a_longer_scrollback_lets_the_ring_grow_as_far() {
    let mut grid = Grid::with_storage(2, 6, 100, usize::MAX, PAGE_BYTES);
    assert_eq!(grid.ring_rows, 100);
    grid.update_history(5_000);
    assert_eq!(grid.ring_rows, 5_000);
    flood(&mut grid, 0, 6_000);
    assert_eq!(grid.history_stats(), (5_000, 0, 0));
    grid.update_history(50);
    assert_eq!(grid.ring_rows, 50);
    assert_eq!(grid.history_size(), 50);
    // A test's small cap stays the cap.
    let mut small = Grid::with_storage(2, 4, 100, 4, 256);
    small.update_history(5_000);
    assert_eq!(small.ring_rows, 4);

    // From no scrollback at all.
    let mut grid = Grid::with_storage(2, 6, 0, usize::MAX, PAGE_BYTES);
    assert_eq!(grid.ring_rows, 0);
    grid.update_history(1_000);
    assert_eq!(grid.ring_rows, 1_000);
    flood(&mut grid, 0, 1_500);
    while grid.settle(300) {}
    assert_eq!(grid.history_stats().0, 1_000);
    // The newest line of the scrollback is one of the ring's still.
    let newest: String = grid[Line(-1)][..].iter().map(|cell| cell.c()).collect();
    assert_eq!(newest.trim_end(), "01498");
    let oldest: String = grid.line(Line(-1_000)).cells().map(|cell| cell.c()).collect();
    assert_eq!(oldest.trim_end(), "00499");
}

fn cells(grid: &Grid, line: i32) -> Vec<Cell> {
    let mut out = vec![Cell::default(); grid.columns()];
    grid.line(Line(line)).copy_into(&mut out);
    out
}

fn line_str(grid: &Grid, line: i32) -> String {
    grid.line(Line(line)).cells().map(|c| c.c()).collect::<String>().trim_end().to_owned()
}

#[test]
fn scroll_up_region_and_full_screen() {
    let mut grid = Grid::new(4, 6, 10);
    for (i, label) in ["zero", "one", "two", "three"].into_iter().enumerate() {
        grid[Line(i as i32)].write_ascii(0, label.as_bytes(), &Cell::default());
    }

    // Sub-region scroll_up does not push to history and leaves outside lines intact.
    grid.scroll_up(&(Line(1)..Line(3)), 1);
    assert_eq!(grid.history_size(), 0);
    assert_eq!(line_str(&grid, 0), "zero");
    assert_eq!(line_str(&grid, 1), "two");
    assert_eq!(line_str(&grid, 2), "");
    assert_eq!(line_str(&grid, 3), "three");

    // Full-screen scroll_up retires top lines to history and tracks display_offset when scrolled.
    grid.scroll_up(&(Line(0)..Line(4)), 1);
    assert_eq!(grid.history_size(), 1);
    assert_eq!(line_str(&grid, -1), "zero");
    assert_eq!(line_str(&grid, 0), "two");

    grid.scroll_display(Scroll::Top);
    assert_eq!(grid.display_offset(), 1);
    grid.scroll_up(&(Line(0)..Line(4)), 2);
    assert_eq!(grid.history_size(), 3);
    assert_eq!(grid.display_offset(), 3);
    assert_eq!(line_str(&grid, -3), "zero");
    assert_eq!(line_str(&grid, -2), "two");
    assert_eq!(line_str(&grid, -1), "");
}

#[test]
fn scroll_down_shifts_lines_and_fills_template_background() {
    let mut grid = Grid::new(4, 5, 10);
    for (i, label) in ["row0", "row1", "row2", "row3"].into_iter().enumerate() {
        grid[Line(i as i32)].write_ascii(0, label.as_bytes(), &Cell::default());
    }
    grid.cursor.template.set_bg(crate::ansi::Color::Indexed(4));

    grid.scroll_down(&(Line(1)..Line(4)), 2);
    assert_eq!(grid.history_size(), 0);
    assert_eq!(line_str(&grid, 0), "row0");
    assert_eq!(line_str(&grid, 1), "");
    assert_eq!(line_str(&grid, 2), "");
    assert_eq!(line_str(&grid, 3), "row1");
    assert_eq!(grid[Line(1)][Column(0)].bg(), crate::ansi::Color::Indexed(4));
    assert_eq!(grid[Line(2)][Column(4)].bg(), crate::ansi::Color::Indexed(4));
}

#[test]
fn clear_viewport_retires_only_occupied_prefix() {
    let mut grid = Grid::new(5, 8, 20);
    // Clearing an already empty viewport does not pollute history.
    grid.clear_viewport();
    assert_eq!(grid.history_size(), 0);

    grid[Line(0)].write_ascii(0, b"first", &Cell::default());
    grid[Line(2)].write_ascii(0, b"third", &Cell::default());
    grid.cursor.template.set_bg(crate::ansi::Color::Indexed(2));

    grid.clear_viewport();
    assert_eq!(grid.history_size(), 3);
    assert_eq!(line_str(&grid, -3), "first");
    assert_eq!(line_str(&grid, -2), "");
    assert_eq!(line_str(&grid, -1), "third");
    for line in 0..5 {
        assert_eq!(line_str(&grid, line), "");
        assert_eq!(grid[Line(line)][Column(0)].bg(), crate::ansi::Color::Indexed(2));
        assert_eq!(grid[Line(line)][Column(7)].bg(), crate::ansi::Color::Indexed(2));
    }
}

