//! Measures a terminal emulator from the inside, the same way for any
//! terminal, so two of them can be compared.
//!
//!   termbench inside --out FILE --label NAME [--t0 NS] [--iters N] [--settle-ms MS] [--hold-ms MS]
//!                    [--usage-of APP] [--rest-ms MS] FILES...
//!       Run as the terminal's command. Writes each file to the terminal, then
//!       asks for a cursor position report and waits for the answer, so the
//!       time covers everything the terminal had to process and not only what
//!       it had buffered. Appends one JSON line per run to FILE. It first
//!       waits --settle-ms, so that no terminal is measured while it is still
//!       starting up; launch time is measured separately.
//!       With --usage-of, it also records what the terminal's own processes
//!       (every one whose program is inside the folder APP) used meanwhile:
//!       processor time and energy for the whole of the output; or, with no
//!       files, per second while the terminal is left alone for --rest-ms,
//!       once as it is and once with its cursor hidden.
//!       An argument @FILE stands for the lines of FILE, one argument a line.
//!   termbench now
//!       Prints the time in nanoseconds; pass it as --t0 to get the time from
//!       launch to the moment the terminal's child runs.
//!   termbench window PID T0 LABEL FILE
//!       Waits for the first window of process PID to be on screen and appends
//!       the time since T0 to FILE.
//!   termbench gen DIR [--mb N]
//!       Writes the five payloads (ascii, dense, unicode, sgr, long).
//!   termbench summarize FILE...
//!       Prints the medians from result files, side by side. Key presses, which
//!       scripts/keylat.swift times, are shown by their median and their mean.
//!   termbench vtebench FILE...
//!       Prints the mean of each benchmark in vtebench's .dat files, side by
//!       side. A file named vtebench-NAME.dat is shown under NAME.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

fn now_ns() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// The arguments, with one that starts with `@` replaced by the lines of the
/// file it names: some terminals take their command as one string of limited
/// length, which the paths of five payloads go past.
fn arguments() -> Result<std::vec::IntoIter<String>, String> {
    let mut all = Vec::new();
    for arg in std::env::args().skip(1) {
        match arg.strip_prefix('@') {
            Some(path) => {
                let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
                all.extend(text.lines().filter(|line| !line.is_empty()).map(String::from));
            }
            None => all.push(arg),
        }
    }
    Ok(all.into_iter())
}

fn main() -> ExitCode {
    let now = now_ns();
    let mut args = match arguments() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("termbench: {e}");
            return ExitCode::from(2);
        }
    };
    let result = match args.next().as_deref() {
        Some("now") => {
            println!("{now}");
            Ok(())
        }
        Some("inside") => inside(now, args),
        Some("window") => window(args),
        Some("gen") => generate(args),
        Some("summarize") => summarize(args),
        Some("vtebench") => vtebench(args),
        _ => Err(
            "usage: termbench inside|now|window|gen|summarize|vtebench ... (see the top of src/main.rs)"
                .to_string(),
        ),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("termbench: {e}");
            ExitCode::from(2)
        }
    }
}

// ---- inside: the part that runs in the terminal under test ----

fn write_all(fd: i32, mut bytes: &[u8]) -> Result<(), String> {
    while !bytes.is_empty() {
        // SAFETY: the pointer and length describe the live slice.
        let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len().min(1 << 16)) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("write to the terminal: {e}"));
        }
        bytes = &bytes[n as usize..];
    }
    Ok(())
}

/// Asks where the cursor is and waits for the reply. The terminal answers only
/// after it has processed everything written before the question.
fn wait_until_processed() -> Result<bool, String> {
    write_all(1, b"\x1b[6n")?;
    let mut buf = [0u8; 64];
    let mut pfd = libc::pollfd {
        fd: 0,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        // SAFETY: one valid pollfd.
        let r = unsafe { libc::poll(&mut pfd, 1, 120_000) };
        if r <= 0 {
            return Ok(false);
        }
        // SAFETY: the buffer is live and its length is passed.
        let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            return Ok(false);
        }
        if buf[..n as usize].contains(&b'R') {
            return Ok(true);
        }
    }
}

