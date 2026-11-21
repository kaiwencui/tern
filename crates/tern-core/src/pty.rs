//! The pseudo-terminal and the child process: opening the PTY, preparing and
//! starting the child, reaping it, and asking the kernel about the processes
//! running on the terminal.

use std::ffi::{CString, OsStr, OsString};
use std::io;
use std::mem::{self, MaybeUninit};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::{Mutex, PoisonError};

use libc::{c_char, c_int, pid_t};

/// Exact names of variables other terminals leave behind that would mislead
/// programs running in this one.
const FOREIGN_VARS: &[&[u8]] = &[
    b"TERMINFO",
    b"TERMINFO_DIRS",
    b"TERM_SESSION_ID",
    b"TMUX",
    b"TMUX_PANE",
    b"LC_TERMINAL",
    b"LC_TERMINAL_VERSION",
    b"VTE_VERSION",
    // A stale size from the parent would override the PTY's real size in
    // programs that honor these.
    b"COLUMNS",
    b"LINES",
];

/// Prefixes of variables other terminals leave behind.
const FOREIGN_PREFIXES: &[&[u8]] = &[
    b"GHOSTTY_",
    b"KITTY_",
    b"ALACRITTY_",
    b"ITERM_",
    b"WEZTERM_",
];

const DEFAULT_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// Everything the child needs, built before fork so the child itself only
/// makes system calls.
pub(crate) struct Command {
    program: CString,
    argv: Vec<CString>,
    envp: Vec<CString>,
    dir: CString,
    home: CString,
    /// The child is the user's login shell rather than an explicit command.
    pub(crate) login_shell: bool,
}

impl Command {
    pub(crate) fn new(
        shell: Option<&str>,
        command: Option<&[String]>,
        working_dir: Option<&Path>,
        extra_env: &[(String, String)],
    ) -> io::Result<Command> {
        let user = passwd_entry();
        let mut env = build_env(std::env::vars_os(), extra_env);
        let home = user
            .home
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/"));

        let (program, argv, login_shell) = match command {
            Some([]) => {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty command"));
            }
            Some(argv) => {
                let program = find_program(&argv[0], path(&env))?;
                let argv = argv.iter().map(|a| OsString::from(a.as_str())).collect();
                (program, argv, false)
            }
            None => {
                let shell = shell
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from)
                    .or(user.shell)
                    .or_else(|| {
                        std::env::var_os("SHELL")
                            .filter(|s| !s.is_empty())
                            .map(PathBuf::from)
                    })
                    .unwrap_or_else(|| PathBuf::from("/bin/zsh"));
                // A bare name such as "fish" in the configuration is looked
                // up on PATH; execve would take it as a relative path.
                let shell = match shell.to_str() {
                    Some(name) if !name.contains('/') => find_program(name, path(&env))?,
                    _ => shell,
                };
                let argv0 = login_argv0(&shell);
                (shell, vec![argv0], true)
            }
        };

        let dir = working_dir
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.clone());
        // The app's own PWD would name the wrong directory.
        env.retain(|(k, _)| k != "PWD");
        env.push(("PWD".into(), dir.clone().into_os_string()));
        let envp = env
            .iter()
            .map(|(k, v)| {
                let mut kv = Vec::with_capacity(k.len() + v.len() + 1);
                kv.extend_from_slice(k.as_bytes());
                kv.push(b'=');
                kv.extend_from_slice(v.as_bytes());
                cstring(kv)
            })
            .collect::<io::Result<Vec<_>>>()?;
        Ok(Command {
            program: cstring(program.into_os_string().into_vec())?,
            argv: argv
                .into_iter()
                .map(|a| cstring(a.into_vec()))
                .collect::<io::Result<_>>()?,
            envp,
            dir: cstring(dir.into_os_string().into_vec())?,
            home: cstring(home.into_os_string().into_vec())?,
            login_shell,
        })
    }
}

