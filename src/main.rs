use std::io::{IsTerminal, Read};
use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod client;
mod daemon;
mod keys;
mod protocol;
mod render;

use protocol::Request;

const LONG_ABOUT: &str = "\
ptywrap manages persistent PTY (pseudo-terminal) sessions that can be driven
from the command line. It is designed for LLMs and other non-interactive
programs that need to interact with full-screen terminal applications
(vim, htop, less, REPLs, etc.) which require a real TTY.

Each session is a daemonized process that owns a PTY, runs a command in it,
maintains an in-memory virtual terminal (80x24 by default), and exposes a
Unix socket at ~/.ptywrap/<session>.sock for control.

Typical workflow:
  ptywrap -s s start -- bash        # spawn bash in a fresh PTY
  ptywrap -s s write -e 'ls\\n'      # type 'ls' + ENTER (-e enables \\n)
  ptywrap -s s view --wait          # see the rendered screen
  ptywrap -s s send-key Ctrl-C      # interrupt
  ptywrap -s s stop                 # end the session

Input shortcuts:
  cat foo.txt | ptywrap -s s write  # pipe a file's contents into the PTY
  echo done   | ptywrap -s s write  # read stdin when DATA is omitted or '-'
  ptywrap -s s write -e 'hi\\n'      # -e/--escaped enables backslash
                                    #   escapes: \\n \\r \\t \\xHH \\uHHHH ...
  ptywrap -s s write -- --escaped   # use `--` to send DATA that starts
                                    #   with a dash
  ptywrap -s s send-key ^C h i      # ^X caret notation + single-char keys

Session lifetime:
  A session's daemon stays alive AFTER the child command exits, so you can
  still call `view`, `output`, `status`, and `screenshot` to inspect the
  final state. `status` reports the exit code (or signal) in that case.
  Run `ptywrap -s NAME stop` to release the session name for reuse.

Socket / pid files:
  ~/.ptywrap/<session>.sock and ~/.ptywrap/<session>.pid. These are removed
  when the daemon shuts down. If a stale socket is found at `start`, it is
  cleaned up automatically.";

#[derive(Parser)]
#[command(
    name = "ptywrap",
    about = "PTY session manager for driving interactive terminal programs",
    long_about = LONG_ABOUT,
    after_help = "Run `ptywrap help <COMMAND>` (or `ptywrap <COMMAND> --help`) for details on a subcommand.",
    verbatim_doc_comment
)]
struct Cli {
    /// Session name. Required for every subcommand except `list`.
    #[arg(long, short, global = true)]
    session: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start a new PTY session running COMMAND.
    ///
    /// Spawns a background daemon that owns a PTY of the given size and runs
    /// COMMAND inside it. The daemon survives the child's exit so its final
    /// terminal state can still be inspected; use `stop` to terminate it.
    ///
    /// `TERM` is set to `xterm-256color` for the child by default (the
    /// in-process emulator is xterm-ish, and this lets curses programs --
    /// vim/htop/less -- run directly without an `env TERM=...` wrapper).
    /// Override with `--term`.
    ///
    /// Fails if a session with the same name is already running. Place
    /// COMMAND and its arguments after `--` so flags meant for the child
    /// aren't consumed by ptywrap.
    ///
    /// Examples:
    ///   ptywrap -s s start -- bash
    ///   ptywrap -s s start --cols 120 --rows 40 -- bash -l
    ///   ptywrap -s s start --term screen-256color -- bash
    #[command(long_about, verbatim_doc_comment)]
    Start {
        /// Terminal width in columns.
        #[arg(long, default_value_t = 80)]
        cols: u16,
        /// Terminal height in rows.
        #[arg(long, default_value_t = 24)]
        rows: u16,
        /// Value of the TERM environment variable for the child.
        #[arg(long, default_value = "xterm-256color")]
        term: String,
        /// Command and arguments to run, after `--`.
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },

    /// Write text to the PTY's stdin.
    ///
    /// By default bytes are sent LITERALLY -- backslash sequences are NOT
    /// interpreted, so `\n` is the two characters '\' and 'n'.
    ///
    /// Pass -e/--escaped to interpret bash/zsh-style escape sequences:
    ///
    ///   \n  \r  \t  \a  \b  \f  \v   single control bytes
    ///   \e  \E                       ESC (0x1B)
    ///   \\                           literal backslash
    ///   \0                           NUL byte (0x00)
    ///   \xHH                         one raw byte, 1-2 hex digits
    ///   \uHHHH                       Unicode codepoint, 4 hex (UTF-8)
    ///   \UHHHHHHHH                   Unicode codepoint, 8 hex (UTF-8)
    ///
    /// Unknown sequences are left as a literal backslash followed by the
    /// next character.
    ///
    /// If no DATA argument is given (or it is exactly `-`), text is read
    /// from stdin. When stdin is a TTY and no argument is given, the command
    /// errors out instead of hanging.
    ///
    /// To send DATA that starts with a dash (e.g. `--escaped` or `-x`),
    /// put `--` before it so clap stops parsing flags:
    ///   ptywrap -s s write -- --escaped
    ///
    /// To send special keys (arrow keys, function keys, Ctrl-X), prefer
    /// `send-key`, which knows the right escape sequences.
    ///
    /// Examples:
    ///   ptywrap -s s write 'ls -la'           # 6 bytes, no newline
    ///   ptywrap -s s write -e 'ls -la\n'      # types 'ls -la' + ENTER
    ///   ptywrap -s s write -e 'snowman: ☃\n'
    ///   echo 'ls -la' | ptywrap -s s write    # reads from stdin
    ///   ptywrap -s s write -- --escaped       # sends literal text "--escaped"
    #[command(long_about, verbatim_doc_comment)]
    Write {
        /// Text to write. Omit or pass `-` to read from stdin.
        data: Option<String>,
        /// Interpret C-style escape sequences in DATA.
        #[arg(short = 'e', long = "escaped")]
        escaped: bool,
    },

    /// Send named keys (or single characters) to the PTY.
    ///
    /// Each argument is one keystroke. Recognised forms:
    ///
    ///   Named keys (case-insensitive):
    ///     Enter, Return, Tab, Escape/Esc, Space, Backspace/BS,
    ///     Delete/Del, Up, Down, Left, Right, Home, End,
    ///     PageUp/PgUp, PageDown/PgDn, Insert/Ins, F1..F12
    ///
    ///   Control keys:
    ///     Ctrl-C, C-c, ^C  (all equivalent; case-insensitive)
    ///     Caret notation also covers: ^@ (NUL), ^[ (ESC), ^\, ^], ^^,
    ///     ^_, and ^? (DEL/0x7F).
    ///
    ///   Single character: any one-character argument is sent as-is.
    ///     That includes letters, digits, punctuation, and a single space
    ///     (use shell quoting: send-key ' ' or `send-key Space`).
    ///
    /// Multiple arguments are concatenated in order, so:
    ///   ptywrap -s s send-key h i Space w o r l d Enter
    ///
    /// Unknown multi-character names error out so typos aren't silently
    /// turned into nothing.
    #[command(long_about, verbatim_doc_comment)]
    SendKey {
        /// Key names (and/or single characters) to send, in order.
        #[arg(required = true)]
        keys: Vec<String>,
    },

    /// Show the rendered terminal screen (what a human would see).
    ///
    /// Returns the current contents of the virtual terminal as plain text,
    /// preceded by a `[COLSxROWS cursor=(ROW,COL)]` header.
    ///
    /// Combine with --wait when the program you just drove is still
    /// painting: ptywrap will wait until no new bytes have arrived from
    /// the child for --settle ms (default 500), or until 30s pass.
    #[command(long_about, verbatim_doc_comment)]
    View {
        /// Wait for the child's output to settle before reading the screen.
        #[arg(long)]
        wait: bool,
        /// Milliseconds of quiet on the PTY required before view returns.
        /// Implies --wait. Default 500ms.
        #[arg(long)]
        settle: Option<u64>,
        /// Include ANSI color/style escape codes in the output.
        #[arg(long)]
        color: bool,
    },

    /// Show raw PTY output, including ANSI escape codes.
    ///
    /// Reads from an in-memory ring buffer of up to ~2MB of recent PTY
    /// output. Useful when you need the exact byte stream (timing, control
    /// sequences, scrollback) rather than the rendered screen.
    #[command(long_about, verbatim_doc_comment)]
    Output {
        /// Show only the last N output lines.
        #[arg(long)]
        tail: Option<usize>,
        /// Wait for output to settle first.
        #[arg(long)]
        wait: bool,
        /// Settle time in ms. Implies --wait. Default 500ms.
        #[arg(long)]
        settle: Option<u64>,
    },

    /// Wait until the child's PTY output has been quiet for a while.
    ///
    /// Useful as an explicit synchronization point between sending input
    /// and reading the screen. The command returns when no new bytes have
    /// arrived for --settle ms, or when --timeout ms have elapsed.
    #[command(long_about, verbatim_doc_comment)]
    Wait {
        /// Settle time in ms (no new bytes for this long).
        #[arg(long, default_value_t = 500)]
        settle: u64,
        /// Maximum total time to wait, in ms.
        #[arg(long, default_value_t = 30000)]
        timeout: u64,
    },

    /// Resize the PTY (cols x rows).
    ///
    /// Sends TIOCSWINSZ to the child. Programs that subscribe to SIGWINCH
    /// (vim, less, htop, etc.) will redraw at the new size.
    #[command(long_about, verbatim_doc_comment)]
    Resize {
        /// New width in columns.
        cols: u16,
        /// New height in rows.
        rows: u16,
    },