fn inside(started: u128, mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let mut out = None;
    let mut t0: Option<u128> = None;
    let mut label = String::from("terminal");
    let mut iters = 3usize;
    let mut hold_ms = 1500u64;
    let mut settle_ms = 0u64;
    let mut rest_ms = 3000u64;
    let mut usage_of: Option<String> = None;
    let mut files = Vec::new();
    while let Some(a) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
        match a.as_str() {
            "--out" => out = Some(value("--out")?),
            "--t0" => t0 = value("--t0")?.parse().ok(),
            "--label" => label = value("--label")?,
            "--iters" => {
                iters = value("--iters")?
                    .parse()
                    .map_err(|_| "--iters: not a number")?
            }
            "--hold-ms" => {
                hold_ms = value("--hold-ms")?
                    .parse()
                    .map_err(|_| "--hold-ms: not a number")?
            }
            "--settle-ms" => {
                settle_ms = value("--settle-ms")?
                    .parse()
                    .map_err(|_| "--settle-ms: not a number")?
            }
            "--rest-ms" => {
                rest_ms = value("--rest-ms")?
                    .parse()
                    .map_err(|_| "--rest-ms: not a number")?
            }
            "--usage-of" => {
                // As the system spells it, which is how programs' paths come back.
                let app = value("--usage-of")?;
                let real = std::fs::canonicalize(&app).map_err(|e| format!("{app}: {e}"))?;
                usage_of = Some(format!("{}/", real.display()));
            }
            _ => files.push(a),
        }
    }
    let out = out.ok_or("--out is required")?;
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&out)
        .map_err(|e| format!("{out}: {e}"))?;
    let mut emit = |line: String| {
        let _ = writeln!(log, "{line}");
        let _ = log.sync_all();
    };

    // SAFETY: winsize is plain data; the ioctl fills it.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) };
    let startup = t0
        .map(|t| format!("{:.1}", started.saturating_sub(t) as f64 / 1e6))
        .unwrap_or_else(|| "null".into());
    emit(format!(
        "{{\"label\":\"{label}\",\"event\":\"start\",\"startup_ms\":{startup},\"cols\":{},\"rows\":{},\"term\":\"{}\"}}",
        ws.ws_col,
        ws.ws_row,
        std::env::var("TERM").unwrap_or_default()
    ));

    // Leave output processing as a shell's cat would have it; only stop echo
    // and line buffering so the reply can be read.
    // SAFETY: termios is plain data filled by tcgetattr.
    let mut old: libc::termios = unsafe { std::mem::zeroed() };
    unsafe {
        libc::tcgetattr(0, &mut old);
        let mut raw = old;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        libc::tcsetattr(0, libc::TCSANOW, &raw);
    }
    let answered = wait_until_processed()?;
    emit(format!(
        "{{\"label\":\"{label}\",\"event\":\"ready\",\"ok\":{answered},\"ready_ms\":{}}}",
        t0.map(|t| format!("{:.1}", now_ns().saturating_sub(t) as f64 / 1e6))
            .unwrap_or_else(|| "null".into())
    ));
    if !files.is_empty() {
        std::thread::sleep(std::time::Duration::from_millis(settle_ms));
    }
    // What the terminal's processes have used before the output, to take
    // from what they have used after it.
    let before = usage_of.as_deref().map(|app| (usage::of(app), Instant::now()));
    for path in &files {
        let data = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        let name = Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        for i in 0..iters {
            // The grid at the time of the run, so two terminals can be shown
            // to have been measured at the same size.
            // SAFETY: winsize is plain data; the ioctl fills it.
            unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) };
            let t = Instant::now();
            write_all(1, &data)?;
            let ok = wait_until_processed()?;
            let secs = t.elapsed().as_secs_f64();
            emit(format!(
                "{{\"label\":\"{label}\",\"event\":\"run\",\"file\":\"{name}\",\"iter\":{i},\"bytes\":{},\"secs\":{secs:.4},\"mbps\":{:.1},\"cols\":{},\"rows\":{},\"ok\":{ok}}}",
                data.len(),
                data.len() as f64 / 1e6 / secs,
                ws.ws_col,
                ws.ws_row
            ));
        }
    }
    if let (Some((before, began)), Some(app), false) = (before, usage_of.as_deref(), files.is_empty()) {
        let secs = began.elapsed().as_secs_f64();
        let used = usage::of(app).since(&before);
        emit(format!(
            "{{\"label\":\"{label}\",\"event\":\"usage\",\"when\":\"output\",\"secs\":{secs:.4},\"cpu_s\":{:.4},\"energy_j\":{:.4},\"processes\":{}}}",
            used.cpu_ns as f64 / 1e9,
            used.energy_nj as f64 / 1e9,
            used.processes
        ));
    }
    // SAFETY: restores the settings read above.
    unsafe { libc::tcsetattr(0, libc::TCSANOW, &old) };
    emit(format!("{{\"label\":\"{label}\",\"event\":\"done\"}}"));
    // A terminal left alone: what it uses per second with nothing to do but
    // show its cursor, as it shows one out of the box; and then with the
    // cursor hidden, since some terminals blink theirs and some do not. Each
    // time it is given a while first: to finish what starting up and the
    // caller's first look at its memory set off, and then for the caller's
    // second look, which comes [SECOND_LOOK_MS] after the first.
    let began = Instant::now();
    if let (Some(app), true) = (usage_of.as_deref(), files.is_empty()) {
        let mut rest = |from_ms: u64, when: &str| {
            let from = std::time::Duration::from_millis(from_ms);
            std::thread::sleep(from.saturating_sub(began.elapsed()));
            let (before, from) = (usage::of(app), Instant::now());
            std::thread::sleep(std::time::Duration::from_millis(rest_ms));
            let secs = from.elapsed().as_secs_f64();
            let used = usage::of(app).since(&before);
            emit(format!(
                "{{\"label\":\"{label}\",\"event\":\"usage\",\"when\":\"{when}\",\"secs\":{secs:.4},\"cpu_ms_per_s\":{:.3},\"power_mw\":{:.3},\"wakeups_per_s\":{:.2},\"processes\":{}}}",
                used.cpu_ns as f64 / 1e6 / secs,
                used.energy_nj as f64 / 1e6 / secs,
                used.wakeups as f64 / secs,
                used.processes
            ));
        };
        rest(REST_AFTER_MS, "rest");
        // Hidden only after the caller's second look, which a frame drawn
        // for this would show in the memory it reads; and then left a
        // moment for that frame to be over with.
        let second_look = std::time::Duration::from_millis(REST_AFTER_MS + rest_ms + SECOND_LOOK_MS);
        std::thread::sleep(second_look.saturating_sub(began.elapsed()));
        let _ = write_all(1, b"\x1b[?25l");
        rest(REST_AFTER_MS + rest_ms + SECOND_LOOK_MS + HIDDEN_SETTLE_MS, "rest hidden");
        let _ = write_all(1, b"\x1b[?25h");
    }
    // Stay alive so the caller can read the terminal's memory use.
    let hold = std::time::Duration::from_millis(hold_ms);
    std::thread::sleep(hold.saturating_sub(began.elapsed()));
    Ok(())
}

