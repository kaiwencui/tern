//! DEC private mode 12, the "AT&T blinking cursor", is ignored. terminfo's
//! cnorm and cvvis set and reset it, so a program such as vim that merely
//! shows its cursor again leaves the shell's cursor steady or blinking
//! against the configuration. Whether the cursor blinks is the
//! configuration's call, and DECSCUSR's (CSI Ps SP q), which programs send
//! when they mean to choose a cursor.
//!
//! The parser is not wrapped: the byte stream is scanned for a
//! CSI ? ... h or l whose parameters include 12, and the caller hands the
//! parser that sequence's final byte on its own, so it can put the
//! blinking back while the other modes of the sequence still apply.

use memchr::memchr;
use tern_vt::engine::{CursorStyle, Terminal};

const ESC: u8 = 0x1b;

/// Finds the sequences that set or reset mode 12. Keeps its place between
/// calls, so a sequence split across reads is found too.
#[derive(Debug, Default)]
pub(crate) struct BlinkModeScan {
    /// Inside the parameters of a CSI ? sequence.
    in_params: bool,
    /// The parameter being read.
    param: u32,
    /// An earlier parameter of the sequence was 12.
    has_12: bool,
    /// The last two bytes seen before the current call, oldest first.
    tail: [u8; 2],
}

impl BlinkModeScan {
    /// The index of the final byte of the first sequence in bytes that sets
    /// or resets mode 12, or None. The next call goes on after that byte.
    pub(crate) fn find(&mut self, bytes: &[u8]) -> Option<usize> {
        let found = self.scan(bytes);
        let seen = &bytes[..found.map_or(bytes.len(), |i| i + 1)];
        match seen {
            [.., a, b] => self.tail = [*a, *b],
            [b] => self.tail = [self.tail[1], *b],
            [] => {}
        }
        found
    }

    fn scan(&mut self, bytes: &[u8]) -> Option<usize> {
        let mut i = 0;
        while i < bytes.len() {
            if !self.in_params {
                // The question mark is rare in output, far rarer than the
                // escapes of colored text, so it is what is searched for;
                // the two bytes before it tell whether it opens CSI ?.
                let q = i + memchr(b'?', &bytes[i..])?;
                let back = |n: usize| match q.checked_sub(n) {
                    Some(at) => bytes[at],
                    None => self.tail[2 + q - n],
                };
                if back(2) == ESC && back(1) == b'[' {
                    self.in_params = true;
                    self.param = 0;
                    self.has_12 = false;
                }
                i = q + 1;
                continue;
            }
            match bytes[i] {
                b @ b'0'..=b'9' => {
                    self.param = self
                        .param
                        .saturating_mul(10)
                        .saturating_add(u32::from(b - b'0'));
                }
                b';' | b':' => {
                    self.has_12 |= self.param == 12;
                    self.param = 0;
                }
                b'h' | b'l' if self.has_12 || self.param == 12 => {
                    self.in_params = false;
                    return Some(i);
                }
                _ => self.in_params = false,
            }
            i += 1;
        }
        None
    }
}

/// After the parser applied a mode 12 change: the cursor blinks as it did
/// before, and a cursor style no program chose stays unset, so the
/// configuration keeps deciding it.
pub(crate) fn keep_blinking(term: &mut Terminal, before: CursorStyle) {
    let after = term.cursor_style();
    if after.blinking == before.blinking {
        return;
    }
    let style = CursorStyle {
        blinking: before.blinking,
        ..after
    };
    // With no style set the terminal shows its default, which is how the
    // default is read.
    term.set_cursor_style(None);
    if term.cursor_style() != style {
        term.set_cursor_style(Some(style));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every index find reports, with the bytes fed in these pieces.
    fn found(pieces: &[&[u8]]) -> Vec<usize> {
        let mut scan = BlinkModeScan::default();
        let mut out = Vec::new();
        let mut base = 0;
        for piece in pieces {
            let mut at = 0;
            while let Some(i) = scan.find(&piece[at..]) {
                out.push(base + at + i);
                at += i + 1;
            }
            base += piece.len();
        }
        out
    }

    #[test]
    fn mode_12_is_found_alone_and_among_others() {
        assert_eq!(found(&[b"ab\x1b[?12lcd"]), [7]);
        assert_eq!(found(&[b"\x1b[?12h"]), [5]);
        assert_eq!(found(&[b"\x1b[?12;25h"]), [8]);
        assert_eq!(found(&[b"\x1b[?25;12l"]), [8]);
        assert_eq!(found(&[b"\x1b[?1;12;1049h x \x1b[?12l"]), [12, 21]);
    }

    #[test]
    fn other_sequences_are_left_alone() {
        // Other modes, ANSI mode 12 (no ?), DECRQM, and look-alikes.
        for bytes in [
            &b"\x1b[?25l\x1b[?1049h\x1b[?112h\x1b[?120l"[..],
            b"\x1b[12h\x1b[12l",
            b"\x1b[?12$p",
            b"\x1b[?12m\x1b[2 q\x1b]12;red\x07",
            b"plain text ? 12 h",
        ] {
            assert_eq!(found(&[bytes]), [] as [usize; 0], "{bytes:?}");
        }
        // An escape in the middle starts over.
        assert_eq!(found(&[b"\x1b[?1\x1b[?12l"]), [9]);
    }

    #[test]
    fn a_sequence_split_across_reads_is_found() {
        let whole = b"x\x1b[?25;12l";
        for cut in 1..whole.len() {
            let (a, b) = whole.split_at(cut);
            assert_eq!(found(&[a, b]), [whole.len() - 1], "cut at {cut}");
        }
        assert_eq!(found(&[b"\x1b", b"[", b"?", b"1", b"2", b"h"]), [5]);
    }
}