    /// Render the current terminal screen as a PNG file.
    ///
    /// Uses an 8x8 bitmap font scaled by --scale. Useful for visual
    /// inspection by a human or by a vision-capable LLM.
    #[command(long_about, verbatim_doc_comment)]
    Screenshot {
        /// Output PNG path.
        path: String,
        /// Pixel scale per glyph pixel (1 = 8x8 per char, 2 = 16x16, ...).
        #[arg(long, default_value_t = 2)]
        scale: u32,
        /// Wait for output to settle first.
        #[arg(long)]
        wait: bool,
        /// Settle time in ms. Implies --wait. Default 500ms.
        #[arg(long)]
        settle: Option<u64>,
    },

    /// Show session status (size, cursor, title, alive/exit, output bytes).
    ///
    /// After the child has exited, `alive: false` is reported along with
    /// `exit_status: ...` (e.g. `exited with code 0` or `killed by
    /// signal SIGTERM`).
    #[command(long_about, verbatim_doc_comment)]
    Status,

    /// Stop a session.
    ///
    /// Sends SIGHUP + SIGTERM to the child, waits briefly, then SIGKILL if
    /// it hasn't exited. Closes the PTY, removes the socket and pid files,
    /// and the daemon exits. The session name becomes available for reuse.
    #[command(long_about, verbatim_doc_comment)]
    Stop,

    /// List active session sockets in ~/.ptywrap.
    ///
    /// Each entry shows the session name and whether its daemon socket is
    /// reachable (`running`) or stale. Use `status` for more detail on a
    /// specific session.
    #[command(long_about, verbatim_doc_comment)]
    List,

    /// Print ptywrap's version.
    Version,
}

fn runtime_dir() -> anyhow::Result<PathBuf> {
    let home = std::env::var("HOME").map_err(|_| anyhow::anyhow!("HOME not set"))?;
    Ok(PathBuf::from(home).join(".ptywrap"))
}

fn read_stdin_bytes() -> anyhow::Result<Vec<u8>> {
    let mut buf = Vec::new();
    std::io::stdin().read_to_end(&mut buf)?;
    Ok(buf)
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

fn print_version() {
    println!("ptywrap {}", env!("CARGO_PKG_VERSION"));
}

fn main() -> anyhow::Result<()> {
    // Undocumented `-v`/`-V`/`--version` aliases of the `version`
    // subcommand. Pre-parsed so they don't need a visible clap arg.
    if let Some(arg) = std::env::args().nth(1).as_deref()
        && matches!(arg, "-v" | "-V" | "--version")
    {
        print_version();
        return Ok(());
    }

    let cli = Cli::parse();
    let dir = runtime_dir()?;

    match cli.command {
        Command::Version => {
            print_version();
        }
        Command::List => {
            list_sessions(&dir)?;
        }
        Command::Start {
            cols,
            rows,
            term,
            command,
        } => {
            let session = require_session(cli.session)?;
            daemon::start(&session, &command, cols, rows, &term, &dir)?;
        }
        cmd => {
            let session = require_session(cli.session)?;
            let socket_path = dir.join(format!("{}.sock", session));

            match cmd {
                Command::Write { data, escaped } => {
                    let raw: Vec<u8> = match data.as_deref() {
                        Some("-") => read_stdin_bytes()?,
                        Some(s) => s.as_bytes().to_vec(),
                        None => {
                            if std::io::stdin().is_terminal() {
                                anyhow::bail!(
                                    "no DATA argument and stdin is a TTY; pass text as an argument, pipe to stdin, or use '-'"
                                );
                            }
                            read_stdin_bytes()?
                        }
                    };
                    let bytes = if escaped {
                        let s = std::str::from_utf8(&raw).map_err(|_| {
                            anyhow::anyhow!("--escaped requires UTF-8 input; got non-UTF-8 bytes")
                        })?;
                        keys::interpret_escapes(s)
                    } else {
                        raw
                    };
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
            let status = session_status(&socket_path);
            println!("{}\t{}", session, status);
            found = true;
        }
    }

    if !found {
        println!("No active sessions.");
    }

    Ok(())
}

/// Ask the daemon for its session status and reduce it to a one-word
/// (or short-phrase) label suitable for the `list` output.
fn session_status(socket_path: &std::path::Path) -> String {
    match client::send(socket_path, &Request::Status) {
        Ok(resp) if resp.success => {
            let data = resp.data.unwrap_or_default();
            if data.lines().any(|l| l.trim() == "alive: true") {
                return "running".to_string();
            }
            data.lines()
                .find_map(|l| l.strip_prefix("exit_status: "))
                .map(|s| s.to_string())
                .unwrap_or_else(|| "exited".to_string())
        }
        _ => "stale".to_string(),
    }
}