/// How long a terminal is left alone before what it uses at rest is
/// measured.
const REST_AFTER_MS: u64 = 3500;

/// How long is left for the caller's second look at the terminal's memory,
/// between the two measurements of what it uses at rest.
const SECOND_LOOK_MS: u64 = 2500;

/// How long a terminal is given after its cursor was hidden before it is
/// measured at rest again: a frame was drawn for that, and what drawing
/// set off takes a moment to die down.
const HIDDEN_SETTLE_MS: u64 = 2500;

// ---- usage: what a terminal's processes have used ----

mod usage {
    use std::ffi::c_void;

    /// How many nanoseconds a tick of the kernel's clock is: `numer / denom`.
    #[repr(C)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }

    unsafe extern "C" {
        fn mach_timebase_info(info: *mut Timebase) -> libc::c_int;
    }

    /// `rusage_info_v6` of `<sys/resource.h>`: its number, its size in
    /// 64-bit words, and where in it the counters read here are.
    const RUSAGE_INFO_V6: libc::c_int = 6;
    const V6_WORDS: usize = 464 / 8;
    const USER_TIME: usize = 16 / 8;
    const SYSTEM_TIME: usize = 24 / 8;
    const INTERRUPT_WAKEUPS: usize = 40 / 8;
    const ENERGY_NJ: usize = 336 / 8;

