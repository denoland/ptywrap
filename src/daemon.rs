use std::collections::VecDeque;
use std::ffi::CString;
use std::io::{BufRead, BufReader};
use std::os::fd::IntoRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use nix::pty::{Winsize, openpty};
use nix::sys::signal;
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::{ForkResult, Pid};

use std::io::Read as _;
use std::os::fd::FromRawFd;

use crate::protocol::{Request, Response};
use crate::render;

const MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;

struct SessionState {
    parser: vt100::Parser,
    output_buf: VecDeque<u8>,
    pty_master: i32,
    child_pid: Pid,
    alive: bool,
    exit_status: Option<String>,
}

fn format_wait_status(status: &WaitStatus) -> String {
    match status {
        WaitStatus::Exited(_, code) => format!("exited with code {}", code),
        WaitStatus::Signaled(_, sig, core) => {
            let core = if *core { " (core dumped)" } else { "" };
            format!("killed by signal {}{}", sig, core)
        }
        other => format!("{:?}", other),
    }
}

pub fn start(
    session_name: &str,
    command: &[String],
    cols: u16,
    rows: u16,
    term: &str,
    runtime_dir: &Path,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(runtime_dir)?;

    let socket_path = runtime_dir.join(format!("{}.sock", session_name));
    let pid_path = runtime_dir.join(format!("{}.pid", session_name));

    if socket_path.exists() {
        if std::os::unix::net::UnixStream::connect(&socket_path).is_ok() {
            anyhow::bail!("Session '{}' is already running", session_name);
        }
        // Stale socket, remove it
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_file(&pid_path);
    }

    // Status pipe: the daemon (and the exec'd child) writes a short error
    // string here if startup fails; on success it closes the write end
    // silently. The parent (us) reads until EOF -- non-empty data means
    // startup failed and we should clean up.
    //
    // CLOEXEC on both ends means the eventual execvp'd child auto-closes
    // the write end on success; on failure the child explicitly writes
    // its errno before exiting. (libc::pipe + fcntl, since pipe2 isn't
    // available on macOS.)
    let (status_r, status_w) = {
        let mut fds = [0i32; 2];
        let r = unsafe { libc::pipe(fds.as_mut_ptr()) };
        if r != 0 {
            anyhow::bail!("pipe(2): {}", std::io::Error::last_os_error());
        }
        unsafe {
            libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(fds[1], libc::F_SETFD, libc::FD_CLOEXEC);
        }
        (fds[0], fds[1])
    };

    match unsafe { nix::unistd::fork() }? {
        ForkResult::Parent { child } => {
            unsafe { libc::close(status_w) }; // parent doesn't write
            let mut rfd = unsafe { std::fs::File::from_raw_fd(status_r) };
            let mut err_buf = Vec::new();
            rfd.read_to_end(&mut err_buf).ok();

            if !err_buf.is_empty() {
                // Daemon failed to start. Make sure it's actually dead and
                // that socket/pid files don't linger.
                let _ = signal::kill(child, signal::Signal::SIGTERM);
                let _ = waitpid(child, None);
                let _ = std::fs::remove_file(&socket_path);
                let _ = std::fs::remove_file(&pid_path);
                let msg = String::from_utf8_lossy(&err_buf).trim().to_string();
                anyhow::bail!("Failed to start session '{}': {}", session_name, msg);
            }
            eprintln!("Session '{}' started (daemon pid {})", session_name, child);
            Ok(())
        }
        ForkResult::Child => {
            unsafe { libc::close(status_r) }; // daemon doesn't read
            let status_w_raw = status_w;
            let result = daemonize_and_run(
                &socket_path,
                &pid_path,
                command,
                cols,
                rows,
                term,
                status_w_raw,
            );

            // On success, run_session has already closed status_w_raw to
            // signal the parent. On error, status_w_raw is still open (we
            // close before any fallible path in run_session) -- write the
            // error message; process exit will close the fd.
            if let Err(e) = &result {
                write_status_err(status_w_raw, &format!("{}", e));
            }

            let _ = std::fs::remove_file(&socket_path);
            let _ = std::fs::remove_file(&pid_path);

            match result {
                Ok(()) => std::process::exit(0),
                Err(_) => std::process::exit(1),
            }
        }
    }
}

/// Best-effort write of an error message to the status pipe so the
/// `ptywrap start` parent can surface it. Failures are intentionally
/// ignored: at this point we're already on an error path and exiting.
fn write_status_err(fd: i32, msg: &str) {
    let bytes = msg.as_bytes();
    unsafe {
        let _ = libc::write(fd, bytes.as_ptr() as *const _, bytes.len());
    }
}

