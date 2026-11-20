//! Turns keys, mouse events, pasted text and dropped files into the bytes a
//! terminal program expects, given the modes that program has set.
//!
//! Everything here is a pure function of its arguments: the app builds the
//! events from `NSEvent`s, calls in, and writes whatever comes back to the PTY.
//! Empty output means "send nothing".

mod key;
mod kitty;
mod legacy;
mod mouse;
mod paste;

pub use key::{encode_key, encode_text, KeyEvent, KeyKind, KeyOptions, OptionAsAlt};
pub use mouse::{
    alternate_scroll_bytes, encode_mouse, wants_mouse, MouseButton, MouseEvent, MouseKind,
};
pub use paste::{drop_text, paste_bytes};

pub(crate) const ESC: u8 = 0x1b;
pub(crate) const CSI: &[u8] = b"\x1b[";
pub(crate) const SS3: &[u8] = b"\x1bO";

/// Appends n in decimal without going through the formatting machinery.
pub(crate) fn push_decimal(out: &mut Vec<u8>, mut n: u32) {
    let mut digits = [0u8; 10];
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    out.extend_from_slice(&digits[start..]);
}

pub(crate) fn push_char(out: &mut Vec<u8>, c: char) {
    out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
}

#[cfg(test)]
pub(crate) fn show(bytes: &[u8]) -> String {
    bytes.escape_ascii().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal() {
        for n in [0, 1, 9, 10, 57_441, 99_999, u32::MAX] {
            let mut out = Vec::new();
            push_decimal(&mut out, n);
            assert_eq!(out, n.to_string().into_bytes());
        }
    }
}