    /// Processor time, energy and the times a timer or another interrupt woke
    /// a thread, summed over some processes.
    #[derive(Clone, Copy, Default)]
    pub struct Used {
        pub cpu_ns: u64,
        pub energy_nj: u64,
        pub wakeups: u64,
        pub processes: usize,
    }

    impl Used {
        /// What was used since `earlier`. A process that has gone since takes
        /// its share with it, so nothing goes below nought.
        pub fn since(&self, earlier: &Used) -> Used {
            Used {
                cpu_ns: self.cpu_ns.saturating_sub(earlier.cpu_ns),
                energy_nj: self.energy_nj.saturating_sub(earlier.energy_nj),
                wakeups: self.wakeups.saturating_sub(earlier.wakeups),
                processes: self.processes,
            }
        }
    }

    /// What every process whose program lies under `app` (a folder's path,
    /// ending in a slash) has used since it started: the terminal itself and
    /// whatever helpers it runs from its own bundle, but not the programs
    /// running in it.
    pub fn of(app: &str) -> Used {
        let mut pids = vec![0 as libc::pid_t; 8192];
        let size = std::mem::size_of_val(&pids[..]) as libc::c_int;
        // SAFETY: the buffer is live and its size in bytes is passed.
        let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast::<c_void>(), size) };
        pids.truncate(count.max(0) as usize);

        // The kernel counts processor time in ticks of its own.
        let mut timebase = Timebase { numer: 1, denom: 1 };
        // SAFETY: the struct is plain data the call fills.
        unsafe { mach_timebase_info(&mut timebase) };
        let ticks_to_ns = |ticks: u64| ticks as u128 * timebase.numer as u128 / timebase.denom.max(1) as u128;

        let mut used = Used::default();
        let mut path = [0u8; 4096];
        for pid in pids {
            // SAFETY: the buffer is live and its length is passed.
            let len = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast::<c_void>(), path.len() as u32) };
            if len <= 0 || !path[..len as usize].starts_with(app.as_bytes()) {
                continue;
            }
            // The sixth version of the record, the first with the energy the
            // processor itself counted: 464 bytes, all but the first sixteen
            // of them 64-bit counters.
            let mut info = [0u64; V6_WORDS];
            let buffer = info.as_mut_ptr().cast::<libc::rusage_info_t>();
            // SAFETY: the buffer is as large as the record of this version.
            if unsafe { libc::proc_pid_rusage(pid, RUSAGE_INFO_V6, buffer) } != 0 {
                continue;
            }
            used.cpu_ns += ticks_to_ns(info[USER_TIME] + info[SYSTEM_TIME]) as u64;
            used.energy_nj += info[ENERGY_NJ];
            used.wakeups += info[INTERRUPT_WAKEUPS];
            used.processes += 1;
        }
        used
    }
}

// ---- window: when a process's first window reaches the screen ----

