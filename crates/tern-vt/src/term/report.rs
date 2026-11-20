//! What the terminal tells the program and the app: reports, colors, the
//! clipboard, titles and the bell.
//!
//! A reply the terminal can make itself goes to the program at once. One
//! that needs the app, such as a color the program did not set, the
//! clipboard or the size of the window in pixels, goes to the app with a
//! function that makes the reply from what the app knows.

use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as Base64;

use crate::ansi::{NamedColor, Rgb};
use crate::event::{Event, EventListener};
use crate::grid::Dimensions;
use crate::term::{REPORTED_VERSION, TITLE_STACK_MAX_DEPTH, Term};

impl<T: EventListener> Term<T> {
    /// DA, and with `>` the secondary DA: a VT102, and this version.
    #[inline]
    pub fn identify_terminal(&mut self, intermediate: Option<char>) {
        match intermediate {
            None => self.reply("\x1b[?6c".into()),
            Some('>') => self.reply(format!("\x1b[>0;{REPORTED_VERSION};1c")),
            Some(_) => (),
        }
    }

    /// DSR: 5 for whether all is well, 6 for where the cursor is (CPR).
    #[inline]
    pub fn device_status(&mut self, report: usize) {
        match report {
            5 => self.reply("\x1b[0n".into()),
            6 => {
                let point = self.grid.cursor.point;
                self.reply(format!("\x1b[{};{}R", point.line.0 + 1, point.column.0 + 1));
            },
            _ => (),
        }
    }

