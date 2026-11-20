//! The automation socket: a Unix domain socket that takes one request per
//! line and answers one line of JSON. Requests are parsed on the socket's
//! thread and carried out on the main thread.
//!
//! Requests (tokens are separated by spaces; a backslash escapes the next
//! character, and `\n` `\t` `\r` `\e` stand for newline, tab, return and escape):
//!
//! ```text
//!   state                         every window, its geometry, flags and recent input
//!   menu                          the main menu as AppKit shows it
//!   menu-click <command>          chooses the menu item for a command, as a click does
//!                                 (command names as for action)
//!   key <spec>                    a key press, e.g. a, shift+a, ctrl+c, ralt+left, f5;
//!                                 a modifier alone (ralt, shift...) is pressed and released
//!   text <string>                 text through insertText:
//!   mouse <down|up|drag|move> <x> <y> [left|right|middle|N] [mods] [xN]
//!                                 xN: the click count (x2 for a double click); a
//!                                 point above the view (y below 0) is in the tab bar
//!   scroll <x> <y> <dx> <dy> [lines]
//!                                 a precise scroll, deltas in pixels; with lines,
//!                                 a mouse wheel's notches
//!   drag-over <x> <y>             a drag from another app moving over the view
//!   drag-exit                     that drag leaving the view
//!   drop <x> <y> path <p>...      what a drag of files onto the view delivers
//!   drop <x> <y> text <string>    what a drag of text delivers
//!   action <name>                 an action as a config file names it (new_tab,
//!                                 goto_tab:2, font_size:increase, text:\x01) or a
//!                                 standard command (about, hide, minimize, zoom...)
//!   resize <w> <h> [live]         the current window's content size in points; live
//!                                 wraps it in a live resize, as dragging an edge does
//!   move <x> <y>                  the current window's origin in screen points (AppKit's
//!                                 coordinates: origin at the primary display's bottom left)
//!   cursor <style>                the mouse pointer over the view
//!   tab-plus                      what the tab bar's + button sends
//!   tab-double-click <n>          a double click on the current window's nth tab (from 1),
//!                                 which puts a field over the tab to type its name in;
//!                                 key requests then go to the field
//!   tab-center <n>                where the middle of the current window's nth tab is,
//!                                 in view points, for mouse requests
//!   tab-pick <window> <n>         presses the nth tab (from 1) of the tab bar a window's
//!                                 tab is in, and holds the button, so a drag of the tab
//!                                 begins; AppKit follows it as it follows the mouse
//!                                 (windows by their ids in state)
//!   tab-drag <window> <x> <y>     drags the held tab to a point of a window's view; the
//!                                 pointer AppKit and the app go by stays the user's
//!   tab-drop                      lets the held tab go there: over the edge of a pane it
//!                                 docks beside the pane
//!   close-button                  the window's red close button
//!   tab-detach                    Window > Move Tab to New Window, as dragging a tab out does
//!   tab-merge                     Window > Merge All Windows
//!   tab-move <window> <to>        moves a window's tab into another window's tab bar,
//!                                 as dropping a dragged tab there does
//!   marked <string>               input method composition text through setMarkedText:
//!   focus-window <on|off>         what the window becoming or ceasing to be key does
//!   backing-scale <s>             what moving the window to a display of scale s does
//!                                 to its drawing (for machines with one display)
//!   confirm <yes|no>              answers the close or quit question a window shows
//!   panes                         every window's panes: rectangle, grid, title, process...
//!   focus-pane <id>               gives a pane the focus, as a click in it does
//!   screen [pane]                 a pane's visible text (default: the focused pane)
//!   drawn [pane]                  the text of the pane's last drawn frame, without a
//!                                 new snapshot
//!   selection [pane]              the pane's selected text
//!   screenshot <path>             a PNG of what the current window shows, drawn offscreen
//!   window-image <path>           a PNG of the current window as it is on screen, title
//!                                 bar included, at one pixel per point
//!   stats                         frame counts, the main thread's recent frame
//!                                 times (wall clock) and snapshots per frame, and
//!                                 the time from key presses to the frames showing them
//!   stats-reset                   forgets those frame and key times
//!   clipboard                     the text on the app's pasteboard
//!   clipboard-set <string>        puts text on the app's pasteboard
//!   open <path>...                what opening files or folders with the app sends
//!                                 (the Dock, Open With): a window in each folder
//!   reopen                        what a click on the Dock icon sends
//!   quit                          quit the app
//! ```
//!
//! Positions are in view points with the origin at the top left; the input
//! log reports them in view pixels. Requests act on the current window. A
//! request line longer than 1 MiB is refused and the connection closed.

mod exec;
mod json;
mod keys;
mod parse;

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use tern_config::Action;

use crate::mainthread::post_to_main;
use crate::menu::AppCommand;
use json::error;
use keys::KeySpec;
use parse::{parse, tokenize, Command};

/// How long a request waits for the main thread, which may be busy in a
/// modal loop.
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);
/// The longest request line accepted, newline included.
const MAX_REQUEST: u64 = 1 << 20;

pub struct Server {
    path: PathBuf,
}