fn daemonize_and_run(
    socket_path: &Path,
    pid_path: &Path,
    command: &[String],
    cols: u16,
    rows: u16,
    term: &str,
    status_w_fd: i32,
) -> anyhow::Result<()> {
    nix::unistd::setsid()?;

    // Redirect stdio to /dev/null
    let dev_null = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")?;
    let null_fd = dev_null.into_raw_fd();
    nix::unistd::dup2(null_fd, 0)?;
    nix::unistd::dup2(null_fd, 1)?;
    nix::unistd::dup2(null_fd, 2)?;
    if null_fd > 2 {
        unsafe { libc::close(null_fd) };
    }

    std::fs::write(pid_path, format!("{}", nix::unistd::getpid()))?;

    // Create PTY
    let winsize = Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let pty = openpty(Some(&winsize), None)?;
    let master_fd = pty.master.into_raw_fd();
    let slave_fd = pty.slave.into_raw_fd();

    // Fork child process for the command
    match unsafe { nix::unistd::fork() }? {
        ForkResult::Parent { child } => {
            unsafe { libc::close(slave_fd) };
            run_session(socket_path, master_fd, child, cols, rows, status_w_fd)
        }
        ForkResult::Child => {
            unsafe { libc::close(master_fd) };

            // New session so PTY becomes controlling terminal
            nix::unistd::setsid()?;

            nix::unistd::dup2(slave_fd, 0)?;
            nix::unistd::dup2(slave_fd, 1)?;
            nix::unistd::dup2(slave_fd, 2)?;
            if slave_fd > 2 {
                unsafe { libc::close(slave_fd) };
            }

            // Set controlling terminal
            unsafe { libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0) };

            // Setting TERM here means curses programs (vim/htop/less) can
            // be run directly without `env TERM=... bash` indirection.
            // CARGO_PKG_VERSION pollution etc. is fine -- the child sees
            // whatever environment we inherited plus this override.
            unsafe {
                std::env::set_var("TERM", term);
            }

            let cmd = CString::new(command[0].as_str())?;
            let args: Vec<CString> = command
                .iter()
                .map(|a| CString::new(a.as_str()).unwrap())
                .collect();
            // On successful exec, CLOEXEC closes status_w_fd, which lets
            // the `ptywrap start` parent's read return EOF. On failure we
            // explicitly write the errno before exiting so the parent
            // gets a useful error.
            match nix::unistd::execvp(&cmd, &args) {
                Ok(_) => unreachable!(),
                Err(errno) => {
                    write_status_err(status_w_fd, &format!("exec {:?}: {}", command[0], errno));
                    unsafe { libc::close(status_w_fd) };
                    std::process::exit(127);
                }
            }
        }
    }
}