mod screen {
    use std::ffi::c_void;

    type Ref = *const c_void;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> Ref;
        static kCGWindowOwnerPID: Ref;
        static kCGWindowLayer: Ref;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFArrayGetCount(array: Ref) -> isize;
        fn CFArrayGetValueAtIndex(array: Ref, index: isize) -> Ref;
        fn CFDictionaryGetValue(dict: Ref, key: Ref) -> Ref;
        fn CFNumberGetValue(number: Ref, kind: isize, out: *mut c_void) -> bool;
        fn CFRelease(object: Ref);
    }

    const ON_SCREEN_ONLY: u32 = 1;
    const SINT64: isize = 4;

    /// Reads one integer entry of a window's description.
    ///
    /// SAFETY: dict must be a live CFDictionary and key a CFString.
    unsafe fn number(dict: Ref, key: Ref) -> Option<i64> {
        let value = unsafe { CFDictionaryGetValue(dict, key) };
        let mut out = 0i64;
        (!value.is_null()
            && unsafe { CFNumberGetValue(value, SINT64, (&mut out as *mut i64).cast()) })
        .then_some(out)
    }

    /// Whether the process has an ordinary window on screen right now. The
    /// window server answers this for any process without special permission.
    pub fn has_window(pid: i64) -> bool {
        // SAFETY: the list is owned by this call and released below; the
        // entries are dictionaries that live as long as the list.
        unsafe {
            let list = CGWindowListCopyWindowInfo(ON_SCREEN_ONLY, 0);
            if list.is_null() {
                return false;
            }
            let found = (0..CFArrayGetCount(list)).any(|i| {
                let window = CFArrayGetValueAtIndex(list, i);
                number(window, kCGWindowOwnerPID) == Some(pid)
                    && number(window, kCGWindowLayer) == Some(0)
            });
            CFRelease(list);
            found
        }
    }
}

fn window(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let mut next = |name: &str| args.next().ok_or(format!("window needs {name}"));
    let pid: i64 = next("a process id")?
        .parse()
        .map_err(|_| "window: the process id is not a number")?;
    let t0: u128 = next("a start time")?
        .parse()
        .map_err(|_| "window: the start time is not a number")?;
    let label = next("a label")?;
    let out = next("a result file")?;
    let deadline = Instant::now() + std::time::Duration::from_secs(20);
    while !screen::has_window(pid) {
        if Instant::now() > deadline {
            return Err(format!("process {pid} showed no window within 20 s"));
        }
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
    let ms = now_ns().saturating_sub(t0) as f64 / 1e6;
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&out)
        .map_err(|e| format!("{out}: {e}"))?;
    writeln!(
        log,
        "{{\"label\":\"{label}\",\"event\":\"window\",\"window_ms\":{ms:.1}}}"
    )
    .map_err(|e| e.to_string())
}

// ---- gen: the payloads ----

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// Repeats a block of generated text until the file reaches the size asked.
fn write_repeated(path: &Path, block: &str, bytes: usize) -> Result<(), String> {
    let mut f = std::io::BufWriter::new(
        std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?,
    );
    let mut written = 0;
    while written < bytes {
        f.write_all(block.as_bytes()).map_err(|e| e.to_string())?;
        written += block.len();
    }
    f.flush().map_err(|e| e.to_string())
}

