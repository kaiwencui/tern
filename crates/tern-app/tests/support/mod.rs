//! What the integration tests share: a JSON reader for the app's replies,
//! the app under test, and a reader for the PNGs it writes.
//!
//! Environment:
//!   TERN_TEST_DIR       where sockets, logs, configs and screenshots go
//!                       (default: cargo's target tmp directory)
//!   TERN_TEST_POSITION  "x,y" for --window-position-x/y (default "-20,-20",
//!                       the bottom-right of the main screen). The window
//!                       must land on a screen that shows a normal desktop
//!                       Space, or it presents no frames.
//!   TERN_WINLIST        a tool that lists a process's windows as lines of
//!                       "id=N ... onscreen=true ..."; when set, a test also
//!                       checks the window server's view of the window.
//!
//! Every app runs /bin/zsh with ZDOTDIR pointing into the test directory, so
//! no startup file of the user's runs, and a pasteboard of its own, so no
//! test touches the clipboard.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime};

// ---- a small JSON reader, enough for the app's replies ----

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(BTreeMap<String, Json>),
}

impl Json {
    pub fn parse(text: &str) -> Json {
        let mut p = Parser {
            s: text.as_bytes(),
            i: 0,
        };
        let v = p.value();
        p.ws();
        assert_eq!(p.i, p.s.len(), "trailing data in {text}");
        v
    }

    pub fn get(&self, key: &str) -> &Json {
        match self {
            Json::Obj(m) => m
                .get(key)
                .unwrap_or_else(|| panic!("no {key:?} in {self:?}")),
            _ => panic!("not an object: {self:?}"),
        }
    }

    pub fn num(&self) -> f64 {
        match self {
            Json::Num(n) => *n,
            _ => panic!("not a number: {self:?}"),
        }
    }

    pub fn int(&self) -> i64 {
        self.num() as i64
    }

    pub fn str(&self) -> &str {
        match self {
            Json::Str(s) => s,
            _ => panic!("not a string: {self:?}"),
        }
    }

    pub fn bool(&self) -> bool {
        match self {
            Json::Bool(b) => *b,
            _ => panic!("not a bool: {self:?}"),
        }
    }

    pub fn arr(&self) -> &[Json] {
        match self {
            Json::Arr(a) => a,
            _ => panic!("not an array: {self:?}"),
        }
    }

    pub fn strs(&self) -> Vec<&str> {
        self.arr().iter().map(Json::str).collect()
    }

    pub fn ints(&self) -> Vec<i64> {
        self.arr().iter().map(Json::int).collect()
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn eat(&mut self, b: u8) {
        self.ws();
        assert_eq!(
            self.s.get(self.i),
            Some(&b),
            "expected {:?} at {}",
            b as char,
            self.i
        );
        self.i += 1;
    }

    fn value(&mut self) -> Json {
        self.ws();
        match self.s[self.i] {
            b'{' => {
                self.i += 1;
                let mut m = BTreeMap::new();
                self.ws();
                if self.s[self.i] == b'}' {
                    self.i += 1;
                    return Json::Obj(m);
                }
                loop {
                    self.ws();
                    let Json::Str(k) = self.value() else {
                        panic!("object key is not a string")
                    };
                    self.eat(b':');
                    m.insert(k, self.value());
                    self.ws();
                    self.i += 1;
                    if self.s[self.i - 1] == b'}' {
                        return Json::Obj(m);
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let mut a = Vec::new();
                self.ws();
                if self.s[self.i] == b']' {
                    self.i += 1;
                    return Json::Arr(a);
                }
                loop {
                    a.push(self.value());
                    self.ws();
                    self.i += 1;
                    if self.s[self.i - 1] == b']' {
                        return Json::Arr(a);
                    }
                }
            }
            b'"' => {
                self.i += 1;
                let mut out = String::new();
                loop {
                    let start = self.i;
                    while self.s[self.i] != b'"' && self.s[self.i] != b'\\' {
                        self.i += 1;
                    }
                    out.push_str(std::str::from_utf8(&self.s[start..self.i]).expect("utf-8"));
                    if self.s[self.i] == b'"' {
                        self.i += 1;
                        return Json::Str(out);
                    }
                    self.i += 1;
                    let c = self.s[self.i];
                    self.i += 1;
                    match c {
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'u' => {
                            let hex =
                                std::str::from_utf8(&self.s[self.i..self.i + 4]).expect("hex");
                            self.i += 4;
                            let code = u32::from_str_radix(hex, 16).expect("hex");
                            out.push(char::from_u32(code).expect("scalar"));
                        }
                        other => out.push(other as char),
                    }
                }
            }
            b't' => {
                self.i += 4;
                Json::Bool(true)
            }
            b'f' => {
                self.i += 5;
                Json::Bool(false)
            }
            b'n' => {
                self.i += 4;
                Json::Null
            }
            _ => {
                let start = self.i;
                while self.i < self.s.len() && b"+-.eE0123456789".contains(&self.s[self.i]) {
                    self.i += 1;
                }
                let text = std::str::from_utf8(&self.s[start..self.i]).expect("utf-8");
                Json::Num(
                    text.parse()
                        .unwrap_or_else(|_| panic!("bad number {text:?}")),
                )
            }
        }
    }
}

// ---- the app under test ----

pub struct App {
    pub child: Child,
    pub socket: PathBuf,
    pub stderr: PathBuf,
    conn: BufReader<UnixStream>,
}

pub fn test_dir() -> PathBuf {
    let dir = std::env::var_os("TERN_TEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")));
    std::fs::create_dir_all(&dir).expect("test directory");
    dir
}

/// The configuration every test starts from; tests add lines after it.
const BASE_CONFIG: &str = "shell = /bin/zsh\nconfirm-close = false\n";

impl App {
    /// Starts the app with the base configuration and these command-line
    /// arguments.
    pub fn launch(name: &str, extra: &[&str]) -> App {
        App::launch_with(name, "", extra)
    }

