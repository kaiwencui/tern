//! Wakes the reader of a session whose child has exited.
//!
//! A reader that sleeps inside read() (see [crate::pty::frame_reads]) hears
//! of output and of the terminal closing, but not of the child's exit while
//! something else still holds the terminal open, a background job for
//! instance. One thread for the whole process watches every such child and
//! gives the reader a wake-up call; the reader then checks for itself.

use std::collections::HashMap;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use libc::pid_t;

use crate::sync::lock;
use crate::pty;

type Masters = Arc<Mutex<HashMap<pid_t, Weak<OwnedFd>>>>;

struct Watch {
    kq: OwnedFd,
    /// The master of each watched child's terminal. Weak, so watching never
    /// keeps a terminal from hanging up.
    masters: Masters,
}

/// None when the queue or the thread could not be made; sessions then do
/// without framed reads.
fn watch() -> Option<&'static Watch> {
    static WATCH: OnceLock<Option<Watch>> = OnceLock::new();
    WATCH.get_or_init(|| start().ok()).as_ref()
}

fn start() -> io::Result<Watch> {
    // SAFETY: kqueue has no preconditions.
    let kq = unsafe { libc::kqueue() };
    if kq < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: kqueue returned a fresh descriptor we now own. A queue is
    // not passed on to a forked child, so it needs no close-on-exec.
    let kq = unsafe { OwnedFd::from_raw_fd(kq) };
    let masters = Masters::default();
    let (queue, seen) = (kq.as_raw_fd(), masters.clone());
    std::thread::Builder::new()
        .name("tern-exit-watch".to_owned())
        .spawn(move || {
            // SAFETY: sets the calling thread's own class. An exit is the
            // user's news as much as output is.
            unsafe {
                libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INITIATED, 0)
            };
            run(queue, &seen)
        })?;
    Ok(Watch { kq, masters })
}

/// The queue lives as long as the process, in the static above.
fn run(kq: i32, masters: &Masters) {
    loop {
        // SAFETY: kevent is a plain C struct, valid when zeroed.
        let mut events: [libc::kevent; 8] = unsafe { std::mem::zeroed() };
        // SAFETY: the kernel writes at most events.len() entries.
        let n = unsafe {
            libc::kevent(
                kq,
                std::ptr::null(),
                0,
                events.as_mut_ptr(),
                events.len() as i32,
                std::ptr::null(),
            )
        };
        if n < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        for event in &events[..n as usize] {
            // The entry stays until the reader forgets it: a process id can
            // come around again, and this event may be the earlier child's.
            let master = lock(masters)
                .get(&(event.ident as pid_t))
                .and_then(Weak::upgrade);
            if let Some(master) = master {
                pty::wake_reader(&master);
            }
        }
    }
}

/// Whether children can be watched at all.
pub(crate) fn available() -> bool {
    watch().is_some()
}

/// Has the reader on `master` woken when the child exits. False when the
/// child is already gone, and no call will come.
pub(crate) fn exit(pid: pid_t, master: &Arc<OwnedFd>) -> bool {
    let Some(watch) = watch() else { return false };
    lock(&watch.masters).insert(pid, Arc::downgrade(master));
    let change = libc::kevent {
        ident: pid as usize,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_ONESHOT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    // SAFETY: one change from a local struct, no events requested.
    let rc = unsafe {
        libc::kevent(
            watch.kq.as_raw_fd(),
            &change,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    if rc < 0 {
        lock(&watch.masters).remove(&pid);
        return false;
    }
    true
}

/// Ends the watch on a child; its reader has finished.
pub(crate) fn forget(pid: pid_t) {
    if let Some(watch) = watch() {
        lock(&watch.masters).remove(&pid);
    }
}