fn cstring(bytes: Vec<u8>) -> io::Result<CString> {
    CString::new(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL byte in argument"))
}

/// A login shell is started the way login(1) would, by putting "-" in front of
/// its name in argv[0], without going through /usr/bin/login.
fn login_argv0(shell: &Path) -> OsString {
    let name = shell.file_name().unwrap_or(shell.as_os_str());
    let mut argv0 = OsString::from("-");
    argv0.push(name);
    argv0
}

/// The app's environment adjusted for a program running in this terminal.
fn build_env(
    inherited: impl IntoIterator<Item = (OsString, OsString)>,
    extra: &[(String, String)],
) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = inherited
        .into_iter()
        .filter(|(k, _)| !is_foreign(k.as_bytes()))
        .collect();
    let has_locale = env
        .iter()
        .any(|(k, v)| matches!(k.as_bytes(), b"LANG" | b"LC_ALL" | b"LC_CTYPE") && !v.is_empty());
    set_var(&mut env, "TERM", "xterm-256color");
    set_var(&mut env, "COLORTERM", "truecolor");
    set_var(&mut env, "TERM_PROGRAM", "Tern");
    set_var(&mut env, "TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
    if !has_locale {
        set_var(&mut env, "LANG", "en_US.UTF-8");
    }
    for (k, v) in extra {
        // A name with '=' or NUL cannot be expressed in envp.
        if !k.is_empty() && !k.contains(['=', '\0']) {
            set_var(&mut env, k, v);
        }
    }
    env
}

fn is_foreign(key: &[u8]) -> bool {
    FOREIGN_VARS.contains(&key) || FOREIGN_PREFIXES.iter().any(|p| key.starts_with(p))
}

fn set_var(env: &mut Vec<(OsString, OsString)>, key: &str, value: &str) {
    match env.iter_mut().find(|(k, _)| k == key) {
        Some((_, v)) => *v = value.into(),
        None => env.push((key.into(), value.into())),
    }
}

/// The PATH the child will see.
fn path(env: &[(OsString, OsString)]) -> &OsStr {
    env_value(env, b"PATH").unwrap_or(OsStr::new(DEFAULT_PATH))
}

fn env_value<'a>(env: &'a [(OsString, OsString)], key: &[u8]) -> Option<&'a OsStr> {
    env.iter()
        .find(|(k, _)| k.as_bytes() == key)
        .map(|(_, v)| v.as_os_str())
}

/// Resolves a program name through PATH before fork, because execvp is not
/// safe to call in the child of a multi-threaded process.
fn find_program(name: &str, path: &OsStr) -> io::Result<PathBuf> {
    if name.contains('/') {
        return Ok(PathBuf::from(name));
    }
    std::env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("{name}: command not found"),
            )
        })
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

struct UserInfo {
    shell: Option<PathBuf>,
    home: Option<PathBuf>,
}

/// The current user's shell and home directory from the password database.
fn passwd_entry() -> UserInfo {
    let mut buf = vec![0 as c_char; 4096];
    loop {
        let mut pwd = MaybeUninit::<libc::passwd>::uninit();
        let mut result: *mut libc::passwd = ptr::null_mut();
        // SAFETY: every pointer refers to live storage of the stated size.
        let rc = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                pwd.as_mut_ptr(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut result,
            )
        };
        if rc == libc::ERANGE && buf.len() < 1 << 20 {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc != 0 || result.is_null() {
            return UserInfo {
                shell: None,
                home: None,
            };
        }
        // SAFETY: getpwuid_r succeeded, so pwd is initialized and its strings
        // point into buf, which outlives this block.
        let pwd = unsafe { pwd.assume_init() };
        let field = |p: *const c_char| {
            // SAFETY: a non-null passwd field is a NUL-terminated string in buf.
            (!p.is_null())
                .then(|| unsafe { std::ffi::CStr::from_ptr(p) })
                .map(|s| PathBuf::from(OsStr::from_bytes(s.to_bytes())))
                .filter(|p| !p.as_os_str().is_empty())
        };
        return UserInfo {
            shell: field(pwd.pw_shell),
            home: field(pwd.pw_dir),
        };
    }
}

pub(crate) fn winsize(cols: u16, rows: u16, cell_width: u16, cell_height: u16) -> libc::winsize {
    libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: cols.saturating_mul(cell_width),
        ws_ypixel: rows.saturating_mul(cell_height),
    }
}

/// A started child and the master side of its terminal.
pub(crate) struct Child {
    pub(crate) master: OwnedFd,
    pub(crate) pid: pid_t,
}