    /// Starts the app with config lines added to the base configuration.
    pub fn launch_with(name: &str, config: &str, extra: &[&str]) -> App {
        let dir = test_dir();
        // Socket paths are limited to 104 bytes, so the name stays short.
        let socket = dir.join(format!("{name}.sock"));
        let stderr = dir.join(format!("{name}.log"));
        let config_file = dir.join(format!("{name}.conf"));
        std::fs::write(&config_file, format!("{BASE_CONFIG}{config}")).expect("config file");
        let zdot = dir.join("zdot");
        std::fs::create_dir_all(&zdot).expect("zsh directory");
        // A plain prompt, no reverse-video % marking a missing newline, and
        // no history carried from one test to the next.
        std::fs::write(
            zdot.join(".zshrc"),
            "PS1='%# '\nunsetopt PROMPT_SP\nunset HISTFILE\n",
        )
        .expect("zshrc");
        let position = std::env::var("TERN_TEST_POSITION").unwrap_or_else(|_| "-20,-20".into());
        let (x, y) = position.split_once(',').expect("TERN_TEST_POSITION is x,y");
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tern"));
        cmd.args(["--no-activate", "--log-startup", "--control"])
            .arg(&socket)
            .arg("--config-file")
            .arg(&config_file)
            .arg("--pasteboard")
            .arg(format!("tern-test-{name}-{}", std::process::id()))
            .arg(format!("--window-position-x={x}"))
            .arg(format!("--window-position-y={y}"))
            .args(extra)
            .env("ZDOTDIR", &zdot)
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&stderr).expect("log file"));
        let mut child = cmd.spawn().expect("launch tern");
        let deadline = Instant::now() + Duration::from_secs(10);
        let stream = loop {
            if let Ok(s) = UnixStream::connect(&socket) {
                break s;
            }
            if let Ok(Some(status)) = child.try_wait() {
                panic!("tern exited early with {status}: {}", read(&stderr));
            }
            assert!(
                Instant::now() < deadline,
                "the control socket never appeared"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(40)))
            .expect("timeout");
        App {
            child,
            socket,
            stderr,
            conn: BufReader::new(stream),
        }
    }

    pub fn send(&mut self, line: &str) -> Json {
        self.try_send(line)
            .unwrap_or_else(|| panic!("no reply to {line:?}"))
    }

    /// A request to an app that may be on its way out: None when the
    /// connection is gone.
    pub fn try_send(&mut self, line: &str) -> Option<Json> {
        writeln!(self.conn.get_mut(), "{line}").ok()?;
        let mut reply = String::new();
        self.conn.read_line(&mut reply).ok()?;
        (!reply.is_empty()).then(|| Json::parse(&reply))
    }

    pub fn ok(&mut self, line: &str) -> Json {
        let reply = self.send(line);
        assert!(reply.get("ok").bool(), "{line:?} failed: {reply:?}");
        reply
    }

    pub fn state(&mut self) -> Json {
        self.ok("state")
    }

    pub fn wait_for(&mut self, what: &str, test: impl Fn(&Json) -> bool) -> Json {
        self.wait_on("state", what, test)
    }

    /// Repeats a request until it succeeds and its reply passes the test.
    /// A request may fail at first: the first window comes a moment after
    /// the control socket.
    pub fn wait_on(&mut self, request: &str, what: &str, test: impl Fn(&Json) -> bool) -> Json {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let reply = self.send(request);
            if reply.get("ok").bool() && test(&reply) {
                return reply;
            }
            if Instant::now() >= deadline {
                // What reached the windows, to tell a slow program from
                // input that went somewhere else.
                let input: Vec<String> = self
                    .try_send("state")
                    .filter(|s| s.get("ok").bool())
                    .map(|s| {
                        windows(&s)
                            .iter()
                            .map(|w| format!("window {}: {:?}", w.get("id").int(), w.get("events")))
                            .collect()
                    })
                    .unwrap_or_default();
                panic!("timed out waiting for {what}: {reply:?}\ninput: {input:?}");
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    }

    /// The current window's focused pane's screen.
    pub fn screen(&mut self) -> String {
        self.ok("screen").get("text").str().to_string()
    }

    pub fn wait_screen(&mut self, what: &str, test: impl Fn(&str) -> bool) -> String {
        let reply = self.wait_on("screen", what, |r| test(r.get("text").str()));
        reply.get("text").str().to_string()
    }

    /// Waits for the shell's prompt on the last non-empty line.
    pub fn wait_prompt(&mut self) -> String {
        self.wait_screen("the prompt", |s| {
            s.lines().rev().find(|l| !l.is_empty()) == Some("%")
        })
    }

    /// The panes of the first window.
    pub fn panes(&mut self) -> Vec<Json> {
        let reply = self.ok("panes");
        reply.get("windows").arr()[0].get("panes").arr().to_vec()
    }

    /// Types a line into the focused pane as text, then presses Enter.
    pub fn type_line(&mut self, line: &str) {
        self.ok(&format!("text {}", escape(line)));
        self.ok("key enter");
    }

    pub fn stats(&mut self) -> Json {
        self.ok("stats")
    }

    /// Every pane of every window, in each window's visual order.
    pub fn all_panes(&mut self) -> Vec<Json> {
        let reply = self.ok("panes");
        reply
            .get("windows")
            .arr()
            .iter()
            .flat_map(|w| w.get("panes").arr().to_vec())
            .collect()
    }

    /// A pane, wherever it is.
    pub fn pane(&mut self, id: i64) -> Json {
        self.all_panes()
            .into_iter()
            .find(|p| p.get("id").int() == id)
            .unwrap_or_else(|| panic!("no pane {id}"))
    }

    pub fn screen_of(&mut self, id: i64) -> String {
        self.ok(&format!("screen {id}"))
            .get("text")
            .str()
            .to_string()
    }

    pub fn wait_screen_of(&mut self, id: i64, what: &str, test: impl Fn(&str) -> bool) -> String {
        let reply = self.wait_on(&format!("screen {id}"), what, |r| test(r.get("text").str()));
        reply.get("text").str().to_string()
    }

    /// Waits for a pane's shell prompt on its last non-empty line.
    pub fn wait_prompt_of(&mut self, id: i64) -> String {
        self.wait_screen_of(id, "the prompt", |s| {
            s.lines().rev().find(|l| !l.is_empty()) == Some("%")
        })
    }

    /// Focuses a pane and types a line into it.
    pub fn run_in(&mut self, id: i64, line: &str) {
        self.ok(&format!("focus-pane {id}"));
        self.type_line(line);
    }

    /// Checks with stty that the program in a pane sees the pane's grid.
    pub fn check_stty(&mut self, id: i64) {
        let pane = self.wait_on("panes", "the grid applied", |r| {
            r.get("windows").arr().iter().any(|w| {
                w.get("panes")
                    .arr()
                    .iter()
                    .any(|p| p.get("id").int() == id && p.get("grid") == p.get("frame_grid"))
            })
        });
        let grid = pane
            .get("windows")
            .arr()
            .iter()
            .flat_map(|w| w.get("panes").arr().iter())
            .find(|p| p.get("id").int() == id)
            .expect("the pane")
            .get("grid")
            .ints();
        self.wait_prompt_of(id);
        self.run_in(id, "clear; stty size");
        let want = format!("{} {}", grid[1], grid[0]);
        self.wait_screen_of(id, &format!("stty size {want}"), |s| {
            s.lines().any(|l| l == want)
        });
    }

    /// The first window, from a fresh state.
    pub fn first_window(&mut self) -> Json {
        let state = self.state();
        windows(&state)[0].clone()
    }

    /// The process ids of the app's child processes.
    pub fn children(&self) -> Vec<u32> {
        let out = Command::new("/bin/ps")
            .args(["-A", "-o", "pid=,ppid=,stat="])
            .output()
            .expect("ps");
        let me = self.child.id();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| {
                let mut f = l.split_whitespace();
                let pid: u32 = f.next()?.parse().ok()?;
                let ppid: u32 = f.next()?.parse().ok()?;
                (ppid == me).then_some(pid)
            })
            .collect()
    }

    /// Zombie children: exited and never reaped.
    pub fn zombies(&self) -> usize {
        let out = Command::new("/bin/ps")
            .args(["-A", "-o", "ppid=,stat="])
            .output()
            .expect("ps");
        let me = self.child.id().to_string();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| {
                let mut f = l.split_whitespace();
                f.next() == Some(me.as_str()) && f.next().is_some_and(|s| s.starts_with('Z'))
            })
            .count()
    }

    /// The app's open file descriptors, as lsof describes them (without
    /// the file offset, which changes).
    pub fn open_files(&self) -> Vec<String> {
        let out = Command::new("/usr/sbin/lsof")
            .args(["-n", "-P", "-p", &self.child.id().to_string()])
            .output()
            .expect("lsof");
        // A header line, then one per descriptor or mapping; only numbered
        // descriptors count.
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .skip(1)
            .filter_map(|l| {
                let fields: Vec<&str> = l.split_whitespace().collect();
                let fd = fields.get(3)?;
                fd.starts_with(|c: char| c.is_ascii_digit()).then(|| {
                    let mut kept: Vec<&str> = fields[3..].to_vec();
                    if kept.len() > 3 {
                        kept.remove(3);
                    }
                    kept.join(" ")
                })
            })
            .collect()
    }

    /// How many objects of each of these Objective-C classes the app holds,
    /// as heap(1) counts them.
    pub fn heap_counts(&self, classes: &[&str]) -> Vec<usize> {
        let out = Command::new("/usr/bin/heap")
            .arg(self.child.id().to_string())
            .output()
            .expect("heap");
        let listing = String::from_utf8_lossy(&out.stdout);
        classes
            .iter()
            .map(|class| {
                // "  COUNT  BYTES  AVG  CLASS_NAME  TYPE  BINARY"
                listing
                    .lines()
                    .filter_map(|l| {
                        let f: Vec<&str> = l.split_whitespace().collect();
                        (f.len() >= 6 && f[3] == *class && f[4] == "ObjC")
                            .then(|| f[0].parse::<usize>().ok())
                            .flatten()
                    })
                    .sum()
            })
            .collect()
    }

    /// How many threads the app runs.
    pub fn threads(&self) -> usize {
        let out = Command::new("/bin/ps")
            .args(["-M", "-p", &self.child.id().to_string()])
            .output()
            .expect("ps");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .count()
            .saturating_sub(1)
    }

    /// The scheduling priority of each of the app's threads, the main
    /// thread first.
    pub fn thread_priorities(&self) -> Vec<u32> {
        let out = Command::new("/bin/ps")
            .args(["-M", "-p", &self.child.id().to_string()])
            .output()
            .expect("ps");
        // The first thread's line starts with the user and the terminal;
        // the others start with the pid. PRI follows STAT, as in "46T".
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .skip(1)
            .filter_map(|l| {
                let fields: Vec<&str> = l.split_whitespace().collect();
                let at = if fields.first()?.parse::<u32>().is_ok() {
                    3
                } else {
                    5
                };
                fields
                    .get(at)?
                    .trim_end_matches(|c: char| c.is_ascii_alphabetic())
                    .parse()
                    .ok()
            })
            .collect()
    }

    pub fn wait_exit(&mut self) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = self.child.try_wait().expect("wait") {
                return status;
            }
            assert!(Instant::now() < deadline, "tern did not exit");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn quit(&mut self) {
        self.ok("quit");
        let status = self.wait_exit();
        assert!(status.success(), "exit status {status}");
    }
}