    /// DEC private DSR `?996n`: report whether the terminal background is dark
    /// (`?997;1n`) or light (`?997;2n`).
    #[inline]
    pub fn report_color_scheme(&mut self) {
        self.event_proxy.send_event(Event::ColorRequest(
            NamedColor::Background as usize,
            Arc::new(|Rgb { r, g, b }| {
                let dark = 299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b) <= 127_500;
                if dark { "\x1b[?997;1n" } else { "\x1b[?997;2n" }.into()
            }),
        ));
    }

    /// XTVERSION (`CSI > 0 q`): report the terminal name and version.
    #[inline]
    pub fn report_terminal_version(&mut self) {
        self.reply(concat!("\x1bP>|Tern ", env!("CARGO_PKG_VERSION"), "\x1b\\").into());
    }

    /// XTWINOPS 14: how big the text area is in pixels.
    #[inline]
    pub fn text_area_size_pixels(&mut self) {
        self.event_proxy.send_event(Event::TextAreaSizeRequest(Arc::new(|size| {
            let height = size.num_lines * size.cell_height;
            let width = size.num_cols * size.cell_width;
            format!("\x1b[4;{height};{width}t")
        })));
    }

    /// XTWINOPS 18: how big the text area is in cells.
    #[inline]
    pub fn text_area_size_chars(&mut self) {
        self.reply(format!("\x1b[8;{};{}t", self.screen_lines(), self.columns()));
    }

    /// OSC 4, 10, 11 and 12: the color in slot `index`. Everything is drawn
    /// again for one that changes, but for the cursor's.
    #[inline]
    pub fn set_color(&mut self, index: usize, color: Rgb) {
        if index != NamedColor::Cursor as usize && self.colors[index] != Some(color) {
            self.mark_fully_damaged();
        }
        self.colors[index] = Some(color);
    }

    /// A query of the color in slot `index`, answered with the OSC `prefix`
    /// and ended as the query was.
    #[inline]
    pub fn dynamic_color_sequence(&mut self, prefix: String, index: usize, terminator: &str) {
        let terminator = terminator.to_owned();
        self.event_proxy.send_event(Event::ColorRequest(
            index,
            Arc::new(move |Rgb { r, g, b }| {
                format!("\x1b]{prefix};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}{terminator}")
            }),
        ));
    }

    /// OSC 104, 110, 111 and 112: the color in slot `index` as the app has
    /// it.
    #[inline]
    pub fn reset_color(&mut self, index: usize) {
        if index != NamedColor::Cursor as usize && self.colors[index].is_some() {
            self.mark_fully_damaged();
        }
        self.colors[index] = None;
    }

    /// OSC 52: text for the clipboard (`c`) or the selection (`p`, `s`), in
    /// base64.
    #[inline]
    pub fn clipboard_store(&mut self, clipboard: u8, base64: &[u8]) {
        if !is_clipboard(clipboard) {
            return;
        }
        if let Ok(Ok(text)) = Base64.decode(base64).map(String::from_utf8) {
            self.event_proxy.send_event(Event::ClipboardStore(text));
        }
    }

    /// OSC 52 with `?`: what the clipboard has. Whether a program may read it
    /// is the app's call.
    #[inline]
    pub fn clipboard_load(&mut self, clipboard: u8, terminator: &str) {
        if !is_clipboard(clipboard) {
            return;
        }
        let terminator = terminator.to_owned();
        self.event_proxy.send_event(Event::ClipboardLoad(Arc::new(move |text| {
            let base64 = Base64.encode(text);
            format!("\x1b]52;{};{base64}{terminator}", clipboard as char)
        })));
    }

    /// OSC 0 and 2; None for the title the app gives.
    #[inline]
    pub fn set_title(&mut self, title: Option<String>) {
        self.title.clone_from(&title);
        self.event_proxy.send_event(match title {
            Some(title) => Event::Title(title),
            None => Event::ResetTitle,
        });
    }

    /// XTWINOPS 22. A full stack gives up its oldest.
    #[inline]
    pub fn push_title(&mut self) {
        if self.title_stack.len() >= TITLE_STACK_MAX_DEPTH {
            self.title_stack.remove(0);
        }
        self.title_stack.push(self.title.clone());
    }

    /// XTWINOPS 23: the title last pushed, if there is one.
    #[inline]
    pub fn pop_title(&mut self) {
        if let Some(title) = self.title_stack.pop() {
            self.set_title(title);
        }
    }

    /// BEL.
    #[inline]
    pub fn bell(&mut self) {
        self.event_proxy.send_event(Event::Bell);
    }

    /// OSC 7: records a local working directory URI (`file://host/path`).
    /// Remote hosts clear any stored path so local lookups do not use stale state.
    pub fn set_working_directory(&mut self, uri: &str) {
        self.working_directory = parse_osc7_path(uri.trim());
        self.event_proxy.send_event(Event::MouseCursorDirty);
    }

    /// OSC 133: records semantic prompt (`A`) and command-output (`C`) marks on
    /// the current primary-screen cursor line.
    pub fn prompt_mark(&mut self, kind: u8) {
        if self.mode.contains(crate::term::TermMode::ALT_SCREEN) {
            return;
        }
        let line = self.grid.cursor.point.line;
        match kind {
            b'A' => self.grid[line].add_mark(crate::grid::row::MARK_PROMPT),
            b'C' => self.grid[line].add_mark(crate::grid::row::MARK_OUTPUT),
            _ => {}
        }
    }

    /// OSC 9 / OSC 777: forwards a notification to the host application.
    pub fn notify(&mut self, title: Option<String>, body: String) {
        self.event_proxy.send_event(Event::Notify(title, body));
    }

    /// OSC 9;4: forwards a progress indicator state to the host application.
    pub fn set_progress(&mut self, progress: crate::event::Progress) {
        self.event_proxy.send_event(Event::Progress(progress));
    }

    /// Sends `text` to the program.
    #[inline]
    pub(super) fn reply(&mut self, text: String) {
        self.event_proxy.send_event(Event::PtyWrite(text));
    }
}

/// Whether OSC 52 names the clipboard (`c`) or the selection (`p`, `s`).
fn is_clipboard(clipboard: u8) -> bool {
    matches!(clipboard, b'c' | b'p' | b's')
}

pub(crate) fn parse_osc7_path(uri: &str) -> Option<String> {
    if uri.is_empty() {
        return None;
    }
    let rest = if let Some(r) = uri.strip_prefix("file://") {
        r
    } else if let Some(r) = uri.strip_prefix("kitty-shell-cwd://") {
        r
    } else if uri.starts_with('/') {
        return percent_decode(uri);
    } else {
        return None;
    };
    let slash = rest.find('/')?;
    let host = &rest[..slash];
    let path = &rest[slash..];
    if !host_is_local(host) {
        return None;
    }
    percent_decode(path)
}