/// Opens a PTY and starts the command on it. Returns once the child has
/// successfully called execve, or with the reason it could not.
pub(crate) fn spawn(cmd: &Command, size: libc::winsize) -> io::Result<Child> {
    let (master, slave) = open_pty(size)?;
    let (err_read, err_write) = pipe()?;

    // Everything the child touches is prepared here, since it must not
    // allocate or take locks between fork and exec.
    let argv: Vec<*const c_char> = cmd
        .argv
        .iter()
        .map(|a| a.as_ptr())
        .chain(std::iter::once(ptr::null()))
        .collect();
    let envp: Vec<*const c_char> = cmd
        .envp
        .iter()
        .map(|a| a.as_ptr())
        .chain(std::iter::once(ptr::null()))
        .collect();
    let fd_capacity = open_fd_count().saturating_mul(2).max(64);
    // SAFETY: proc_fdinfo is plain integers, valid when zeroed.
    let mut fd_list: Vec<libc::proc_fdinfo> = vec![unsafe { mem::zeroed() }; fd_capacity];
    let fd_limit = open_fd_limit();
    // SAFETY: sigset_t and sigaction are plain C structs, valid when zeroed.
    let (mut empty_set, mut default_action): (libc::sigset_t, libc::sigaction) =
        unsafe { (mem::zeroed(), mem::zeroed()) };
    // SAFETY: both point at owned, writable structs.
    unsafe { libc::sigemptyset(&mut empty_set) };
    default_action.sa_sigaction = libc::SIG_DFL;
    default_action.sa_mask = empty_set;

    let ctx = ChildContext {
        slave: slave.as_raw_fd(),
        err_pipe: err_write.as_raw_fd(),
        program: cmd.program.as_ptr(),
        argv: argv.as_ptr(),
        envp: envp.as_ptr(),
        dir: cmd.dir.as_ptr(),
        home: cmd.home.as_ptr(),
        fd_list: fd_list.as_mut_ptr(),
        fd_list_bytes: (fd_list.len() * mem::size_of::<libc::proc_fdinfo>()) as c_int,
        fd_limit,
        empty_set: &empty_set,
        default_action: &default_action,
    };

    // SAFETY: fork has no preconditions; the child branch below only calls
    // async-signal-safe functions on memory prepared above.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if pid == 0 {
        // SAFETY: we are the freshly forked child; ctx points at memory the
        // fork copied, and exec_child never returns.
        unsafe { exec_child(&ctx) }
    }

    drop(slave);
    drop(err_write);
    match read_exec_error(&err_read) {
        None => Ok(Child { master, pid }),
        Some(errno) => {
            reap(pid);
            Err(io::Error::from_raw_os_error(errno))
        }
    }
}

struct ChildContext {
    slave: RawFd,
    err_pipe: RawFd,
    program: *const c_char,
    argv: *const *const c_char,
    envp: *const *const c_char,
    dir: *const c_char,
    home: *const c_char,
    fd_list: *mut libc::proc_fdinfo,
    fd_list_bytes: c_int,
    fd_limit: c_int,
    empty_set: *const libc::sigset_t,
    default_action: *const libc::sigaction,
}

/// The child between fork and exec. Only async-signal-safe calls are allowed
/// here: no allocation, no locks, no Rust std I/O.
unsafe fn exec_child(ctx: &ChildContext) -> ! {
    unsafe {
        // Undo whatever the app did to signals; a blocked or ignored signal
        // would otherwise be inherited by the shell.
        libc::sigprocmask(libc::SIG_SETMASK, ctx.empty_set, ptr::null_mut());
        for sig in 1..32 {
            libc::sigaction(sig, ctx.default_action, ptr::null_mut());
        }

        if libc::setsid() < 0 {
            fail(ctx.err_pipe);
        }
        if libc::ioctl(ctx.slave, libc::TIOCSCTTY as _, 0) < 0 {
            fail(ctx.err_pipe);
        }
        for fd in 0..3 {
            if libc::dup2(ctx.slave, fd) < 0 {
                fail(ctx.err_pipe);
            }
            // dup2 onto the same number keeps close-on-exec; clear it.
            libc::fcntl(fd, libc::F_SETFD, 0);
        }
        close_inherited_fds(ctx);

        if libc::chdir(ctx.dir) != 0 {
            libc::chdir(ctx.home);
        }
        libc::execve(ctx.program, ctx.argv, ctx.envp);
        fail(ctx.err_pipe)
    }
}

