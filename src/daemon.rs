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
use nix::sys::wait::waitpid;
use nix::unistd::{ForkResult, Pid};

use crate::protocol::{Request, Response};
use crate::render;

const MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;

struct SessionState {
    parser: vt100::Parser,
    output_buf: VecDeque<u8>,
    pty_master: i32,
    child_pid: Pid,
    alive: bool,
}

pub fn start(
    session_name: &str,
    command: &[String],
    cols: u16,
    rows: u16,
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

    match unsafe { nix::unistd::fork() }? {
        ForkResult::Parent { child } => {
            for _ in 0..100 {
                if socket_path.exists() {
                    eprintln!("Session '{}' started (daemon pid {})", session_name, child);
                    return Ok(());
                }
                thread::sleep(Duration::from_millis(50));
            }
            anyhow::bail!("Timed out waiting for session '{}' to start", session_name);
        }
        ForkResult::Child => {
            let result = daemonize_and_run(&socket_path, &pid_path, command, cols, rows);

            let _ = std::fs::remove_file(&socket_path);
            let _ = std::fs::remove_file(&pid_path);

            match result {
                Ok(()) => std::process::exit(0),
                Err(_) => std::process::exit(1),
            }
        }
    }
}

fn daemonize_and_run(
    socket_path: &Path,
    pid_path: &Path,
    command: &[String],
    cols: u16,
    rows: u16,
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
            run_session(socket_path, master_fd, child, cols, rows)
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

            let cmd = CString::new(command[0].as_str())?;
            let args: Vec<CString> = command
                .iter()
                .map(|a| CString::new(a.as_str()).unwrap())
                .collect();
            nix::unistd::execvp(&cmd, &args)?;

            unreachable!()
        }
    }
}

fn run_session(
    socket_path: &Path,
    master_fd: i32,
    child_pid: Pid,
    cols: u16,
    rows: u16,
) -> anyhow::Result<()> {
    let state = Arc::new(Mutex::new(SessionState {
        parser: vt100::Parser::new(rows, cols, 0),
        output_buf: VecDeque::with_capacity(MAX_OUTPUT_BYTES),
        pty_master: master_fd,
        child_pid,
        alive: true,
    }));

    // Thread: read PTY master output (uses poll so it can be interrupted)
    let state_clone = Arc::clone(&state);
    let reader_thread = thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            let mut fds = [libc::pollfd {
                fd: master_fd,
                events: libc::POLLIN,
                revents: 0,
            }];
            let ret = unsafe { libc::poll(fds.as_mut_ptr(), 1, 200) };
            if ret < 0 {
                state_clone.lock().unwrap().alive = false;
                break;
            }
            if ret == 0 {
                // Timeout -- check if we should exit
                if !state_clone.lock().unwrap().alive {
                    break;
                }
                continue;
            }
            if fds[0].revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
                state_clone.lock().unwrap().alive = false;
                break;
            }
            if fds[0].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
                let n = unsafe { libc::read(master_fd, buf.as_mut_ptr() as *mut _, buf.len()) };
                if n <= 0 {
                    state_clone.lock().unwrap().alive = false;
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

    // Main thread: accept client connections
    let listener = UnixListener::bind(socket_path)?;
    listener.set_nonblocking(true)?;

    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                if handle_client(stream, &state) {
                    break;
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if !state.lock().unwrap().alive {
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(_) => break,
        }
    }

    // Close master fd -- triggers SIGHUP on the slave side
    unsafe { libc::close(master_fd) };

    // Wait for reader thread to notice and exit
    let _ = reader_thread.join();

    // Wait for child, SIGKILL if it doesn't exit promptly
    if let Ok(nix::sys::wait::WaitStatus::StillAlive) =
        waitpid(child_pid, Some(nix::sys::wait::WaitPidFlag::WNOHANG))
    {
        let _ = signal::kill(child_pid, signal::Signal::SIGKILL);
        let _ = waitpid(child_pid, None);
    }

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
        // Wait needs to poll with the lock released between checks
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
                let st = state.lock().unwrap();
                if !st.alive {
                    break;
                }
                let current_size = st.output_buf.len();
                if current_size != last_size {
                    last_size = current_size;
                    last_change = std::time::Instant::now();
                }
                drop(st);
                if last_change.elapsed() >= settle {
                    break;
                }
                if start.elapsed() >= timeout {
                    break;
                }
            }
            Response::ok(None)
        }
        // All other requests hold the lock for the duration
        other => {
            let mut st = state.lock().unwrap();
            match other {
                Request::Write { data } => {
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
                Request::View { color } => {
                    let screen = st.parser.screen();
                    let cursor = screen.cursor_position();
                    let (rows, cols) = screen.size();
                    let header = format!("[{}x{} cursor=({},{})]", cols, rows, cursor.0, cursor.1);
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
                Request::Status => {
                    let screen = st.parser.screen();
                    let (rows, cols) = screen.size();
                    let title = screen.title();
                    let cursor = screen.cursor_position();
                    let info = format!(
                        "alive: {}\nsize: {}x{}\ncursor: ({},{})\ntitle: {}\noutput_bytes: {}",
                        st.alive,
                        cols,
                        rows,
                        cursor.0,
                        cursor.1,
                        title,
                        st.output_buf.len()
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
                    let _ = signal::kill(st.child_pid, signal::Signal::SIGHUP);
                    let _ = signal::kill(st.child_pid, signal::Signal::SIGTERM);
                    st.alive = false;
                    should_stop = true;
                    Response::ok(None)
                }
                Request::Wait { .. } => unreachable!(),
            }
        }
    };

    let _ = serde_json::to_writer(&mut writer, &response);
    should_stop
}
