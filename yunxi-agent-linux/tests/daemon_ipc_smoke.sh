#!/usr/bin/env bash
set -euo pipefail

# Real Unix-socket smoke for the Linux daemon.  It deliberately uses a
# deterministic provider-selection error instead of a live model, so the test
# validates the protocol and lifecycle without credentials or network access.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BINARY="${1:-${ROOT_DIR}/target/release/yunxi-linux}"

command -v python3 >/dev/null || {
  echo "python3 is required" >&2
  exit 77
}
test -x "$BINARY" || {
  echo "release binary not found: $BINARY" >&2
  exit 77
}

TMP_ROOT="$(mktemp -d)"
RUNTIME_DIR="$TMP_ROOT/runtime"
mkdir -p "$RUNTIME_DIR"
export XDG_RUNTIME_DIR="$RUNTIME_DIR"
export HOME="$TMP_ROOT/home"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_STATE_HOME="$HOME/.local/state"
mkdir -p "$HOME" "$XDG_CONFIG_HOME" "$XDG_STATE_HOME"
SOCKET="$RUNTIME_DIR/yunxi/yunxi.sock"
DAEMON_LOG="$TMP_ROOT/daemon.log"
DAEMON_PID=""

cleanup() {
  if [[ -n "$DAEMON_PID" ]] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    kill -TERM "$DAEMON_PID" 2>/dev/null || true
    wait "$DAEMON_PID" 2>/dev/null || true
  fi
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

"$BINARY" daemon >"$DAEMON_LOG" 2>&1 &
DAEMON_PID=$!

for _ in $(seq 1 40); do
  [[ -S "$SOCKET" ]] && break
  sleep 0.05
done
[[ -S "$SOCKET" ]] || {
  echo "daemon socket did not appear" >&2
  cat "$DAEMON_LOG" >&2
  exit 1
}

python3 - "$SOCKET" <<'PY'
import json
import socket
import struct
import sys

socket_path = sys.argv[1]
protocol_version = 2


def send(sock, frame):
    payload = json.dumps(frame, ensure_ascii=False, separators=(",", ":")).encode()
    sock.sendall(struct.pack(">I", len(payload)) + payload)


def recv_exact(sock, size):
    chunks = bytearray()
    while len(chunks) < size:
        chunk = sock.recv(size - len(chunks))
        if not chunk:
            raise AssertionError("daemon closed the socket before the frame completed")
        chunks.extend(chunk)
    return bytes(chunks)


def recv(sock):
    length = struct.unpack(">I", recv_exact(sock, 4))[0]
    assert 0 < length <= 24 * 1024 * 1024, length
    return json.loads(recv_exact(sock, length))


def hello(sock):
    send(
        sock,
        {
            "kind": "hello",
            "protocol_version": protocol_version,
            "client": "yunxi-daemon-smoke",
            "capabilities": ["ping", "turn", "follow"],
        },
    )
    frame = recv(sock)
    assert frame["kind"] == "hello_ack", frame
    assert frame["protocol_version"] == protocol_version, frame


with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
    sock.connect(socket_path)
    hello(sock)
    send(sock, {"kind": "ping", "request_id": "ping-1"})
    assert recv(sock) == {"kind": "pong", "request_id": "ping-1"}

with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
    sock.connect(socket_path)
    hello(sock)
    send(
        sock,
        {
            "kind": "follow",
            "run_id": "missing-run",
            "session_id": None,
            "after_seq": 0,
        },
    )
    frame = recv(sock)
    assert frame["kind"] == "resync_required", frame
    assert frame["run_id"] == "missing-run", frame

with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
    sock.connect(socket_path)
    hello(sock)
    send(
        sock,
        {
            "kind": "turn",
            "request_id": "failure-1",
            "cwd": "/tmp",
            "prompt": "deterministic smoke failure",
            "session_id": None,
            "offline": True,
            "live": True,
            "provider": None,
            "model": None,
        },
    )
    accepted = recv(sock)
    assert accepted["kind"] == "run_accepted", accepted
    error = recv(sock)
    assert error["kind"] == "error", error
    assert "--offline" in error["message"] and "--live" in error["message"], error

print("daemon-ipc-smoke=ok")
PY

kill -TERM "$DAEMON_PID"
set +e
wait "$DAEMON_PID"
STATUS=$?
set -e
DAEMON_PID=""
if [[ "$STATUS" -ne 0 ]]; then
  echo "daemon did not exit cleanly after SIGTERM: status=$STATUS" >&2
  cat "$DAEMON_LOG" >&2
  exit "$STATUS"
fi
[[ ! -e "$SOCKET" ]] || {
  echo "daemon socket remained after SIGTERM" >&2
  exit 1
}
trap - EXIT
rm -rf "$TMP_ROOT"
echo "daemon-lifecycle-smoke=ok"
