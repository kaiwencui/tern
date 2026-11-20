//! Sessions running real children on real PTYs.

mod common;

use std::time::{Duration, Instant};

use common::{config, sh, spawn, spawn_with, Harness, Recorder};
use tern_core::{Scroll, SelectionKind, Session, SessionConfig, Side};
use tern_types::{flags, CursorShape, MouseEncoding, MouseTracking, Theme};

/// A script that switches the terminal to raw input, makes the terminal
/// answer a query, and prints the reply with ESC shown as E and BEL as B.
fn query(request: &str, reply_len: usize) -> Harness {
    sh(&format!(
        "stty -echo -icanon min 1; printf '{request}'; r=$(dd bs=1 count={reply_len} 2>/dev/null); \
         printf 'GOT:%s\\n' \"$(printf '%s' \"$r\" | tr '\\033\\007' EB)\"; sleep 5"
    ))
}

#[test]
fn printf_output_is_visible() {
    let h = sh("printf 'hello\\nworld'");
    h.wait_for("hello\nworld");
    assert_eq!(h.wait_exit(), Some(0));
}

#[test]
fn colors_and_attributes_resolve() {
    let mut h = sh("printf '\\033[1;31mA\\033[0;7mB\\033[0;38;5;196mC\\033[0;38;2;1;2;3;48;2;4;5;6mD\
        \\033[0;4mE\\033[0;4:2mF\\033[0;4:3mG\\033[0;4:4mH\\033[0;4:5mI\\033[0;58;2;9;8;7;4mJ\\033[0;3;9mK\\033[0m'");
    h.wait_exit();
    let theme = Theme::default();
    let frame = h.snapshot();
    let row = frame.row(0);
    assert_eq!(frame.row_text(0), "ABCDEFGHIJK");
    assert_eq!(row[0].fg, theme.palette[1]);
    assert!(row[0].has(flags::BOLD));
    assert_eq!((row[1].fg, row[1].bg), (theme.bg, theme.fg));
    assert_eq!(row[2].fg, 0xff0000);
    assert_eq!((row[3].fg, row[3].bg), (0x010203, 0x040506));
    assert!(row[4].has(flags::UNDERLINE));
    assert!(row[5].has(flags::DOUBLE_UNDERLINE));
    assert!(row[6].has(flags::UNDERCURL));
    assert!(row[7].has(flags::DOTTED_UNDERLINE));
    assert!(row[8].has(flags::DASHED_UNDERLINE));
    assert_eq!(row[4].ul, row[4].fg);
    assert_eq!(row[9].ul, 0x090807);
    assert!(row[10].has(flags::ITALIC) && row[10].has(flags::STRIKEOUT));
    assert_eq!(row[11].flags, 0);
}

#[test]
fn wide_and_combining_characters() {
    let mut h = sh("printf '%s' '日本e\u{301}!'");
    h.wait_exit();
    let frame = h.snapshot();
    assert_eq!(frame.row_text(0), "日本e\u{301}!");
    assert!(frame.cell(0, 0).has(flags::WIDE));
    assert!(frame.cell(0, 1).has(flags::WIDE_SPACER));
    assert!(frame.cell(0, 2).has(flags::WIDE));
    assert!(frame.cell(0, 4).has(flags::HAS_EXTRA));
    assert_eq!(frame.extras(4), &['\u{301}']);
    assert_eq!(frame.cell(0, 5).char(), '!');
}

#[test]
fn input_echoes_through_cat() {
    let h = spawn(config(&["cat"]));
    h.session.write(b"ping\r");
    // Once echoed by the terminal driver, once printed by cat.
    h.wait_for("ping\nping");
}

#[test]
fn cursor_position_report_round_trips() {
    let h = query("\\033[3;5H\\033[6n", 6);
    h.wait_for("GOT:E[3;5R");
}

#[test]
fn device_attributes_and_size_reports() {
    query("\\033[c", 5).wait_for("GOT:E[?6c");
    // Text area in pixels: 24 rows * 16 px, 80 columns * 8 px.
    query("\\033[14t", 12).wait_for("GOT:E[4;384;640t");
    query("\\033[18t", 10).wait_for("GOT:E[8;24;80t");
}

#[test]
fn color_query_answers_current_colors() {
    query("\\033]11;?\\007", 24).wait_for("GOT:E]11;rgb:1818/1a1a/1f1fB");
    // After the program changed palette entry 1, the query sees the change.
    query("\\033]4;1;rgb:12/34/56\\007\\033]4;1;?\\007", 25)
        .wait_for("GOT:E]4;1;rgb:1212/3434/5656B");
}

#[test]
fn resize_is_seen_by_the_child() {
    let mut h = sh("stty size; read x; stty size; sleep 5");
    h.wait_for("24 80");
    h.session.resize(100, 30, 8, 16);
    // The parser thread applies the size, then tells the child.
    h.wait_grid(100, 30);
    h.session.write(b"\r");
    h.wait_for("30 100");
}

