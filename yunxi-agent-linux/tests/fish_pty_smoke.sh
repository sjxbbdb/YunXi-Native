#!/usr/bin/env bash
set -euo pipefail

# Real fish + pseudo-terminal regression test for the generated hook.
# The fake binary only replaces the provider/daemon side; fish itself and the
# generated hook run for real, so this catches prompt/event/Enter regressions.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BINARY="${1:-${ROOT_DIR}/target/release/yunxi-linux}"

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
  shell-classify)
    input="$(cat)"
    logged_input="${input//$'\n'/\\n}"
    printf 'classify:%s\n' "$logged_input" >> "${YUNXI_FAKE_LOG:?}"
    case "$input" in
      printf\ *|echo\ *|cd\ *|pwd|true|false|exit|sleep\ *|function\ *) exit 0 ;;
      # Force this otherwise-missing command through fish once so the PTY
      # matrix exercises fish_command_not_found itself. Production classify
      # still rejects unknown commands; this fixture isolates the callback.
      command_not_found_probe) exit 0 ;;
      *) exit 1 ;;
    esac
    ;;
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
"$BINARY" fish-init --print | sed "s#$(printf '%s' "$BINARY" | sed 's/[.[\*^$()+?{|\\]/\\&/g')#$FAKE_BIN#g" >"$HOOK"

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
import pty
import select
import sys
import time

log_path = sys.argv[1]
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
os.write(fd, b"set h (__yunxi_first_token_raw \"printf 'fish-command\\n'\"); echo h:$h; __yunxi_fish_knows_head $h; echo knows:$status\r")
read_until(b"knows:")
read_until(b"> ")
os.write(fd, b"printf 'fish-command\\n'\r")
read_until(b"fish-command")
read_until(b"> ")

# Fish-defined aliases/functions must stay on the shell side even though the
# Rust PATH probe cannot see them.
os.write(fd, b"alias yunxi_alias 'printf alias-ok\\n'\r")
read_until(b"> ")
os.write(fd, b"yunxi_alias\r")
read_until(b"alias-ok")
read_until(b"> ")
os.write(fd, b"function yunxi_fn; printf function-ok\\n; end\r")
read_until(b"> ")
os.write(fd, b"yunxi_fn\r")
read_until(b"function-ok")
read_until(b"> ")

# Fish syntax must stay on the fish side: command substitutions, redirections,
# and pipelines execute for real and never reach shell-intercept.
os.write(fd, b"printf 'sub:%s\\n' (printf nested)\r")
read_until(b"sub:nested")
read_until(b"> ")
os.write(fd, b"printf 'redirect-ok\\n' > \"$YUNXI_TEST_FILE\"; cat \"$YUNXI_TEST_FILE\"\r")
read_until(b"redirect-ok")
read_until(b"> ")
os.write(fd, b"printf 'pipe-ok\\n' | cat\r")
read_until(b"pipe-ok")
read_until(b"> ")

# An unknown first command is intercepted before fish executes it. A second
# fixture forces classification success so fish invokes command_not_found and
# the generated callback is exercised without relying on PATH races.
os.write(fd, b"missing_yunxi_command\r")
read_until(b"[yunxi intercepted] missing_yunxi_command")
read_until(b"> ")
os.write(fd, b"command_not_found_probe\r")
read_until(b"[yunxi intercepted] command_not_found_probe")
read_until(b"> ")
os.write(fd, b"echo missing-status:$status\r")
read_until(b"missing-status:127")
read_until(b"> ")

os.write(fd, "你好，帮我看看项目".encode() + b"\r")
read_until(b"[yunxi intercepted]")
read_until(b"> ")

# Ctrl+J inserts a newline; the completed two-line natural-language buffer is
# still intercepted once when Enter is pressed.
os.write(fd, "第一行".encode())
time.sleep(0.1)
os.write(fd, "\x0a第二行".encode() + b"\r")
read_until("[yunxi intercepted]".encode())
read_until(b"> ")

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

assert any(line.startswith("intercept:fish-") and line.endswith(":你好，帮我看看项目") for line in lines), lines
assert any(line.startswith("intercept:fish-") and line.endswith(":第一行\\n第二行") for line in lines), lines
assert any(line.startswith("intercept:fish-") and line.endswith(":missing_yunxi_command") for line in lines), lines
assert any(line.startswith("intercept:fish-") and line.endswith(":command_not_found_probe") for line in lines), lines
assert not any(
    line.startswith("intercept:")
    and any(token in line for token in ("sub:%s", "redirect-ok", "pipe-ok"))
    for line in lines
), lines
assert not any(":printf" in line for line in lines if line.startswith("intercept:")), lines
print("fish-pty-smoke=ok")
PY
