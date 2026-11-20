//! A cell is 16 bytes because what few cells have (combining characters, an
//! underline color, a hyperlink) is kept outside it, in one table for the
//! whole process, and the cell holds a number. Nothing frees that table's
//! entries but the cells giving them back, so one lost here would be lost
//! until the app quits.
//!
//! The table is shared, so this file holds a single test: nothing else in
//! its process takes or returns entries while it counts them.

mod common;

use std::time::{Duration, Instant};

use tern_vt::engine::live_extras;

#[test]
fn every_extra_a_cell_takes_is_given_back() {
    assert_eq!(live_extras(), 0);

    // Through a session, whose interpreter writes whole stretches of
    // text itself: where it knows that a row's cells hold nothing to give
    // back it writes over them without a look, so it had better know. Rows
    // are used again and again here, by lines of every kind in turn, and
    // the scrollback is packed as lines go into it and read again.
    {
        let mut h = common::sh(
            "i=0; while [ $i -lt 3000 ]; do printf 'plain line %d\\n' $i; \
             printf 'e\\314\\201a\\314\\210 \\033]8;;https://example.com/%d\\033\\\\link\\033]8;;\\033\\\\ \\033[4;58;5;%dmcurly\\033[0m\\n' $i $((i%256)); \
             printf '\\346\\227\\245\\346\\234\\254\\350\\252\\236 %d\\n' $i; printf '\\ttab %d\\033[K\\n' $i; \
             i=$((i+1)); done; printf 'the end'; sleep 30",
        );
        h.wait_for("the end");
        // Every line of the scrollback but the newest few dozen is packed,
        // extras and all: the cells that hold entries are the screen's and
        // those few dozen lines', with three on each line of the kind that
        // has them, every fourth line.
        // Once output has stopped, that is.
        h.wait_until("the scrollback to be packed", || {
            let (history, packed, _) = h.session.scrollback_stats();
            history >= 10_000 && packed + 32 >= history
        });
        let rows = usize::from(h.snapshot().rows);
        let most = (rows + 32).div_ceil(4) * 12;
        assert!(live_extras() > 0 && live_extras() <= most, "{} extras with the history full", live_extras());
        // The packed lines still have theirs: links and colored underlines
        // show where they were written.
        h.session.scroll(tern_core::Scroll::Top);
        h.snapshot();
        let linked = h.frame.cells.iter().filter(|cell| cell.has(tern_types::flags::HYPERLINK)).count();
        assert!(linked >= rows / 4 * 4, "{linked} cells with a link at the top of the scrollback");
        h.session.scroll(tern_core::Scroll::Pages(-3));
        h.snapshot();
        h.session.resize(57, 19, 8, 16);
        h.wait_grid(57, 19);
        h.session.scroll(tern_core::Scroll::Bottom);
        h.snapshot();
        h.session.write(b"");
        assert!(live_extras() > 0);
    }
    // The session's threads let go of the terminal a moment after it is
    // dropped.
    let deadline = Instant::now() + common::TIMEOUT;
    while live_extras() != 0 {
        assert!(Instant::now() < deadline, "{} extras were never given back", live_extras());
        std::thread::sleep(Duration::from_millis(5));
    }
}