fn generate(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let dir = args.next().ok_or("gen needs a directory")?;
    let mut mb = 30usize;
    while let Some(a) = args.next() {
        if a == "--mb" {
            mb = args
                .next()
                .and_then(|v| v.parse().ok())
                .ok_or("--mb needs a number")?;
        }
    }
    let dir = Path::new(&dir);
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let bytes = mb << 20;
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);

    // Source code: short lines, indentation, punctuation. The common case.
    const WORDS: &[&str] = &[
        "static", "const", "struct", "return", "if", "else", "for", "while", "void", "int",
        "size_t", "uint32_t", "buffer", "length", "index", "result", "error", "context", "handle",
        "state", "value", "count", "NULL", "#include", "#define", "typedef", "enum", "char",
        "unsigned", "callback", "options", "flags", "offset",
    ];
    const PUNCT: &[&str] = &[
        " = ", "(", ")", ", ", ";", " {", "}", " -> ", " * ", "[", "]", " == ", " && ", " /* ",
        " */",
    ];
    let mut ascii = String::new();
    while ascii.len() < 2 << 20 {
        let indent = rng.below(4) * 4;
        ascii.extend(std::iter::repeat_n(' ', indent));
        for _ in 0..1 + rng.below(9) {
            ascii.push_str(rng.pick::<&str>(WORDS));
            ascii.push_str(rng.pick::<&str>(PUNCT));
        }
        ascii.push('\n');
        if rng.below(12) == 0 {
            ascii.push('\n');
        }
    }
    write_repeated(&dir.join("ascii.txt"), &ascii, bytes)?;

    // Dense: every cell of a 100-column line filled.
    let printable: Vec<char> = (33u8..127).map(char::from).collect();
    let mut dense = String::new();
    while dense.len() < 1 << 20 {
        for _ in 0..100 {
            dense.push(*rng.pick(&printable));
        }
        dense.push('\n');
    }
    write_repeated(&dir.join("dense.txt"), &dense, bytes)?;

    // Unicode: CJK, kana, accented Latin, emoji, box drawing, Greek.
    let pools: Vec<Vec<char>> = vec![
        (0x4e00u32..0x4e00 + 2000)
            .filter_map(char::from_u32)
            .collect(),
        (0x3041u32..0x3096).filter_map(char::from_u32).collect(),
        "àáâãäåæçèéêëìíîïñòóôõöùúûüýÿœßĄĆĘŁŃŚŹŻ".chars().collect(),
        "😀😁😂🤣😃😄😅😆😉😊🚀🔥✨🎉👍🙏💯🧠🐙🦀".chars().collect(),
        "─│┌┐└┘├┤┬┴┼═║╔╗╚╝█▓▒░▀▄".chars().collect(),
        (0x391u32..0x3c9)
            .filter_map(char::from_u32)
            .filter(|c| c.is_alphabetic())
            .collect(),
        "abcdefghijklmnopqrstuvwxyz ".chars().collect(),
    ];
    let mut unicode = String::new();
    while unicode.len() < 1 << 20 {
        let pool = &pools[rng.below(pools.len())];
        for _ in 0..20 + rng.below(40) {
            unicode.push(*rng.pick(pool));
        }
        unicode.push('\n');
    }
    write_repeated(&dir.join("unicode.txt"), &unicode, bytes)?;

    // SGR: colored words, as compilers and ls --color print them.
    const SHORT: &[&str] = &[
        "fn",
        "let",
        "mut",
        "struct",
        "impl",
        "return",
        "match",
        "error",
        "warning",
        "note",
        "src/main.rs",
        "Compiling",
        "Finished",
        "0x7ffee",
        "=>",
        "{",
        "}",
        "foo_bar",
        "String::from",
        "\"text\"",
    ];
    let mut sgr = String::new();
    while sgr.len() < 1 << 20 {
        for _ in 0..4 + rng.below(10) {
            let w = rng.pick(SHORT);
            match rng.below(10) {
                0..=2 => sgr.push_str(&format!(
                    "\x1b[{};{}m{w}\x1b[0m",
                    [1, 3, 4, 7][rng.below(4)],
                    30 + rng.below(8)
                )),
                3..=4 => sgr.push_str(&format!("\x1b[38;5;{}m{w}\x1b[39m", rng.below(256))),
                5..=7 => sgr.push_str(&format!(
                    "\x1b[38;2;{};{};{}m{w}\x1b[m",
                    rng.below(256),
                    rng.below(256),
                    rng.below(256)
                )),
                8 => sgr.push_str(&format!("\x1b[48;5;{}m{w}\x1b[49m", rng.below(256))),
                _ => sgr.push_str(w),
            }
            sgr.push(' ');
        }
        sgr.push('\n');
    }
    write_repeated(&dir.join("sgr.txt"), &sgr, bytes)?;

    // Long lines: thousands of columns each, so every line wraps many times.
    let mut long = String::new();
    while long.len() < 2 << 20 {
        for _ in 0..2000 + rng.below(10000) {
            long.push(if rng.below(6) == 0 {
                ' '
            } else {
                *rng.pick(&printable)
            });
        }
        long.push('\n');
    }
    write_repeated(&dir.join("long.txt"), &long, bytes)?;
    Ok(())
}

