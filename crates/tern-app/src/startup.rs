//! --log-startup: how long the process took to show the terminal, and where
//! the time went.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use objc2_quartz_core::CACurrentMediaTime;

/// Collects timestamps from any thread and prints them once the first frame
/// with terminal content is on screen.
pub struct StartupProbe {
    /// How long the process had been running when the probe was made, and
    /// when that was; marks are measured from process start through these.
    before_main: Duration,
    created: Instant,
    process_start: SystemTime,
    marks: Mutex<Vec<(String, Duration)>>,
    first_frame: AtomicBool,
    /// The number of the first drawn frame (counting from 1) that showed
    /// terminal content; 0 until there is one.
    content_frame: AtomicU64,
    content_shown: AtomicBool,
}

impl StartupProbe {
    pub fn new(process_start: SystemTime) -> StartupProbe {
        let before_main = SystemTime::now()
            .duration_since(process_start)
            .unwrap_or_default();
        let probe = StartupProbe {
            before_main,
            created: Instant::now(),
            process_start,
            marks: Mutex::new(Vec::new()),
            first_frame: AtomicBool::new(false),
            content_frame: AtomicU64::new(0),
            content_shown: AtomicBool::new(false),
        };
        probe.mark("main entered (dynamic loader and runtime)");
        probe
    }

    /// Time since process start.
    fn now(&self) -> Duration {
        self.before_main + self.created.elapsed()
    }

    pub fn mark(&self, what: &str) {
        let at = self.now();
        self.push(what.to_string(), at);
    }

    /// Records a span that ran on its own, ending now.
    pub fn mark_span(&self, what: &str, took: Duration) {
        let at = self.now();
        self.push(format!("{what} (took {:.1} ms)", ms(took)), at);
    }

    fn push(&self, what: String, at: Duration) {
        if let Ok(mut marks) = self.marks.lock() {
            marks.push((what, at));
        }
    }

    /// Called on the main thread when the window's frame number frame (its
    /// draws counted from 1) showed terminal content.
    pub fn content_drawn(&self, frame: u64) {
        if self
            .content_frame
            .compare_exchange(0, frame, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            self.mark("first frame with terminal content drawn");
        }
    }

    pub fn wants_content(&self) -> bool {
        self.content_frame.load(Ordering::Relaxed) == 0
    }

    /// Called on a Metal thread when the window's frame number frame was
    /// presented, at a time on the media clock. Metal presents a layer's
    /// drawables in the order they were drawn, so a frame numbered at or
    /// after the content frame shows content. Returns true when this is the
    /// presentation that showed content first, and the times were printed.
    pub fn presented(&self, frame: u64, media_time: f64) -> bool {
        // The presentation time is on the media clock; carry it over to the
        // wall clock the process start time is on.
        let ago = Duration::from_secs_f64((CACurrentMediaTime() - media_time).max(0.0));
        let at = (SystemTime::now() - ago)
            .duration_since(self.process_start)
            .unwrap_or_default();
        if !self.first_frame.swap(true, Ordering::Relaxed) {
            eprintln!(
                "tern: first frame presented {:.1} ms after process start",
                ms(at)
            );
        }
        let content = self.content_frame.load(Ordering::Relaxed);
        if content != 0 && frame >= content && !self.content_shown.swap(true, Ordering::Relaxed) {
            eprintln!(
                "tern: first frame with terminal content presented {:.1} ms after process start",
                ms(at)
            );
            if let Ok(marks) = self.marks.lock() {
                for (what, at) in marks.iter() {
                    eprintln!("tern:   {:7.1} ms  {what}", ms(*at));
                }
            }
            return true;
        }
        false
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_frame_drawn_with_content_counts_as_showing_it() {
        let probe = StartupProbe::new(SystemTime::now());
        let now = CACurrentMediaTime();
        // An empty frame drawn first, then the frame with content.
        probe.content_drawn(2);
        assert!(!probe.wants_content());
        assert!(!probe.presented(1, now), "the empty frame shows no content");
        assert!(probe.presented(2, now));
        assert!(!probe.presented(3, now), "reported once");
        // A dropped content frame: the next one shown has content too.
        let probe = StartupProbe::new(SystemTime::now());
        probe.content_drawn(1);
        assert!(probe.presented(2, now));
    }
}
