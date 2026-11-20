//! How long a key press takes to reach the screen: from the key event to
//! the presented frame that first shows what the program did with it.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use objc2_foundation::NSProcessInfo;
use tern_types::PaneId;

use super::WindowState;

/// Key latencies kept for the automation socket.
const KEPT: usize = 512;
/// A draw that never reported its presentation is given up on after this
/// many more draws, so measuring goes on.
const GIVE_UP_DRAWS: u64 = 30;
/// Seconds a key's change may take to show and still be timed. A key that
/// changed nothing on screen would otherwise be charged with whatever
/// shows next, however much later.
const KEY_WAIT_LIMIT: f64 = 1.0;

/// Shared between the main thread, which knows which draw first shows a
/// key, and the presented handler, which knows when that draw reached the
/// screen.
#[derive(Default)]
pub struct LatencyProbe {
    /// The draw that first shows the key; 0 when none is awaited.
    draw: AtomicU64,
    /// When the key was pressed, as f64 bits, on the clock of presented
    /// times (seconds since boot).
    pressed: AtomicU64,
    /// The awaited draw was dropped by the compositor; the window draws it
    /// again, and that draw is timed instead.
    redrawn: AtomicBool,
    samples: Mutex<Samples>,
}

struct Samples {
    ring: [Duration; KEPT],
    next: usize,
    len: usize,
}

impl Default for Samples {
    fn default() -> Samples {
        Samples {
            ring: [Duration::ZERO; KEPT],
            next: 0,
            len: 0,
        }
    }
}

/// The median and the worst of the latencies kept, and how many there are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LatencySummary {
    pub count: usize,
    pub median: Duration,
    pub max: Duration,
}

impl LatencyProbe {
    /// Metal thread: draw number frame reached the screen at presented.
    pub fn presented(&self, frame: u64, presented: f64) {
        if self.draw.load(Ordering::Acquire) != frame {
            return;
        }
        let pressed = f64::from_bits(self.pressed.load(Ordering::Acquire));
        self.draw.store(0, Ordering::Release);
        if presented <= 0.0 {
            self.redrawn.store(true, Ordering::Release);
            return;
        }
        let took = presented - pressed;
        if took.is_finite() && took >= 0.0 {
            let mut s = self.samples.lock().unwrap_or_else(PoisonError::into_inner);
            let next = s.next;
            s.ring[next] = Duration::from_secs_f64(took);
            s.next = (next + 1) % KEPT;
            s.len = (s.len + 1).min(KEPT);
        }
    }

    pub fn summary(&self) -> LatencySummary {
        let s = self.samples.lock().unwrap_or_else(PoisonError::into_inner);
        let mut v: Vec<Duration> = s.ring[..s.len].to_vec();
        drop(s);
        v.sort_unstable();
        LatencySummary {
            count: v.len(),
            median: v.get(v.len() / 2).copied().unwrap_or_default(),
            max: v.last().copied().unwrap_or_default(),
        }
    }

    pub fn clear(&self) {
        let mut s = self.samples.lock().unwrap_or_else(PoisonError::into_inner);
        s.len = 0;
        s.next = 0;
    }
}

/// What a drawn frame means for a key waiting to show its change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyWait {
    /// The pane changed since the key: this draw shows it.
    Shown,
    Waiting,
    /// Not measured: the pane closed, or the key has waited too long.
    GivenUp,
}

/// The key went at sent (seconds since boot) to a pane whose frame seq was
/// seq then; now is the time of the draw and now_seq the pane's seq, None
/// when the pane has gone.
fn key_wait(sent: f64, seq: u64, now: f64, now_seq: Option<u64>) -> KeyWait {
    match now_seq {
        None => KeyWait::GivenUp,
        Some(_) if now - sent > KEY_WAIT_LIMIT => KeyWait::GivenUp,
        Some(now_seq) if now_seq > seq => KeyWait::Shown,
        Some(_) => KeyWait::Waiting,
    }
}