// ---- summarize ----

/// Pulls one field out of a flat JSON line written by this program.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let start = line.find(&format!("\"{key}\":"))? + key.len() + 3;
    let rest = &line[start..];
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    Some(rest[..end].trim_matches('"'))
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let mid = values.len() / 2;
    Some(if values.len() % 2 == 1 {
        values[mid]
    } else {
        (values[mid - 1] + values[mid]) / 2.0
    })
}

fn summarize(args: impl Iterator<Item = String>) -> Result<(), String> {
    // label -> metric -> samples. Metrics keep the order they first appear in.
    let mut data: BTreeMap<String, BTreeMap<String, Vec<f64>>> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    for path in args {
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
        for line in text.lines() {
            let (Some(label), Some(event)) = (field(line, "label"), field(line, "event")) else {
                continue;
            };
            let sample = match event {
                "window" => {
                    field(line, "window_ms").map(|v| ("launch to window, ms".to_string(), v))
                }
                "start" => {
                    field(line, "startup_ms").map(|v| ("launch to child, ms".to_string(), v))
                }
                "run" if field(line, "ok") == Some("true") => field(line, "file")
                    .zip(field(line, "mbps"))
                    .map(|(f, v)| (format!("{f}, MB/s"), v)),
                "memory" => field(line, "when")
                    .zip(field(line, "footprint_mb"))
                    .map(|(when, v)| (format!("memory {when}, MB"), v)),
                _ => None,
            };
            // What the terminal's processes used: several figures to a line.
            let mut samples: Vec<(String, &str)> = sample.into_iter().collect();
            // A key press: its time counts toward the middle one and the mean.
            if event == "key"
                && let Some(v) = field(line, "ms") {
                    samples.push(("key press to screen, ms".to_string(), v));
                    samples.push(("mean key press to screen, ms".to_string(), v));
                }
            if event == "usage" {
                let figures: &[(&str, &str)] = match field(line, "when") {
                    Some("output") => &[
                        ("cpu_s", "processor time for the output, s"),
                        ("energy_j", "energy for the output, J"),
                    ],
                    Some("rest") => &[
                        ("cpu_ms_per_s", "processor time at rest, ms/s"),
                        ("power_mw", "power at rest, mW"),
                        ("wakeups_per_s", "wake-ups at rest, /s"),
                    ],
                    Some("rest hidden") => &[
                        ("cpu_ms_per_s", "at rest, cursor hidden, ms/s"),
                        ("power_mw", "at rest, cursor hidden, mW"),
                        ("wakeups_per_s", "at rest, cursor hidden, wake-ups/s"),
                    ],
                    _ => &[],
                };
                for (key, metric) in figures {
                    if let Some(v) = field(line, key) {
                        samples.push((metric.to_string(), v));
                    }
                }
            }
            for (metric, value) in samples {
                let Ok(value) = value.parse::<f64>() else {
                    continue;
                };
                if !order.contains(&metric) {
                    order.push(metric.clone());
                }
                data.entry(label.to_string())
                    .or_default()
                    .entry(metric)
                    .or_default()
                    .push(value);
            }
        }
    }
    let labels: Vec<&String> = data.keys().collect();
    print!("{:<38}", "median");
    for l in &labels {
        print!("{l:>14}");
    }
    println!();
    for metric in &order {
        print!("{metric:<38}");
        for l in &labels {
            // The middle value, but for the figures that say they are a mean.
            let cell = data[*l].get(metric).cloned().and_then(|mut v| {
                let middle = match metric.starts_with("mean ") {
                    true if !v.is_empty() => Some(v.iter().sum::<f64>() / v.len() as f64),
                    _ => median(&mut v),
                };
                middle.map(|m| (m, v.len()))
            });
            match cell {
                Some((m, n)) if m.abs() < 10.0 => print!("{:>14}", format!("{m:.2} (n={n})")),
                Some((m, n)) => print!("{:>14}", format!("{m:.1} (n={n})")),
                None => print!("{:>14}", "-"),
            }
        }
        println!();
    }
    Ok(())
}