/// Reports errno to the parent through the close-on-exec pipe and exits.
unsafe fn fail(err_pipe: RawFd) -> ! {
    unsafe {
        let errno = *libc::__error();
        let bytes = errno.to_ne_bytes();
        libc::write(err_pipe, bytes.as_ptr().cast(), bytes.len());
        libc::_exit(127)
    }
}

/// Closes every descriptor above stderr except the error pipe, so nothing the
/// app had open leaks into the shell.
unsafe fn close_inherited_fds(ctx: &ChildContext) {
    unsafe {
        let n = libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDLISTFDS,
            0,
            ctx.fd_list.cast(),
            ctx.fd_list_bytes,
        );
        // A full buffer may mean the list was cut short; fall back to closing
        // every possible descriptor.
        if n > 0 && n < ctx.fd_list_bytes {
            let count = n as usize / mem::size_of::<libc::proc_fdinfo>();
            for i in 0..count {
                let fd = (*ctx.fd_list.add(i)).proc_fd;
                if fd > 2 && fd != ctx.err_pipe {
                    libc::close(fd);
                }
            }
        } else {
            for fd in 3..ctx.fd_limit {
                if fd != ctx.err_pipe {
                    libc::close(fd);
                }
            }
        }
    }
}

/// Waits for the child to exec. None: it did. Some(errno): it failed.
fn read_exec_error(pipe: &OwnedFd) -> Option<i32> {
    let mut buf = [0u8; 4];
    let mut got = 0;
    while got < buf.len() {
        // SAFETY: reading into the unread tail of a local buffer.
        let n = unsafe {
            libc::read(
                pipe.as_raw_fd(),
                buf[got..].as_mut_ptr().cast(),
                buf.len() - got,
            )
        };
        if n > 0 {
            got += n as usize;
        } else if n == 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            break;
        }
    }
    (got == buf.len()).then(|| i32::from_ne_bytes(buf))
}

fn open_pty(mut size: libc::winsize) -> io::Result<(OwnedFd, OwnedFd)> {
    // macOS's openpty is not thread-safe (it goes through ptsname's static
    // buffer): sessions spawned on several threads at once made it fail now
    // and then. Nothing it guards can panic, so poisoning is ignored.
    static OPENPTY: Mutex<()> = Mutex::new(());
    let (mut master, mut slave): (c_int, c_int) = (-1, -1);
    let rc = {
        let _serialized = OPENPTY.lock().unwrap_or_else(PoisonError::into_inner);
        // SAFETY: openpty writes two descriptors; name and termios may be null.
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut size,
            )
        }
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openpty succeeded, so both are open descriptors we now own.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    set_cloexec(&master)?;
    set_cloexec(&slave)?;

    // SAFETY: termios is a plain C struct, valid when zeroed.
    let mut t: libc::termios = unsafe { mem::zeroed() };
    // SAFETY: slave is an open terminal and t is writable.
    if unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut t) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // UTF-8 aware line editing (backspace over a multi-byte character) and
    // DEL as erase, which is what the Backspace key sends.
    t.c_iflag |= libc::IUTF8;
    t.c_cc[libc::VERASE] = 0x7f;
    // SAFETY: as above.
    if unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &t) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((master, slave))
}

/// A pipe whose ends are close-on-exec.
pub(crate) fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as c_int; 2];
    // SAFETY: pipe writes two descriptors into fds.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pipe succeeded; we own both descriptors.
    let (r, w) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    set_cloexec(&r)?;
    set_cloexec(&w)?;
    Ok((r, w))
}

fn set_cloexec(fd: &OwnedFd) -> io::Result<()> {
    // SAFETY: fcntl on a descriptor we own.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn set_nonblocking(fd: &OwnedFd) -> io::Result<()> {
    // SAFETY: fcntl on a descriptor we own.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    // SAFETY: as above.
    if flags < 0
        || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Makes reads and writes on the descriptor wait, or not. It is ours and
/// open, so the calls cannot fail in a way there is anything to do about.
pub(crate) fn set_blocking(fd: &OwnedFd, blocking: bool) {
    // SAFETY: fcntl on a descriptor we own.
    unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
        if flags < 0 {
            return;
        }
        let wanted = if blocking {
            flags & !libc::O_NONBLOCK
        } else {
            flags | libc::O_NONBLOCK
        };
        if wanted != flags {
            libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, wanted);
        }
    }
}