impl WindowState {
    /// A key press went to the focused pane at time (seconds since boot,
    /// the key event's timestamp). Measured unless an earlier key is still
    /// on its way to the screen.
    pub(crate) fn key_sent(&self, time: f64) {
        if self.key_wait.get().is_some() || self.latency.draw.load(Ordering::Acquire) != 0 {
            return;
        }
        if let Some((id, seq)) = self.with_focused(|p| (p.id, p.frame.seq)) {
            self.key_wait.set(Some((time, id, seq)));
        }
    }

    /// Called with each draw that went out: when it is the first to show a
    /// change in the pane a key went to, the presented handler times it.
    pub(super) fn key_drawn(&self, draw: u64, seq_of: impl Fn(PaneId) -> Option<u64>) {
        let awaited = self.latency.draw.load(Ordering::Acquire);
        if awaited != 0 && draw > awaited + GIVE_UP_DRAWS {
            self.latency.draw.store(0, Ordering::Release);
        }
        if self.latency.redrawn.swap(false, Ordering::AcqRel) {
            // The key's frame was dropped; this draw shows it instead.
            self.latency.draw.store(draw, Ordering::Release);
            return;
        }
        let Some((time, pane, seq)) = self.key_wait.get() else {
            return;
        };
        let now = NSProcessInfo::processInfo().systemUptime();
        match key_wait(time, seq, now, seq_of(pane)) {
            KeyWait::Shown => {
                self.key_wait.set(None);
                self.latency
                    .pressed
                    .store(time.to_bits(), Ordering::Release);
                self.latency.draw.store(draw, Ordering::Release);
            }
            KeyWait::Waiting => {}
            KeyWait::GivenUp => self.key_wait.set(None),
        }
    }

    /// Bytes went to a pane as if typed. A key still waiting to show its
    /// change is not timed: the next change may be this input's doing.
    pub(super) fn typed(&self) {
        self.key_wait.set(None);
    }

    /// Whether a key is waiting to show its change.
    pub(crate) fn key_waiting(&self) -> bool {
        self.key_wait.get().is_some()
    }

    pub fn key_latency(&self) -> LatencySummary {
        self.latency.summary()
    }

    pub fn clear_key_latency(&self) {
        self.latency.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_timed_only_to_a_change_soon_after_it() {
        assert_eq!(key_wait(10.0, 5, 10.01, Some(6)), KeyWait::Shown);
        assert_eq!(key_wait(10.0, 5, 10.01, Some(5)), KeyWait::Waiting);
        assert_eq!(key_wait(10.0, 5, 10.01, None), KeyWait::GivenUp);
        // A change long after the key is not the key's.
        assert_eq!(key_wait(10.0, 5, 11.5, Some(6)), KeyWait::GivenUp);
        assert_eq!(key_wait(10.0, 5, 11.5, Some(5)), KeyWait::GivenUp);
    }

    #[test]
    fn only_the_awaited_draw_is_timed() {
        let probe = LatencyProbe::default();
        probe.pressed.store(10.0f64.to_bits(), Ordering::Release);
        probe.draw.store(5, Ordering::Release);
        probe.presented(4, 10.5);
        assert_eq!(probe.summary().count, 0);
        probe.presented(5, 10.012);
        let s = probe.summary();
        assert_eq!(s.count, 1);
        assert_eq!(s.median.as_micros(), 12_000);
        // Measured once.
        probe.presented(5, 11.0);
        assert_eq!(probe.summary().count, 1);
        // A dropped frame hands the key on to the redraw.
        probe.draw.store(6, Ordering::Release);
        probe.presented(6, 0.0);
        assert_eq!(probe.summary().count, 1);
        assert!(probe.redrawn.load(Ordering::Acquire));
        probe.clear();
        assert_eq!(probe.summary(), LatencySummary::default());
    }
}
