#!/usr/bin/env bash
set -euo pipefail

# Real fish + pseudo-terminal regression test for the generated all-takeover
# hook. The fake binary only replaces the provider/daemon side; fish itself
# and the generated hook run for real, so this catches prompt/event/Enter
# regressions.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BINARY="${1:-${ROOT_DIR}/target/release/yunxi-linux}"
MODE="${2:-}"
if [[ -n "$MODE" && "$MODE" != "--takeover" ]]; then
  echo "usage: $0 [binary] [--takeover]" >&2
  exit 2
fi

command -v fish >/dev/null || { echo "fish is required" >&2; exit 77; }
command -v python3 >/dev/null || { echo "python3 is required" >&2; exit 77; }
test -x "$BINARY" || { echo "release binary not found: $BINARY" >&2; exit 77; }
# `fish-init --print` resolves the running executable to an absolute path.  Use
# the same canonical spelling for the replacement below so callers may pass a
# convenient relative path such as `./target/release/yunxi-linux`.
BINARY="$(cd "$(dirname "$BINARY")" && pwd)/$(basename "$BINARY")"

TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT
FAKE_BIN="$TMP_ROOT/fake-yunxi"
FAKE_LOG="$TMP_ROOT/fake.log"
HOME_DIR="$TMP_ROOT/home"
mkdir -p "$HOME_DIR/.config/fish/conf.d"

cat >"$FAKE_BIN" <<'FAKE'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  shell-intercept)
    shift
    input="$(cat)"
    session="missing"
    while [ "$#" -gt 0 ]; do
      if [ "${1:-}" = "--session-id" ]; then
        session="${2:-missing}"
        shift 2
      else
        shift
      fi
    done
    input="${input//$'\n'/\\n}"
    printf 'intercept:%s:%s\n' "$session" "$input" >> "${YUNXI_FAKE_LOG:?}"
    printf '[yunxi intercepted] %s\n' "$input"
    ;;
  *)
    echo "unexpected fake command: $*" >&2
    exit 2
    ;;
esac
FAKE
chmod +x "$FAKE_BIN"

# Generate the real hook, then point only its executable at the deterministic
# fake. This preserves the production fish functions and bindings verbatim.
HOOK="$HOME_DIR/.config/fish/conf.d/yunxi.fish"
HOOK_ARGS=(fish-init --print)
if [[ "$MODE" == "--takeover" ]]; then
  # Kept as a compatibility invocation; fish-init is already all-takeover.
  HOOK_ARGS+=(--takeover)
fi
"$BINARY" "${HOOK_ARGS[@]}" | sed "s#$(printf '%s' "$BINARY" | sed 's/[.[\*^$()+?{|\\]/\\&/g')#$FAKE_BIN#g" >"$HOOK"

export HOME="$HOME_DIR"
export XDG_CONFIG_HOME="$HOME_DIR/.config"
export YUNXI_FAKE_LOG="$FAKE_LOG"
export YUNXI_TEST_FILE="$TMP_ROOT/redirection.txt"

if ! fish -i -c 'functions __yunxi_accept_line' >/dev/null 2>&1; then
  echo "generated fish hook was not loaded" >&2
  sed -n '1,12p' "$HOOK" >&2
  exit 1
fi

python3 - "$FAKE_LOG" <<'PY'
import os
import fcntl
import pty
import select
import struct
import sys
import termios
import time

log_path = sys.argv[1]
transcript = bytearray()
pid, fd = pty.fork()
if pid == 0:
    os.environ["TERM"] = "dumb"
    os.environ["YUNXI_SHELL_SESSION"] = "pty-test"
    os.execlp("fish", "fish", "-i")

def read_until(needle: bytes, timeout: float = 4.0) -> bytes:
    data = bytearray()
    deadline = time.time() + timeout
    while time.time() < deadline:
        ready, _, _ = select.select([fd], [], [], 0.1)
        if not ready:
            continue
        try:
            chunk = os.read(fd, 4096)
        except OSError:
            break
        if not chunk:
            break
        data.extend(chunk)
        transcript.extend(chunk)
        if needle in data:
            # Fish may repaint the prompt in several writes.  Let the final
            # repaint and the hook's prompt event settle before the next
            # command is injected, otherwise a fast host can make this PTY
            # smoke race its own input queue.
            time.sleep(0.15)
            return bytes(data)
    try:
        with open(log_path, encoding="utf-8") as handle:
            debug_log = handle.read()
    except FileNotFoundError:
        debug_log = "<missing>"
    raise AssertionError(f"timed out waiting for {needle!r}; got {bytes(data)!r}; log={debug_log!r}")

