//! Process-wide resources: descriptors, threads and CPU time. Alone in its
//! own test binary because it measures the whole process; the tests take a
//! lock so they do not run at the same time.

mod common;

use std::sync::Mutex;
use std::time::{Duration, Instant};

use common::{config, sh, spawn};

static SERIAL: Mutex<()> = Mutex::new(());

fn open_fds() -> usize {
    std::fs::read_dir("/dev/fd").expect("/dev/fd").count()
}

fn thread_count() -> i32 {
    // SAFETY: proc_taskinfo is plain integers, valid when zeroed.
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
    // SAFETY: proc_pidinfo writes at most size bytes into info.
    let n = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDTASKINFO,
            0,
            (&mut info as *mut libc::proc_taskinfo).cast(),
            size,
        )
    };
    assert_eq!(n, size, "proc_pidinfo");
    info.pti_threadnum
}

fn cpu_time() -> Duration {
    // SAFETY: rusage is a plain C struct, valid when zeroed.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: getrusage writes into the local struct.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let tv = |t: libc::timeval| Duration::new(t.tv_sec as u64, t.tv_usec as u32 * 1000);
    tv(usage.ru_utime) + tv(usage.ru_stime)
}

fn wait_for(what: &str, pred: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !pred() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn sessions_leave_no_descriptors_or_threads_behind() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    // The first session of a process starts the thread that watches for
    // children's exits, with its kernel queue, and both are there for good.
    // They are not any session's, so one session comes and goes first.
    let first = sh("exit 0");
    assert_eq!(first.wait_exit(), Some(0));
    drop(first);
    // Threads of sessions dropped earlier may still be finishing.
    let mut threads = thread_count();
    loop {
        std::thread::sleep(Duration::from_millis(200));
        let now = thread_count();
        if now == threads {
            break;
        }
        threads = now;
    }
    let fds = open_fds();

    let exited = sh("echo done; exit 2");
    assert_eq!(exited.wait_exit(), Some(2));
    let running = sh("sleep 100");
    let flooding = sh("yes");
    flooding.wait_for("y\ny");
    let detached = sh("printf ready; exec 0<&- 1>&- 2>&-; sleep 100");
    detached.wait_for("ready");
    let stuck = spawn(config(&["sleep", "100"]));
    stuck.session.write(&vec![b'x'; 1 << 20]);
    let during = thread_count();
    assert!(
        during > threads + 8,
        "{threads} threads before, {during} with sessions"
    );

    drop((exited, running, flooding, detached, stuck));
    wait_for("descriptors to close", || open_fds() <= fds);
    wait_for("threads to finish", || thread_count() <= threads);
}

#[test]
fn idle_and_blocked_sessions_use_no_cpu() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    // One child that never reads its input, so the writer thread waits on a
    // full input queue, and one that is simply idle.
    let stuck = spawn(config(&["sleep", "100"]));
    stuck.session.write(&vec![b'x'; 1 << 20]);
    let idle = sh("printf ready; sleep 100");
    idle.wait_for("ready");
    std::thread::sleep(Duration::from_millis(300));

    let before = cpu_time();
    std::thread::sleep(Duration::from_secs(1));
    let used = cpu_time() - before;
    assert!(
        used < Duration::from_millis(100),
        "used {used:?} of CPU in 1 s"
    );
    assert!(stuck.session.is_alive() && idle.session.is_alive());
}