/// How many descriptors this process has open right now.
fn open_fd_count() -> usize {
    // SAFETY: a null buffer asks proc_pidinfo for the size it needs.
    let bytes =
        unsafe { libc::proc_pidinfo(libc::getpid(), libc::PROC_PIDLISTFDS, 0, ptr::null_mut(), 0) };
    (bytes.max(0) as usize) / mem::size_of::<libc::proc_fdinfo>()
}

/// Upper bound for the fallback close loop in the child.
fn open_fd_limit() -> c_int {
    // SAFETY: rlimit is a plain struct, valid when zeroed.
    let mut lim: libc::rlimit = unsafe { mem::zeroed() };
    // SAFETY: lim is writable.
    let cur = if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } == 0 {
        lim.rlim_cur
    } else {
        10_240
    };
    cur.min(1 << 16) as c_int
}

pub(crate) fn set_winsize(master: &OwnedFd, size: &libc::winsize) {
    // SAFETY: TIOCSWINSZ reads a winsize from the pointer. A failure means the
    // terminal is gone, which the reader thread reports on its own.
    unsafe {
        libc::ioctl(
            master.as_raw_fd(),
            libc::TIOCSWINSZ,
            size as *const libc::winsize,
        )
    };
}

/// TIOCUCNTL and UIOCCMD(1) from <sys/ttycom.h>: "user control mode" of a
/// pseudo-terminal and its first control operation.
const TIOCUCNTL: libc::c_ulong = 0x8004_7466;
const UIOCCMD_1: libc::c_ulong = 0x2000_7501;

/// Puts the master in user control mode, where every read is marked: a
/// first byte of zero and then output, or one non-zero byte, which is a
/// control operation someone asked for with [wake_reader]. That second kind
/// is what lets a thread sleep inside read() and still be woken on demand.
/// False when the system refuses, and reads stay as they were.
pub(crate) fn frame_reads(master: &OwnedFd) -> bool {
    let on: c_int = 1;
    // SAFETY: the ioctl reads one int from a local.
    unsafe { libc::ioctl(master.as_raw_fd(), TIOCUCNTL, &on) == 0 }
}

/// Makes the read a thread is sleeping in on a framed master return a
/// wake-up call, or the next read if none is under way.
pub(crate) fn wake_reader(master: &OwnedFd) {
    // SAFETY: an ioctl without an argument on a live descriptor.
    unsafe { libc::ioctl(master.as_raw_fd(), UIOCCMD_1) };
}

/// Whether a read would return now instead of sleeping.
pub(crate) fn readable_now(fd: &OwnedFd) -> bool {
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: polls one local entry without waiting.
    unsafe { libc::poll(&mut poll, 1, 0) > 0 }
}

/// Whether the child has exited, leaving it for [reap] to collect.
pub(crate) fn has_exited(pid: pid_t) -> bool {
    // SAFETY: siginfo_t is a plain C struct, valid when zeroed; waitid
    // fills it and, with WNOWAIT, changes nothing else.
    unsafe {
        let mut info: libc::siginfo_t = mem::zeroed();
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        ) == 0
            && info.si_pid == pid
    }
}

/// Waits for the child to exit and returns its exit code; None when it was
/// killed by a signal or cannot be waited for.
pub(crate) fn reap(pid: pid_t) -> Option<i32> {
    let mut status: c_int = 0;
    loop {
        // SAFETY: waitpid writes the status into a local.
        let r = unsafe { libc::waitpid(pid, &mut status, 0) };
        if r == pid {
            break;
        }
        if r < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return None;
    }
    libc::WIFEXITED(status).then(|| libc::WEXITSTATUS(status))
}

/// The process group in the foreground of the terminal.
pub(crate) fn foreground_group(master: &OwnedFd) -> Option<pid_t> {
    // SAFETY: tcgetpgrp only reads the descriptor's terminal state.
    let pgrp = unsafe { libc::tcgetpgrp(master.as_raw_fd()) };
    (pgrp > 0).then_some(pgrp)
}