impl Drop for App {
    fn drop(&mut self) {
        // Only the process this test started, and only if it is still running.
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// A string as one token of a request: spaces and backslashes escaped,
/// newline, return, tab and escape spelled as the tokenizer reads them.
pub fn escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{1b}' => out.push_str("\\e"),
            c if c.is_whitespace() => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

pub fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

pub fn windows(state: &Json) -> &[Json] {
    state.get("windows").arr()
}

pub fn window(state: &Json, id: i64) -> &Json {
    windows(state)
        .iter()
        .find(|w| w.get("id").int() == id)
        .unwrap_or_else(|| panic!("no window {id}"))
}

pub fn ids(state: &Json) -> Vec<i64> {
    windows(state).iter().map(|w| w.get("id").int()).collect()
}

/// The first event of the given type in a reply (for keys, the press).
pub fn event<'a>(reply: &'a Json, ty: &str) -> &'a Json {
    reply
        .get("events")
        .arr()
        .iter()
        .find(|e| e.get("type").str() == ty && (ty != "key" || e.get("down").bool()))
        .unwrap_or_else(|| panic!("no {ty} event in {reply:?}"))
}

pub fn px(points: f64, scale: f64) -> i64 {
    (points * scale).floor() as i64
}

pub fn crash_reports_since(start: SystemTime) -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    let dir = Path::new(&home).join("Library/Logs/DiagnosticReports");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("tern"))
        .filter(|e| {
            e.metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| t >= start)
        })
        .map(|e| e.path())
        .collect()
}

