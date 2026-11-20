//! How fast the engine interprets output on its own, without a terminal
//! device or a program in front of it: a file, with its line ends as a
//! terminal device delivers them, handed over in pieces of a kilobyte as a
//! program's output arrives, several times over.
//!
//! cargo run --release -p tern-vt --example parse -- <file> [cols] [rows] [piece]
//!
//! `TERN_PARSER` says what reads the output:
//!
//! - `engine` (the default): the engine as Tern runs it, the feeder's
//!   shortcut for plain text in front of the parser.
//! - `parser`: the parser on its own, driving a terminal directly, without
//!   the feeder.
//! - `count`: the parser driving a handler that only counts the characters
//!   it is given, for the cost of parsing alone.

use std::time::{Duration, Instant};

use tern_vt::engine::{CursorStyle, Options, Terminal};
use tern_vt::event::VoidListener;
use tern_vt::term::test::TermSize;
use tern_vt::{BareParser, Term};

/// What reads the output.
#[derive(Clone, Copy)]
enum Way {
    /// The engine, feeder and all.
    Engine,
    /// The parser driving a terminal.
    Parser,
    /// The parser driving a handler that counts characters.
    Count,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: parse <file> [cols] [rows] [piece]");
        std::process::exit(2);
    };
    let cols = args.next().and_then(|a| a.parse().ok()).unwrap_or(200);
    let rows = args.next().and_then(|a| a.parse().ok()).unwrap_or(60);
    let piece = args.next().and_then(|a| a.parse().ok()).unwrap_or(1024);
    let named = std::env::var("TERN_PARSER").unwrap_or_else(|_| "engine".to_owned());
    let way = match named.as_str() {
        "engine" => Way::Engine,
        "parser" => Way::Parser,
        "count" => Way::Count,
        _ => {
            eprintln!("TERN_PARSER is one of engine, parser and count");
            std::process::exit(2);
        },
    };
    let file = std::fs::read(&path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(1);
    });
    // What a program writes to a terminal reaches it with every line feed
    // made a carriage return and a line feed (the terminal device's ONLCR).
    let mut bytes = Vec::with_capacity(file.len() + file.len() / 16);
    for &byte in &file {
        if byte == b'\n' {
            bytes.push(b'\r');
        }
        bytes.push(byte);
    }
    // The session's threads ask for this; without it the thread may be put
    // on a slower core.
    // SAFETY: sets the calling thread's own class; no memory is involved.
    unsafe { libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INITIATED, 0) };
    let mut best = f64::MAX;
    for _ in 0..5 {
        let elapsed = run(way, &bytes, cols, rows, piece);
        best = best.min(elapsed.as_secs_f64());
    }
    println!("{path}: {:.1} MB/s at best of 5, {cols}x{rows}, {named}", bytes.len() as f64 / best / 1e6);
}

/// Reads `bytes` once in pieces of `piece` the way `way` says, from a fresh
/// start, and returns how long that took.
fn run(way: Way, bytes: &[u8], cols: usize, rows: usize, piece: usize) -> Duration {
    let pieces = bytes.chunks(piece);
    match way {
        Way::Engine => {
            let options =
                Options { scrollback_lines: 10_000, cursor: CursorStyle::default(), ..Options::default() };
            let mut term = Terminal::new(&options, cols, rows);
            let start = Instant::now();
            pieces.for_each(|piece| term.advance(piece));
            let elapsed = start.elapsed();
            std::hint::black_box(&term);
            elapsed
        },
        Way::Parser => {
            let mut term = bare_term(cols, rows);
            let mut parser = BareParser::default();
            let start = Instant::now();
            pieces.for_each(|piece| parser.advance(&mut term, piece));
            let elapsed = start.elapsed();
            std::hint::black_box(&term);
            elapsed
        },
        Way::Count => {
            let mut count = 0;
            let mut parser = BareParser::default();
            let start = Instant::now();
            pieces.for_each(|piece| parser.advance_counting(&mut count, piece));
            let elapsed = start.elapsed();
            std::hint::black_box(count);
            elapsed
        },
    }
}

/// A terminal set up as the engine sets one up, with no one listening.
fn bare_term(cols: usize, rows: usize) -> Term<VoidListener> {
    let options = Options {
        scrollback_lines: 10_000,
        ..Options::default()
    };
    Term::new(options, &TermSize::new(cols.max(2), rows.max(1)), VoidListener)
}