read_until(b"> ")

# YunXi owns every submitted buffer, including shell-looking text and fish
# syntax. Fish remains the line editor and prompt host. Empty submits and
# Ctrl+C while editing stay local to fish, and a resize must not change routing.
os.write(fd, b"\r")
read_until(b"> ")
os.write(fd, b"draft")
time.sleep(0.1)
os.write(fd, b"\x03")
read_until(b"> ")
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 48, 140, 0, 0))
before = len(transcript)
os.write(fd, b"ls -la\r")
read_until(b"[yunxi intercepted] ls -la")
read_until(b"> ")
redraw = bytes(transcript[before:])
assert b"\x1b[?25l" in redraw and b"\x1b[?25h" in redraw, redraw
assert redraw.index(b"\x1b[?25l") < redraw.index(b"\x1b[?25h"), redraw
assert b"\x1b[1A\x1b[" in redraw, redraw
os.write(fd, b"printf 'sub:%s\\n' (printf nested)\r")
read_until(b"[yunxi intercepted] printf 'sub:%s\\n' (printf nested)")
read_until(b"> ")
os.write(fd, b"printf 'redirect-ok\\n' > \"$YUNXI_TEST_FILE\"; cat \"$YUNXI_TEST_FILE\"\r")
read_until(b"[yunxi intercepted] printf 'redirect-ok\\n'")
read_until(b"> ")
os.write(fd, b"printf 'pipe-ok\\n' | cat\r")
read_until(b"[yunxi intercepted] printf 'pipe-ok\\n' | cat")
read_until(b"> ")
os.write(fd, b"alias yunxi_alias 'printf alias-ok\\n'\r")
read_until(b"[yunxi intercepted] alias yunxi_alias")
read_until(b"> ")
os.write(fd, "你好，帮我看看项目".encode() + b"\r")
read_until(b"[yunxi intercepted]")
read_until(b"> ")
before = len(transcript)
os.write(fd, "第一行".encode())
time.sleep(0.1)
os.write(fd, "\x0a第二行".encode() + b"\r")
read_until(b"[yunxi intercepted]")
read_until(b"> ")
multiline = bytes(transcript[before:])
assert b"\x1b[?25l" in multiline and b"\x1b[?25h" in multiline, multiline
assert "第一行".encode() in multiline, multiline
assert "  第二行".encode() in multiline, multiline

os.write(fd, b"\x04")
_, status = os.waitpid(pid, 0)
if status != 0:
    try:
        with open(log_path, encoding="utf-8") as handle:
            debug_log = handle.read()
    except FileNotFoundError:
        debug_log = "<missing>"
    raise AssertionError(f"fish exited with status {status}; log={debug_log!r}")

with open(log_path, encoding="utf-8") as handle:
    lines = [line.strip() for line in handle if line.strip()]

assert any(line.startswith("intercept:fish-") and line.endswith(":ls -la") for line in lines), lines
assert any(line.startswith("intercept:fish-") and line.endswith(":printf 'sub:%s\\n' (printf nested)") for line in lines), lines
assert any(line.startswith("intercept:fish-") and line.endswith(":printf 'redirect-ok\\n' > \"$YUNXI_TEST_FILE\"; cat \"$YUNXI_TEST_FILE\"") for line in lines), lines
assert any(line.startswith("intercept:fish-") and line.endswith(":printf 'pipe-ok\\n' | cat") for line in lines), lines
assert any(line.startswith("intercept:fish-") and line.endswith(":alias yunxi_alias 'printf alias-ok\\n'") for line in lines), lines
assert any(line.startswith("intercept:fish-") and line.endswith(":你好，帮我看看项目") for line in lines), lines
assert any(line.startswith("intercept:fish-") and line.endswith(":第一行\\n第二行") for line in lines), lines
assert not any("这条请求会被取消" in line for line in lines if line.startswith("intercept:")), lines
assert len([line for line in lines if line.startswith("intercept:")]) == 7, lines
assert not any(line.startswith("classify:") for line in lines), lines
print("fish-pty-smoke=ok")
PY
