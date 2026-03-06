use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod client;
mod daemon;
mod keys;
mod protocol;
mod render;

use protocol::Request;

#[derive(Parser)]
#[command(name = "ptywrap", about = "PTY session manager for LLM interaction")]
struct Cli {
    /// Session name
    #[arg(long, short)]
    session: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start a new PTY session
    Start {
        /// Terminal width
        #[arg(long, default_value_t = 80)]
        cols: u16,
        /// Terminal height
        #[arg(long, default_value_t = 24)]
        rows: u16,
        /// Command to run (after --)
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Write text to the PTY (interprets \\n, \\t, \\x1b, etc.)
    Write {
        /// Text to write
        data: String,
    },
    /// Send named keys to the PTY (e.g. Enter, Tab, Up, Ctrl-C)
    SendKey {
        /// Key names to send
        #[arg(required = true)]
        keys: Vec<String>,
    },
    /// Show the rendered terminal screen
    View {
        /// Wait for output to settle before showing the screen
        #[arg(long)]
        wait: bool,
        /// Settle time in ms (how long to wait after last output, default 500)
        #[arg(long)]
        settle: Option<u64>,
        /// Include ANSI color/style escape codes in output
        #[arg(long)]
        color: bool,
    },
    /// Show raw PTY output
    Output {
        /// Show last N lines
        #[arg(long)]
        tail: Option<usize>,
        /// Wait for output to settle first
        #[arg(long)]
        wait: bool,
        /// Settle time in ms (default 500)
        #[arg(long)]
        settle: Option<u64>,
    },
    /// Wait for output to settle (no new output for --settle ms)
    Wait {
        /// Settle time in ms (default 500)
        #[arg(long, default_value_t = 500)]
        settle: u64,
        /// Max time to wait in ms (default 30000)
        #[arg(long, default_value_t = 30000)]
        timeout: u64,
    },
    /// Resize the PTY
    Resize {
        /// New width
        cols: u16,
        /// New height
        rows: u16,
    },
    /// Take a screenshot of the terminal as PNG
    Screenshot {
        /// Output file path
        path: String,
        /// Scale factor (default 2 = 16x16 per char)
        #[arg(long, default_value_t = 2)]
        scale: u32,
        /// Wait for output to settle first
        #[arg(long)]
        wait: bool,
        /// Settle time in ms (default 500)
        #[arg(long)]
        settle: Option<u64>,
    },
    /// Show session status
    Status,
    /// Stop a session
    Stop,
    /// List active sessions
    List,
}

fn runtime_dir() -> anyhow::Result<PathBuf> {
    let home = std::env::var("HOME").map_err(|_| anyhow::anyhow!("HOME not set"))?;
    Ok(PathBuf::from(home).join(".ptywrap"))
}

fn require_session(session: Option<String>) -> anyhow::Result<String> {
    session.ok_or_else(|| anyhow::anyhow!("--session is required for this command"))
}

fn send_and_print(socket_path: &std::path::Path, request: &Request) -> anyhow::Result<()> {
    let resp = client::send(socket_path, request)?;
    if resp.success {
        if let Some(data) = resp.data {
            println!("{}", data);
        }
    } else {
        let msg = resp.error.unwrap_or_else(|| "Unknown error".into());
        eprintln!("Error: {}", msg);
        std::process::exit(1);
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let dir = runtime_dir()?;

    match cli.command {
        Command::List => {
            list_sessions(&dir)?;
        }
        Command::Start {
            cols,
            rows,
            command,
        } => {
            let session = require_session(cli.session)?;
            daemon::start(&session, &command, cols, rows, &dir)?;
        }
        cmd => {
            let session = require_session(cli.session)?;
            let socket_path = dir.join(format!("{}.sock", session));

            match cmd {
                Command::Write { data } => {
                    let bytes = keys::interpret_escapes(&data);
                    let data_str = String::from_utf8(bytes)
                        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).to_string());
                    send_and_print(&socket_path, &Request::Write { data: data_str })?;
                }
                Command::SendKey { keys: key_names } => {
                    let mut all_bytes = Vec::new();
                    for name in &key_names {
                        match keys::key_to_bytes(name) {
                            Some(bytes) => all_bytes.extend(bytes),
                            None => anyhow::bail!("Unknown key: {}", name),
                        }
                    }
                    let data_str = String::from_utf8(all_bytes)
                        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).to_string());
                    send_and_print(&socket_path, &Request::Write { data: data_str })?;
                }
                Command::View {
                    wait,
                    settle,
                    color,
                } => {
                    if wait || settle.is_some() {
                        send_and_print(
                            &socket_path,
                            &Request::Wait {
                                settle_ms: Some(settle.unwrap_or(500)),
                                timeout_ms: Some(30000),
                            },
                        )?;
                    }
                    send_and_print(&socket_path, &Request::View { color })?;
                }
                Command::Output { tail, wait, settle } => {
                    if wait || settle.is_some() {
                        send_and_print(
                            &socket_path,
                            &Request::Wait {
                                settle_ms: Some(settle.unwrap_or(500)),
                                timeout_ms: Some(30000),
                            },
                        )?;
                    }
                    send_and_print(&socket_path, &Request::Output { tail })?;
                }
                Command::Wait { settle, timeout } => {
                    send_and_print(
                        &socket_path,
                        &Request::Wait {
                            settle_ms: Some(settle),
                            timeout_ms: Some(timeout),
                        },
                    )?;
                }
                Command::Resize { cols, rows } => {
                    send_and_print(&socket_path, &Request::Resize { cols, rows })?;
                }
                Command::Screenshot {
                    path,
                    scale,
                    wait,
                    settle,
                } => {
                    if wait || settle.is_some() {
                        send_and_print(
                            &socket_path,
                            &Request::Wait {
                                settle_ms: Some(settle.unwrap_or(500)),
                                timeout_ms: Some(30000),
                            },
                        )?;
                    }
                    send_and_print(
                        &socket_path,
                        &Request::Screenshot {
                            path,
                            scale: Some(scale),
                        },
                    )?;
                }
                Command::Status => {
                    send_and_print(&socket_path, &Request::Status)?;
                }
                Command::Stop => {
                    send_and_print(&socket_path, &Request::Stop)?;
                }
                _ => unreachable!(),
            }
        }
    }

    Ok(())
}

fn list_sessions(dir: &std::path::Path) -> anyhow::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("No active sessions.");
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    let mut found = false;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(session) = name.strip_suffix(".sock") {
            let socket_path = entry.path();
            let status = if std::os::unix::net::UnixStream::connect(&socket_path).is_ok() {
                "running"
            } else {
                "stale"
            };
            println!("{}\t{}", session, status);
            found = true;
        }
    }

    if !found {
        println!("No active sessions.");
    }

    Ok(())
}
