//! Measures how fast a session swallows a flood of output while a UI thread
//! takes snapshots, at most 120 a second, whenever it is woken.
//!
//! cargo run --release -p tern-core --example throughput -- <file> [cols] [rows]
//!
//! With TERN_WAY=asleep-handed, asleep-own, awake-handed or awake-own the
//! reader deals with output that one way only.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tern_core::{Session, SessionConfig, SessionEvents};
use tern_types::Frame;

#[derive(Default)]
struct Ui {
    state: Mutex<UiState>,
    changed: Condvar,
}

#[derive(Default)]
struct UiState {
    woken: bool,
    exited: bool,
}

impl Ui {
    fn update(&self, f: impl FnOnce(&mut UiState)) {
        f(&mut self.state.lock().unwrap_or_else(|p| p.into_inner()));
        self.changed.notify_all();
    }

    /// Waits for a wakeup or the exit; returns true when the child exited.
    fn wait(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        while !state.woken && !state.exited {
            state = self.changed.wait(state).unwrap_or_else(|p| p.into_inner());
        }
        state.woken = false;
        state.exited
    }
}

impl SessionEvents for Ui {
    fn wakeup(&self) {
        self.update(|s| s.woken = true);
    }
    fn title(&self, title: Option<String>) {
        // With TERN_REST the child stays, and says when its output is done.
        if title.as_deref() == Some("done") {
            self.update(|s| s.exited = true);
        }
    }
    fn bell(&self) {}
    fn exited(&self, _: Option<i32>) {
        self.update(|s| s.exited = true);
    }
    fn clipboard_store(&self, _: String) {}
}

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: throughput <file>");
        std::process::exit(2);
    };
    let bytes = match std::fs::metadata(&path) {
        Ok(m) => m.len(),
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        }
    };

    // TERN_WAY (asleep|awake-handed|own) holds the reader to one way of
    // dealing with output, to measure each.
    if let Ok(way) = std::env::var("TERN_WAY") {
        let (awake, own) = way.split_once('-').expect("TERN_WAY: asleep|awake, -, handed|own");
        tern_core::force_reader_way(Some((awake == "awake", own == "own")));
    }
    let ui = Arc::new(Ui::default());
    let mut size = std::env::args().skip(2).map(|a| a.parse().expect("a number"));
    let cols = size.next().unwrap_or(200);
    let rows = size.next().unwrap_or(60);
    let config = SessionConfig {
        cols,
        rows,
        command: Some(if std::env::var_os("TERN_REST").is_some() {
            let script = "cat \"$0\"; printf '\\033]2;done\\007'; sleep 600";
            vec!["sh".into(), "-c".into(), script.into(), path.clone()]
        } else {
            vec!["cat".into(), path.clone()]
        }),
        ..SessionConfig::default()
    };
    let start = Instant::now();
    let session = match Session::spawn(config, ui.clone()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("spawn: {e}");
            std::process::exit(1);
        }
    };

    let frame_interval = Duration::from_secs(1) / 120;
    let mut frame = Frame::default();
    let mut times = Vec::new();
    let mut last_frame = Instant::now() - frame_interval;
    loop {
        let exited = ui.wait();
        let since = last_frame.elapsed();
        if !exited && since < frame_interval {
            std::thread::sleep(frame_interval - since);
        }
        last_frame = Instant::now();
        session.snapshot(&mut frame);
        times.push(last_frame.elapsed());
        if exited {
            break;
        }
    }
    let seconds = start.elapsed().as_secs_f64();

    times.sort();
    let p99 = times[(times.len() * 99 / 100).min(times.len() - 1)];
    let worst = times[times.len() - 1];
    println!(
        "{path}: {bytes} bytes in {seconds:.3} s = {:.1} MB/s; {} snapshots, p99 {:.3} ms, worst {:.3} ms",
        bytes as f64 / seconds / 1e6,
        times.len(),
        p99.as_secs_f64() * 1e3,
        worst.as_secs_f64() * 1e3,
    );

    // TERN_REST=seconds: what the session holds once it has been left
    // alone that long after the output.
    if let Some(rest) = std::env::var("TERN_REST").ok().and_then(|s| s.parse::<f64>().ok()) {
        let footprint = || {
            // SAFETY: rusage_info_v4 is plain integers, filled in by the call.
            let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
            let got = unsafe {
                libc::proc_pid_rusage(libc::getpid(), libc::RUSAGE_INFO_V4, (&mut info as *mut libc::rusage_info_v4).cast())
            };
            if got == 0 { info.ri_phys_footprint as f64 / 1e6 } else { f64::NAN }
        };
        println!("  right after: {:.1} MB, scrollback {:?}", footprint(), session.scrollback_stats());
        std::thread::sleep(Duration::from_secs_f64(rest));
        println!("  at rest: {:.1} MB, scrollback {:?}", footprint(), session.scrollback_stats());
    }

    // The cost of rewriting every cell: two frames taken in turn are each
    // out of step with the session, so each snapshot fills all 200x60 cells.
    // Then the cost of a snapshot when nothing changed.
    let mut frames = [Frame::default(), Frame::default()];
    let mut full = Vec::new();
    let mut idle = Vec::new();
    for i in 0..200 {
        let t = Instant::now();
        session.snapshot(&mut frames[i % 2]);
        full.push(t.elapsed());
    }
    for _ in 0..200 {
        let t = Instant::now();
        session.snapshot(&mut frames[0]);
        idle.push(t.elapsed());
    }
    let micros = |mut v: Vec<Duration>| {
        v.sort();
        (
            v[v.len() / 2].as_secs_f64() * 1e6,
            v[v.len() - 1].as_secs_f64() * 1e6,
        )
    };
    let (full_median, full_worst) = micros(full);
    let (idle_median, idle_worst) = micros(idle);
    println!(
        "  full 200x60 rewrite: median {full_median:.1} us, worst {full_worst:.1} us; \
         idle snapshot: median {idle_median:.1} us, worst {idle_worst:.1} us"
    );
}