// ---- vtebench: the samples another benchmark wrote ----

/// The mean of each column of a vtebench .dat file: a line of benchmark
/// names, then one line per sample in milliseconds. A benchmark that ran
/// fewer samples than the others leaves a placeholder, which is skipped.
fn dat_means(text: &str) -> Vec<(String, f64)> {
    let mut lines = text.lines();
    let names: Vec<&str> = lines.next().unwrap_or("").split_whitespace().collect();
    let mut sums = vec![(0.0f64, 0usize); names.len()];
    for line in lines {
        for (cell, sum) in line.split_whitespace().zip(sums.iter_mut()) {
            if let Ok(ms) = cell.parse::<f64>() {
                sum.0 += ms;
                sum.1 += 1;
            }
        }
    }
    names
        .iter()
        .zip(sums)
        .filter(|(_, (_, n))| *n > 0)
        .map(|(name, (total, n))| (name.to_string(), total / n as f64))
        .collect()
}

fn vtebench(args: impl Iterator<Item = String>) -> Result<(), String> {
    let mut data: BTreeMap<String, BTreeMap<String, f64>> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    for path in args {
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
        let stem = Path::new(&path)
            .file_stem()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let label = stem.strip_prefix("vtebench-").unwrap_or(&stem).to_string();
        for (name, mean) in dat_means(&text) {
            if !order.contains(&name) {
                order.push(name.clone());
            }
            data.entry(label.clone()).or_default().insert(name, mean);
        }
    }
    let labels: Vec<&String> = data.keys().collect();
    print!("{:<32}", "mean ms");
    for l in &labels {
        print!("{l:>11}");
    }
    println!();
    for name in &order {
        print!("{name:<32}");
        for l in &labels {
            match data[*l].get(name) {
                Some(mean) => print!("{mean:>11.2}"),
                None => print!("{:>11}", "-"),
            }
        }
        println!();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_fields_from_its_own_lines() {
        let line = r#"{"label":"tern","event":"run","file":"ascii.txt","iter":0,"bytes":10,"secs":0.5,"mbps":20.0,"ok":true}"#;
        assert_eq!(field(line, "label"), Some("tern"));
        assert_eq!(field(line, "mbps"), Some("20.0"));
        assert_eq!(field(line, "ok"), Some("true"));
        assert_eq!(field(line, "missing"), None);
    }

    #[test]
    fn median_of_odd_and_even_counts() {
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&mut [4.0, 1.0, 2.0, 3.0]), Some(2.5));
        assert_eq!(median(&mut []), None);
    }

    #[test]
    fn dat_means_skip_missing_samples() {
        let means = dat_means("a b\n2 10\n4 _\n");
        assert_eq!(means, vec![("a".to_string(), 3.0), ("b".to_string(), 10.0)]);
    }

    #[test]
    fn the_generator_is_deterministic() {
        let (mut a, mut b) = (Rng(7), Rng(7));
        assert!((0..100).all(|_| a.next() == b.next()));
    }
}
