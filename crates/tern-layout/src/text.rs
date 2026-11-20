//! The compact text form of a layout, as the automation socket reports it
//! and tests write it.
//!
//! A pane is its id in decimal. A split is `h` (side by side) or `v`
//! (stacked), its ratio, then its two children in parentheses separated by a
//! comma. The focused pane carries `*` before its id, or `!` if it is also
//! zoomed. An empty layout is the empty string. No spaces are allowed.
//!
//! `h0.5(1,v0.25(*2,3))` is pane 1 on the left half, and on the right pane 2
//! above pane 3, which has three quarters of the height; pane 2 has focus.

use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;

use tern_types::PaneId;

use crate::tree::{Node, Split};
use crate::{Axis, Layout, MAX_DEPTH};

/// Why text could not be read as a layout.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ParseLayoutError {
    offset: usize,
    reason: &'static str,
}

impl ParseLayoutError {
    /// The byte offset in the text where the problem was found.
    pub fn offset(&self) -> usize {
        self.offset
    }
}

impl fmt::Display for ParseLayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid layout at byte {}: {}", self.offset, self.reason)
    }
}

impl std::error::Error for ParseLayoutError {}

impl fmt::Display for Layout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.root {
            Some(root) => write_node(f, root, self.focus, self.zoomed),
            None => Ok(()),
        }
    }
}

fn write_node(
    f: &mut fmt::Formatter<'_>,
    node: &Node,
    focus: Option<PaneId>,
    zoomed: bool,
) -> fmt::Result {
    match node {
        Node::Leaf(p) => {
            if focus == Some(*p) {
                f.write_str(if zoomed { "!" } else { "*" })?;
            }
            write!(f, "{p}")
        }
        Node::Split(s) => {
            let axis = match s.axis {
                Axis::Horizontal => 'h',
                Axis::Vertical => 'v',
            };
            // A float's Display is the shortest text that reads back as the
            // same value, so ratios round-trip exactly.
            write!(f, "{axis}{}(", s.ratio)?;
            write_node(f, &s.first, focus, zoomed)?;
            f.write_str(",")?;
            write_node(f, &s.second, focus, zoomed)?;
            f.write_str(")")
        }
    }
}

impl FromStr for Layout {
    type Err = ParseLayoutError;

    fn from_str(text: &str) -> Result<Layout, ParseLayoutError> {
        if text.is_empty() {
            return Ok(Layout::empty());
        }
        let mut parser = Parser {
            text,
            pos: 0,
            focus: None,
            zoomed: false,
            panes: HashSet::new(),
            splits: 0,
        };
        let root = parser.node(0)?;
        if parser.pos != text.len() {
            return Err(parser.error_here("unexpected text after the layout"));
        }
        let Some(focus) = parser.focus else {
            return Err(ParseLayoutError {
                offset: 0,
                reason: "no pane is marked as focused",
            });
        };
        if parser.zoomed && parser.panes.len() < 2 {
            return Err(ParseLayoutError {
                offset: 0,
                reason: "a lone pane cannot be zoomed",
            });
        }
        Ok(Layout {
            root: Some(root),
            focus: Some(focus),
            zoomed: parser.zoomed,
            history: vec![focus],
            next_split: parser.splits,
        })
    }
}

struct Parser<'a> {
    text: &'a str,
    /// Always on a character boundary: the parser only steps over ASCII.
    pos: usize,
    focus: Option<PaneId>,
    zoomed: bool,
    panes: HashSet<PaneId>,
    splits: u64,
}