/// Whether the PTY line discipline is in canonical (line-buffered) mode.
/// Non-interactive batch commands leave `ICANON` enabled; interactive TUIs and
/// REPLs (Claude Code, vim, ssh, etc.) turn it off to read raw keystrokes.
pub(crate) fn is_canonical(master: &OwnedFd) -> bool {
    // SAFETY: termios is a plain C struct, valid when zeroed.
    let mut t: libc::termios = unsafe { mem::zeroed() };
    // SAFETY: tcgetattr only reads the descriptor's terminal attributes.
    unsafe { libc::tcgetattr(master.as_raw_fd(), &mut t) == 0 && (t.c_lflag & libc::ICANON) != 0 }
}

/// The current working directory of a process.
pub(crate) fn process_cwd(pid: pid_t) -> Option<PathBuf> {
    let mut info = MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    let size = mem::size_of::<libc::proc_vnodepathinfo>() as c_int;
    // SAFETY: proc_pidinfo writes at most size bytes into info.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if n != size {
        return None;
    }
    // SAFETY: the kernel filled the whole struct.
    let info = unsafe { info.assume_init() };
    // SAFETY: vip_path is a flat char array of MAXPATHLEN bytes, written by
    // the kernel as a NUL-terminated string.
    let raw: &[u8] = unsafe {
        std::slice::from_raw_parts(
            info.pvi_cdir.vip_path.as_ptr().cast::<u8>(),
            mem::size_of_val(&info.pvi_cdir.vip_path),
        )
    };
    let len = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    (len > 0).then(|| PathBuf::from(OsStr::from_bytes(&raw[..len])))
}

