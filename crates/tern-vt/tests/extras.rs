//! A cell is 16 bytes because what few cells have (combining characters, an
//! underline color, a hyperlink) is kept outside it, in one table for the
//! whole process, and the cell holds a number. Nothing frees that table's
//! entries but the cells giving them back, so one lost here would be lost
//! until the app quits.
//!
//! The table is shared, so this file holds a single test: nothing else in
//! its process takes or returns entries while it counts them.

use tern_vt::event::VoidListener;
use tern_vt::grid::Dimensions;
use tern_vt::index::{Column, Line};
use tern_vt::term::cell::live_extras;
use tern_vt::term::test::TermSize;
use tern_vt::engine::Options;
use tern_vt::term::Term;
use tern_vt::BareParser;

/// Output that gives cells extras in every way there is.
fn decorated(lines: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..lines {
        let line = format!(
            "{i} e\u{301}a\u{308}\u{323} \x1b]8;id={i};https://example.com/{i}\x1b\\link\x1b]8;;\x1b\\ \
             \x1b[4:3;58:2::255:0:{}mcurly\x1b[0m \u{1f469}\u{200d}\u{1f52c} \u{2764}\u{fe0f}\r\n",
            i % 256
        );
        out.extend_from_slice(line.as_bytes());
    }
    out
}

#[test]
fn every_extra_a_cell_takes_is_given_back() {
    assert_eq!(live_extras(), 0);
    {
        let options = Options {
            scrollback_lines: 50,
            ..Options::default()
        };
        let mut term = Term::new(options, &TermSize::new(40, 10), VoidListener);
        let mut parser = BareParser::default();
        let mut feed = |term: &mut Term<VoidListener>, bytes: &[u8]| {
            parser.advance(term, bytes);
        };

        // A screenful: every line has combining characters, a link and a
        // colored underline.
        feed(&mut term, &decorated(8));
        let held = live_extras();
        assert!(held >= 8 * 3, "{held} extras for 8 decorated lines");
        let cell = &term.grid()[Line(0)][Column(2)];
        assert_eq!(cell.c(), 'e');
        assert_eq!(cell.zerowidth(), Some(&['\u{301}'][..]));

        // Far more lines than there are rows: once the history is full, rows
        // are used again, and what their cells held goes back.
        let a_line = held / 8;
        feed(&mut term, &decorated(4000));
        let full = live_extras();
        assert!(
            full <= (10 + 50 + 1000) * (a_line + 1),
            "{full} extras after 4000 lines of {a_line} each"
        );

        // Copies of the screen hold shares of their own.
        let copy = term.grid().clone();
        assert!(live_extras() >= full);
        drop(copy);
        assert_eq!(live_extras(), full);

        // Narrower and wider again (lines are rewrapped, cells move from
        // row to row), shorter and taller.
        for (columns, lines) in [(17, 10), (80, 10), (80, 3), (40, 24), (5, 2), (40, 10)] {
            term.resize(TermSize::new(columns, lines));
            assert_eq!(term.columns(), columns);
        }

        // The other screen and back, with decorated text on both.
        feed(&mut term, b"\x1b[?1049h");
        feed(&mut term, &decorated(30));
        feed(&mut term, b"\x1b[?1049l");
        feed(&mut term, &decorated(5));

        // Overwriting, erasing and inserting over decorated cells.
        feed(&mut term, b"\x1b[H plain text over the first line\x1b[K");
        feed(&mut term, b"\x1b[3;1H\x1b[5P\x1b[5@\x1b[2X\x1b[2L\x1b[1M");
        feed(&mut term, b"\x1b[5;10H\x1b[1J\x1b[6;1H\x1b[0J");
        feed(&mut term, b"\x1b#8");
        feed(&mut term, &decorated(20));
        assert!(live_extras() > 0);

        // The screen and the history cleared, then a full reset.
        feed(&mut term, b"\x1b[2J\x1b[3J");
        feed(&mut term, &decorated(20));
        feed(&mut term, b"\x1bc");
        feed(&mut term, &decorated(3));
        assert!(live_extras() > 0);
    }
    // The terminal is gone, and with it every cell.
    assert_eq!(live_extras(), 0);
}