/// Checks the window server's view of a window when TERN_WINLIST is set.
pub fn check_on_screen(app: &App, number: i64) {
    let Ok(tool) = std::env::var("TERN_WINLIST") else {
        return;
    };
    let out = Command::new(tool)
        .arg(app.child.id().to_string())
        .output()
        .expect("run winlist");
    let listing = String::from_utf8_lossy(&out.stdout);
    let line = listing
        .lines()
        .find(|l| l.starts_with(&format!("id={number} ")))
        .unwrap_or_else(|| panic!("window {number} not in the window list: {listing}"));
    assert!(line.contains("onscreen=true"), "{line}");
}

// ---- reading the app's screenshots ----

/// An RGBA image read from the PNGs the app writes: 8-bit RGBA, unfiltered
/// rows, in stored deflate blocks.
pub struct Png {
    pub width: u32,
    pub height: u32,
    rgba: Vec<u8>,
}

impl Png {
    pub fn read(path: &Path) -> Png {
        let bytes = std::fs::read(path).expect("read PNG");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");
        let (mut width, mut height, mut zlib) = (0, 0, Vec::new());
        let mut at = 8;
        while at + 8 <= bytes.len() {
            let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            let kind = &bytes[at + 4..at + 8];
            let data = &bytes[at + 8..at + 8 + len];
            match kind {
                b"IHDR" => {
                    width = u32::from_be_bytes(data[0..4].try_into().unwrap());
                    height = u32::from_be_bytes(data[4..8].try_into().unwrap());
                    assert_eq!(&data[8..10], &[8, 6], "8-bit RGBA");
                }
                b"IDAT" => zlib.extend_from_slice(data),
                _ => {}
            }
            at += 12 + len;
        }
        // Stored deflate blocks after the two-byte zlib header.
        let mut raw = Vec::new();
        let mut i = 2;
        loop {
            let last = zlib[i] & 1 == 1;
            assert_eq!(zlib[i] & 6, 0, "stored blocks only");
            let n = u16::from_le_bytes([zlib[i + 1], zlib[i + 2]]) as usize;
            raw.extend_from_slice(&zlib[i + 5..i + 5 + n]);
            i += 5 + n;
            if last {
                break;
            }
        }
        let stride = width as usize * 4 + 1;
        assert_eq!(raw.len(), stride * height as usize);
        let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
        for row in raw.chunks(stride) {
            assert_eq!(row[0], 0, "unfiltered rows only");
            rgba.extend_from_slice(&row[1..]);
        }
        Png {
            width,
            height,
            rgba,
        }
    }

    /// The pixel as 0xRRGGBB.
    pub fn rgb(&self, x: u32, y: u32) -> u32 {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        u32::from(self.rgba[i]) << 16
            | u32::from(self.rgba[i + 1]) << 8
            | u32::from(self.rgba[i + 2])
    }

    /// Every pixel of a rectangle.
    pub fn pixels(&self, x: u32, y: u32, w: u32, h: u32) -> Vec<u32> {
        let mut out = Vec::new();
        for yy in y..(y + h).min(self.height) {
            for xx in x..(x + w).min(self.width) {
                out.push(self.rgb(xx, yy));
            }
        }
        out
    }
}