fn run_session(
    socket_path: &Path,
    master_fd: i32,
    child_pid: Pid,
    cols: u16,
    rows: u16,
    status_w_fd: i32,
) -> anyhow::Result<()> {
    let state = Arc::new(Mutex::new(SessionState {
        parser: vt100::Parser::new(rows, cols, 0),
        output_buf: VecDeque::with_capacity(MAX_OUTPUT_BYTES),
        pty_master: master_fd,
        child_pid,
        alive: true,
        exit_status: None,
    }));

    // Thread: read PTY master output (uses poll so it can be interrupted by
    // closing the master fd).
    let state_clone = Arc::clone(&state);
    let reader_thread = thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            let mut fds = [libc::pollfd {
                fd: master_fd,
                events: libc::POLLIN,
                revents: 0,
            }];
            let ret = unsafe { libc::poll(fds.as_mut_ptr(), 1, 1000) };
            if ret < 0 {
                break;
            }
            if ret == 0 {
                continue;
            }
            if fds[0].revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
                break;
            }
            if fds[0].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
                let n = unsafe { libc::read(master_fd, buf.as_mut_ptr() as *mut _, buf.len()) };
                if n <= 0 {
                    break;
                }
                let data = &buf[..n as usize];
                let mut st = state_clone.lock().unwrap();
                st.parser.process(data);
                for &byte in data {
                    if st.output_buf.len() >= MAX_OUTPUT_BYTES {
                        st.output_buf.pop_front();
                    }
                    st.output_buf.push_back(byte);
                }
            }
        }
    });

    // Thread: wait for the child to exit and record its status. This runs
    // independently of the reader so we can know the child has gone away even
    // if there's still buffered PTY output being drained.
    let reaper_state = Arc::clone(&state);
    let reaper_thread = thread::spawn(move || {
        let result = waitpid(child_pid, None);
        let mut st = reaper_state.lock().unwrap();
        st.alive = false;
        if let Ok(status) = result {
            st.exit_status = Some(format_wait_status(&status));
        }
    });

    // Main thread: accept client connections. The daemon keeps running after
    // the child exits so callers can still query the final screen, output
    // buffer, and exit status; only an explicit `stop` ends the daemon.
    let listener = UnixListener::bind(socket_path)?;
    listener.set_nonblocking(true)?;

    // Startup is past the point where it can fail. Close our copy of the
    // status pipe so the `ptywrap start` parent sees EOF (success). The
    // exec'd child still holds the other copy with CLOEXEC, which closes
    // on successful exec or stays open until the child writes an error.
    unsafe { libc::close(status_w_fd) };

    let mut should_stop = false;
    while !should_stop {
        match listener.accept() {
            Ok((stream, _)) => {
                if handle_client(stream, &state) {
                    should_stop = true;
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(_) => break,
        }
    }

    // Give the child a moment to exit after Stop signalled it; if it's still
    // running, force SIGKILL so the reaper can complete.
    let deadline = std::time::Instant::now() + Duration::from_millis(500);
    while !reaper_thread.is_finished() && std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if !reaper_thread.is_finished() {
        let _ = signal::kill(child_pid, signal::Signal::SIGKILL);
    }
    let _ = reaper_thread.join();

    // Closing master triggers POLLNVAL on the reader's poll.
    unsafe { libc::close(master_fd) };
    let _ = reader_thread.join();

    Ok(())
}

/// Handle a single client connection. Returns true if the daemon should stop.
fn handle_client(stream: UnixStream, state: &Arc<Mutex<SessionState>>) -> bool {
    let stream2 = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return false,
    };
    let mut reader = BufReader::new(stream);
    let mut writer = stream2;

    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.is_empty() {
        return false;
    }

    let request: Request = match serde_json::from_str(line.trim()) {
        Ok(r) => r,
        Err(e) => {
            let resp = Response::error(format!("Invalid request: {}", e));
            let _ = serde_json::to_writer(&mut writer, &resp);
            return false;
        }
    };

    let mut should_stop = false;

    let response = match request {
        // Wait needs to poll with the lock released between checks. It does
        // NOT bail out when the child dies -- we still want to drain any
        // buffered output that hasn't been consumed yet.
        Request::Wait {
            settle_ms,
            timeout_ms,
        } => {
            let settle = Duration::from_millis(settle_ms.unwrap_or(500));
            let timeout = Duration::from_millis(timeout_ms.unwrap_or(30000));
            let start = std::time::Instant::now();
            let mut last_size = state.lock().unwrap().output_buf.len();
            let mut last_change = std::time::Instant::now();

            loop {
                thread::sleep(Duration::from_millis(50));
                let current_size = state.lock().unwrap().output_buf.len();
                if current_size != last_size {
                    last_size = current_size;
                    last_change = std::time::Instant::now();
                }
                if last_change.elapsed() >= settle {
                    break;
                }
                if start.elapsed() >= timeout {
                    break;
                }
            }
            Response::ok(None)
        }
        // WriteChunks (simulated typing) also sleeps with the lock released
        // so the reader thread can process the echo while we "type" --
        // otherwise the screen would only update once at the end.
        Request::WriteChunks { chunks, delay_ms } => {
            let (fd, alive) = {
                let st = state.lock().unwrap();
                (st.pty_master, st.alive)
            };
            if !alive {
                Response::error("Session is no longer alive; child process has exited")
            } else {
                // xorshift PRNG for keystroke jitter; seeded from the clock.
                let mut seed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos() as u64)
                    .unwrap_or(0x9e3779b9)
                    | 1;
                let mut failed = false;
                for (i, chunk) in chunks.iter().enumerate() {
                    if i > 0 && delay_ms > 0 {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        // Uniform in [delay/2, 3*delay/2): human-ish cadence
                        // rather than a metronomic (paste-like) one.
                        let ms = delay_ms / 2 + seed % delay_ms;
                        thread::sleep(Duration::from_millis(ms));
                    }
                    let bytes = chunk.as_bytes();
                    let n = unsafe { libc::write(fd, bytes.as_ptr() as *const _, bytes.len()) };
                    if n < 0 {
                        failed = true;
                        break;
                    }
                }
                if failed {
                    Response::error("Failed to write to PTY")
                } else {
                    Response::ok(None)
                }
            }
        }
        // All other requests hold the lock for the duration
        other => {
            let mut st = state.lock().unwrap();
            match other {
                Request::Write { data } => {
                    if !st.alive {
                        Response::error("Session is no longer alive; child process has exited")
                    } else {
                        let bytes = data.as_bytes();
                        let n = unsafe {
                            libc::write(st.pty_master, bytes.as_ptr() as *const _, bytes.len())
                        };
                        if n < 0 {
                            Response::error("Failed to write to PTY")
                        } else {
                            Response::ok(None)
                        }
                    }
                }
                Request::View { color } => {
                    let screen = st.parser.screen();
                    let cursor = screen.cursor_position();
                    let (rows, cols) = screen.size();
                    let exit_marker = if !st.alive {
                        match &st.exit_status {
                            Some(s) => format!(" {}", s),
                            None => " exited".to_string(),
                        }
                    } else {
                        String::new()
                    };
                    let header = format!(
                        "[{}x{} cursor=({},{}){}]",
                        cols, rows, cursor.0, cursor.1, exit_marker
                    );
                    let contents = if color {
                        String::from_utf8_lossy(&screen.contents_formatted()).to_string()
                    } else {
                        screen.contents()
                    };
                    Response::ok(Some(format!("{}\n{}", header, contents)))
                }
                Request::Output { tail } => {
                    let buf: Vec<u8> = st.output_buf.iter().copied().collect();
                    let text = match tail {
                        Some(n) => {
                            let mut count = 0;
                            let mut pos = buf.len();
                            for i in (0..buf.len()).rev() {
                                if buf[i] == b'\n' {
                                    count += 1;
                                    if count >= n {
                                        pos = i + 1;
                                        break;
                                    }
                                }
                            }
                            String::from_utf8_lossy(&buf[pos..]).to_string()
                        }
                        None => String::from_utf8_lossy(&buf).to_string(),
                    };
                    Response::ok(Some(text))
                }
                Request::Resize { cols, rows } => {
                    if !st.alive {
                        Response::error("Session is no longer alive; child process has exited")
                    } else {
                        let ws = libc::winsize {
                            ws_row: rows,
                            ws_col: cols,
                            ws_xpixel: 0,
                            ws_ypixel: 0,
                        };
                        let ret = unsafe { libc::ioctl(st.pty_master, libc::TIOCSWINSZ, &ws) };
                        if ret < 0 {
                            Response::error("Failed to resize PTY")
                        } else {
                            st.parser.set_size(rows, cols);
                            Response::ok(None)
                        }
                    }
                }
                Request::Status => {
                    let screen = st.parser.screen();
                    let (rows, cols) = screen.size();
                    let title = screen.title();
                    let cursor = screen.cursor_position();
                    let exit_line = match (&st.exit_status, st.alive) {
                        (Some(s), _) => format!("\nexit_status: {}", s),
                        (None, false) => "\nexit_status: unknown".to_string(),
                        (None, true) => String::new(),
                    };
                    let info = format!(
                        "alive: {}\nsize: {}x{}\ncursor: ({},{})\ntitle: {}\noutput_bytes: {}{}",
                        st.alive,
                        cols,
                        rows,
                        cursor.0,
                        cursor.1,
                        title,
                        st.output_buf.len(),
                        exit_line,
                    );
                    Response::ok(Some(info))
                }
                Request::Screenshot { path, scale } => {
                    let img = render::render_screenshot(st.parser.screen(), scale.unwrap_or(2));
                    match img.save(&path) {
                        Ok(()) => Response::ok(Some(format!("Screenshot saved to {}", path))),
                        Err(e) => Response::error(format!("Failed to save screenshot: {}", e)),
                    }
                }
                Request::Stop => {
                    if st.alive {
                        let _ = signal::kill(st.child_pid, signal::Signal::SIGHUP);
                        let _ = signal::kill(st.child_pid, signal::Signal::SIGTERM);
                    }
                    should_stop = true;
                    Response::ok(None)
                }
                Request::Wait { .. } | Request::WriteChunks { .. } => unreachable!(),
            }
        }
    };

    let _ = serde_json::to_writer(&mut writer, &response);
    should_stop
}