impl Parser<'_> {
    fn node(&mut self, depth: usize) -> Result<Node, ParseLayoutError> {
        match self.peek() {
            Some(c @ (b'h' | b'v')) => {
                if depth >= MAX_DEPTH {
                    return Err(self.error_here("splits nest too deeply"));
                }
                self.pos += 1;
                let axis = if c == b'h' {
                    Axis::Horizontal
                } else {
                    Axis::Vertical
                };
                let ratio = self.ratio()?;
                self.expect(b'(')?;
                let first = self.node(depth + 1)?;
                self.expect(b',')?;
                let second = self.node(depth + 1)?;
                self.expect(b')')?;
                let id = self.splits;
                self.splits += 1;
                Ok(Node::Split(Box::new(Split {
                    id,
                    axis,
                    ratio,
                    first,
                    second,
                })))
            }
            Some(b'*' | b'!' | b'0'..=b'9') => self.leaf(),
            _ => Err(self.error_here("expected a pane id or a split")),
        }
    }

    fn leaf(&mut self) -> Result<Node, ParseLayoutError> {
        let start = self.pos;
        let marker = match self.peek() {
            Some(b'*') => Some(false),
            Some(b'!') => Some(true),
            _ => None,
        };
        if marker.is_some() {
            self.pos += 1;
        }
        let digits_at = self.pos;
        let digits = self.take_while(|c| c.is_ascii_digit());
        if digits.is_empty() {
            return Err(self.error_here("expected a pane id"));
        }
        let pane: PaneId = digits.parse().map_err(|_| ParseLayoutError {
            offset: digits_at,
            reason: "pane id is too large",
        })?;
        if !self.panes.insert(pane) {
            return Err(ParseLayoutError {
                offset: digits_at,
                reason: "pane appears more than once",
            });
        }
        if let Some(zoomed) = marker {
            if self.focus.is_some() {
                return Err(ParseLayoutError {
                    offset: start,
                    reason: "more than one pane is marked as focused",
                });
            }
            self.focus = Some(pane);
            self.zoomed = zoomed;
        }
        Ok(Node::Leaf(pane))
    }

    fn ratio(&mut self) -> Result<f64, ParseLayoutError> {
        let start = self.pos;
        let bad = ParseLayoutError {
            offset: start,
            reason: "expected a ratio from 0 to 1",
        };
        // Starting with a digit rules out signs and words like "inf" and "NaN".
        if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
            return Err(bad);
        }
        let token = self
            .take_while(|c| c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'+' | b'-'));
        match token.parse::<f64>() {
            Ok(ratio) if (0.0..=1.0).contains(&ratio) => Ok(ratio),
            _ => Err(bad),
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), ParseLayoutError> {
        if self.peek() != Some(byte) {
            return Err(self.error_here(match byte {
                b'(' => "expected '('",
                b',' => "expected ','",
                _ => "expected ')'",
            }));
        }
        self.pos += 1;
        Ok(())
    }

    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    /// Steps over ASCII bytes matching `pred` and returns them.
    fn take_while(&mut self, pred: impl Fn(u8) -> bool) -> &str {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii() && pred(c)) {
            self.pos += 1;
        }
        &self.text[start..self.pos]
    }

    fn error_here(&self, reason: &'static str) -> ParseLayoutError {
        ParseLayoutError {
            offset: self.pos,
            reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Direction, Zone};
    use tern_types::{Point, Rect};

    fn parse(text: &str) -> Layout {
        text.parse().unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    fn error(text: &str) -> ParseLayoutError {
        match text.parse::<Layout>() {
            Ok(layout) => panic!("{text} parsed as {layout:?}"),
            Err(e) => e,
        }
    }

    #[test]
    fn writes_the_documented_form() {
        let mut layout = Layout::new(1);
        layout.split(1, Direction::Right, 2).unwrap();
        layout.split(2, Direction::Down, 3).unwrap();
        layout.set_focus(2);
        assert_eq!(layout.to_string(), "h0.5(1,v0.5(*2,3))");
        layout.toggle_zoom();
        assert_eq!(layout.to_string(), "h0.5(1,v0.5(!2,3))");
        let area = Rect::new(0, 0, 1001, 601);
        layout.toggle_zoom();
        let inner = layout.dividers(area, 1)[1].handle;
        assert!(layout.drag_divider(inner, area, 1, Point { x: 700, y: 150 }, 0));
        assert_eq!(layout.to_string(), "h0.5(1,v0.25(*2,3))");
    }

    #[test]
    fn round_trips() {
        for text in [
            "*7",
            "h0.5(*1,2)",
            "v0.3333333333333333(1,!2)",
            "h0(1,v1(2,*3))",
            "h0.1(v0.5(1,2),h0.7(3,v0.75(*18446744073709551615,0)))",
        ] {
            let layout = parse(text);
            assert_eq!(layout.to_string(), text);
            assert_eq!(parse(&layout.to_string()), layout);
        }
    }

    #[test]
    fn the_empty_layout_is_the_empty_string() {
        let mut layout = Layout::new(1);
        layout.close(1).unwrap();
        assert_eq!(layout.to_string(), "");
        assert_eq!(parse(""), layout);
        assert!(parse("").is_empty());
    }

    #[test]
    fn reads_other_spellings_of_the_same_numbers() {
        assert_eq!(parse("h5e-1(*01,2)"), parse("h0.5(*1,2)"));
        assert_eq!(parse("h0.50(*1,2)").to_string(), "h0.5(*1,2)");
    }

    #[test]
    fn a_parsed_layout_works_like_a_built_one() {
        let mut layout = parse("h0.5(1,v0.5(*2,3))");
        let area = Rect::new(0, 0, 1001, 601);
        let handles: Vec<_> = layout.dividers(area, 1).iter().map(|d| d.handle).collect();
        assert_eq!(handles.len(), 2);
        assert_ne!(handles[0], handles[1]);
        assert!(layout.drag_divider(handles[0], area, 1, Point { x: 300, y: 10 }, 0));
        layout.split(1, Direction::Down, 4).unwrap();
        let after: Vec<_> = layout.dividers(area, 1).iter().map(|d| d.handle).collect();
        assert_eq!(after.len(), 3);
        assert!(handles.iter().all(|h| after.contains(h)));
        layout.move_pane(4, 3, Zone::Center).unwrap();
        assert_eq!(layout.to_string(), "h0.3(v0.5(1,3),v0.5(2,*4))");
    }

    #[test]
    fn rejects_malformed_text() {
        for (text, offset, reason) in [
            (" ", 0, "expected a pane id or a split"),
            ("1", 0, "no pane is marked as focused"),
            ("h0.5(1,2)", 0, "no pane is marked as focused"),
            ("*", 1, "expected a pane id"),
            ("**1", 1, "expected a pane id"),
            ("*1 ", 2, "unexpected text after the layout"),
            ("*1,2", 2, "unexpected text after the layout"),
            ("x", 0, "expected a pane id or a split"),
            ("H0.5(*1,2)", 0, "expected a pane id or a split"),
            ("h(*1,2)", 1, "expected a ratio from 0 to 1"),
            ("h1.5(*1,2)", 1, "expected a ratio from 0 to 1"),
            ("h-0.5(*1,2)", 1, "expected a ratio from 0 to 1"),
            ("hNaN(*1,2)", 1, "expected a ratio from 0 to 1"),
            ("hinf(*1,2)", 1, "expected a ratio from 0 to 1"),
            ("h0.5.5(*1,2)", 1, "expected a ratio from 0 to 1"),
            ("h1e5(*1,2)", 1, "expected a ratio from 0 to 1"),
            ("h0.5 (*1,2)", 4, "expected '('"),
            ("h0.5(*1 2)", 7, "expected ','"),
            ("h0.5(*1,2", 9, "expected ')'"),
            ("h0.5(*1,2]", 9, "expected ')'"),
            ("h0.5(*1)", 7, "expected ','"),
            ("h0.5(*1,)", 8, "expected a pane id or a split"),
            ("h0.5(*1,1)", 8, "pane appears more than once"),
            ("h0.5(*1,*2)", 8, "more than one pane is marked as focused"),
            ("h0.5(!1,*2)", 8, "more than one pane is marked as focused"),
            ("!1", 0, "a lone pane cannot be zoomed"),
            ("*18446744073709551616", 1, "pane id is too large"),
            ("h0.5(*1,é)", 8, "expected a pane id or a split"),
            ("*1é", 2, "unexpected text after the layout"),
        ] {
            let e = error(text);
            assert_eq!((e.offset(), e.reason), (offset, reason), "{text}");
        }
    }

    #[test]
    fn nesting_is_bounded() {
        let nested = |depth: usize| {
            let mut text = "h0.5(".repeat(depth);
            text.push_str("*0");
            for i in 1..=depth {
                text.push_str(&format!(",{i})"));
            }
            text
        };
        assert_eq!(parse(&nested(MAX_DEPTH)).len(), MAX_DEPTH + 1);
        assert_eq!(
            error(&nested(MAX_DEPTH + 1)).reason,
            "splits nest too deeply"
        );
        // Far deeper input fails at the bound instead of overflowing the stack.
        assert_eq!(
            error(&"v1(".repeat(1_000_000)).reason,
            "splits nest too deeply"
        );
    }

    #[test]
    fn errors_describe_themselves() {
        assert_eq!(
            error("h0.5(*1,1)").to_string(),
            "invalid layout at byte 8: pane appears more than once"
        );
    }
}