/// The short name of a process (its executable's file name).
pub(crate) fn process_name(pid: pid_t) -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: proc_name writes at most buf.len() bytes.
    let n = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect()
    }

    fn get<'a>(env: &'a [(OsString, OsString)], key: &str) -> Option<&'a str> {
        env_value(env, key.as_bytes()).and_then(OsStr::to_str)
    }

    #[test]
    fn env_drops_other_terminals_and_sets_ours() {
        let env = build_env(
            os(&[
                ("HOME", "/Users/x"),
                ("TERM", "xterm-ghostty"),
                ("TERMINFO", "/Applications/Ghostty.app/terminfo"),
                ("GHOSTTY_RESOURCES_DIR", "/x"),
                ("KITTY_WINDOW_ID", "1"),
                ("ITERM_SESSION_ID", "1"),
                ("TMUX", "/tmp/tmux"),
                ("TMUX_PANE", "%1"),
                ("TERM_SESSION_ID", "abc"),
                ("COLUMNS", "80"),
                ("LANG", "de_DE.UTF-8"),
            ]),
            &[],
        );
        assert_eq!(get(&env, "HOME"), Some("/Users/x"));
        assert_eq!(get(&env, "TERM"), Some("xterm-256color"));
        assert_eq!(get(&env, "COLORTERM"), Some("truecolor"));
        assert_eq!(get(&env, "TERM_PROGRAM"), Some("Tern"));
        assert_eq!(
            get(&env, "TERM_PROGRAM_VERSION"),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(get(&env, "LANG"), Some("de_DE.UTF-8"));
        for gone in [
            "TERMINFO",
            "GHOSTTY_RESOURCES_DIR",
            "KITTY_WINDOW_ID",
            "ITERM_SESSION_ID",
            "TMUX",
            "TMUX_PANE",
            "TERM_SESSION_ID",
            "COLUMNS",
        ] {
            assert_eq!(get(&env, gone), None, "{gone} should be removed");
        }
    }

    #[test]
    fn lang_is_only_added_without_a_locale() {
        let env = build_env(os(&[("PATH", "/bin")]), &[]);
        assert_eq!(get(&env, "LANG"), Some("en_US.UTF-8"));
        let env = build_env(os(&[("LC_CTYPE", "UTF-8")]), &[]);
        assert_eq!(get(&env, "LANG"), None);
        // An empty LC_ALL does not select a locale.
        let env = build_env(os(&[("LC_ALL", "")]), &[]);
        assert_eq!(get(&env, "LANG"), Some("en_US.UTF-8"));
    }

    #[test]
    fn configured_env_wins_and_bad_names_are_skipped() {
        let extra = vec![
            ("TERM".to_string(), "xterm".to_string()),
            ("FOO".to_string(), "bar".to_string()),
            ("A=B".to_string(), "x".to_string()),
        ];
        let env = build_env(os(&[]), &extra);
        assert_eq!(get(&env, "TERM"), Some("xterm"));
        assert_eq!(get(&env, "FOO"), Some("bar"));
        assert!(!env.iter().any(|(k, _)| k.as_bytes().contains(&b'=')));
    }

    #[test]
    fn login_shell_argv0_has_a_dash() {
        assert_eq!(login_argv0(Path::new("/bin/zsh")), OsString::from("-zsh"));
        assert_eq!(
            login_argv0(Path::new("/opt/homebrew/bin/fish")),
            OsString::from("-fish")
        );
    }

    #[test]
    fn programs_are_found_on_path() {
        assert_eq!(
            find_program("sh", OsStr::new("/nonexistent:/bin")).ok(),
            Some(PathBuf::from("/bin/sh"))
        );
        assert_eq!(
            find_program("./x", OsStr::new("")).ok(),
            Some(PathBuf::from("./x"))
        );
        let err = find_program("no-such-program-tern", OsStr::new("/bin")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn command_for_login_shell_and_explicit_argv() {
        let cmd = Command::new(Some("/bin/zsh"), None, None, &[]).expect("command");
        assert!(cmd.login_shell);
        assert_eq!(cmd.argv[0].to_bytes(), b"-zsh");
        assert_eq!(cmd.program.to_bytes(), b"/bin/zsh");

        let argv = vec!["sh".to_string(), "-c".to_string(), "true".to_string()];
        let cmd = Command::new(None, Some(&argv), Some(Path::new("/tmp")), &[]).expect("command");
        assert!(!cmd.login_shell);
        assert!(cmd.envp.iter().any(|e| e.to_bytes() == b"PWD=/tmp"));
        assert_eq!(cmd.program.to_bytes(), b"/bin/sh");
        assert_eq!(cmd.argv.len(), 3);
        assert_eq!(cmd.dir.to_bytes(), b"/tmp");
        assert!(Command::new(None, Some(&[]), None, &[]).is_err());
    }

    #[test]
    fn a_shell_given_by_name_is_found_on_path() {
        let env = [("PATH".to_string(), "/nonexistent:/bin".to_string())];
        let cmd = Command::new(Some("sh"), None, None, &env).expect("command");
        assert_eq!(cmd.program.to_bytes(), b"/bin/sh");
        assert_eq!(cmd.argv[0].to_bytes(), b"-sh");
        let err = Command::new(Some("no-such-shell-tern"), None, None, &env)
            .err()
            .expect("missing shell");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        // An empty setting means "not set": the user's own shell.
        let cmd = Command::new(Some(""), None, None, &[]).expect("command");
        assert!(cmd.login_shell && !cmd.program.to_bytes().is_empty());
        assert!(cmd.program.to_bytes().starts_with(b"/"));
    }

    #[test]
    fn process_queries_answer_for_ourselves() {
        // SAFETY: getpid has no preconditions.
        let me = unsafe { libc::getpid() };
        let cwd = process_cwd(me).expect("cwd");
        let real = std::env::current_dir()
            .expect("current dir")
            .canonicalize()
            .expect("canonical");
        assert_eq!(cwd.canonicalize().expect("canonical"), real);
        assert!(process_name(me).is_some_and(|n| !n.is_empty()));
    }

    #[test]
    fn canonical_mode_tracks_slave_termios() {
        let mut master_fd: c_int = -1;
        let mut slave_fd: c_int = -1;
        // SAFETY: openpty writes valid fds on success.
        assert_eq!(
            unsafe { libc::openpty(&mut master_fd, &mut slave_fd, ptr::null_mut(), ptr::null_mut(), ptr::null_mut()) },
            0
        );
        // SAFETY: both descriptors were just returned by openpty.
        let master = unsafe { OwnedFd::from_raw_fd(master_fd) };
        let slave = unsafe { OwnedFd::from_raw_fd(slave_fd) };

        assert!(is_canonical(&master));

        // SAFETY: termios is plain data; toggling ICANON on slave updates PTY line discipline.
        unsafe {
            let mut t: libc::termios = mem::zeroed();
            assert_eq!(libc::tcgetattr(slave.as_raw_fd(), &mut t), 0);
            t.c_lflag &= !libc::ICANON;
            assert_eq!(libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &t), 0);
        }
        assert!(!is_canonical(&master));
    }
}
