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
# Start a session running bash (TERM defaults to xterm-256color)
ptywrap -s mysession start -- bash

# Run a command (-e enables \n interpretation; alternative: send-key Enter)
ptywrap -s mysession write -e 'ls -la\n'

# View the terminal screen (waits for output to settle by default)
ptywrap -s mysession view
ptywrap -s mysession view --no-wait    # ... or skip the settle wait

# Launch an interactive program directly (no `env TERM=...` needed)
ptywrap -s mysession start -- htop
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
ptywrap -s NAME start [--cols 80] [--rows 24] [--term xterm-256color] -- COMMAND [ARGS...]
ptywrap -s NAME stop
ptywrap -s NAME status
ptywrap list
```

`start` exits non-zero (with the underlying error) if COMMAND fails to
exec — e.g. binary not found.

### Input

```sh
# Write text literally (bytes sent as-is, no escape interpretation)
ptywrap -s NAME write 'echo hello'

# Pipe text from stdin (no DATA argument, or pass '-')
echo 'echo hello' | ptywrap -s NAME write
cat script.txt    | ptywrap -s NAME write

# Send DATA that starts with a dash: put `--` first
ptywrap -s NAME write -- --escaped

# Interpret bash/zsh-style escape sequences with -e/--escaped
ptywrap -s NAME write -e 'echo hello\n'
ptywrap -s NAME write -e 'snowman: ☃\n'

# Supported escapes (with -e):
#   \n \r \t \a \b \f \v          control bytes
#   \e \E                          ESC (0x1B)
#   \\                             literal backslash
#   \0                             NUL byte
#   \xHH                           one raw byte (1-2 hex)
#   \uHHHH \UHHHHHHHH              Unicode codepoint (UTF-8)

# Send named keys
ptywrap -s NAME send-key Enter Tab Escape Up Down Left Right
ptywrap -s NAME send-key Ctrl-C Ctrl-D Ctrl-Z Ctrl-L
ptywrap -s NAME send-key ^C ^D ^Z ^L           # caret notation also works
ptywrap -s NAME send-key Home End PageUp PageDown Backspace Delete
ptywrap -s NAME send-key F1 F2 ... F12

# Single-character arguments are sent as-is (letters, digits, punctuation)
ptywrap -s NAME send-key h i Space w o r l d Enter
```

Multiple keys can be sent in one call: `send-key Up Up Enter`

### Output

```sh
# View the rendered terminal screen (what a human would see).
# Waits for output to settle by default (no new bytes for --settle ms).
ptywrap -s NAME view [--settle 500]

# Skip the settle wait and return the current screen immediately
ptywrap -s NAME view --no-wait

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
- The daemon stays alive AFTER the child exits, so the final screen,
  output buffer, and exit status remain queryable; `status` reports
  the exit code (or signal)
- `stop` sends SIGHUP+SIGTERM to the child (SIGKILL as fallback), then
  cleans up the socket and PID files and exits

No central daemon process is needed. Sessions are fully independent.

## Example: LLM workflow

An LLM can use ptywrap like this:

```
# Start a shell session
$ ptywrap -s work start -- bash

# Run a command and see the result
$ ptywrap -s work write -e 'git status\n'
$ ptywrap -s work view --wait
[80x24 cursor=(8,2)]
On branch main
Changes not staged for commit:
  modified:   src/main.rs

# Edit a file with vim
$ ptywrap -s work write -e 'vim src/main.rs\n'
$ ptywrap -s work view --wait
[80x24 cursor=(0,0)]
use std::path::PathBuf;
...

# Navigate and edit
$ ptywrap -s work write -e '/fn main\n'     # search
$ ptywrap -s work write -e 'olet x = 42;\e' # insert line, back to normal mode
$ ptywrap -s work write -e ':wq\n'          # save and quit
$ ptywrap -s work view --wait
```

## License

MIT
