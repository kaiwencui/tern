//! One terminal session: the pseudo-terminal, the child process, the emulator
//! state and the threads that feed it.
//!
//! The app holds an `Arc<Session>` per pane, feeds it input and resizes, and
//! asks it for a [tern_types::Frame] snapshot once per rendered frame. The
//! emulator behind it (tern-vt) never leaks through this API.

mod blink;
#[cfg(test)]
mod differential;
mod io;
mod modes;
mod pty;
mod session;
mod snapshot;
mod sync;
mod watch;

pub use session::{
    Progress, Scroll, SearchMatch, SelectionKind, Session, SessionConfig, SessionEvents, Side,
};

/// For this crate's tests: has every reader deal with output one way,
/// instead of choosing by what moves output fastest, so the tests can run on
/// each of the four. `awake`: it never sleeps between reads. `own`: it
/// interprets what it reads itself. None lets readers choose again.
#[doc(hidden)]
pub fn force_reader_way(way: Option<(bool, bool)>) {
    let place = way.map_or(0, |(awake, own)| 1 + usize::from(awake) + 2 * usize::from(own));
    io::FORCED_WAY.store(place, std::sync::atomic::Ordering::Relaxed);
}