fn host_is_local(host: &str) -> bool {
    if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is valid writable memory of length 256.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return false;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let Ok(local) = std::str::from_utf8(&buf[..end]) else {
        return false;
    };
    fn trim_local(s: &str) -> &str {
        s.strip_suffix(".local").unwrap_or(s)
    }
    trim_local(host).eq_ignore_ascii_case(trim_local(local))
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push(((h << 4) | l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok().filter(|p| p.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_osc7_file_uris_and_rejects_remote_hosts() {
        assert_eq!(
            parse_osc7_path("file:///Users/test/my%20dir").as_deref(),
            Some("/Users/test/my dir")
        );
        assert_eq!(
            parse_osc7_path("file://localhost/tmp/caf%C3%A9").as_deref(),
            Some("/tmp/caf\u{e9}")
        );
        assert_eq!(parse_osc7_path("file://remote-box.example.com/home/u"), None);
        assert_eq!(parse_osc7_path(""), None);
    }

    #[test]
    fn osc133_prompt_jumping_output_selection_and_notifications() {
        use crate::engine::{Options, Terminal};
        use crate::event::Event;

        let opts = Options {
            scrollback_lines: 200,
            ring_rows: 1,
            page_bytes: 128,
            ..Options::default()
        };
        let mut term = Terminal::new(&opts, 20, 3);
        term.advance(
            b"\x1b]133;A\x07$ cmd1\x1b]133;C\x07\r\nout1\r\nout2\r\n\
              \x1b]133;A\x07$ cmd2\x1b]133;C\x07\r\nalpha\r\nbeta\r\n\
              \x1b]133;A\x07$ \x1b]9;build done\x07\x1b]9;4;1;50\x07\x1b]777;notify;CI;green\x07",
        );
        while term.settle(64) {}

        let mut events = Vec::new();
        term.take_events(&mut events);
        let mut notes = Vec::new();
        let mut progress = Vec::new();
        for e in events {
            match e {
                Event::Notify(t, b) => notes.push((t, b)),
                Event::Progress(p) => progress.push(p),
                _ => {}
            }
        }
        assert_eq!(
            notes,
            vec![
                (None, "build done".to_owned()),
                (Some("CI".to_owned()), "green".to_owned()),
            ]
        );
        assert_eq!(progress, vec![crate::event::Progress::Set(50)]);

        assert!(term.select_last_output());
        assert_eq!(term.selection_text().as_deref(), Some("alpha\nbeta"));

        assert!(term.jump_prompt(-1));
        let off1 = term.display_offset();
        assert!(off1 > 0);
        assert!(term.jump_prompt(-1));
        assert!(term.display_offset() > off1);
        assert!(term.jump_prompt(1));
        assert_eq!(term.display_offset(), off1);
        assert!(term.jump_prompt(1));
        assert_eq!(term.display_offset(), 0);
    }

    #[test]
    fn reports_color_scheme_mode_2031_and_xtversion() {
        use crate::ansi::{NamedColor, Rgb};
        use crate::engine::{Options, Terminal};
        use crate::event::Event;

        let mut term = Terminal::new(&Options::default(), 20, 4);
        term.advance(b"\x1b[?2031$p\x1b[?2031h\x1b[?2031$p\x1b[?996n\x1b[>0q\x1b[?2031l\x1b[?2031$p");

        let mut events = Vec::new();
        term.take_events(&mut events);

        let mut writes = Vec::new();
        let mut dark_reply = None;
        let mut light_reply = None;
        for e in events {
            match e {
                Event::PtyWrite(s) => writes.push(s),
                Event::ColorRequest(idx, fmt) if idx == NamedColor::Background as usize => {
                    dark_reply = Some(fmt(Rgb { r: 0x18, g: 0x1a, b: 0x1f }));
                    light_reply = Some(fmt(Rgb { r: 0xfd, g: 0xf6, b: 0xe3 }));
                },
                _ => {},
            }
        }
        assert_eq!(dark_reply.as_deref(), Some("\x1b[?997;1n"));
        assert_eq!(light_reply.as_deref(), Some("\x1b[?997;2n"));
        assert_eq!(
            writes,
            vec![
                "\x1b[?2031;2$y".to_owned(),
                "\x1b[?2031;1$y".to_owned(),
                concat!("\x1bP>|Tern ", env!("CARGO_PKG_VERSION"), "\x1b\\").to_owned(),
                "\x1b[?2031;2$y".to_owned(),
            ]
        );
    }
}