#[test]
fn resizing_a_long_history_does_not_block_the_caller() {
    // 60,000 lines of 150 characters, every one kept in the history.
    let mut h = spawn(SessionConfig {
        cols: 160,
        rows: 40,
        scrollback_lines: 100_000,
        ..config(&[
            "/bin/sh",
            "-c",
            "yes \"$(printf '%0150d' 0)\" | head -n 60000; printf DONE; sleep 30",
        ])
    });
    h.wait_for("DONE");
    let start = Instant::now();
    h.session.resize(70, 40, 8, 16);
    let call = start.elapsed();
    let mut longest = Duration::ZERO;
    loop {
        let t = Instant::now();
        h.session.snapshot(&mut h.frame);
        longest = longest.max(t.elapsed());
        if h.frame.cols == 70 {
            break;
        }
        assert!(
            start.elapsed() < common::TIMEOUT,
            "the grid was never reflowed"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let reflow = start.elapsed();
    eprintln!("resize call {call:?}, reflow {reflow:?}, longest snapshot {longest:?}");
    assert!(call < Duration::from_millis(10), "resize took {call:?}");
    // Snapshots skip the terminal while it reflows instead of waiting.
    if reflow > Duration::from_millis(40) {
        assert!(longest * 2 < reflow, "a snapshot waited {longest:?}");
    }
    // Each line now takes three rows, more than the history keeps.
    assert_eq!(h.frame.history, 100_000);
}

#[test]
fn calls_from_the_app_never_wait_out_a_reflow() {
    // The same long history; while it reflows, every call the app makes on
    // its main thread returns at once, and the changes apply afterwards.
    let mut h = spawn(SessionConfig {
        cols: 160,
        rows: 40,
        scrollback_lines: 100_000,
        ..config(&[
            "/bin/sh",
            "-c",
            "yes \"$(printf '%0150d' 0)\" | head -n 60000; printf DONE; sleep 30",
        ])
    });
    h.wait_for("DONE");
    let start = Instant::now();
    h.session.resize(70, 40, 8, 16);
    let mut longest = Duration::ZERO;
    let mut calls = 0;
    loop {
        let t = Instant::now();
        h.session.focus(calls % 2 == 0);
        h.session.scroll(Scroll::Top);
        h.session
            .selection_start(0, 0, Side::Left, SelectionKind::Simple);
        h.session.selection_update(1, 5, Side::Right);
        let _ = h.session.has_selection();
        let _ = h.session.selection_text();
        let _ = h.session.hyperlink_at(0, 0);
        h.session.set_theme(Theme::default());
        h.session.snapshot(&mut h.frame);
        longest = longest.max(t.elapsed());
        calls += 1;
        if h.frame.cols == 70 {
            break;
        }
        assert!(
            start.elapsed() < common::TIMEOUT,
            "the grid was never reflowed"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let reflow = start.elapsed();
    eprintln!("{calls} rounds of calls during a {reflow:?} reflow; the longest took {longest:?}");
    if reflow > Duration::from_millis(40) {
        assert!(longest * 2 < reflow, "a call waited {longest:?}");
    }
    // What was asked for during the reflow took effect.
    h.session.snapshot(&mut h.frame);
    assert_eq!(h.frame.display_offset, h.frame.history);
    assert!(h.session.has_selection());
    let text = h.session.selection_text().expect("selected text");
    assert!(text.starts_with("000000"), "{text:?}");
}

#[test]
fn calls_from_the_app_never_wait_for_a_large_copy() {
    // Turning a selection of a long history into text takes a while. The
    // app does it on another thread, and the calls its main thread makes
    // meanwhile return at once; the changes they ask for apply afterwards.
    let mut h = spawn(SessionConfig {
        cols: 160,
        rows: 40,
        scrollback_lines: 100_000,
        ..config(&[
            "/bin/sh",
            "-c",
            "yes \"$(printf '%0150d' 0)\" | head -n 100000; printf DONE; sleep 30",
        ])
    });
    h.wait_for("DONE");
    h.session.select_all();
    let session = std::sync::Arc::clone(&h.session);
    let copy = std::thread::spawn(move || {
        let start = Instant::now();
        let text = session.selection_text();
        (text, start.elapsed())
    });
    let mut longest = Duration::ZERO;
    let mut rounds = 0;
    while !copy.is_finished() {
        let t = Instant::now();
        h.session.focus(rounds % 2 == 0);
        h.session.scroll(Scroll::Lines(1));
        let _ = h.session.has_selection();
        let _ = h.session.hyperlink_at(0, 0);
        h.session.snapshot(&mut h.frame);
        longest = longest.max(t.elapsed());
        rounds += 1;
        std::thread::sleep(Duration::from_millis(1));
    }
    let (text, took) = copy.join().expect("the copy thread");
    eprintln!("{rounds} rounds of calls during a {took:?} copy; the longest took {longest:?}");
    let text = text.expect("the selected text");
    assert!(
        text.lines().count() >= 100_000,
        "{} lines",
        text.lines().count()
    );
    if took > Duration::from_millis(40) {
        assert!(longest * 2 < took, "a call waited {longest:?}");
    }
    // The scrolls asked for during the copy took effect after it.
    h.session.snapshot(&mut h.frame);
    assert!(h.frame.display_offset > 0);
}

#[test]
fn messages_from_the_app_show_on_the_screen_even_after_the_exit() {
    let mut h = sh("printf 'program output\\n'; exit 3");
    assert_eq!(h.wait_exit(), Some(3));
    h.session
        .show_message("\r\n\x1b[1m[the program exited]\x1b[0m\r\n");
    let text = h.session.text();
    assert!(
        text.contains("program output\n\n[the program exited]\n"),
        "{text}"
    );
    h.session.snapshot(&mut h.frame);
    let row = (0..h.frame.rows)
        .find(|&r| h.frame.row_text(r) == "[the program exited]")
        .expect("the message row");
    assert!(h.frame.cell(row, 0).has(flags::BOLD));
}

#[test]
fn the_cursor_style_changes_while_the_program_runs() {
    let mut h = sh("printf ready; sleep 30");
    h.wait_for("ready");
    let shape = |h: &mut Harness| {
        h.session.snapshot(&mut h.frame);
        let c = h.frame.cursor.expect("a cursor");
        (c.shape, c.blinking)
    };
    assert_eq!(shape(&mut h), (CursorShape::Block, false));
    h.session.set_cursor_style(CursorShape::Beam, true);
    assert_eq!(shape(&mut h), (CursorShape::Beam, true));
    h.session.set_cursor_style(CursorShape::Underline, false);
    assert_eq!(shape(&mut h), (CursorShape::Underline, false));
}

#[test]
fn mode_12_leaves_the_configured_blinking_alone() {
    // vim's cnorm and cvvis reset and set mode 12; only the configuration
    // and DECSCUSR decide the blinking. The second sequence comes in two
    // reads and sets mode 25 with it.
    let script = "printf 'one\\033[?12l'; sleep 0.3; printf '\\033[?25;1'; sleep 0.3; \
                  printf '2lsplit\\033[?25h'; sleep 0.3; printf 'steady\\033[2 q'; sleep 0.3; \
                  printf 'reset\\033[0 q'; sleep 30";
    let mut h = spawn(SessionConfig {
        cursor_shape: CursorShape::Beam,
        cursor_blink: true,
        ..config(&["/bin/sh", "-c", script])
    });
    let cursor = |h: &mut Harness| {
        h.session.snapshot(&mut h.frame);
        h.frame.cursor.map(|c| (c.shape, c.blinking))
    };
    h.wait_for("one");
    assert_eq!(cursor(&mut h), Some((CursorShape::Beam, true)));
    h.wait_for("split");
    assert_eq!(cursor(&mut h), Some((CursorShape::Beam, true)));
    h.wait_for("steady");
    assert_eq!(cursor(&mut h), Some((CursorShape::Block, false)));
    h.wait_for("reset");
    assert_eq!(cursor(&mut h), Some((CursorShape::Beam, true)));
    // The configuration still decides afterwards.
    h.session.set_cursor_style(CursorShape::Underline, false);
    assert_eq!(cursor(&mut h), Some((CursorShape::Underline, false)));

    // Nor does setting it make a steady cursor blink, while the other
    // modes of the same sequence apply.
    let mut h = sh("printf 'x\\033[?12;25h\\033[?25l\\033[?25;12h'; sleep 30");
    h.wait_for("x");
    assert_eq!(cursor(&mut h), Some((CursorShape::Block, false)));
}

#[test]
fn session_threads_ask_for_user_initiated_service() {
    // Callbacks run on the session's threads, so the wakeup can read the
    // class of the thread it runs on.
    #[derive(Default)]
    struct Classes(std::sync::Mutex<Vec<u32>>);
    impl tern_core::SessionEvents for Classes {
        fn wakeup(&self) {
            let mut class: u32 = 0;
            // SAFETY: reads the calling thread's class into a u32, which has
            // the size and alignment of qos_class_t.
            unsafe {
                libc::pthread_get_qos_class_np(
                    libc::pthread_self(),
                    (&mut class as *mut u32).cast(),
                    std::ptr::null_mut(),
                )
            };
            self.0.lock().unwrap().push(class);
        }
        fn title(&self, _: Option<String>) {}
        fn bell(&self) {}
        fn exited(&self, _: Option<i32>) {}
        fn clipboard_store(&self, _: String) {}
    }
    let classes = std::sync::Arc::new(Classes::default());
    let session = Session::spawn(config(&["/bin/echo", "hi"]), classes.clone()).expect("spawn");
    let start = Instant::now();
    while classes.0.lock().unwrap().is_empty() {
        assert!(start.elapsed() < common::TIMEOUT, "no wakeup");
        std::thread::sleep(Duration::from_millis(5));
    }
    let seen = classes.0.lock().unwrap().clone();
    assert_eq!(
        seen[0],
        libc::qos_class_t::QOS_CLASS_USER_INITIATED as u32,
        "{seen:?}"
    );
    drop(session);
}

#[test]
fn exit_status_arrives_after_the_last_output() {
    let h = sh("seq 1 3000; exit 7");
    assert_eq!(h.wait_exit(), Some(7));
    let text = h
        .events
        .with(|r| r.exit_text.clone())
        .expect("screen at exit");
    assert!(
        text.contains("2999\n3000\n"),
        "last output missing at exit:\n{text}"
    );
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(h.events.with(|r| r.exits.len()), 1);
    assert!(!h.session.is_alive());
}

#[test]
fn exit_is_seen_while_a_background_job_holds_the_terminal() {
    // The background sleep keeps the terminal open after the shell exits, so
    // the PTY never reports end of file; the exit must still be noticed.
    let h = sh("sleep 3 & printf done; exit 4");
    let start = Instant::now();
    assert_eq!(h.wait_exit(), Some(4));
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(h.session.text().contains("done"));
}

#[test]
fn session_is_send_and_sync() {
    fn check<T: Send + Sync>() {}
    check::<Session>();
}

#[test]
fn killed_child_reports_no_status() {
    let h = sh("kill -9 $$");
    assert_eq!(h.wait_exit(), None);
}

#[test]
fn titles_and_bell() {
    let h =
        sh("printf '\\033]2;hello title\\007'; printf '\\007'; printf '\\033]0;second\\033\\\\'");
    h.events
        .wait("titles", |r| r.titles.len() >= 2 && r.bells >= 1);
    let titles = h.events.with(|r| r.titles.clone());
    assert_eq!(
        titles,
        vec![Some("hello title".to_string()), Some("second".to_string())]
    );
}

/// Returns how many lines the scrollback has, after checking that all but
/// the few dozen newest are packed, in a fraction of the room their cells
/// would take.
fn wait_packed(h: &Harness) -> usize {
    // Lines output leaves as cells are packed a moment after it stops.
    h.wait_until("the scrollback to be packed", || {
        let (history, packed, _) = h.session.scrollback_stats();
        history > 0 && packed + 32 >= history
    });
    let (history, packed, bytes) = h.session.scrollback_stats();
    let columns = usize::from(h.frame.cols.max(1));
    assert!(bytes < packed.max(1) * columns * 16 / 2, "{bytes} bytes for {packed} lines");
    history
}

/// The lines of the scrollback are kept packed, at a fraction of what they
/// take on the screen, whatever they hold: a change of color every few
/// characters, text in another script, tabs. Scrolled back into view, all
/// of them read as written.
#[test]
fn scrollback_is_packed_and_reads_as_it_was_written() {
    let colors = "\\033[31ma\\033[32mb\\033[33mc\\033[34md\\033[35me\\033[36mf\\033[91mg\\033[92mh\\033[93mi\\033[94mj\\033[95mk\\033[0m";
    let mut h = sh(&format!(
        "i=0; while [ $i -lt 400 ]; do printf 'plain line %d\\n' $i; printf '{colors} %d\\n' $i; \
         printf '\\346\\227\\245\\346\\234\\254\\350\\252\\236 %d\\n' $i; printf '\\tindented\\t%d\\n' $i; \
         i=$((i+1)); done; printf 'the end'; sleep 30"
    ));
    h.wait_for("the end");
    let rows = usize::from(h.snapshot().rows);
    let history = wait_packed(&h);
    assert_eq!(history, 4 * 400 + 1 - rows);

    // From the top: the first lines, as they were printed.
    h.session.scroll(Scroll::Top);
    h.snapshot();
    for i in 0..rows / 4 {
        let at = (4 * i) as u16;
        assert_eq!(h.frame.row_text(at), format!("plain line {i}"));
        assert_eq!(h.frame.row_text(at + 1), format!("abcdefghijk {i}"));
        assert_eq!(h.frame.row_text(at + 2), format!("\u{65e5}\u{672c}\u{8a9e} {i}"));
        // A tab shows as blanks up to the next tab stop.
        assert_eq!(h.frame.row_text(at + 3), format!("{:8}indented{:8}{i}", "", ""));
    }
    let theme = Theme::default();
    let colored = h.frame.cells[usize::from(h.frame.cols)..][..11].to_vec();
    assert_eq!(colored[0].fg, theme.palette[1]);
    assert_eq!(colored[5].fg, theme.palette[6]);
    assert_eq!(colored[10].fg, theme.palette[13]);
    assert!(h.frame.cell(2, 0).has(flags::WIDE));

    // Back at the bottom, with the scrollback as it was.
    h.session.scroll(Scroll::Bottom);
    assert!(h.snapshot().text().contains("the end"));
    wait_packed(&h);

    // A wider and a narrower window rewrap the lines, which unpacks them;
    // then they are packed again, and still read the same.
    h.session.resize(100, 30, 8, 16);
    h.wait_grid(100, 30);
    wait_packed(&h);
    h.session.resize(60, 20, 8, 16);
    h.wait_grid(60, 20);
    wait_packed(&h);
    h.session.scroll(Scroll::Top);
    h.snapshot();
    assert_eq!(h.frame.row_text(0), "plain line 0");
    assert_eq!(h.frame.row_text(1), "abcdefghijk 0");
    assert_eq!(h.frame.row_text(2), "\u{65e5}\u{672c}\u{8a9e} 0");
    assert_eq!(h.frame.row_text(4), "plain line 1");
}

#[test]
fn scrollback_and_scrolling() {
    let mut h = sh("seq 1 100");
    h.wait_exit();
    let history = h.snapshot().history;
    assert!(history >= 70, "history {history}");
    assert_eq!(h.frame.display_offset, 0);

    h.session.scroll(Scroll::Top);
    assert_eq!(h.snapshot().display_offset, history);
    assert_eq!(h.frame.row_text(0), "1");
    assert_eq!(h.frame.cursor, None);

    h.session.scroll(Scroll::Bottom);
    h.session.scroll(Scroll::Lines(5));
    assert_eq!(h.snapshot().display_offset, 5);
    h.session.scroll(Scroll::Lines(-2));
    assert_eq!(h.snapshot().display_offset, 3);
    h.session.scroll(Scroll::Pages(1));
    assert_eq!(h.snapshot().display_offset, 27);
    h.session.scroll(Scroll::Pages(-10));
    assert_eq!(h.snapshot().display_offset, 0);
    assert!(h.frame.text().contains("100"));
}

#[test]
fn selection_kinds() {
    let mut h = sh("printf 'alpha beta gamma\\nsecond line here\\nthird'");
    h.wait_exit();
    let s = &h.session;

    s.selection_start(0, 0, Side::Left, SelectionKind::Simple);
    assert!(!s.has_selection());
    s.selection_update(0, 4, Side::Right);
    assert!(s.has_selection());
    assert_eq!(s.selection_text().as_deref(), Some("alpha"));
    let frame = h.snapshot();
    assert!(frame.cell(0, 0).has(flags::SELECTED) && !frame.cell(0, 5).has(flags::SELECTED));

    h.session
        .selection_start(0, 7, Side::Left, SelectionKind::Word);
    h.session.selection_update(0, 7, Side::Left);
    assert_eq!(h.session.selection_text().as_deref(), Some("beta"));

    h.session
        .selection_start(1, 3, Side::Left, SelectionKind::Line);
    assert_eq!(
        h.session.selection_text().as_deref(),
        Some("second line here\n")
    );

    h.session
        .selection_start(0, 0, Side::Left, SelectionKind::Block);
    h.session.selection_update(1, 4, Side::Right);
    assert_eq!(h.session.selection_text().as_deref(), Some("alpha\nsecon"));

    // A drag above the viewport clamps to its first cell.
    h.session
        .selection_start(1, 2, Side::Left, SelectionKind::Simple);
    h.session.selection_update(-5, 9, Side::Right);
    assert_eq!(
        h.session.selection_text().as_deref(),
        Some("alpha beta gamma\nse")
    );
    // And below it to its last.
    h.session
        .selection_start(2, 0, Side::Left, SelectionKind::Simple);
    h.session.selection_update(500, 0, Side::Left);
    assert!(h
        .session
        .selection_text()
        .is_some_and(|t| t.starts_with("third")));

    h.session.select_all();
    let all = h.session.selection_text().expect("all");
    assert!(all.contains("alpha beta gamma") && all.contains("third"));

    h.session.selection_clear();
    assert!(!h.session.has_selection());
    assert_eq!(h.session.selection_text(), None);
    assert!(!h.snapshot().cells.iter().any(|c| c.has(flags::SELECTED)));
}

#[test]
fn alternate_screen() {
    let h = sh("printf main; read a; printf '\\033[?1049h\\033[Halt screen'; read b; printf '\\033[?1049l'; read c");
    h.wait_for("main");
    assert!(!h.session.modes().alt_screen);
    h.session.write(b"\r");
    h.wait_for("alt screen");
    assert!(h.session.modes().alt_screen);
    assert!(!h.session.text().contains("main"));
    h.session.write(b"\r");
    h.wait_text("main screen back", |t| {
        t.contains("main") && !t.contains("alt screen")
    });
    assert!(!h.session.modes().alt_screen);
}

/// Snapshots after every wakeup, as the app does, until the frame's text
/// passes; returns that text.
fn shown(h: &mut Harness, what: &str, pass: impl Fn(&str) -> bool) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut wakeups = 0;
    loop {
        h.snapshot();
        let text = h.frame.text();
        if pass(&text) {
            return text;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}; shown:\n{text}");
        // A wakeup, or a moment: a snapshot clears the first, so one sent
        // while it was being taken is still seen.
        let events = h.events.clone();
        let arrived = move || events.with(|r| r.wakeups > wakeups);
        let until = Instant::now() + Duration::from_millis(20);
        while !arrived() && Instant::now() < until {
            std::thread::sleep(Duration::from_micros(200));
        }
        wakeups = h.events.with(|r| r.wakeups);
    }
}

#[test]
fn synchronized_update_times_out() {
    let mut h = sh("printf 'before\\033[?2026hduring'; sleep 5");
    // What came before the update is shown at once, and the update is not.
    let text = shown(&mut h, "before", |t| t.contains("before"));
    let seen = Instant::now();
    assert!(!text.contains("during"), "a half-finished update was shown");
    // The terminal itself has it, for whoever asks the program's side.
    h.wait_for("during");
    // The session must end the update by itself and wake us for it: after
    // its 150 ms and not sooner.
    shown(&mut h, "during", |t| t.contains("during"));
    let waited = seen.elapsed();
    assert!(waited > Duration::from_millis(100), "shown after {waited:?}");
    assert!(waited < Duration::from_secs(2), "took {waited:?}");
}

#[test]
fn synchronized_update_ends_on_request() {
    let mut h = sh("printf ready; read a; printf '\\033[?2026hfirst\\033[?2026l'; sleep 5");
    shown(&mut h, "ready", |t| t.contains("ready"));
    h.session.write(b"\r");
    let start = Instant::now();
    shown(&mut h, "first", |t| t.contains("first"));
    // Well before the 150 ms timeout would have shown it.
    assert!(
        start.elapsed() < Duration::from_millis(120),
        "took {:?}",
        start.elapsed()
    );
}

#[test]
fn a_program_inside_an_update_gets_its_answers_at_once() {
    // A report asked for inside an update is answered then, not when the
    // update ends (this one never does), and the update's own mode reads
    // as set while it is under way and as reset outside it.
    query("\\033[?2026h\\033[?2026$p", 11).wait_for("GOT:E[?2026;1$y");
    query("\\033[?2026h\\033[?2026l\\033[?2026$p", 11).wait_for("GOT:E[?2026;2$y");
    let h = sh("stty -echo -icanon min 1; printf ready; dd bs=1 count=1 >/dev/null 2>&1; \
         printf '\\033[?2026h\\033[3;5H\\033[6n'; r=$(dd bs=1 count=6 2>/dev/null); \
         printf 'GOT:%s\\n' \"$(printf '%s' \"$r\" | tr '\\033' E)\"; sleep 5");
    h.wait_for("ready");
    h.session.write(b"x");
    let start = Instant::now();
    h.wait_for("GOT:E[3;5R");
    // Well before the update's 150 ms are up.
    assert!(start.elapsed() < Duration::from_millis(120), "took {:?}", start.elapsed());
}

/// Every text the frame shows from now until `end` appears in it.
fn all_shown(h: &mut Harness, end: &str) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut texts: Vec<String> = Vec::new();
    loop {
        if h.session.snapshot(&mut h.frame) || texts.is_empty() {
            let text = h.frame.text();
            let done = text.contains(end);
            if texts.last() != Some(&text) {
                texts.push(text);
            }
            if done {
                return texts;
            }
        }
        assert!(Instant::now() < deadline, "timed out waiting for {end}; shown: {texts:?}");
        std::thread::sleep(Duration::from_micros(300));
    }
}

#[test]
fn a_half_finished_update_is_never_shown() {
    // Two updates written in parts, with pauses a frame could fall in, and
    // ordinary output before, between and after.
    let mut h = sh(
        "printf 'top '; sleep 0.05; \
         printf '\\033[?2026hfirst '; sleep 0.03; printf 'second '; sleep 0.03; printf 'third\\033[?2026l'; \
         sleep 0.05; printf ' between '; sleep 0.05; \
         printf 'more\\033[?2026h fourth'; sleep 0.04; printf ' fifth\\033[?2026l done'; sleep 5",
    );
    let texts = all_shown(&mut h, "done");
    for text in &texts {
        let has = |word: &str| text.contains(word);
        assert_eq!(has("first"), has("third"), "half of the first update: {texts:?}");
        assert_eq!(has("fourth"), has("fifth"), "half of the second update: {texts:?}");
        assert!(!has("second") || has("third"), "{texts:?}");
    }
    // What was written outside the updates did not wait for them.
    let alone = |word: &str, without: &str| {
        texts.iter().any(|t| t.contains(word) && !t.contains(without))
    };
    assert!(alone("top", "first"), "{texts:?}");
    assert!(alone("third", "between"), "{texts:?}");
    assert!(alone("between", "more"), "{texts:?}");
    // In one write with the beginning of an update, and still shown first.
    assert!(alone("more", "fourth"), "{texts:?}");
    assert!(texts.last().is_some_and(|t| t.contains("top first second third between more fourth fifth done")));
}

/// A program that redraws in updates one right after another, with no
/// pause for a frame to fall in: each update writes its number in two
/// places. The screen must show them as they go, `at_least` of them, and
/// never one number in one place and another in the other.
fn updates_in_a_stream(command: &str, last: u32, at_least: usize) {
    let mut h = sh(&format!("{command}; printf 'end'; sleep 5"));
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut seen: Vec<u32> = Vec::new();
    loop {
        if h.session.snapshot(&mut h.frame) {
            let (top, low) = (h.frame.row_text(0), h.frame.row_text(4));
            assert_eq!(top, low, "an update shown half done");
            if let Ok(n) = top.trim().parse::<u32>()
                && seen.last() != Some(&n) {
                    assert!(seen.last().is_none_or(|&before| before < n), "{seen:?} then {n}");
                    seen.push(n);
                }
            if h.frame.row_text(6).contains("end") {
                break;
            }
        }
        assert!(Instant::now() < deadline, "timed out; seen {seen:?}");
        std::thread::sleep(Duration::from_micros(200));
    }
    assert_eq!(seen.last(), Some(&last), "{seen:?}");
    // Shown on the way, not only when it was over.
    assert!(seen.len() >= at_least, "only {} were shown: {seen:?}", seen.len());
}

#[test]
fn a_stream_of_updates_is_shown_as_it_goes() {
    // About two hundred thousand updates a second: too slow to count as a
    // flood, so the screen is put aside wherever an update begins.
    updates_in_a_stream(
        "awk 'BEGIN { for (i = 1; i <= 120000; i++) \
         printf \"\\033[?2026h\\033[1;1H%07d\\033[5;1H%07d\\033[?2026l\", i, i; \
         printf \"\\033[7;1H\" }'",
        120000,
        20,
    );
}

#[test]
fn a_flood_of_updates_is_shown_as_it_goes() {
    // The same from a file, as fast as the terminal takes it.
    let path = std::env::temp_dir().join(format!("tern-updates-{}", std::process::id()));
    let mut bytes = Vec::new();
    for i in 1..=600_000u32 {
        bytes.extend_from_slice(
            format!("\x1b[?2026h\x1b[1;1H{i:07}\x1b[5;1H{i:07}\x1b[?2026l").as_bytes(),
        );
    }
    bytes.extend_from_slice(b"\x1b[7;1H");
    std::fs::write(&path, &bytes).expect("the updates");
    updates_in_a_stream(&format!("cat {}", path.display()), 600_000, 20);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_flood_of_long_updates_is_shown_as_it_goes() {
    // Updates of a hundred kilobytes, each begun where the last one ends:
    // next to never does a piece of output end between two of them, so
    // there is no moment at which the terminal could be looked at. The
    // screen is shown all the same, because the interpreter puts it aside
    // at an update's end once the app has come for it in vain: most of the
    // six hundred are seen, where a dozen would be by chance.
    let path = std::env::temp_dir().join(format!("tern-long-updates-{}", std::process::id()));
    let filler = b"filling the update out, over and over on one line\r".repeat(2000);
    let mut bytes = Vec::new();
    for i in 1..=600u32 {
        bytes.extend_from_slice(format!("\x1b[?2026h\x1b[1;1H{i:07}\x1b[5;1H{i:07}\x1b[9;1H").as_bytes());
        bytes.extend_from_slice(&filler);
        bytes.extend_from_slice(b"\x1b[?2026l");
    }
    bytes.extend_from_slice(b"\x1b[7;1H");
    std::fs::write(&path, &bytes).expect("the updates");
    updates_in_a_stream(&format!("cat {}", path.display()), 600, 100);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn focus_events_are_sent_when_asked_for() {
    let h = query("\\033[?1004h", 3);
    h.wait_until("focus mode", || h.session.modes().focus_events);
    h.session.focus(true);
    h.wait_for("GOT:E[I");
}

#[test]
fn modes_are_published() {
    let h = sh("printf '\\033[?1h\\033[?2004h\\033[?1002h\\033[?1006h\\033[>5u'; sleep 5");
    h.wait_until("modes", || {
        let m = h.session.modes();
        m.app_cursor
            && m.bracketed_paste
            && m.mouse == MouseTracking::Drag
            && m.mouse_encoding == MouseEncoding::Sgr
    });
    h.wait_until("kitty flags", || h.session.modes().kitty_flags == 5);
}

#[test]
fn clipboard_store_and_load() {
    let h = sh("printf '\\033]52;c;aGVsbG8=\\007'; sleep 5");
    h.events.wait("clipboard", |r| r.clipboard == ["hello"]);

    let recorder = Recorder::default();
    *recorder.clipboard_reply.lock().unwrap() = Some("pasted".to_string());
    let h = spawn_with(
        config(&[
            "/bin/sh",
            "-c",
            "stty -echo -icanon min 1; printf '\\033]52;c;?\\007'; r=$(dd bs=1 count=16 2>/dev/null); \
             printf 'GOT:%s\\n' \"$(printf '%s' \"$r\" | tr '\\033\\007' EB)\"; sleep 5",
        ]),
        recorder,
    );
    h.wait_for("GOT:E]52;c;cGFzdGVkB");
}

#[test]
fn hyperlinks_are_reported() {
    let mut h = sh("printf '\\033]8;;https://example.com/x\\033\\\\link\\033]8;;\\033\\\\ plain'");
    h.wait_exit();
    assert_eq!(
        h.session.hyperlink_at(0, 0).as_deref(),
        Some("https://example.com/x")
    );
    assert_eq!(
        h.session.hyperlink_at(0, 3).as_deref(),
        Some("https://example.com/x")
    );
    assert_eq!(h.session.hyperlink_at(0, 6), None);
    assert_eq!(h.session.hyperlink_at(500, 0), None);
    assert!(h.snapshot().cell(0, 0).has(flags::HYPERLINK));
}

#[test]
fn clear_drops_scrollback_and_screen() {
    let mut h = sh("seq 1 60; read x; sleep 5");
    h.wait_for("60");
    assert!(h.snapshot().history > 0);
    h.session.clear();
    assert_eq!(h.snapshot().history, 0);
    assert!(!h.session.text().contains("60"));
}

#[test]
fn write_never_blocks() {
    // The child never reads, so the terminal's input queue fills up.
    let h = sh("sleep 5");
    let big = vec![b'x'; 4 << 20];
    let start = Instant::now();
    h.session.write(&big);
    h.session.write(&big);
    assert!(start.elapsed() < Duration::from_millis(200));
}

#[test]
fn wakeups_are_coalesced_until_a_snapshot() {
    let mut h = sh("yes");
    h.events.wait("first wakeup", |r| r.wakeups >= 1);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(h.events.with(|r| r.wakeups), 1);
    h.snapshot();
    h.events.wait("second wakeup", |r| r.wakeups >= 2);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(h.events.with(|r| r.wakeups), 2);
}

#[test]
fn spawn_reports_a_missing_program() {
    let err = Session::spawn(
        config(&["/nonexistent/program"]),
        std::sync::Arc::new(Recorder::default()),
    )
    .err()
    .expect("spawn should fail");
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    let err = Session::spawn(
        config(&["no-such-program-tern"]),
        std::sync::Arc::new(Recorder::default()),
    )
    .err()
    .expect("spawn should fail");
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn environment_and_working_directory() {
    let h = spawn(SessionConfig {
        command: Some(vec![
            "/bin/sh".into(),
            "-c".into(),
            "printf '%s|%s|%s|%s|' \"$TERM\" \"$COLORTERM\" \"$TERM_PROGRAM\" \"$FOO\"; pwd".into(),
        ]),
        working_dir: Some("/usr".into()),
        env: vec![("FOO".into(), "bar".into())],
        ..SessionConfig::default()
    });
    h.wait_for("xterm-256color|truecolor|Tern|bar|/usr");
}

#[test]
fn login_shell_cwd_and_foreground_process() {
    // An empty ZDOTDIR keeps the user's zsh startup files out of the test.
    let zdotdir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("zdotdir");
    std::fs::create_dir_all(&zdotdir).expect("zdotdir");
    let h = spawn(SessionConfig {
        shell: Some("/bin/zsh".into()),
        working_dir: Some("/usr".into()),
        env: vec![("ZDOTDIR".into(), zdotdir.display().to_string())],
        ..SessionConfig::default()
    });
    h.wait_text("a prompt", |t| !t.trim().is_empty());
    // The quotes keep the echoed command line from matching.
    h.session.write(b"echo RE''ADY\r");
    h.wait_text("READY", |t| t.lines().any(|l| l == "READY"));
    assert_eq!(
        h.session.cwd().as_deref(),
        Some(std::path::Path::new("/usr"))
    );
    assert_eq!(h.session.foreground_process(), None);

    h.session.write(b"cd /bin && sleep 5\r");
    h.wait_until("sleep in foreground", || {
        h.session.foreground_process().as_deref() == Some("sleep")
    });
    assert_eq!(
        h.session.cwd().as_deref(),
        Some(std::path::Path::new("/bin"))
    );
    h.session.write(b"\x03");
    h.wait_until("back at the prompt", || {
        h.session.foreground_process().is_none()
    });
    h.session.write(b"exit 3\r");
    assert_eq!(h.wait_exit(), Some(3));
}