impl Server {
    pub fn start(path: &Path) -> io::Result<Server> {
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_socket() => {
                // A socket left behind by an earlier run is replaced; one that
                // another instance still answers on is not taken from it.
                if UnixStream::connect(path).is_ok() {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        "another process is listening on it",
                    ));
                }
                fs::remove_file(path)?
            }
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "exists and is not a socket",
                ))
            }
            Err(_) => {}
        }
        // The umask makes the socket owner-only from the moment it exists;
        // no other thread is creating files yet.
        // SAFETY: umask has no memory effects.
        let old = unsafe { libc::umask(0o177) };
        let bound = UnixListener::bind(path);
        // SAFETY: as above.
        unsafe { libc::umask(old) };
        let listener = bound?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        thread::Builder::new()
            .name("tern-control".into())
            .spawn(move || {
                for connection in listener.incoming() {
                    match connection {
                        Ok(stream) => {
                            let spawned = thread::Builder::new()
                                .name("tern-control-client".into())
                                .spawn(move || serve(stream));
                            if let Err(e) = spawned {
                                eprintln!("tern: control socket: {e}");
                            }
                        }
                        Err(e) => eprintln!("tern: control socket: {e}"),
                    }
                }
            })?;
        Ok(Server {
            path: path.to_path_buf(),
        })
    }

    pub fn remove(&self) {
        // Nothing to do if it is already gone.
        let _ = fs::remove_file(&self.path);
    }
}

fn serve(stream: UnixStream) {
    let Ok(input) = stream.try_clone() else {
        return;
    };
    let mut input = BufReader::new(input);
    let mut output = stream;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        // Bounded, so a client that never sends a newline cannot make the
        // app buffer without limit.
        match (&mut input)
            .take(MAX_REQUEST + 1)
            .read_until(b'\n', &mut buf)
        {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if buf.len() as u64 > MAX_REQUEST {
            let _ = writeln!(output, "{}", error("the request is too long"));
            return;
        }
        let parsed = std::str::from_utf8(&buf)
            .map_err(|_| "the request is not UTF-8".to_string())
            .and_then(|line| {
                if line.trim().is_empty() {
                    return Ok(None);
                }
                tokenize(line).and_then(|tokens| parse(&tokens)).map(Some)
            });
        let mut quit = false;
        let reply = match parsed {
            Ok(None) => continue,
            Err(e) => error(&e),
            // Quitting ends the process, so answer before doing it.
            Ok(Some(Command::Quit)) | Ok(Some(Command::Action(AppCommand::Do(Action::Quit)))) => {
                quit = true;
                "{\"ok\":true}".to_string()
            }
            Ok(Some(command)) => run_on_main(command),
        };
        if writeln!(output, "{reply}")
            .and_then(|()| output.flush())
            .is_err()
        {
            return;
        }
        if quit {
            post_to_main(true, |_| crate::app::app().quit());
        }
    }
}

fn run_on_main(command: Command) -> String {
    let (tx, rx) = mpsc::sync_channel(1);
    post_to_main(true, move |mtm| {
        // The requester may have given up waiting.
        let _ = tx.send(exec::execute(mtm, command));
    });
    rx.recv_timeout(REPLY_TIMEOUT)
        .unwrap_or_else(|_| error("the main thread did not answer"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where test sockets go: TERN_TEST_DIR, as for the integration tests,
    /// or the system's temporary directory. Socket paths are limited to 104
    /// bytes, so names stay short.
    fn socket_path(name: &str) -> PathBuf {
        let dir = std::env::var_os("TERN_TEST_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        fs::create_dir_all(&dir).expect("test directory");
        let path = dir.join(format!("{name}{}.s", std::process::id()));
        let _ = fs::remove_file(&path);
        path
    }

    #[test]
    fn server_replaces_a_stale_socket_but_not_a_live_one() {
        let path = socket_path("l");
        // A listener that is still running: another instance.
        let live = UnixListener::bind(&path).expect("bind");
        let err = Server::start(&path)
            .err()
            .expect("must not take a live socket");
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
        assert!(
            UnixStream::connect(&path).is_ok(),
            "the live socket is left alone"
        );
        drop(live);

        // The listener is gone and its file stays behind: replaced.
        let server = Server::start(&path).expect("replaces a stale socket");
        let mode = fs::metadata(&path).expect("socket").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "owner-only");
        assert!(UnixStream::connect(&path).is_ok());
        server.remove();
        assert!(!path.exists());

        // Something that is not a socket is never removed.
        fs::write(&path, b"keep").expect("write");
        assert!(Server::start(&path).is_err());
        assert_eq!(fs::read(&path).expect("read"), b"keep");
        fs::remove_file(&path).expect("clean up");
    }

    /// Runs serve() on one end of a socket pair and returns the other.
    fn client() -> BufReader<UnixStream> {
        let (ours, theirs) = UnixStream::pair().expect("pair");
        thread::spawn(move || serve(theirs));
        ours.set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout");
        BufReader::new(ours)
    }

    fn reply(conn: &mut BufReader<UnixStream>, request: &[u8]) -> String {
        conn.get_mut().write_all(request).expect("write");
        let mut line = String::new();
        conn.read_line(&mut line).expect("read");
        line
    }

    #[test]
    fn bad_requests_get_an_error_and_the_connection_stays_open() {
        let mut conn = client();
        // Blank lines are skipped, so the reply below answers "bogus".
        assert_eq!(
            reply(&mut conn, b"\n  \nbogus\n"),
            "{\"ok\":false,\"error\":\"unknown or malformed request: bogus\"}\n"
        );
        assert_eq!(
            reply(&mut conn, b"text \xff\xfe\n"),
            "{\"ok\":false,\"error\":\"the request is not UTF-8\"}\n"
        );
        assert!(reply(&mut conn, b"resize 0 0\r\n").contains("the size must be positive"));
    }

    #[test]
    fn an_endless_request_is_refused() {
        let mut conn = client();
        // One byte past the limit, all of which the server reads before it
        // answers, so the write cannot race its hang-up.
        let long = vec![b'x'; MAX_REQUEST as usize + 1];
        assert_eq!(
            reply(&mut conn, &long),
            "{\"ok\":false,\"error\":\"the request is too long\"}\n"
        );
        // The server hung up.
        let mut rest = String::new();
        assert_eq!(conn.read_line(&mut rest).expect("read"), 0);
    }
}
