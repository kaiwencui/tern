//! Input reaching a window: keys, text and the pointer. Keys bound to an
//! action never get here; the view performs those itself.

use std::collections::VecDeque;
use std::path::PathBuf;

use tern_config::Action;
use tern_input::{encode_key, encode_text, KeyEvent, KeyKind, KeyOptions, OptionAsAlt};
use tern_types::{Key, Mods, Point};

use crate::app::{app, CloseTarget};
use crate::keymap::{self, flag, KeyPress};
use crate::state::WindowState;

/// Input events kept for the automation socket.
const LOG_CAPACITY: usize = 50;
/// The Escape key's virtual key code (kVK_Escape).
const ESCAPE_CODE: u16 = 0x35;

/// One key event as the view saw it.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyInput {
    pub down: bool,
    pub code: u16,
    /// NSEventModifierFlags, device bits included.
    pub flags: usize,
    /// None for keys that mean nothing to a terminal.
    pub press: Option<KeyPress>,
    pub characters: String,
    pub characters_ignoring_modifiers: String,
    /// What the key types with Shift on the current layout.
    pub shifted: Option<char>,
    pub repeat: bool,
    /// What the input method inserted while handling this key.
    pub text: String,
    /// Selectors the key binding system sent (insertNewline:, moveLeft:...).
    pub commands: Vec<String>,
    /// The input method used the key for composition, so it must not also
    /// reach the terminal as a key.
    pub composing: bool,
    /// When the key was pressed, in seconds since boot.
    pub time: f64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseKind {
    Down,
    Up,
    Drag,
    Move,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct MouseInput {
    pub kind: MouseKind,
    /// 0 left, 1 right, 2 and up the other buttons.
    pub button: u8,
    pub at: Point,
    pub mods: Mods,
    pub clicks: u32,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ScrollInput {
    pub at: Point,
    /// AppKit's scrolling deltas: points when precise, lines otherwise.
    pub dx: f64,
    pub dy: f64,
    pub precise: bool,
    /// NSEventPhase bits of the gesture and of its momentum.
    pub phase: usize,
    pub momentum: usize,
    pub mods: Mods,
}

/// What a drag and drop delivered.
#[derive(Clone, PartialEq, Debug)]
pub enum DropPayload {
    Paths(Vec<PathBuf>),
    Text(String),
}

/// What became of a key.
#[derive(Clone, PartialEq, Debug)]
pub enum KeyOutcome {
    /// These bytes went to the focused pane (none, for keys that send nothing).
    Sent(Vec<u8>),
    /// The key is bound to this action.
    Bound(Action),
}

/// An entry in the input log the automation socket reports.
#[derive(Clone, PartialEq, Debug)]
pub enum InputRecord {
    Key(KeyInput, KeyOutcome),
    Modifier {
        key: Key,
        down: bool,
        mods: Mods,
        sent: Vec<u8>,
    },
    /// Text inserted outside a key press (emoji picker, dictation, automation).
    Text {
        text: String,
        sent: Vec<u8>,
    },
    /// The input method's uncommitted text; empty when composition ends.
    Preedit(String),
    Mouse(MouseInput),
    Scroll(ScrollInput),
    /// A drag over the view: Some(position) while it moves, None when it leaves.
    DragOver(Option<Point>),
    Drop {
        at: Point,
        payload: DropPayload,
    },
}

#[derive(Default)]
pub struct InputLog {
    last_seq: u64,
    records: VecDeque<(u64, InputRecord)>,
}

/// Keys whose presses the terminal never saw, so their releases are not
/// sent either: presses an input method composed with or a binding took.
#[derive(Default)]
pub struct KeyState {
    swallowed: Vec<u16>,
}

impl KeyState {
    fn swallow(&mut self, code: u16) {
        if !self.swallowed.contains(&code) {
            self.swallowed.push(code);
        }
    }

    /// Whether the press of this key was swallowed; forgets it.
    fn release(&mut self, code: u16) -> bool {
        let before = self.swallowed.len();
        self.swallowed.retain(|&c| c != code);
        self.swallowed.len() != before
    }
}

/// The input crate's Option setting from the configuration's.
pub fn option_as_alt(left: bool, right: bool) -> OptionAsAlt {
    match (left, right) {
        (true, true) => OptionAsAlt::Both,
        (true, false) => OptionAsAlt::Left,
        (false, true) => OptionAsAlt::Right,
        (false, false) => OptionAsAlt::None,
    }
}

impl WindowState {
    /// Escape puts back a pane picked up to move, and goes no further.
    /// True when it did. The pane may have been picked up in another tab,
    /// which is hidden now that the drag has brought this one up.
    pub fn escape_cancels(&self, code: u16) -> bool {
        if code != ESCAPE_CODE || !(self.cancel_move() || app().cancel_pane_move()) {
            return false;
        }
        self.keys.borrow_mut().swallow(code);
        true
    }

    /// The action a key press is bound to, if any.
    pub fn bound_action(&self, press: &KeyPress) -> Option<Action> {
        app()
            .config()
            .bindings
            .lookup(press.key, press.mods)
            .cloned()
    }

    /// A key press that performs an action rather than reaching the terminal.
    pub fn key_bound(&self, input: KeyInput, action: Action) {
        self.keys.borrow_mut().swallow(input.code);
        self.record(InputRecord::Key(input, KeyOutcome::Bound(action.clone())));
        app().perform(action, Some(self.id));
    }

    /// Whether Option acts as Alt for a key with these modifier flags, so
    /// the key goes straight to the terminal instead of an input method.
    pub fn option_is_alt(&self, flags: usize) -> bool {
        if flags & flag::OPTION == 0 || flags & (flag::COMMAND | flag::CONTROL) != 0 {
            return false;
        }
        let o = keymap::option_keys(flags);
        let alt = app().config().option_as_alt;
        (o.left && alt.left) || (o.right && alt.right)
    }

    pub fn key_input(&self, input: KeyInput) {
        let sent = self.handle_key(&input);
        self.record(InputRecord::Key(input, KeyOutcome::Sent(sent)));
    }

    fn handle_key(&self, k: &KeyInput) -> Vec<u8> {
        if self.handle_search_key(k) {
            return Vec::new();
        }
        let Some(press) = k.press else {
            return Vec::new();
        };
        if k.down && self.close_if_held(k.code) {
            return Vec::new();
        }
        if !k.down {
            if self.keys.borrow_mut().release(k.code) {
                return Vec::new();
            }
            // Releases matter only to programs that asked for them, and
            // neither scroll nor restart the blink.
            let bytes = self.encode(k, &press, KeyKind::Release, Some(&k.characters));
            self.with_focused(|p| p.session.write(&bytes));
            return bytes;
        }
        let kind = if k.repeat {
            KeyKind::Repeat
        } else {
            KeyKind::Press
        };
        let bytes = if k.composing {
            if k.text == k.characters && !k.text.is_empty() {
                // A dead key's accented letter arrives with the next key,
                // which reaches the program as a key, release included.
                self.encode(k, &press, kind, Some(&k.text))
            } else {
                // The input method took the key; its release is not sent.
                self.keys.borrow_mut().swallow(k.code);
                if k.text.is_empty() {
                    Vec::new()
                } else {
                    encode_text(&k.text, self.modes())
                }
            }
        } else if !k.text.is_empty() && k.text != k.characters {
            // An input method turned the key into other text.
            encode_text(&k.text, self.modes())
        } else {
            self.encode(k, &press, kind, Some(&k.characters))
        };
        // Timed unless a key before it is still waiting, which these bytes
        // leave untimed: what shows next could be either key's doing.
        let timed = !bytes.is_empty() && !self.key_waiting();
        if !bytes.is_empty() {
            self.hide_pointer_while_typing();
        }
        self.send(&bytes);
        if timed {
            self.key_sent(k.time);
        }
        bytes
    }

    fn encode(&self, k: &KeyInput, press: &KeyPress, kind: KeyKind, text: Option<&str>) -> Vec<u8> {
        let event = KeyEvent {
            key: press.key,
            mods: press.mods,
            kind,
            text: text.filter(|t| !t.is_empty()).map(str::to_owned),
            shifted: k.shifted,
            base_layout: keymap::us_characters(k.code).map(|(unshifted, _)| unshifted),
            keypad: k.flags & flag::NUMERIC_PAD != 0,
            left_option: press.option.left,
            right_option: press.option.right,
        };
        encode_key(&event, self.modes(), self.key_options())
    }

    fn modes(&self) -> tern_types::Modes {
        self.with_focused(|p| p.session.modes()).unwrap_or_default()
    }

    fn key_options(&self) -> KeyOptions {
        let o = app().config().option_as_alt;
        KeyOptions {
            option_as_alt: option_as_alt(o.left, o.right),
        }
    }

    /// A key in a pane that shows why its program ended closes the pane.
    /// True when it did.
    fn close_if_held(&self, code: u16) -> bool {
        let Some(id) = self.focused_pane_id() else {
            return false;
        };
        if !self.with_pane(id, |p| p.held).unwrap_or(false) {
            return false;
        }
        self.keys.borrow_mut().swallow(code);
        app().request_close(CloseTarget::Pane(self.id, id));
        true
    }

    /// A modifier key pressed or released on its own; mods is the state
    /// after the change.
    pub fn modifier_changed(&self, key: Key, down: bool, mods: Mods, flags: usize) {
        let options = keymap::option_keys(flags);
        let event = KeyEvent {
            key,
            mods,
            kind: if down {
                KeyKind::Press
            } else {
                KeyKind::Release
            },
            text: None,
            shifted: None,
            base_layout: None,
            keypad: false,
            left_option: options.left,
            right_option: options.right,
        };
        let sent = encode_key(&event, self.modes(), self.key_options());
        self.with_focused(|p| p.session.write(&sent));
        self.modifiers_changed(mods);
        self.record(InputRecord::Modifier {
            key,
            down,
            mods,
            sent,
        });
    }

    /// Text that arrived outside a key press.
    pub fn insert_text(&self, text: String) {
        if self.search_open() {
            self.paste_into_find(&text);
            self.record(InputRecord::Text {
                text,
                sent: Vec::new(),
            });
            return;
        }
        let sent = encode_text(&text, self.modes());
        if !sent.is_empty() {
            self.hide_pointer_while_typing();
        }
        self.send(&sent);
        self.record(InputRecord::Text { text, sent });
    }

    pub fn preedit(&self, text: String) {
        self.set_preedit(text.clone());
        self.record(InputRecord::Preedit(text));
    }

    pub fn mouse(&self, input: MouseInput) {
        self.pointer(&input);
        self.record(InputRecord::Mouse(input));
    }

    pub fn scroll_input(&self, input: ScrollInput) {
        self.wheel(&input);
        self.record(InputRecord::Scroll(input));
    }

    /// A drag from another app is over the view at a point, or has left it.
    pub fn drag_over(&self, at: Option<Point>) {
        self.drag_moved(at);
        self.record(InputRecord::DragOver(at));
    }

    /// Files or text dropped on the view, at a position in view pixels: the
    /// pane under it gets the focus and the text, as a paste. Files arrive
    /// as their paths, quoted for the shell and separated by spaces.
    /// Nothing runs: no newline is added, a dropped text's own trailing
    /// newlines are left off, and its inner line breaks become spaces for a
    /// program that does not bracket pastes (which would run each line).
    pub fn handle_drop(&self, at: Point, payload: DropPayload) {
        self.drag_moved(None);
        if let Some(id) = self.pane_at(at).or_else(|| self.focused_pane_id()) {
            let text = match &payload {
                DropPayload::Paths(paths) => {
                    tern_input::drop_text(paths.iter().map(|p| p.to_string_lossy()))
                }
                DropPayload::Text(text) => {
                    let bracketed = self
                        .with_pane(id, |p| p.session.modes().bracketed_paste)
                        .unwrap_or(false);
                    dropped_text(text, bracketed)
                }
            };
            self.focus_pane(id);
            self.paste_to(id, &text);
        }
        self.record(InputRecord::Drop { at, payload });
    }

    fn record(&self, record: InputRecord) {
        let Some(log) = &self.log else { return };
        let mut log = log.borrow_mut();
        log.last_seq += 1;
        let seq = log.last_seq;
        if log.records.len() == LOG_CAPACITY {
            log.records.pop_front();
        }
        log.records.push_back((seq, record));
    }

    /// Logged input with a sequence number above after, oldest first.
    pub fn records_after(&self, after: u64) -> Vec<(u64, InputRecord)> {
        let Some(log) = &self.log else {
            return Vec::new();
        };
        log.borrow()
            .records
            .iter()
            .filter(|(seq, _)| *seq > after)
            .cloned()
            .collect()
    }

    /// The sequence number of the newest record, or 0.
    pub fn last_seq(&self) -> u64 {
        self.log.as_ref().map_or(0, |log| log.borrow().last_seq)
    }
}

/// Dropped text as it is pasted: without trailing line breaks, and with
/// inner ones as spaces unless the program brackets pastes.
fn dropped_text(text: &str, bracketed: bool) -> String {
    let text = text.trim_end_matches(['\n', '\r']);
    if bracketed {
        text.to_string()
    } else {
        text.replace("\r\n", " ").replace(['\n', '\r'], " ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropped_text_never_runs() {
        assert_eq!(dropped_text("ls\n", false), "ls");
        assert_eq!(dropped_text("a\nb\r\nc\rd\r\n\n", false), "a b c d");
        assert_eq!(dropped_text("a\nb\n", true), "a\nb");
        assert_eq!(dropped_text("", false), "");
    }

    #[test]
    fn swallowed_presses_swallow_their_release_once() {
        let mut keys = KeyState::default();
        keys.swallow(3);
        keys.swallow(3);
        assert!(!keys.release(4));
        assert!(keys.release(3));
        assert!(!keys.release(3));
    }

    #[test]
    fn option_setting_maps_to_the_encoder() {
        assert_eq!(option_as_alt(true, true), OptionAsAlt::Both);
        assert_eq!(option_as_alt(true, false), OptionAsAlt::Left);
        assert_eq!(option_as_alt(false, true), OptionAsAlt::Right);
        assert_eq!(option_as_alt(false, false), OptionAsAlt::None);
    }
}
