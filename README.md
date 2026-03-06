# ptywrap

A CLI tool that lets LLMs (and other non-interactive programs) drive interactive
terminal applications. It manages persistent PTY sessions that can be controlled
via simple commands -- write text, send keys, read the screen, and more.

## Use case

LLMs like Claude can run shell commands, but they can't interact with full-screen
terminal programs (vim, htop, top, etc.) because those require a real PTY with
cursor addressing and keyboard input. ptywrap bridges this gap by:

- Running commands in a real PTY with terminal emulation
- Exposing the rendered screen as a character grid (what a human would see)
- Accepting text and named keystrokes as input
- Persisting sessions across multiple invocations

## Installation

```sh
cargo build --release
cp target/release/ptywrap /usr/local/bin/
```

Requires Rust 1.70+. Works on macOS and Linux.

## Quick start

```sh
# Start a session running bash
ptywrap -s mysession start -- bash

# Run a command
ptywrap -s mysession write 'ls -la\n'

# View the terminal screen (waits for output to settle first)
ptywrap -s mysession view --wait

# Launch an interactive program
ptywrap -s mysession write 'htop\n'
ptywrap -s mysession view --wait

# Send special keys
ptywrap -s mysession send-key F2        # open htop setup
ptywrap -s mysession send-key Up Up Enter
ptywrap -s mysession send-key q         # quit htop

# Stop the session
ptywrap -s mysession stop
```

## Commands

### Session management

```sh
ptywrap -s NAME start [--cols 80] [--rows 24] -- COMMAND [ARGS...]
ptywrap -s NAME stop
ptywrap -s NAME status
ptywrap list
```

### Input

```sh
# Write text with C-style escape sequences
ptywrap -s NAME write 'echo hello\n'

# Supported escapes: \n \r \t \\ \e (ESC) \0 \a (BEL) \xNN (hex byte)

# Send named keys
ptywrap -s NAME send-key Enter Tab Escape Up Down Left Right
ptywrap -s NAME send-key Ctrl-C Ctrl-D Ctrl-Z Ctrl-L
ptywrap -s NAME send-key Home End PageUp PageDown Backspace Delete
ptywrap -s NAME send-key F1 F2 ... F12
```

Multiple keys can be sent in one call: `send-key Up Up Enter`

### Output

```sh
# View the rendered terminal screen (what a human would see)
ptywrap -s NAME view

# Wait for output to settle, then view
ptywrap -s NAME view --wait [--settle 500]

# Show raw PTY output (includes ANSI escape codes)
ptywrap -s NAME output [--tail 100] [--wait]
```

### Other

```sh
# Resize the terminal
ptywrap -s NAME resize 120 40

# Wait for output to settle (no new output for N ms)
ptywrap -s NAME wait [--settle 500] [--timeout 30000]
```

## Architecture

Each session runs as an independent daemon process:

- `start` forks a background daemon that creates a PTY and spawns the command
- The daemon maintains a virtual terminal emulator ([vt100](https://crates.io/crates/vt100))
  and a 2MB ring buffer of raw output
- Communication happens via a Unix domain socket at `~/.ptywrap/SESSION.sock`
- The CLI client connects, sends a JSON request, and reads the response
- `stop` sends SIGHUP+SIGTERM to the child process (SIGKILL as fallback)
- When the child exits, the daemon cleans up its socket and PID files

No central daemon process is needed. Sessions are fully independent.

## Example: LLM workflow

An LLM can use ptywrap like this:

```
# Start a shell session
$ ptywrap -s work start -- bash

# Run a command and see the result
$ ptywrap -s work write 'git status\n'
$ ptywrap -s work view --wait
[80x24 cursor=(8,2)]
On branch main
Changes not staged for commit:
  modified:   src/main.rs

# Edit a file with vim
$ ptywrap -s work write 'vim src/main.rs\n'
$ ptywrap -s work view --wait
[80x24 cursor=(0,0)]
use std::path::PathBuf;
...

# Navigate and edit
$ ptywrap -s work write '/fn main\n'     # search
$ ptywrap -s work write 'olet x = 42;\e' # insert line, back to normal mode
$ ptywrap -s work write ':wq\n'           # save and quit
$ ptywrap -s work view --wait
```

## License

MIT
